use std::{
    fs,
    path::{Path, PathBuf},
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

#[derive(Debug, Clone)]
pub struct IrohSyncProgress {
    pub total: usize,
    pub current: usize,
    pub value: IrohSummary,
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
        let blobs = FsStore::load(root.join("blobs")).await.context("open iroh root blob store")?;
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

    fn get_record(&self, id: &str) -> Result<Option<IrohRecord>> {
        self.values.get(id.as_bytes())?.map(|value| serde_json::from_slice(value.as_ref()).context("decode iroh root record")).transpose()
    }
}

#[derive(Clone)]
pub struct IrohClient {
    remote: EndpointId,
    local: IrohLocal,
}

#[derive(Clone)]
enum IrohLocal {
    Store(IrohStore),
    UploadDir(PathBuf),
}

impl IrohClient {
    pub fn new(remote: EndpointId) -> Result<Self> {
        Self::with_local_dir(remote, default_local_dir())
    }

    pub fn with_local_dir(remote: EndpointId, local_dir: impl AsRef<Path>) -> Result<Self> {
        let local = if local_dir.as_ref().as_os_str().is_empty() { IrohLocal::Store(block_on(async { IrohStore::open(default_local_dir()).await })?) } else { IrohLocal::UploadDir(local_dir.as_ref().to_path_buf()) };
        Ok(Self { remote, local })
    }

    pub fn put_bytes(&self, id: &str, bytes: Bytes) -> Result<()> {
        let IrohLocal::Store(local) = &self.local else {
            bail!("iroh upload dir mount does not support root value writes");
        };
        let local = local.clone();
        let remote = self.remote;
        let id = id.to_string();
        let name = leaf_name(&id);
        let modified_ms = now_ms();
        block_on({
            let local = local.clone();
            let id = id.clone();
            let name = name.clone();
            let bytes = bytes.clone();
            async move {
                local.put_bytes(id, name, bytes, modified_ms).await?;
                Ok(())
            }
        })?;
        spawn_push_bytes(remote, id, name, bytes, modified_ms);
        Ok(())
    }

    pub fn get_bytes(&self, id: &str) -> Result<Bytes> {
        match &self.local {
            IrohLocal::Store(local) => block_on({
                let local = local.clone();
                let id = id.to_string();
                async move { local.bytes_for(&id).await.map(|(_, bytes)| bytes) }
            }),
            IrohLocal::UploadDir(_) => bail!("iroh upload dir mount does not support root value reads"),
        }
    }

    pub fn contains(&self, id: &str) -> bool {
        match &self.local {
            IrohLocal::Store(local) => local.get(id).ok().flatten().is_some(),
            IrohLocal::UploadDir(root) => root.join(id).exists(),
        }
    }

    pub fn delete(&self, id: &str) -> Result<()> {
        let IrohLocal::Store(local) = &self.local else {
            bail!("iroh upload dir mount does not support root value delete");
        };
        let _ = local.delete(id);
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
        let prefix = name.to_string();
        match &self.local {
            IrohLocal::Store(local) => Ok(summaries_with_prefix(&prefix, local.list().unwrap_or_default())),
            IrohLocal::UploadDir(root) => Ok(upload_files(root, &prefix)?.into_iter().map(|file| file.id.into()).collect()),
        }
    }

    pub fn sync<F>(&self, path: &str, mut progress: F) -> Result<Vec<IrohSummary>>
    where
        F: FnMut(IrohSyncProgress) + Send + 'static,
    {
        let remote = self.remote;
        match &self.local {
            IrohLocal::Store(local) => {
                let local = local.clone();
                let values = summaries_for_sync(path, local.list()?);
                if values.is_empty() {
                    return Ok(Vec::new());
                }
                block_on(async move {
                    let (endpoint, conn) = connect(remote).await?;
                    let mut pushed = Vec::new();
                    let total = values.len();
                    for (idx, value) in values.into_iter().enumerate() {
                        let (_, bytes) = local.bytes_for(&value.id).await?;
                        let value = push_bytes(&conn, value.id, value.name, bytes, value.modified_ms).await?;
                        progress(IrohSyncProgress { total, current: idx + 1, value: value.clone() });
                        pushed.push(value);
                    }
                    close(endpoint, conn).await;
                    Ok(pushed)
                })
            }
            IrohLocal::UploadDir(root) => {
                let files = upload_files(root, path)?;
                if files.is_empty() {
                    return Ok(Vec::new());
                }
                block_on(async move {
                    let (endpoint, conn) = connect(remote).await?;
                    let mut pushed = Vec::new();
                    let total = files.len();
                    for (idx, file) in files.into_iter().enumerate() {
                        let bytes = Bytes::from(fs::read(&file.path)?);
                        let value = push_bytes(&conn, file.id, file.name, bytes, file.modified_ms).await?;
                        progress(IrohSyncProgress { total, current: idx + 1, value: value.clone() });
                        pushed.push(value);
                    }
                    close(endpoint, conn).await;
                    Ok(pushed)
                })
            }
        }
    }
}

struct UploadFile {
    id: String,
    name: String,
    path: PathBuf,
    modified_ms: u64,
}

fn upload_files(root: &Path, path: &str) -> Result<Vec<UploadFile>> {
    let target = if path.is_empty() { root.to_path_buf() } else { root.join(path) };
    if !target.exists() {
        return Ok(Vec::new());
    }
    let mut files = Vec::new();
    collect_upload_files(root, &target, &mut files)?;
    files.sort_by(|a, b| a.id.cmp(&b.id));
    Ok(files)
}

fn collect_upload_files(root: &Path, path: &Path, files: &mut Vec<UploadFile>) -> Result<()> {
    let metadata = fs::metadata(path).with_context(|| format!("read upload path {}", path.display()))?;
    if metadata.is_file() {
        let id = upload_id(root, path)?;
        files.push(UploadFile { name: leaf_name(&id), id, path: path.to_path_buf(), modified_ms: metadata_modified_ms(&metadata) });
        return Ok(());
    }
    if metadata.is_dir() {
        for entry in fs::read_dir(path)? {
            collect_upload_files(root, &entry?.path(), files)?;
        }
    }
    Ok(())
}

fn upload_id(root: &Path, path: &Path) -> Result<String> {
    let relative = path.strip_prefix(root).with_context(|| format!("upload path {} is outside {}", path.display(), root.display()))?;
    Ok(relative.iter().map(|part| part.to_string_lossy()).collect::<Vec<_>>().join("/"))
}

fn metadata_modified_ms(metadata: &fs::Metadata) -> u64 {
    metadata.modified().ok().and_then(|time| time.duration_since(UNIX_EPOCH).ok()).map(|duration| duration.as_millis() as u64).unwrap_or_else(now_ms)
}

fn summaries_with_prefix(prefix: &str, values: Vec<IrohSummary>) -> Vec<SmolStr> {
    let mut names = values.into_iter().filter_map(|value| if prefix.is_empty() || value.id.starts_with(prefix) { Some(value.id.into()) } else { None }).collect::<Vec<SmolStr>>();
    names.sort();
    names.dedup();
    names
}

fn summaries_for_sync(path: &str, values: Vec<IrohSummary>) -> Vec<IrohSummary> {
    let prefix = if path.is_empty() || path.ends_with('/') { path.to_string() } else { format!("{path}/") };
    values.into_iter().filter(|value| value.id == path || value.id.starts_with(&prefix)).collect()
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
    println!("zust root iroh local: {}", store.root().display());

    tokio::signal::ctrl_c().await?;
    router.shutdown().await?;
    store.shutdown().await?;
    Ok(())
}

pub fn default_local_dir() -> PathBuf {
    PathBuf::from(".iroh_store")
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
    loop {
        let Ok((mut send, mut recv)) = connection.accept_bi().await else {
            return Ok(());
        };
        let result = async {
            let request = read_json::<Request>(&mut recv).await?;
            handle_request(&store, node_id, request, &mut recv).await
        }
        .await;
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
    }
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

fn spawn_push_bytes(remote: EndpointId, id: String, name: String, bytes: Bytes, modified_ms: u64) {
    std::thread::spawn(move || {
        if let Err(err) = block_on(async move {
            let (endpoint, conn) = connect(remote).await?;
            let response = push_bytes(&conn, id, name, bytes, modified_ms).await;
            close(endpoint, conn).await;
            response.map(|_| ())
        }) {
            log::warn!("iroh root background sync failed: {err}");
        }
    });
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
    fn summaries_with_prefix_returns_local_ids_for_dir_raw() {
        let values = vec![
            IrohSummary { id: "map/demo/bg/0_0.png".into(), name: "0_0.png".into(), hash: "hash-a".into(), size: 1, modified_ms: 10 },
            IrohSummary { id: "map/demo/bg/0_0.png".into(), name: "0_0.png".into(), hash: "hash-a".into(), size: 1, modified_ms: 10 },
            IrohSummary { id: "map/demo/meta.json".into(), name: "meta.json".into(), hash: "hash-b".into(), size: 2, modified_ms: 11 },
        ];
        assert_eq!(summaries_with_prefix("map/demo/bg/", values), vec![SmolStr::new("map/demo/bg/0_0.png")]);
    }

    #[test]
    fn summaries_for_sync_matches_file_or_recursive_dir() {
        let values = vec![
            IrohSummary { id: "map/demo/bg/0_0.png".into(), name: "0_0.png".into(), hash: "hash-a".into(), size: 1, modified_ms: 10 },
            IrohSummary { id: "map/demo/bg/nested/0_1.png".into(), name: "0_1.png".into(), hash: "hash-b".into(), size: 2, modified_ms: 11 },
            IrohSummary { id: "map/demo/meta.json".into(), name: "meta.json".into(), hash: "hash-c".into(), size: 3, modified_ms: 12 },
        ];

        let one = summaries_for_sync("map/demo/meta.json", values.clone());
        assert_eq!(one.iter().map(|value| value.id.as_str()).collect::<Vec<_>>(), vec!["map/demo/meta.json"]);

        let dir = summaries_for_sync("map/demo/bg", values);
        assert_eq!(dir.iter().map(|value| value.id.as_str()).collect::<Vec<_>>(), vec!["map/demo/bg/0_0.png", "map/demo/bg/nested/0_1.png"]);
    }

    #[test]
    fn upload_files_use_relative_ids() {
        let root = std::env::temp_dir().join(format!("zust-root-upload-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(root.join("map/demo/bg")).unwrap();
        std::fs::write(root.join("map/demo/bg/0_0.png"), b"tile").unwrap();
        std::fs::write(root.join("map/demo/meta.json"), b"{}").unwrap();

        let all = upload_files(&root, "map/demo").unwrap();
        assert_eq!(all.iter().map(|file| file.id.as_str()).collect::<Vec<_>>(), vec!["map/demo/bg/0_0.png", "map/demo/meta.json"]);

        let one = upload_files(&root, "map/demo/meta.json").unwrap();
        assert_eq!(one.iter().map(|file| file.id.as_str()).collect::<Vec<_>>(), vec!["map/demo/meta.json"]);

        let missing = upload_files(&root, "missing").unwrap();
        assert!(missing.is_empty());

        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn parse_secret_key_accepts_persisted_hex() {
        let key = SecretKey::generate();
        let encoded = hex_encode(&key.to_bytes());
        let parsed = parse_secret_key(&encoded).unwrap();
        assert_eq!(parsed.to_bytes(), key.to_bytes());
    }
}
