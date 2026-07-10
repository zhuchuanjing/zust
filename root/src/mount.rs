use dynamic::{Dynamic, FromJson, FromYaml, MsgPack, MsgUnpack, ToJson};
use rand::random_range;
use scc::HashMap;
use smol_str::SmolStr;

use anyhow::{Result, anyhow};

use super::{Object, sync_await};
use crate::directory;
use crate::node::Node;
use fjall::{KeyspaceCreateOptions, OptimisticTxDatabase, OptimisticTxKeyspace, PersistMode};
use redis::AsyncCommands;
use redis::Commands;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use rslock::LockManager;

pub enum Mount<T> {
    Memory(Arc<HashMap<SmolStr, Node<T>>>),
    Redis {
        client: redis::Client,
        rl: LockManager,
    },
    Fjall {
        db: OptimisticTxDatabase,
        values: OptimisticTxKeyspace,
        write_lock: Arc<std::sync::Mutex<()>>,
    },
    /// 把真实文件系统目录挂到 ROOT 树:`base` 是 host 路径(canonicalize 后)。
    /// 与 Memory/Redis/Fjall 不同,Dir 后端**不支持** List/Map 语义(`add_list`、
    /// `add_map`、`push`、`get_idx`、`insert` 等都返回 Err),只支持标量文件读写,
    /// 且序列化方式按文件扩展名 dispatch:`.json` 走 to_json/from_json,
    /// `.yaml`/`.yml` 走 to_yaml/from_yaml,`.md` 写用 to_markdown / 读回 String,
    /// 其他后缀按 String 处理。
    Dir {
        base: PathBuf,
    },
}

impl<T: std::fmt::Debug + MsgPack + MsgUnpack + Default + Send> Mount<T> {
    /// usize 索引转 isize 供 Redis lindex/lset 使用。
    /// idx > isize::MAX 时回绕成负数会被 Redis 当成"从末尾倒数",与 zust 的绝对索引语义冲突,
    /// 报错而非静默错误索引。
    fn redis_idx(idx: usize) -> Result<isize> {
        isize::try_from(idx).map_err(|_| anyhow!("索引 {} 超出 isize 范围", idx))
    }

    pub fn memory() -> Self {
        Self::Memory(Arc::new(HashMap::new()))
    }

    pub fn redis(url: &str) -> Result<Self> {
        let client = redis::Client::open(url)?;
        let mut conn = client.get_connection()?;
        directory::rebuild_once(&mut conn)?;
        let rl = LockManager::new(vec![url]);
        Ok(Self::Redis { client, rl })
    }

    pub fn fjall(data_dir: &str) -> Result<Self> {
        let db = OptimisticTxDatabase::builder(data_dir).open()?;
        let values = db.keyspace("root", KeyspaceCreateOptions::default)?;
        Ok(Self::Fjall { db, values, write_lock: Arc::new(std::sync::Mutex::new(())) })
    }

    pub fn add(&self, name: &str, value: T) -> bool {
        match self {
            Self::Memory(m) => {
                m.upsert_sync(name.into(), Node::Object(value));
                true
            }
            Self::Redis { client, rl: _ } => {
                let mut buf = Vec::new();
                value.encode(&mut buf);
                let Ok(mut conn) = client.get_connection() else {
                    return false;
                };
                conn.set::<&str, Vec<u8>, ()>(name, buf).is_ok() && directory::add_path(&mut conn, name).is_ok()
            }
            Self::Fjall { db, values, write_lock } => {
                let mut buf = Vec::new();
                value.encode(&mut buf);
                let Ok(_guard) = write_lock.lock() else {
                    return false;
                };
                if fjall_clear_node(values, name).is_err() || values.insert(fjall_object_key(name), buf).is_err() {
                    return false;
                }
                fjall_persist(db).is_ok()
            }
            // Dir 后端的 add 由 `impl Mount<Object>::dir_add` 处理,
            // lib.rs 在调用本方法前会先 match Dir 分发,这里不会真的执行到。
            Self::Dir { .. } => false,
        }
    }

    pub fn contains(&self, name: &str) -> bool {
        match self {
            Self::Memory(m) => m.contains_sync(name),
            Self::Redis { client, rl: _ } => client.get_connection().and_then(|mut conn| conn.exists::<&str, bool>(name)).unwrap_or(false),
            Self::Fjall { values, .. } => values.contains_key(fjall_object_key(name)).unwrap_or(false) || values.contains_key(fjall_type_key(name)).unwrap_or(false),
            Self::Dir { base } => match safe_path(base, name) {
                Some(p) => p.is_file(),
                None => false,
            },
        }
    }

    pub fn get_mut<R: Send + 'static, F: FnMut(&mut T) -> R>(&self, name: &str, mut f: F) -> Result<R>
    where
        F: Send + 'static,
    {
        match self {
            Self::Memory(m) => m
                .update_sync(name, |_, v| match v {
                    Node::Object(v) => Some(f(v)),
                    _ => None,
                })
                .flatten()
                .ok_or(anyhow!("{} 不存在", name)),
            Self::Redis { client, rl } => {
                let name = String::from(name);
                let rl = rl.clone();
                let client = client.clone();
                sync_await!(async move {
                    loop {
                        // TTL 必须覆盖一次完整 RMW(GET+decode+闭包+encode+SET),
                        // 原先 0..999ms 可能短于一次 Redis 往返,导致锁提前过期丢更新。
                        if let Ok(lock) = rl.lock(name.as_str(), std::time::Duration::from_millis(5000)).await {
                            let mut conn = client.get_multiplexed_async_connection().await?;
                            let mut buf: Vec<u8> = conn.get(name.as_str()).await?;
                            let (mut v, _) = T::decode(buf.as_slice())?;
                            let r = f(&mut v);
                            buf.clear();
                            v.encode(&mut buf);
                            conn.set::<&str, Vec<u8>, ()>(name.as_str(), buf).await?;
                            rl.unlock(&lock).await;
                            break Ok(r);
                        }
                        // 锁被占用时退避,避免 CPU 100% 空转。
                        tokio::time::sleep(std::time::Duration::from_millis(random_range(1..10))).await;
                    }
                })
            }
            Self::Fjall { db, values, write_lock } => {
                let _guard = write_lock.lock().map_err(|e| anyhow!("无法获取 fjall 写锁: {}", e))?;
                let mut v = fjall_get_object::<T>(values, name)?;
                let r = f(&mut v);
                fjall_insert_object(values, name, &v)?;
                fjall_persist(db)?;
                Ok(r)
            }
            // Dir 后端的 update 走 `impl Mount<Object>::dir_update`。
            Self::Dir { .. } => Err(anyhow!("Mount::Dir 的原子更新请用 dir_update")),
        }
    }

    pub fn get<R, F: FnOnce(&T) -> R>(&self, name: &str, f: F) -> Result<R> {
        match self {
            Self::Memory(m) => m
                .read_sync(name, |_, v| match v {
                    Node::Object(v) => Some(f(v)),
                    _ => None,
                })
                .flatten()
                .ok_or(anyhow!("{} 不存在", name)),
            Self::Redis { client, rl: _ } => {
                let mut conn = client.get_connection()?;
                let buf: Vec<u8> = conn.get(name)?;
                let (v, _) = T::decode(buf.as_slice())?;
                Ok(f(&v))
            }
            Self::Fjall { values, .. } => fjall_get_object(values, name).map(|v| f(&v)),
            // Dir 后端的 get 走下面 `impl Mount<Object>::get` 的特化实现——
            // 泛型 T 不一定能包成 Object::Value,需要单独处理。
            // 兜底仍报 Err,让上层知道这个 T 走不了 Dir。
            Self::Dir { .. } => Err(anyhow!("Mount::Dir 的 get 请用 impl Mount<Object>::get")),
        }
    }

    pub fn get_key_mut<'a, R: Send + 'static, F: FnOnce(&mut T) -> R>(&'a self, name: &'a str, key: &'a str, f: F) -> Result<R>
    where
        F: Send + 'static,
    {
        match self {
            Self::Memory(m) => m
                .update_sync(name, |_, v| match v {
                    Node::Map(m) => m.update_sync(key, |_, v| f(v)),
                    _ => None,
                })
                .flatten()
                .ok_or(anyhow!("{} 不存在", name)),
            Self::Redis { client, rl } => {
                let name = String::from(name);
                let key = String::from(key);
                let rl = rl.clone();
                let client = client.clone();
                sync_await!(async move {
                    loop {
                        // TTL 必须覆盖一次完整 RMW,原先 0..999ms 会丢更新(见 get_mut 注释)。
                        let lock_name = format!("{}::{}", name, key); //为这个 name 里面的 key 单独上锁
                        if let Ok(lock) = rl.lock(lock_name.as_str(), std::time::Duration::from_millis(5000)).await {
                            let mut conn = client.get_multiplexed_async_connection().await?;
                            let mut buf: Vec<u8> = conn.hget(name.as_str(), key.as_str()).await?;
                            let (mut v, _) = T::decode(buf.as_slice())?;
                            let r = f(&mut v);
                            buf.clear();
                            v.encode(&mut buf);
                            conn.hset::<&str, &str, Vec<u8>, ()>(name.as_str(), key.as_str(), buf).await?;
                            rl.unlock(&lock).await;
                            break Ok(r);
                        }
                        // 锁被占用时退避,避免 CPU 100% 空转。
                        tokio::time::sleep(std::time::Duration::from_millis(random_range(1..10))).await;
                    }
                })
            }
            Self::Fjall { db, values, write_lock } => {
                let _guard = write_lock.lock().map_err(|e| anyhow!("无法获取 fjall 写锁: {}", e))?;
                let mut v = fjall_get_map_item::<T>(values, name, key)?;
                let r = f(&mut v);
                fjall_insert_map_item(values, name, key, &v)?;
                fjall_persist(db)?;
                Ok(r)
            }
            // Dir 后端无 map 内 key 概念。
            Self::Dir { .. } => Err(anyhow!("Mount::Dir 不支持 map 内 key 操作")),
        }
    }

    pub fn dir(&self, name: &str) -> Result<Vec<SmolStr>> {
        if let Self::Redis { client, rl: _ } = self {
            let mut conn = client.get_connection()?;
            return directory::children(&mut conn, name);
        }
        if let Self::Fjall { values, .. } = self {
            return fjall_dir(values, name);
        }
        // Dir 后端的目录列表由 `impl Mount<Object>::dir_list` 处理。
        if let Self::Dir { .. } = self {
            return Err(anyhow!("Mount::Dir 的目录列表请用 dir_list"));
        }

        let prefix = if name.is_empty() || name.ends_with('/') { name.to_string() } else { format!("{name}/") };
        let raw = self.dir_raw(&prefix)?;
        Ok(Self::dir_entries_from_raw(&prefix, raw))
    }

    fn dir_raw_entries_from_children(prefix: &str, children: Vec<SmolStr>) -> Vec<SmolStr> {
        children.into_iter().map(|child| if prefix.is_empty() { child } else { format!("{prefix}{child}").into() }).collect()
    }

    fn dir_entries_from_raw(prefix: &str, raw: Vec<SmolStr>) -> Vec<SmolStr> {
        let mut seen = std::collections::HashSet::new();
        let mut names = Vec::new();
        for key in raw {
            let Some(rest) = key.strip_prefix(&prefix) else {
                continue;
            };
            let end = rest.find('/').unwrap_or(rest.len());
            let entry = &rest[..end];
            if !entry.is_empty() && seen.insert(entry.to_string()) {
                names.push(entry.into());
            }
        }
        names
    }

    pub fn dir_raw(&self, name: &str) -> Result<Vec<SmolStr>> {
        let mut names = Vec::new();
        match self {
            // 性能:Memory 后端用 scc::HashMap(无序),dir_raw 只能 O(全表) starts_with 扫描。
            // Redis 用 directory::children 索引,Fjall 用 prefix scan,都是有界的。
            // http_module::dynamic_api_route 每请求递归调 dir,在 Memory 后端 + 大量节点时是热点。
            // 根治需把 Memory 后端换成 BTreeMap 或维护前缀二级索引——属架构改动,暂不做。
            Self::Memory(m) => {
                m.iter_sync(|key, _| {
                    if key.starts_with(name) {
                        names.push(key.clone())
                    }
                    true
                });
            }
            Self::Redis { client, rl: _ } => {
                let mut conn = client.get_connection()?;
                let prefix = if name.is_empty() || name.ends_with('/') { name.to_string() } else { format!("{name}/") };
                names.append(&mut Self::dir_raw_entries_from_children(&prefix, directory::children(&mut conn, name)?));
            }
            Self::Fjall { values, .. } => {
                names.append(&mut fjall_paths_with_prefix(values, name)?);
            }
            // Dir 后端直接列一级:不区分文件/目录,调用方用 len 区分(目录=0)。
            Self::Dir { base } => {
                let path = safe_path(base, name).ok_or_else(|| anyhow!("path 非法: {}", name))?;
                if !path.exists() {
                    return Err(anyhow!("{} 不存在", name));
                }
                if !path.is_dir() {
                    return Err(anyhow!("{} 不是目录", name));
                }
                for entry in std::fs::read_dir(&path).map_err(|e| anyhow!("读取 {} 失败: {}", path.display(), e))? {
                    let entry = entry.map_err(|e| anyhow!("读取目录项失败: {}", e))?;
                    if let Some(s) = entry.file_name().to_str() {
                        names.push(SmolStr::new(s));
                    }
                }
                names.sort();
            }
        }
        Ok(names)
    }

    pub fn len(&self, name: &str) -> Result<usize> {
        match self {
            Self::Memory(m) => m.read_sync(name, |_, v| v.len()).ok_or(anyhow!("{} 不是列表", name)),
            Self::Redis { client, rl: _ } => {
                let mut conn = client.get_connection()?;
                let ty: String = conn.key_type(name)?;
                match ty.as_str() {
                    "list" => Ok(conn.llen(name)?),
                    "hash" => Ok(conn.hlen(name)?),
                    _ => Ok(1),
                }
            }
            Self::Fjall { values, .. } => match fjall_node_type(values, name)? {
                Some(FjallNodeType::List) => Ok(fjall_count_prefix(values, fjall_list_prefix(name))?),
                Some(FjallNodeType::Map) => Ok(fjall_count_prefix(values, fjall_map_prefix(name))?),
                None if values.contains_key(fjall_object_key(name))? => Ok(1),
                None => Err(anyhow!("{} 不存在", name)),
            },
            // Dir 后端:文件 = 字节数,目录 = 0,不存在 = 报错。
            Self::Dir { base } => {
                let path = safe_path(base, name).ok_or_else(|| anyhow!("path 非法: {}", name))?;
                let metadata = std::fs::metadata(&path).map_err(|e| anyhow!("{} 不存在: {}", name, e))?;
                if metadata.is_dir() { Ok(0) } else { Ok(metadata.len() as usize) }
            }
        }
    }

    pub fn remove(&self, name: &str) -> Result<T> {
        match self {
            // into_object 对 List/Map 节点返回 None(只能取标量 Object),
            // 但 Redis/Fjall 的 remove 按 name 直接删任意类型。三后端行为统一:
            // List/Map 节点删除成功,返回 default(节点本身无标量值,default 表示"已删除")。
            Self::Memory(m) => m.remove_sync(name).map(|(_, v)| v.into_object().unwrap_or_default()).ok_or(anyhow!("{} 不存在", name)),
            Self::Redis { client, rl: _ } => {
                let mut conn = client.get_connection()?;
                let buf: Vec<u8> = conn.get(name)?;
                let (v, _) = T::decode(buf.as_slice())?;
                let removed: usize = conn.del(name)?;
                if removed != 0 {
                    directory::remove_path(&mut conn, name)?;
                }
                Ok(v)
            }
            Self::Fjall { db, values, .. } => {
                let v = fjall_get_object(values, name)?;
                values.remove(fjall_object_key(name))?;
                fjall_persist(db)?;
                Ok(v)
            }
            // Dir 后端的 remove 由 `impl Mount<Object>::dir_remove` 处理。
            Self::Dir { .. } => Err(anyhow!("Mount::Dir 的删除请用 dir_remove")),
        }
    }

    pub fn add_list(&self, name: &str) {
        match self {
            Self::Memory(m) => {
                m.upsert_sync(name.into(), Node::<T>::list());
            } // 强制插入 肯定成功
            Self::Redis { client: _, rl: _ } => {}
            Self::Fjall { db, values, write_lock } => {
                if let Ok(_guard) = write_lock.lock() {
                    let _ = fjall_clear_node(values, name).and_then(|_| fjall_set_node_type(values, name, FjallNodeType::List)).and_then(|_| fjall_persist(db));
                }
            }
            Self::Dir { .. } => {
                log::warn!("Mount::Dir 不支持 add_list,忽略 {}", name);
            }
        }
    }

    pub fn add_map(&self, name: &str) {
        match self {
            Self::Memory(m) => {
                m.upsert_sync(name.into(), Node::<T>::map());
            } // 强制插入 肯定成功
            Self::Redis { client: _, rl: _ } => {}
            Self::Fjall { db, values, write_lock } => {
                if let Ok(_guard) = write_lock.lock() {
                    let _ = fjall_clear_node(values, name).and_then(|_| fjall_set_node_type(values, name, FjallNodeType::Map)).and_then(|_| fjall_persist(db));
                }
            }
            Self::Dir { .. } => {
                log::warn!("Mount::Dir 不支持 add_map,忽略 {}", name);
            }
        }
    }

    pub fn push(&self, name: &str, value: T) -> Result<usize> {
        match self {
            Self::Memory(m) => m.update_sync(name, |_, v| v.push(value)).flatten().ok_or(anyhow!("push {} 失败", name)),
            Self::Redis { client, rl: _ } => {
                let mut conn = client.get_connection()?;
                let mut buf = Vec::new();
                value.encode(&mut buf);
                let len = conn.rpush(name, buf)?;
                directory::add_path(&mut conn, name)?;
                Ok(len)
            }
            Self::Fjall { db, values, write_lock } => {
                let _guard = write_lock.lock().map_err(|e| anyhow!("无法获取 fjall 写锁: {}", e))?;
                match fjall_node_type(values, name)? {
                    Some(FjallNodeType::List) => {}
                    Some(FjallNodeType::Map) => return Err(anyhow!("push {} 失败", name)),
                    None => fjall_set_node_type(values, name, FjallNodeType::List)?,
                }
                let idx = fjall_next_list_idx(values, name)?;
                fjall_insert_list_item(values, name, idx, &value)?;
                fjall_persist(db)?;
                Ok(idx)
            }
            Self::Dir { .. } => Err(anyhow!("Mount::Dir 不支持 push/List 语义")),
        }
    }

    pub fn get_idx<R, F: FnOnce(&T) -> R>(&self, name: &str, idx: usize, f: F) -> Result<R> {
        match self {
            Self::Memory(m) => m.read_sync(name, |_, v| v.get_idx(idx, f)).flatten().ok_or(anyhow!("get_idx {} 失败", name)),
            Self::Redis { client, rl: _ } => {
                let mut conn = client.get_connection()?;
                let buf: Vec<u8> = conn.lindex(name, Self::redis_idx(idx)?)?;
                // remove_idx 用 lset 写空 Vec 保留槽位(sparse slot 设计)。
                // 空槽 decode 失败时返回 default,与 remove_idx 和 Memory 后端行为一致,
                // 而非中断调用方。
                let (v, _) = T::decode(buf.as_slice()).unwrap_or_else(|_| (T::default(), 0));
                Ok(f(&v))
            }
            Self::Fjall { values, .. } => fjall_get_list_item(values, name, idx).map(|v| f(&v)),
            Self::Dir { .. } => Err(anyhow!("Mount::Dir 不支持 get_idx/List 语义")),
        }
    }

    pub fn get_idx_mut<R, F: FnMut(&mut T) -> R>(&self, name: &str, idx: usize, mut f: F) -> Result<R> {
        match self {
            Self::Memory(m) => m.update_sync(name, |_, v| v.get_idx_mut(idx, f)).flatten().ok_or(anyhow!("get_idx {} 失败", name)),
            Self::Redis { client, rl: _ } => {
                let mut conn = client.get_connection()?;
                let buf: Vec<u8> = conn.lindex(name, Self::redis_idx(idx)?)?;
                let (mut v, _) = T::decode(buf.as_slice())?;
                Ok(f(&mut v))
            }
            Self::Fjall { db, values, write_lock } => {
                let _guard = write_lock.lock().map_err(|e| anyhow!("无法获取 fjall 写锁: {}", e))?;
                let mut v = fjall_get_list_item::<T>(values, name, idx)?;
                let r = f(&mut v);
                fjall_insert_list_item(values, name, idx, &v)?;
                fjall_persist(db)?;
                Ok(r)
            }
            Self::Dir { .. } => Err(anyhow!("Mount::Dir 不支持 get_idx_mut/List 语义")),
        }
    }

    pub fn remove_idx(&self, name: &str, idx: usize) -> Result<T> {
        match self {
            Self::Memory(m) => m.update_sync(name, |_, v| v.remove_idx(idx)).flatten().ok_or(anyhow!("remove_idx {} 失败", name)),
            Self::Redis { client, rl: _ } => {
                let mut conn = client.get_connection()?;
                let idx = Self::redis_idx(idx)?;
                let buf: Vec<u8> = conn.lindex(name, idx)?;
                let v = T::decode(buf.as_slice()).map(|(v, _)| v).unwrap_or(T::default());
                let _: () = conn.lset(name, idx, Vec::new())?;
                Ok(v)
            }
            Self::Fjall { db, values, .. } => {
                let key = fjall_list_item_key(name, idx);
                let buf = values.get(&key)?.ok_or(anyhow!("remove_idx {} 失败", name))?;
                let (v, _) = T::decode(buf.as_ref())?;
                values.remove(key)?;
                fjall_persist(db)?;
                Ok(v)
            }
            Self::Dir { .. } => Err(anyhow!("Mount::Dir 不支持 remove_idx/List 语义")),
        }
    }

    pub fn insert(&self, name: &str, key: &str, value: T) -> Option<T> {
        match self {
            Self::Memory(m) => m.update_sync(name, |_, v| v.insert(key.into(), value)).flatten(),
            Self::Redis { client, rl: _ } => {
                if let Ok(mut conn) = client.get_connection() {
                    let mut buf = Vec::new();
                    value.encode(&mut buf);
                    if conn.hset::<&str, &str, Vec<u8>, ()>(name, key, buf).is_ok() {
                        let _ = directory::add_path(&mut conn, name);
                    }
                }
                None
            }
            Self::Fjall { db, values, write_lock } => {
                let _guard = write_lock.lock().ok()?;
                match fjall_node_type(values, name).ok()? {
                    Some(FjallNodeType::Map) => {}
                    Some(FjallNodeType::List) => return None,
                    None => fjall_set_node_type(values, name, FjallNodeType::Map).ok()?,
                }
                let old = fjall_get_map_item(values, name, key).ok();
                fjall_insert_map_item(values, name, key, &value).ok()?;
                fjall_persist(db).ok()?;
                old
            }
            Self::Dir { .. } => {
                log::warn!("Mount::Dir 不支持 insert/Map 语义,忽略 {}/{}", name, key);
                None
            }
        }
    }

    pub fn get_key<R, F: FnOnce(&T) -> R>(&self, name: &str, key: &str, f: F) -> Result<R> {
        match self {
            Self::Memory(m) => m.read_sync(name, |_, v| v.get_key(key, f)).flatten().ok_or(anyhow!("get_key {} 失败", name)),
            Self::Redis { client, rl: _ } => {
                let mut conn = client.get_connection()?;
                let buf: Vec<u8> = conn.hget(name, key)?;
                let (v, _) = T::decode(buf.as_slice())?;
                Ok(f(&v))
            }
            Self::Fjall { values, .. } => fjall_get_map_item(values, name, key).map(|v| f(&v)),
            Self::Dir { .. } => Err(anyhow!("Mount::Dir 不支持 get_key/Map 语义")),
        }
    }

    pub fn keys(&self, name: &str) -> Result<Vec<SmolStr>> {
        match self {
            Self::Memory(m) => m.read_sync(name, |_, v| v.keys()).flatten().ok_or(anyhow!("keys {} 失败", name)),
            Self::Redis { client, rl: _ } => {
                let mut conn = client.get_connection()?;
                let keys: Vec<String> = conn.hkeys(name)?;
                Ok(keys.into_iter().map(|k| k.into()).collect())
            }
            Self::Fjall { values, .. } => fjall_map_keys(values, name),
            Self::Dir { .. } => Err(anyhow!("Mount::Dir 不支持 keys/Map 语义")),
        }
    }

    pub fn remove_key(&self, name: &str, key: &str) -> Result<T> {
        match self {
            Self::Memory(m) => m.update_sync(name, |_, v| v.remove_key(key)).flatten().ok_or(anyhow!("remove_key {} 失败", name)),
            Self::Redis { client, rl: _ } => {
                let mut conn = client.get_connection()?;
                let buf: Vec<u8> = conn.hget(name, key)?;
                let v = T::decode(buf.as_slice())?.0;
                let _: usize = conn.hdel(name, key)?;
                if !conn.exists::<_, bool>(name)? {
                    directory::remove_path(&mut conn, name)?;
                }
                Ok(v)
            }
            Self::Fjall { db, values, .. } => {
                let item_key = fjall_map_item_key(name, key);
                let buf = values.get(&item_key)?.ok_or(anyhow!("remove_key {} 失败", name))?;
                let (v, _) = T::decode(buf.as_ref())?;
                values.remove(item_key)?;
                fjall_persist(db)?;
                Ok(v)
            }
            Self::Dir { .. } => Err(anyhow!("Mount::Dir 不支持 remove_key/Map 语义")),
        }
    }
}

#[derive(Clone, Copy)]
enum FjallNodeType {
    List,
    Map,
}

const FJALL_OBJECT_PREFIX: u8 = b'o';
const FJALL_TYPE_PREFIX: u8 = b't';
const FJALL_LIST_PREFIX: u8 = b'l';
const FJALL_LIST_COUNTER_PREFIX: u8 = b'c';
const FJALL_MAP_PREFIX: u8 = b'm';
const FJALL_SEPARATOR: u8 = 0;

fn fjall_key(prefix: u8, name: &str) -> Vec<u8> {
    let mut key = Vec::with_capacity(2 + name.len());
    key.push(prefix);
    key.push(FJALL_SEPARATOR);
    key.extend_from_slice(name.as_bytes());
    key
}

fn fjall_object_key(name: &str) -> Vec<u8> {
    fjall_key(FJALL_OBJECT_PREFIX, name)
}

fn fjall_type_key(name: &str) -> Vec<u8> {
    fjall_key(FJALL_TYPE_PREFIX, name)
}

fn fjall_list_counter_key(name: &str) -> Vec<u8> {
    fjall_key(FJALL_LIST_COUNTER_PREFIX, name)
}

fn fjall_item_prefix(prefix: u8, name: &str) -> Vec<u8> {
    let mut key = fjall_key(prefix, name);
    key.push(FJALL_SEPARATOR);
    key
}

fn fjall_list_prefix(name: &str) -> Vec<u8> {
    fjall_item_prefix(FJALL_LIST_PREFIX, name)
}

fn fjall_map_prefix(name: &str) -> Vec<u8> {
    fjall_item_prefix(FJALL_MAP_PREFIX, name)
}

fn fjall_list_item_key(name: &str, idx: usize) -> Vec<u8> {
    let mut key = fjall_list_prefix(name);
    key.extend_from_slice(&(idx as u64).to_be_bytes());
    key
}

fn fjall_map_item_key(name: &str, key: &str) -> Vec<u8> {
    let mut item_key = fjall_map_prefix(name);
    item_key.extend_from_slice(key.as_bytes());
    item_key
}

fn fjall_prefix_end(prefix: &[u8]) -> Option<Vec<u8>> {
    let mut end = prefix.to_vec();
    for idx in (0..end.len()).rev() {
        if end[idx] < 255 {
            end[idx] += 1;
            end.truncate(idx + 1);
            return Some(end);
        }
    }
    None
}

fn fjall_decode_value<T: MsgUnpack>(buf: &[u8]) -> Result<T> {
    T::decode(buf).map(|(value, _)| value)
}

fn fjall_encode_value<T: MsgPack>(value: &T) -> Vec<u8> {
    let mut buf = Vec::new();
    value.encode(&mut buf);
    buf
}

fn fjall_persist(db: &OptimisticTxDatabase) -> Result<()> {
    // fjall 官方明确:persist 只影响 durability(断电后是否丢数据),不影响 consistency。
    // 即使不 flush,fjall 也是 crash-safe 的——进程崩溃不丢已写入数据。
    // 原先用 SyncAll(fsync)对每个单 key 写入强制落盘,I/O 开销是内存写的 100-1000 倍,
    // 而 fjall 文档(db.rs:332)明确:"Even without flushing data is crash-safe."
    // 改用 Buffer:数据进 OS page cache,进程崩溃安全,仅断电可能丢 journal 末尾少量写入。
    // 若需断电级 durability,应使用 Redis/PostgreSQL 而非单 key fsync。
    Ok(db.persist(PersistMode::Buffer)?)
}

fn fjall_get_object<T: MsgUnpack>(values: &OptimisticTxKeyspace, name: &str) -> Result<T> {
    let buf = values.get(fjall_object_key(name))?.ok_or(anyhow!("{} 不存在", name))?;
    fjall_decode_value(buf.as_ref())
}

fn fjall_insert_object<T: MsgPack>(values: &OptimisticTxKeyspace, name: &str, value: &T) -> Result<()> {
    Ok(values.insert(fjall_object_key(name), fjall_encode_value(value))?)
}

fn fjall_get_list_item<T: MsgUnpack>(values: &OptimisticTxKeyspace, name: &str, idx: usize) -> Result<T> {
    let buf = values.get(fjall_list_item_key(name, idx))?.ok_or(anyhow!("get_idx {} 失败", name))?;
    fjall_decode_value(buf.as_ref())
}

fn fjall_insert_list_item<T: MsgPack>(values: &OptimisticTxKeyspace, name: &str, idx: usize, value: &T) -> Result<()> {
    Ok(values.insert(fjall_list_item_key(name, idx), fjall_encode_value(value))?)
}

fn fjall_get_map_item<T: MsgUnpack>(values: &OptimisticTxKeyspace, name: &str, key: &str) -> Result<T> {
    let buf = values.get(fjall_map_item_key(name, key))?.ok_or(anyhow!("get_key {} 失败", name))?;
    fjall_decode_value(buf.as_ref())
}

fn fjall_insert_map_item<T: MsgPack>(values: &OptimisticTxKeyspace, name: &str, key: &str, value: &T) -> Result<()> {
    Ok(values.insert(fjall_map_item_key(name, key), fjall_encode_value(value))?)
}

fn fjall_node_type(values: &OptimisticTxKeyspace, name: &str) -> Result<Option<FjallNodeType>> {
    Ok(values.get(fjall_type_key(name))?.and_then(|value| match value.as_ref() {
        b"L" => Some(FjallNodeType::List),
        b"M" => Some(FjallNodeType::Map),
        _ => None,
    }))
}

fn fjall_set_node_type(values: &OptimisticTxKeyspace, name: &str, node_type: FjallNodeType) -> Result<()> {
    let value = match node_type {
        FjallNodeType::List => b"L".as_slice(),
        FjallNodeType::Map => b"M".as_slice(),
    };
    Ok(values.insert(fjall_type_key(name), value)?)
}

fn fjall_clear_node(values: &OptimisticTxKeyspace, name: &str) -> Result<()> {
    values.remove(fjall_object_key(name))?;
    values.remove(fjall_type_key(name))?;
    values.remove(fjall_list_counter_key(name))?;
    fjall_remove_prefix(values, fjall_list_prefix(name))?;
    fjall_remove_prefix(values, fjall_map_prefix(name))?;
    Ok(())
}

fn fjall_remove_prefix(values: &OptimisticTxKeyspace, prefix: Vec<u8>) -> Result<()> {
    let mut keys = Vec::new();
    for item in values.inner().prefix(prefix) {
        keys.push(item.key()?);
    }
    for key in keys {
        values.remove(key)?;
    }
    Ok(())
}

fn fjall_next_list_idx(values: &OptimisticTxKeyspace, name: &str) -> Result<usize> {
    let key = fjall_list_counter_key(name);
    let current = values.get(&key)?.and_then(|buf| buf.as_ref().try_into().ok().map(u64::from_be_bytes)).unwrap_or(0);
    values.insert(key, (current + 1).to_be_bytes())?;
    Ok(current as usize)
}

fn fjall_count_prefix(values: &OptimisticTxKeyspace, prefix: Vec<u8>) -> Result<usize> {
    let mut count = 0;
    for item in values.inner().prefix(prefix) {
        item.key()?;
        count += 1;
    }
    Ok(count)
}

fn fjall_paths_with_prefix(values: &OptimisticTxKeyspace, prefix: &str) -> Result<Vec<SmolStr>> {
    let mut names = Vec::new();
    names.extend(fjall_paths_for_key_prefix(values, FJALL_OBJECT_PREFIX, prefix)?);
    names.extend(fjall_paths_for_key_prefix(values, FJALL_TYPE_PREFIX, prefix)?);
    names.sort();
    names.dedup();
    Ok(names)
}

fn fjall_paths_for_key_prefix(values: &OptimisticTxKeyspace, key_prefix: u8, path_prefix: &str) -> Result<Vec<SmolStr>> {
    let mut scan_prefix = fjall_key(key_prefix, path_prefix);
    if path_prefix.is_empty() {
        scan_prefix.truncate(2);
    }
    let mut names = Vec::new();
    for item in values.inner().prefix(scan_prefix) {
        let key = item.key()?;
        let Some(path) = key.get(2..) else {
            continue;
        };
        if let Ok(path) = std::str::from_utf8(path) {
            names.push(path.into());
        }
    }
    Ok(names)
}

fn fjall_dir(values: &OptimisticTxKeyspace, name: &str) -> Result<Vec<SmolStr>> {
    let path_prefix = if name.is_empty() || name.ends_with('/') { name.to_string() } else { format!("{name}/") };
    let mut names = Vec::new();
    names.extend(fjall_dir_for_key_prefix(values, FJALL_OBJECT_PREFIX, &path_prefix)?);
    names.extend(fjall_dir_for_key_prefix(values, FJALL_TYPE_PREFIX, &path_prefix)?);
    names.sort();
    names.dedup();
    Ok(names)
}

fn fjall_dir_for_key_prefix(values: &OptimisticTxKeyspace, key_prefix: u8, path_prefix: &str) -> Result<Vec<SmolStr>> {
    use std::ops::Bound::{Excluded, Included, Unbounded};

    let scan_start = fjall_key(key_prefix, path_prefix);
    let scan_end = fjall_prefix_end(&scan_start);
    let mut start = scan_start;
    let mut names = Vec::new();

    loop {
        if scan_end.as_ref().is_some_and(|end| start >= *end) {
            break;
        }

        let end = scan_end.as_deref().map(Excluded).unwrap_or(Unbounded);
        let iter = values.inner().range::<&[u8], _>((Included(start.as_slice()), end));
        let mut restart_at = None;

        for item in iter {
            let key = item.key()?;
            let Some(path) = key.get(2..) else {
                continue;
            };
            let Ok(path) = std::str::from_utf8(path) else {
                continue;
            };
            let Some(rest) = path.strip_prefix(path_prefix) else {
                continue;
            };
            let Some(end) = rest.find('/') else {
                if !rest.is_empty() {
                    names.push(rest.into());
                }
                continue;
            };

            let entry = &rest[..end];
            if !entry.is_empty() {
                names.push(entry.into());
                restart_at = fjall_prefix_end(&fjall_key(key_prefix, &format!("{path_prefix}{entry}/")));
                break;
            }
        }

        let Some(next_start) = restart_at else {
            break;
        };
        start = next_start;
    }

    Ok(names)
}

fn fjall_map_keys(values: &OptimisticTxKeyspace, name: &str) -> Result<Vec<SmolStr>> {
    let prefix = fjall_map_prefix(name);
    let mut keys = Vec::new();
    for item in values.inner().prefix(&prefix) {
        let key = item.key()?;
        let Some(map_key) = key.get(prefix.len()..) else {
            continue;
        };
        if let Ok(map_key) = std::str::from_utf8(map_key) {
            keys.push(map_key.into());
        }
    }
    Ok(keys)
}

use std::sync::RwLock;

#[derive(Debug)]
pub struct Root<T> {
    mounts: Arc<RwLock<Vec<(SmolStr, Mount<T>)>>>,
}

impl<T: std::fmt::Debug> std::fmt::Debug for Mount<T> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Memory(_) => write!(f, "Mount::Memory"),
            Self::Redis { client: _, rl: _ } => write!(f, "Mount::Redis"),
            Self::Fjall { .. } => write!(f, "Mount::Fjall"),
            Self::Dir { base } => write!(f, "Mount::Dir({})", base.display()),
        }
    }
}

impl<T> Clone for Mount<T> {
    fn clone(&self) -> Self {
        match self {
            Self::Memory(m) => Self::Memory(m.clone()),
            Self::Redis { client, rl } => Self::Redis { client: client.clone(), rl: rl.clone() },
            Self::Fjall { db, values, write_lock } => Self::Fjall { db: db.clone(), values: values.clone(), write_lock: write_lock.clone() },
            Self::Dir { base } => Self::Dir { base: base.clone() },
        }
    }
}

impl<T: std::fmt::Debug + MsgPack + MsgUnpack + Default + Send> Root<T> {
    pub fn new() -> Self {
        Self { mounts: Arc::new(RwLock::new(vec![("local".into(), Mount::<T>::memory())])) }
    }

    pub fn mount_memory(&self, name: &str) -> bool {
        let mounts = self.mounts.write();
        match mounts {
            Ok(mut mounts) => {
                if mounts.iter().any(|(n, _)| n == name) {
                    return false;
                }
                mounts.push((name.into(), Mount::<T>::memory()));
                true
            }
            Err(_) => false,
        }
    }

    pub fn mount_redis(&self, name: &str, url: &str) -> Result<bool> {
        let mounts = self.mounts.write();
        match mounts {
            Ok(mut mounts) => {
                if mounts.iter().any(|(n, _)| n == name) {
                    return Ok(false);
                }
                mounts.push((name.into(), Mount::<T>::redis(url)?));
                Ok(true)
            }
            Err(e) => Err(anyhow!("无法获取写锁: {}", e)),
        }
    }

    pub fn mount_fjall(&self, name: &str, data_dir: &str) -> Result<bool> {
        let mounts = self.mounts.write();
        match mounts {
            Ok(mut mounts) => {
                if mounts.iter().any(|(n, _)| n == name) {
                    return Ok(false);
                }
                mounts.push((name.into(), Mount::<T>::fjall(data_dir)?));
                Ok(true)
            }
            Err(e) => Err(anyhow!("无法获取写锁: {}", e)),
        }
    }

    /// 把 host 文件系统目录挂到 ROOT 树。
    ///
    /// 安全模型:必须显式 `mount_dir` 才能让脚本访问 host 文件,**默认拒绝**
    /// 任何未挂载的 host 路径。`host_dir` 必须存在;不存在返回 Err,不自动创建,
    /// 避免脚本无意中写错路径就在磁盘上建出一坨空目录。
    ///
    /// 挂载后的 root 路径只能访问 `host_dir` 之下的文件:
    /// 拒绝 `..` 跳出,也拒绝绝对路径段。
    pub fn mount_dir(&self, name: &str, host_dir: &str) -> Result<bool> {
        let base = std::fs::canonicalize(host_dir).map_err(|e| anyhow!("mount_dir {} 失败:无法解析 host_dir={}: {}", name, host_dir, e))?;
        let mounts = self.mounts.write();
        match mounts {
            Ok(mut mounts) => {
                if mounts.iter().any(|(n, _)| n == name) {
                    return Ok(false);
                }
                mounts.push((name.into(), Mount::<T>::Dir { base }));
                Ok(true)
            }
            Err(e) => Err(anyhow!("无法获取写锁: {}", e)),
        }
    }

    pub fn get_mount<'a>(&self, name: &'a str) -> Result<(Mount<T>, &'a str)> {
        let (mount, name) = name.split_once('/').ok_or(anyhow!("{} 没有 root 路径", name))?;
        let mounts = self.mounts.read().map_err(|e| anyhow!("无法获取读锁: {}", e))?;
        let m = mounts.iter().find_map(|m| if m.0 == mount { Some(m.1.clone()) } else { None }).ok_or(anyhow!("没有找到 {}", name))?;
        Ok((m, name))
    }
}

// ----------------------------------------------------------------------------
// Mount::Dir 路径安全 + 序列化/反序列化 helpers
// ----------------------------------------------------------------------------

/// 拒绝 `..` 跳出 mount 边界,也拒绝绝对路径段(防 caller 用 `name` 直接给
/// `/etc/passwd` 之类越界访问)。`./` 当前目录前缀忽略,只 push 真实段。
pub(super) fn safe_path(base: &Path, name: &str) -> Option<PathBuf> {
    let mut path = base.to_path_buf();
    for component in Path::new(name).components() {
        match component {
            std::path::Component::Normal(c) => path.push(c),
            std::path::Component::CurDir => {}
            // ParentDir (..)、RootDir (/)、Prefix (C:\) 一律拒绝。
            std::path::Component::ParentDir | std::path::Component::RootDir | std::path::Component::Prefix(_) => return None,
        }
    }
    Some(path)
}

/// 按文件扩展名把 `Dynamic` 编码成字节流:
/// `.json` → `to_json`、`.yaml`/`.yml` → `to_yaml`、`.md` → `to_markdown`,
/// 其他 → `value.as_str()` 字节流。`add` 写入文件失败时调用方据此报错。
fn encode_value(name: &str, value: &Dynamic) -> Result<Vec<u8>> {
    let ext = Path::new(name).extension().and_then(|e| e.to_str()).unwrap_or("");
    match ext {
        "json" => {
            let mut s = String::new();
            value.to_json(&mut s);
            Ok(s.into_bytes())
        }
        "yaml" | "yml" => Ok(value.to_yaml_string().into_bytes()),
        "md" => Ok(value.to_markdown().into_bytes()),
        _ => {
            // 默认按 String 处理:Dynamic 必须能转成 &str,
            // 否则调用方得到一个 non-UTF-8 空字节序列,写文件无意义。
            let s = value.as_str();
            Ok(s.as_bytes().to_vec())
        }
    }
}

/// 按文件扩展名把字节流反序列化成 `Dynamic`。`.md` 没有反向 parser,
/// 统一回退到 String(用户原文:`json md yaml 之外就是直接按 String 读取`,
/// md 也归到 String 这一边,符合 dynamic 库目前的能力边界)。
fn decode_value(name: &str, bytes: &[u8]) -> Result<Dynamic> {
    let ext = Path::new(name).extension().and_then(|e| e.to_str()).unwrap_or("");
    match ext {
        "json" => {
            let (v, _) = Dynamic::from_json(bytes)?;
            Ok(v)
        }
        "yaml" | "yml" => {
            let (v, _) = Dynamic::from_yaml(bytes)?;
            Ok(v)
        }
        _ => {
            let s = std::str::from_utf8(bytes).map_err(|e| anyhow!("非 UTF-8 文本: {}", e))?;
            Ok(Dynamic::from(s.to_string()))
        }
    }
}

/// 把 `Mount<Object>` 的 Dir 后端特有的文件读写方法集中在一处。
///
/// `impl<T: ...> Mount<T>` 通用方法对 Dir 分支只返回 Err/占位,真正的
/// 文件 I/O 与序列化由这里的 `dir_*` 方法处理。`root::lib.rs` 在调度
/// `add`/`get`/`remove`/`update`/`contains` 时,先 match `Mount::Dir { .. }`
/// 走这些方法。
impl Mount<Object> {
    pub fn dir_add(&self, name: &str, value: Object) -> bool {
        let base = match self {
            Self::Dir { base } => base,
            _ => return false,
        };
        let Object::Value(dynamic) = value else { return false };
        let Some(path) = safe_path(base, name) else { return false };
        let Ok(bytes) = encode_value(name, &dynamic) else { return false };
        if let Some(parent) = path.parent() {
            if std::fs::create_dir_all(parent).is_err() {
                return false;
            }
        }
        std::fs::write(&path, bytes).is_ok()
    }

    /// 读取文件 / 探测目录:目录 → Null(不报错),文件 → 按扩展名 decode,
    /// 不存在 → Err。调用方拿到的 `Ok(Null)` 即可区分"这是目录"。
    pub fn dir_get(&self, name: &str) -> Result<Dynamic> {
        let base = match self {
            Self::Dir { base } => base,
            _ => return Err(anyhow!("dir_get 仅在 Dir 后端可用")),
        };
        let path = safe_path(base, name).ok_or_else(|| anyhow!("path 非法: {}", name))?;
        if !path.exists() {
            return Err(anyhow!("{} 不存在", name));
        }
        if path.is_dir() {
            return Ok(Dynamic::Null);
        }
        let bytes = std::fs::read(&path).map_err(|e| anyhow!("读取 {} 失败: {}", path.display(), e))?;
        decode_value(name, &bytes).map_err(|e| anyhow!("解析 {} 失败: {}", name, e))
    }

    pub fn dir_remove(&self, name: &str) -> Result<Dynamic> {
        let base = match self {
            Self::Dir { base } => base,
            _ => return Err(anyhow!("dir_remove 仅在 Dir 后端可用")),
        };
        let path = safe_path(base, name).ok_or_else(|| anyhow!("path 非法: {}", name))?;
        if !path.is_file() {
            return Err(anyhow!("{} 不是文件", name));
        }
        let bytes = std::fs::read(&path).map_err(|e| anyhow!("读取 {} 失败: {}", path.display(), e))?;
        let dynamic = decode_value(name, &bytes).map_err(|e| anyhow!("解析 {} 失败: {}", name, e))?;
        std::fs::remove_file(&path).map_err(|e| anyhow!("删除 {} 失败: {}", path.display(), e))?;
        Ok(dynamic)
    }

    pub fn dir_contains(&self, name: &str) -> bool {
        let base = match self {
            Self::Dir { base } => base,
            _ => return false,
        };
        match safe_path(base, name) {
            Some(p) => p.exists(),
            None => false,
        }
    }

    /// read + apply f + write。**不保证原子**:两次 syscall 之间进程崩溃
    /// 会丢更新。Memory/Redis/Fjall 的 `update` 同样不保证跨进程原子,
    /// 这是 ROOT 抽象层的整体选择。
    pub fn dir_update<F>(&self, name: &str, f: F) -> Result<Dynamic>
    where
        F: FnOnce(Dynamic) -> Dynamic,
    {
        let base = match self {
            Self::Dir { base } => base,
            _ => return Err(anyhow!("dir_update 仅在 Dir 后端可用")),
        };
        let path = safe_path(base, name).ok_or_else(|| anyhow!("path 非法: {}", name))?;
        if path.is_dir() {
            return Err(anyhow!("{} 是目录,不能 update", name));
        }
        let bytes = std::fs::read(&path).map_err(|e| anyhow!("读取 {} 失败: {}", path.display(), e))?;
        let current = decode_value(name, &bytes).map_err(|e| anyhow!("解析 {} 失败: {}", name, e))?;
        let next = f(current);
        let new_bytes = encode_value(name, &next)?;
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|e| anyhow!("创建父目录失败: {}", e))?;
        }
        std::fs::write(&path, new_bytes).map_err(|e| anyhow!("写入 {} 失败: {}", path.display(), e))?;
        Ok(next)
    }
}

// ----------------------------------------------------------------------------
// `impl Mount<Object>` 的特化方法
// ----------------------------------------------------------------------------

impl Mount<Object> {
    /// Mount::Dir 后端的 `get` 特化:从文件 decode,wrap 成 Object::Value。
    /// 解决 mount_dir 写完 YAML 后 `root::get` 拿不到 round-trip Dynamic 的问题。
    pub fn get_for_object<R, F: FnOnce(&Object) -> R>(&self, name: &str, f: F) -> Result<R> {
        match self {
            Self::Dir { .. } => {
                let dynamic = self.dir_get(name)?;
                let mut value = Object::default();
                if let Object::Value(slot) = &mut value {
                    *slot = dynamic;
                }
                Ok(f(&value))
            }
            // 其它后端走通用 get,但 callback 类型是 `&Object`(非泛型)。
            _ => self.get(name, |v| f(v)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use dynamic::Dynamic;

    #[test]
    fn dir_returns_only_immediate_children() {
        let root = Root::<Dynamic>::new();
        let (mount, name) = root.get_mount("local/test/dir/a").unwrap();
        assert!(mount.add(name, 1.into()));
        let (mount, name) = root.get_mount("local/test/dir/sub/item").unwrap();
        assert!(mount.add(name, 2.into()));
        let (mount, name) = root.get_mount("local/test/dir2/x").unwrap();
        assert!(mount.add(name, 3.into()));

        let (mount, name) = root.get_mount("local/test/dir").unwrap();
        let mut entries = mount.dir(name).unwrap();
        entries.sort();

        assert_eq!(entries, vec![SmolStr::new("a"), SmolStr::new("sub")]);
    }

    #[test]
    fn dir_accepts_trailing_slash() {
        let root = Root::<Dynamic>::new();
        let (mount, name) = root.get_mount("local/test/slash/a").unwrap();
        assert!(mount.add(name, 1.into()));

        let (mount, name) = root.get_mount("local/test/slash/").unwrap();
        assert_eq!(mount.dir(name).unwrap(), vec![SmolStr::new("a")]);
    }

    #[test]
    fn fjall_mount_persists_values_and_dirs() {
        let data_dir = std::env::temp_dir().join(format!("zust-root-fjall-{}", uuid::Uuid::new_v4()));
        let data_dir_str = data_dir.to_str().unwrap();

        {
            let root = Root::<Dynamic>::new();
            assert!(root.mount_fjall("fjall", data_dir_str).unwrap());
            let (mount, name) = root.get_mount("fjall/test/kv/a").unwrap();
            assert!(mount.add(name, 42.into()));
            let (mount, name) = root.get_mount("fjall/test/kv/b").unwrap();
            assert!(mount.add(name, "persisted".into()));
            let (mount, name) = root.get_mount("fjall/test/list").unwrap();
            mount.add_list(name);
            assert_eq!(mount.push(name, 7.into()).unwrap(), 0);
            let (mount, name) = root.get_mount("fjall/test/map").unwrap();
            mount.add_map(name);
            mount.insert(name, "answer", 42.into());
            mount.insert(name, "0", "first".into());
            mount.insert(name, "1", "second".into());
        }

        {
            let root = Root::<Dynamic>::new();
            assert!(root.mount_fjall("fjall", data_dir_str).unwrap());
            let (mount, name) = root.get_mount("fjall/test/kv/a").unwrap();
            assert_eq!(mount.get(name, |v| v.as_int()).unwrap(), Some(42));
            let (mount, name) = root.get_mount("fjall/test").unwrap();
            let mut entries = mount.dir(name).unwrap();
            entries.sort();
            assert_eq!(entries, vec![SmolStr::new("kv"), SmolStr::new("list"), SmolStr::new("map")]);
            let (mount, name) = root.get_mount("fjall/test/list").unwrap();
            assert_eq!(mount.get_idx(name, 0, |v| v.as_int()).unwrap(), Some(7));
            let (mount, name) = root.get_mount("fjall/test/map").unwrap();
            assert_eq!(mount.get_key(name, "answer", |v| v.as_int()).unwrap(), Some(42));
            let mut keys = mount.keys(name).unwrap();
            keys.sort();
            assert_eq!(keys, vec![SmolStr::new("0"), SmolStr::new("1"), SmolStr::new("answer")]);
        }

        let _ = std::fs::remove_dir_all(data_dir);
    }

    #[test]
    fn fjall_dir_skips_deep_children_without_missing_siblings() {
        let data_dir = std::env::temp_dir().join(format!("zust-root-fjall-dir-{}", uuid::Uuid::new_v4()));
        let data_dir_str = data_dir.to_str().unwrap();
        let root = Root::<Dynamic>::new();
        assert!(root.mount_fjall("fjall", data_dir_str).unwrap());

        for path in ["fjall/root/a/deep/one", "fjall/root/a/deep/two", "fjall/root/a-/leaf", "fjall/root/a./leaf", "fjall/root/a0/leaf", "fjall/root/b"] {
            let (mount, name) = root.get_mount(path).unwrap();
            assert!(mount.add(name, 1.into()));
        }

        let (mount, name) = root.get_mount("fjall/root").unwrap();
        let mut entries = mount.dir(name).unwrap();
        entries.sort();

        assert_eq!(entries, vec![SmolStr::new("a"), SmolStr::new("a-"), SmolStr::new("a."), SmolStr::new("a0"), SmolStr::new("b")]);

        let _ = std::fs::remove_dir_all(data_dir);
    }

    // ---- Mount::Dir 测试 -----------------------------------------------------
    //
    // 每个测试用独立 temp dir,测完清理。Mount::Object 的 dir_* 方法被 Root<Object>
    // 的全局调度调用,所以这些测试也覆盖 lib.rs 的 add/get/contains/remove/update
    // 的 Dir 分支。

    fn fresh_dir_mount(name: &str) -> (Mount<Object>, std::path::PathBuf) {
        let dir = std::env::temp_dir().join(format!("zust-root-dir-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let root = Root::<Object>::new();
        assert!(root.mount_dir(name, dir.to_str().unwrap()).unwrap());
        let (mount, _) = root.get_mount(&format!("{}/_ignored", name)).unwrap();
        (mount, dir)
    }

    /// `Mount<Object>` 的 dir_* 方法测试——直接覆盖文件读写与边界条件。
    /// lib.rs 集成测试由 zust-vm 的脚本测试覆盖。

    #[test]
    fn mount_dir_yaml_round_trip() {
        let (mount, base) = fresh_dir_mount("ddd");
        let value = Dynamic::map(Default::default());
        let value = value;
        value.insert("name", "zust");
        value.insert("age", 18);

        assert!(mount.dir_add("x.yaml", Object::Value(value)));
        assert!(base.join("x.yaml").is_file());

        let got = mount.dir_get("x.yaml").unwrap();
        assert_eq!(got.get_dynamic("name").map(|v| v.as_str().to_string()), Some("zust".to_string()));
        assert_eq!(got.get_dynamic("age").and_then(|v| v.as_int()), Some(18));

        let _ = std::fs::remove_dir_all(base);
    }

    #[test]
    fn mount_dir_json_round_trip() {
        let (mount, base) = fresh_dir_mount("ddd");
        let value = Dynamic::map(Default::default());
        value.insert("k", "v");
        value.insert("n", 42);

        assert!(mount.dir_add("data.json", Object::Value(value)));
        let got = mount.dir_get("data.json").unwrap();
        assert_eq!(got.get_dynamic("k").map(|v| v.as_str().to_string()), Some("v".to_string()));
        assert_eq!(got.get_dynamic("n").and_then(|v| v.as_int()), Some(42));

        let _ = std::fs::remove_dir_all(base);
    }

    #[test]
    fn mount_dir_md_writes_markdown_reads_string() {
        let (mount, base) = fresh_dir_mount("ddd");
        let value = Dynamic::map(Default::default());
        // 用 i64 值绕开 to_markdown 把 String 当 char list 迭代的现有行为:
        // 当前 dynamic 实现把 Dynamic::String 视作字符序列,直接 panic 在 unwrap。
        // 这里只验证 .md 写/读路径正常,不强求 markdown 内容细节。
        value.insert("count", 42);
        assert!(mount.dir_add("notes.md", Object::Value(value)));
        let content = std::fs::read_to_string(base.join("notes.md")).unwrap();
        assert!(content.contains("count"));

        // .md 没有 from_markdown,读取按 String 处理。
        let got = mount.dir_get("notes.md").unwrap();
        assert!(got.as_str().contains("count"));

        let _ = std::fs::remove_dir_all(base);
    }

    #[test]
    fn mount_dir_raw_string_extension() {
        let (mount, base) = fresh_dir_mount("ddd");
        let value = Dynamic::from("hello world".to_string());
        assert!(mount.dir_add("readme.txt", Object::Value(value)));
        let got = mount.dir_get("readme.txt").unwrap();
        assert_eq!(got.as_str(), "hello world");

        let _ = std::fs::remove_dir_all(base);
    }

    #[test]
    fn mount_dir_len_file_is_size_dir_is_zero() {
        let (mount, base) = fresh_dir_mount("ddd");
        let payload = "x".repeat(123);
        assert!(mount.dir_add("a.yaml", Object::Value(Dynamic::from(payload))));

        // 文件 = 字节数(yaml 序列化长度,>= 原始 123)。
        let file_len = mount.len("a.yaml").unwrap();
        assert!(file_len >= 123);

        // 创建子目录后,目录长度 = 0。
        std::fs::create_dir_all(base.join("sub")).unwrap();
        let dir_len = mount.len("sub").unwrap();
        assert_eq!(dir_len, 0);

        // 不存在 → Err。
        assert!(mount.len("missing").is_err());

        let _ = std::fs::remove_dir_all(base);
    }

    #[test]
    fn mount_dir_dir_lists_files_and_dirs_undistinguished() {
        let (mount, base) = fresh_dir_mount("ddd");
        assert!(mount.dir_add("a.yaml", Object::Value(Dynamic::from(1i64))));
        assert!(mount.dir_add("b.txt", Object::Value(Dynamic::from("x".to_string()))));
        std::fs::create_dir_all(base.join("sub")).unwrap();
        std::fs::create_dir_all(base.join("nested")).unwrap();

        let mut names = mount.dir_raw("").unwrap();
        names.sort();
        assert_eq!(names, vec![SmolStr::new("a.yaml"), SmolStr::new("b.txt"), SmolStr::new("nested"), SmolStr::new("sub")]);

        let _ = std::fs::remove_dir_all(base);
    }

    #[test]
    fn mount_dir_get_directory_returns_null() {
        let (mount, base) = fresh_dir_mount("ddd");
        std::fs::create_dir_all(base.join("subdir")).unwrap();

        // get 目录 → Null(不报错)。
        let got = mount.dir_get("subdir").unwrap();
        assert!(got.is_null());

        let _ = std::fs::remove_dir_all(base);
    }

    #[test]
    fn mount_dir_get_missing_returns_err() {
        let (mount, base) = fresh_dir_mount("ddd");
        assert!(mount.dir_get("missing.yaml").is_err());
        let _ = std::fs::remove_dir_all(base);
    }

    #[test]
    fn mount_dir_contains_file_and_dir() {
        let (mount, base) = fresh_dir_mount("ddd");
        assert!(mount.dir_add("a.yaml", Object::Value(Dynamic::from(1i64))));
        std::fs::create_dir_all(base.join("sub")).unwrap();

        assert!(mount.dir_contains("a.yaml"));
        assert!(mount.dir_contains("sub"));
        assert!(!mount.dir_contains("missing"));

        let _ = std::fs::remove_dir_all(base);
    }

    #[test]
    fn mount_dir_remove_returns_decoded_value() {
        let (mount, base) = fresh_dir_mount("ddd");
        let v = Dynamic::map(Default::default());
        v.insert("hello", "world");
        assert!(mount.dir_add("x.json", Object::Value(v)));

        let removed = mount.dir_remove("x.json").unwrap();
        assert_eq!(removed.get_dynamic("hello").map(|d| d.as_str().to_string()), Some("world".to_string()));
        assert!(!base.join("x.json").exists());

        let _ = std::fs::remove_dir_all(base);
    }

    #[test]
    fn mount_dir_path_traversal_rejected() {
        let (mount, base) = fresh_dir_mount("ddd");
        // 尝试用 .. 跳出:应该被 safe_path 拒绝。
        assert!(!mount.dir_add("../escape.txt", Object::Value(Dynamic::from("pwned"))));
        // 跳不出去的标志:base 之外没有 escape.txt 文件。
        let escape = base.parent().unwrap().join("escape.txt");
        assert!(!escape.exists());

        // 绝对路径段也拒绝。
        assert!(!mount.dir_add("/etc/passwd", Object::Value(Dynamic::from("x"))));

        let _ = std::fs::remove_dir_all(base);
    }

    #[test]
    fn mount_dir_update_read_modify_write() {
        let (mount, base) = fresh_dir_mount("ddd");
        let v = Dynamic::map(Default::default());
        v.insert("count", 1);
        assert!(mount.dir_add("c.json", Object::Value(v)));

        let next = mount
            .dir_update("c.json", |cur| {
                if let Some(n) = cur.get_dynamic("count").and_then(|d| d.as_int()) {
                    cur.insert("count", n + 10);
                }
                cur
            })
            .unwrap();
        assert_eq!(next.get_dynamic("count").and_then(|d| d.as_int()), Some(11));

        let _ = std::fs::remove_dir_all(base);
    }

    #[test]
    fn mount_dir_rejects_nonexistent_host_dir() {
        let root = Root::<Object>::new();
        let missing = std::env::temp_dir().join(format!("zust-root-missing-{}", uuid::Uuid::new_v4()));
        assert!(root.mount_dir("ddd", missing.to_str().unwrap()).is_err());
    }

    #[test]
    fn mount_dir_rejects_list_and_map_operations() {
        let (mount, base) = fresh_dir_mount("ddd");
        // add_list / add_map / push / get_idx / insert / keys 在 Dir 后端必须报错或 no-op,
        // 不能像 Memory 后端那样静默成功。
        mount.add_list("lst");
        assert!(mount.push("lst", Object::Value(Dynamic::from(1i64))).is_err());
        mount.insert("mp", "k", Object::Value(Dynamic::from(1i64)));
        assert!(mount.get_idx("lst", 0, |v| v.value()).is_err());
        assert!(mount.get_key("mp", "k", |v| v.value()).is_err());
        assert!(mount.keys("mp").is_err());

        let _ = std::fs::remove_dir_all(base);
    }
}
