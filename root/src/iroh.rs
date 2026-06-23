use std::{
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::{SystemTime, UNIX_EPOCH},
};

use anyhow::{Context, Result, anyhow, bail};
use bytes::Bytes;
use fjall::{Database, Keyspace, KeyspaceCreateOptions, PersistMode};
use iroh::{
    Endpoint, EndpointId, SecretKey,
    endpoint::{Connection, presets},
    protocol::{AcceptError, ProtocolHandler, Router},
};
use iroh_blobs::{Hash, api::Store, store::fs::FsStore};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use smol_str::SmolStr;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

pub const ALPN: &[u8] = b"zust-root/iroh/0";
const MAX_FRAME_SIZE: u32 = 16 * 1024 * 1024;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct IrohSummary {
    pub id: String,
    pub name: String,
    pub hash: String,
    pub size: u64,
    pub modified_ms: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum Request {
    Ping,
    List,
    Get { id: String },
    Push { id: String, name: String, size: u64, modified_ms: u64 },
    Pull { id: String },
    Delete { id: String },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum Response {
    Pong { node_id: String },
    List { values: Vec<IrohSummary> },
    Record { value: IrohSummary },
    Pull { value: IrohSummary, size: u64 },
    Pushed { value: IrohSummary },
    Deleted { id: String },
    Error { message: String },
}

impl Response {
    fn into_result(self) -> Result<Self> {
        match self {
            Self::Error { message } => Err(anyhow!(message)),
            other => Ok(other),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
struct IrohRecord {
    id: String,
    name: String,
    hash: String,
    size: u64,
    modified_ms: u64,
    created_ms: u64,
}

impl IrohRecord {
    fn summary(&self) -> IrohSummary {
        IrohSummary { id: self.id.clone(), name: self.name.clone(), hash: self.hash.clone(), size: self.size, modified_ms: self.modified_ms }
    }
}

#[derive(Clone)]
pub struct IrohStore {
    root: PathBuf,
    db: Database,
    values: Keyspace,
    blobs: FsStore,
}

impl IrohStore {
    pub async fn open(root: impl AsRef<Path>) -> Result<Self> {
        let root = root.as_ref().to_path_buf();
        tokio::fs::create_dir_all(root.join("kv")).await?;
        tokio::fs::create_dir_all(root.join("blobs")).await?;
        let db = Database::builder(root.join("kv")).open().context("open iroh root fjall store")?;
        let values = db.keyspace("values", KeyspaceCreateOptions::default)?;
        let blobs = FsStore::load(root.join("blobs")).await.context("open iroh root blob cache")?;
        Ok(Self { root, db, values, blobs })
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn blob_store(&self) -> Store {
        self.blobs.clone().into()
    }

    pub async fn put_bytes(&self, id: String, name: String, bytes: Bytes, modified_ms: u64) -> Result<IrohSummary> {
        let tag = self.blobs.blobs().add_bytes(bytes.clone()).await?;
        let record = IrohRecord { id, name, hash: tag.hash.to_string(), size: bytes.len() as u64, modified_ms, created_ms: now_ms() };
        self.put_record(&record)?;
        Ok(record.summary())
    }

    pub async fn bytes_for(&self, id: &str) -> Result<(IrohSummary, Bytes)> {
        let record = self.get_record(id)?.ok_or_else(|| anyhow!("iroh root value not found: {id}"))?;
        let hash: Hash = record.hash.parse().with_context(|| format!("parse blob hash {}", record.hash))?;
        let mut reader = self.blobs.blobs().reader(hash);
        let mut bytes = Vec::new();
        reader.read_to_end(&mut bytes).await?;
        Ok((record.summary(), Bytes::from(bytes)))
    }

    pub fn get(&self, id: &str) -> Result<Option<IrohSummary>> {
        Ok(self.get_record(id)?.map(|record| record.summary()))
    }

    pub fn list(&self) -> Result<Vec<IrohSummary>> {
        let mut values = Vec::new();
        for item in self.values.iter() {
            let (_, value) = item.into_inner()?;
            let record: IrohRecord = serde_json::from_slice(value.as_ref())?;
            values.push(record.summary());
        }
        values.sort_by(|a, b| a.id.cmp(&b.id));
        Ok(values)
    }

    pub fn delete(&self, id: &str) -> Result<bool> {
        let existed = self.values.get(id.as_bytes())?.is_some();
        self.values.remove(id.as_bytes())?;
        self.db.persist(PersistMode::SyncAll)?;
        Ok(existed)
    }

    pub async fn shutdown(&self) -> Result<()> {
        self.blobs.shutdown().await?;
        self.db.persist(PersistMode::SyncAll)?;
        Ok(())
    }

    fn put_record(&self, record: &IrohRecord) -> Result<()> {
        self.values.insert(record.id.as_bytes(), serde_json::to_vec(record)?)?;
        self.db.persist(PersistMode::SyncAll)?;
        Ok(())
    }

    fn put_summary(&self, summary: IrohSummary) -> Result<()> {
        self.put_record(&IrohRecord {
            id: summary.id,
            name: summary.name,
            hash: summary.hash,
            size: summary.size,
            modified_ms: summary.modified_ms,
            created_ms: now_ms(),
        })
    }

    fn get_record(&self, id: &str) -> Result<Option<IrohRecord>> {
        self.values.get(id.as_bytes())?.map(|value| serde_json::from_slice(value.as_ref()).context("decode iroh root record")).transpose()
    }
}

#[derive(Clone)]
pub struct IrohClient {
    remote: EndpointId,
    cache: IrohStore,
    list_refreshing: Arc<AtomicBool>,
}

impl IrohClient {
    pub fn new(remote: EndpointId) -> Result<Self> {
        let cache = block_on(async { IrohStore::open(default_cache_dir()).await })?;
        Ok(Self { remote, cache, list_refreshing: Arc::new(AtomicBool::new(false)) })
    }

    pub fn with_cache(remote: EndpointId, cache_dir: impl AsRef<Path>) -> Result<Self> {
        let cache_dir = cache_dir.as_ref().to_path_buf();
        let cache = block_on(async move { IrohStore::open(cache_dir).await })?;
        Ok(Self { remote, cache, list_refreshing: Arc::new(AtomicBool::new(false)) })
    }

    pub fn put_bytes(&self, id: &str, bytes: Bytes) -> Result<()> {
        let cache = self.cache.clone();
        let remote = self.remote;
        let id = id.to_string();
        let name = leaf_name(&id);
        block_on(async move {
            let modified_ms = now_ms();
            cache.put_bytes(id.clone(), name.clone(), bytes.clone(), modified_ms).await?;
            let (endpoint, conn) = connect(remote).await?;
            let response = push_bytes(&conn, id, name, bytes, modified_ms).await;
            close(endpoint, conn).await;
            response.map(|_| ())
        })
    }

    pub fn get_bytes(&self, id: &str) -> Result<Bytes> {
        if let Ok((_, bytes)) = block_on({
            let cache = self.cache.clone();
            let id = id.to_string();
            async move { cache.bytes_for(&id).await }
        }) {
            return Ok(bytes);
        }
        let cache = self.cache.clone();
        let remote = self.remote;
        let id = id.to_string();
        block_on(async move {
            let (endpoint, conn) = connect(remote).await?;
            let (summary, bytes) = pull_bytes(&conn, id).await?;
            close(endpoint, conn).await;
            cache.put_bytes(summary.id, summary.name, bytes.clone(), summary.modified_ms).await?;
            Ok(bytes)
        })
    }

    pub fn contains(&self, id: &str) -> bool {
        self.cache.get(id).ok().flatten().is_some()
            || block_on({
                let remote = self.remote;
                let id = id.to_string();
                async move {
                    let (endpoint, conn) = connect(remote).await?;
                    let found = matches!(request(&conn, Request::Get { id }).await?.into_result()?, Response::Record { .. });
                    close(endpoint, conn).await;
                    Ok(found)
                }
            })
            .unwrap_or(false)
    }

    pub fn delete(&self, id: &str) -> Result<()> {
        let _ = self.cache.delete(id);
        let remote = self.remote;
        let id = id.to_string();
        block_on(async move {
            let (endpoint, conn) = connect(remote).await?;
            request(&conn, Request::Delete { id }).await?.into_result()?;
            close(endpoint, conn).await;
            Ok(())
        })
    }

    pub fn dir(&self, name: &str) -> Result<Vec<SmolStr>> {
        let prefix = if name.is_empty() || name.ends_with('/') { name.to_string() } else { format!("{name}/") };
        let raw = self.dir_raw(&prefix)?;
        Ok(dir_entries_from_raw(&prefix, raw))
    }

    pub fn dir_raw(&self, name: &str) -> Result<Vec<SmolStr>> {
        let remote = self.remote;
        let prefix = name.to_string();
        let names = summaries_with_prefix(&prefix, self.cache.list().unwrap_or_default());
        if !self.list_refreshing.swap(true, Ordering::AcqRel) {
            let cache = self.cache.clone();
            let refreshing = self.list_refreshing.clone();
            std::thread::spawn(move || {
                let _ = block_on(async move {
                    let (endpoint, conn) = connect(remote).await?;
                    let response = request(&conn, Request::List).await?.into_result()?;
                    close(endpoint, conn).await;
                    let Response::List { values } = response else {
                        bail!("unexpected iroh list response");
                    };
                    for value in values {
                        cache.put_summary(value)?;
                    }
                    Ok::<(), anyhow::Error>(())
                });
                refreshing.store(false, Ordering::Release);
            });
        }
        Ok(names)
    }
}

fn summaries_with_prefix(prefix: &str, values: Vec<IrohSummary>) -> Vec<SmolStr> {
    let mut names = values.into_iter().filter_map(|value| if prefix.is_empty() || value.id.starts_with(prefix) { Some(value.id.into()) } else { None }).collect::<Vec<SmolStr>>();
    names.sort();
    names.dedup();
    names
}

pub async fn run_daemon(root: impl AsRef<Path>, secret_key: SecretKey) -> Result<()> {
    let store = IrohStore::open(root).await?;
    let endpoint = Endpoint::builder(presets::N0).secret_key(secret_key).bind().await?;
    let node_id = endpoint.id();
    let blobs = iroh_blobs::BlobsProtocol::new(&store.blob_store(), None);
    let router = Router::builder(endpoint).accept(ALPN, IrohRootProtocol { store: store.clone(), node_id }).accept(iroh_blobs::ALPN, blobs).spawn();

    router.endpoint().online().await;
    println!("zust root iroh node: {}", router.endpoint().id());
    println!("zust root iroh addr json: {}", serde_json::to_string(&router.endpoint().addr())?);
    println!("zust root iroh cache: {}", store.root().display());

    tokio::signal::ctrl_c().await?;
    router.shutdown().await?;
    store.shutdown().await?;
    Ok(())
}

pub fn default_cache_dir() -> PathBuf {
    PathBuf::from(".iroh").join("cache")
}

pub fn default_daemon_dir() -> PathBuf {
    PathBuf::from(".iroh").join("daemon")
}

pub fn load_or_create_secret_key(path: impl AsRef<Path>) -> Result<SecretKey> {
    let path = path.as_ref();
    if path.exists() {
        let text = std::fs::read_to_string(path)?;
        return parse_secret_key(text.trim());
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let key = SecretKey::generate();
    std::fs::write(path, hex_encode(&key.to_bytes()))?;
    Ok(key)
}

pub fn parse_secret_key(text: &str) -> Result<SecretKey> {
    text.parse::<SecretKey>().or_else(|_| {
        let bytes = hex_decode_32(text)?;
        Ok(SecretKey::from_bytes(&bytes))
    })
}

#[derive(Clone)]
struct IrohRootProtocol {
    store: IrohStore,
    node_id: EndpointId,
}

impl std::fmt::Debug for IrohRootProtocol {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("IrohRootProtocol").field("node_id", &self.node_id).finish_non_exhaustive()
    }
}

impl ProtocolHandler for IrohRootProtocol {
    async fn accept(&self, connection: Connection) -> Result<(), AcceptError> {
        if let Err(err) = handle_connection(self.store.clone(), self.node_id, connection).await {
            log::error!("iroh root protocol error: {err}");
        }
        Ok(())
    }
}

async fn handle_connection(store: IrohStore, node_id: EndpointId, connection: Connection) -> Result<()> {
    let (mut send, mut recv) = connection.accept_bi().await?;
    let request = read_json::<Request>(&mut recv).await?;
    let result = handle_request(&store, node_id, request, &mut recv).await;
    match result {
        Ok((response, body)) => {
            write_json(&mut send, &response).await?;
            if let Some(body) = body {
                send.write_all(&body).await?;
            }
        }
        Err(err) => write_json(&mut send, &Response::Error { message: err.to_string() }).await?,
    }
    send.finish()?;
    connection.closed().await;
    Ok(())
}

async fn handle_request(store: &IrohStore, node_id: EndpointId, request: Request, recv: &mut iroh::endpoint::RecvStream) -> Result<(Response, Option<Bytes>)> {
    match request {
        Request::Ping => Ok((Response::Pong { node_id: node_id.to_string() }, None)),
        Request::List => Ok((Response::List { values: store.list()? }, None)),
        Request::Get { id } => {
            let value = store.get(&id)?.ok_or_else(|| anyhow!("iroh root value not found: {id}"))?;
            Ok((Response::Record { value }, None))
        }
        Request::Push { id, name, size, modified_ms } => {
            let mut body = vec![0; size as usize];
            recv.read_exact(&mut body).await?;
            let value = store.put_bytes(id, name, Bytes::from(body), modified_ms).await?;
            Ok((Response::Pushed { value }, None))
        }
        Request::Pull { id } => {
            let (value, bytes) = store.bytes_for(&id).await?;
            Ok((Response::Pull { value, size: bytes.len() as u64 }, Some(bytes)))
        }
        Request::Delete { id } => {
            store.delete(&id)?;
            Ok((Response::Deleted { id }, None))
        }
    }
}

async fn connect(remote: EndpointId) -> Result<(Endpoint, iroh::endpoint::Connection)> {
    let endpoint = Endpoint::bind(presets::N0).await?;
    let conn = endpoint.connect(remote, ALPN).await?;
    Ok((endpoint, conn))
}

async fn close(endpoint: Endpoint, conn: iroh::endpoint::Connection) {
    conn.close(0u32.into(), b"done");
    endpoint.close().await;
}

async fn push_bytes(conn: &iroh::endpoint::Connection, id: String, name: String, bytes: Bytes, modified_ms: u64) -> Result<IrohSummary> {
    let (mut send, mut recv) = conn.open_bi().await?;
    write_json(&mut send, &Request::Push { id, name, size: bytes.len() as u64, modified_ms }).await?;
    send.write_all(&bytes).await?;
    send.finish()?;
    match read_json::<Response>(&mut recv).await?.into_result()? {
        Response::Pushed { value } => Ok(value),
        other => bail!("unexpected iroh push response: {other:?}"),
    }
}

async fn pull_bytes(conn: &iroh::endpoint::Connection, id: String) -> Result<(IrohSummary, Bytes)> {
    let (mut send, mut recv) = conn.open_bi().await?;
    write_json(&mut send, &Request::Pull { id }).await?;
    send.finish()?;
    let response = read_json::<Response>(&mut recv).await?.into_result()?;
    let (value, size) = match response {
        Response::Pull { value, size } => (value, size),
        other => bail!("unexpected iroh pull response: {other:?}"),
    };
    let mut bytes = vec![0; size as usize];
    recv.read_exact(&mut bytes).await?;
    Ok((value, Bytes::from(bytes)))
}

async fn request(conn: &iroh::endpoint::Connection, request: Request) -> Result<Response> {
    let (mut send, mut recv) = conn.open_bi().await?;
    write_json(&mut send, &request).await?;
    send.finish()?;
    read_json::<Response>(&mut recv).await
}

async fn write_json<T: Serialize>(send: &mut iroh::endpoint::SendStream, value: &T) -> Result<()> {
    let bytes = serde_json::to_vec(value)?;
    if bytes.len() > MAX_FRAME_SIZE as usize {
        bail!("iroh root frame too large: {}", bytes.len());
    }
    send.write_u32(bytes.len() as u32).await?;
    send.write_all(&bytes).await?;
    Ok(())
}

async fn read_json<T: DeserializeOwned>(recv: &mut iroh::endpoint::RecvStream) -> Result<T> {
    let len = recv.read_u32().await?;
    if len > MAX_FRAME_SIZE {
        bail!("iroh root frame too large: {len}");
    }
    let mut bytes = vec![0; len as usize];
    recv.read_exact(&mut bytes).await?;
    Ok(serde_json::from_slice(&bytes)?)
}

fn block_on<F, T>(future: F) -> Result<T>
where
    F: std::future::Future<Output = Result<T>> + Send + 'static,
    T: Send + 'static,
{
    if tokio::runtime::Handle::try_current().is_ok() {
        let (tx, rx) = std::sync::mpsc::sync_channel(1);
        std::thread::spawn(move || {
            let result = tokio::runtime::Runtime::new().and_then(|runtime| Ok(runtime.block_on(future))).unwrap_or_else(|error| Err(anyhow!("create iroh root runtime: {error}")));
            let _ = tx.send(result);
        });
        rx.recv().map_err(|error| anyhow!("iroh root worker failed: {error}"))?
    } else {
        tokio::runtime::Runtime::new()?.block_on(future)
    }
}

fn dir_entries_from_raw(prefix: &str, raw: Vec<SmolStr>) -> Vec<SmolStr> {
    let mut seen = std::collections::HashSet::new();
    let mut names = Vec::new();
    for key in raw {
        let Some(rest) = key.strip_prefix(prefix) else {
            continue;
        };
        let end = rest.find('/').unwrap_or(rest.len());
        let entry = &rest[..end];
        if !entry.is_empty() && seen.insert(entry.to_string()) {
            names.push(format!("{prefix}{entry}").into());
        }
    }
    names
}

fn leaf_name(id: &str) -> String {
    id.rsplit('/').next().filter(|name| !name.is_empty()).unwrap_or(id).to_string()
}

fn now_ms() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|duration| duration.as_millis() as u64).unwrap_or(0)
}

fn hex_encode(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push(HEX[(byte >> 4) as usize] as char);
        out.push(HEX[(byte & 0x0f) as usize] as char);
    }
    out
}

fn hex_decode_32(text: &str) -> Result<[u8; 32]> {
    let text = text.trim();
    if text.len() != 64 {
        bail!("secret key hex must be 64 characters");
    }
    let mut out = [0u8; 32];
    for (idx, chunk) in text.as_bytes().chunks_exact(2).enumerate() {
        out[idx] = (hex_value(chunk[0])? << 4) | hex_value(chunk[1])?;
    }
    Ok(out)
}

fn hex_value(byte: u8) -> Result<u8> {
    match byte {
        b'0'..=b'9' => Ok(byte - b'0'),
        b'a'..=b'f' => Ok(byte - b'a' + 10),
        b'A'..=b'F' => Ok(byte - b'A' + 10),
        _ => bail!("invalid hex byte"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn store_round_trips_bytes_and_records() {
        let data_dir = std::env::temp_dir().join(format!("zust-root-iroh-{}", uuid::Uuid::new_v4()));
        let store = IrohStore::open(&data_dir).await.unwrap();

        let summary = store.put_bytes("assets/hero.png".into(), "hero.png".into(), Bytes::from_static(b"image bytes"), 7).await.unwrap();
        assert_eq!(summary.id, "assets/hero.png");
        assert_eq!(summary.name, "hero.png");
        assert_eq!(summary.size, 11);

        let (fetched, bytes) = store.bytes_for("assets/hero.png").await.unwrap();
        assert_eq!(fetched, summary);
        assert_eq!(bytes.as_ref(), b"image bytes");
        assert_eq!(store.list().unwrap(), vec![summary]);

        assert!(store.delete("assets/hero.png").unwrap());
        assert!(store.get("assets/hero.png").unwrap().is_none());

        store.shutdown().await.unwrap();
        let _ = std::fs::remove_dir_all(data_dir);
    }

    #[test]
    fn dir_entries_match_root_immediate_child_semantics() {
        let raw = vec!["assets/a.png".into(), "assets/nested/b.png".into(), "assets/nested/c.png".into(), "other/d.png".into()];
        assert_eq!(dir_entries_from_raw("assets/", raw), vec![SmolStr::new("assets/a.png"), SmolStr::new("assets/nested")]);
    }

    #[test]
    fn summaries_with_prefix_returns_cached_ids_for_dir_raw() {
        let values = vec![
            IrohSummary { id: "map/demo/bg/0_0.png".into(), name: "0_0.png".into(), hash: "hash-a".into(), size: 1, modified_ms: 10 },
            IrohSummary { id: "map/demo/bg/0_0.png".into(), name: "0_0.png".into(), hash: "hash-a".into(), size: 1, modified_ms: 10 },
            IrohSummary { id: "map/demo/meta.json".into(), name: "meta.json".into(), hash: "hash-b".into(), size: 2, modified_ms: 11 },
        ];
        assert_eq!(summaries_with_prefix("map/demo/bg/", values), vec![SmolStr::new("map/demo/bg/0_0.png")]);
    }

    #[test]
    fn parse_secret_key_accepts_persisted_hex() {
        let key = SecretKey::generate();
        let encoded = hex_encode(&key.to_bytes());
        let parsed = parse_secret_key(&encoded).unwrap();
        assert_eq!(parsed.to_bytes(), key.to_bytes());
    }
}
