use dynamic::{MsgPack, MsgUnpack};
use rand::random_range;
use scc::HashMap;
use smol_str::SmolStr;

use anyhow::{Result, anyhow};

use super::sync_await;
use crate::directory;
use crate::node::Node;
use fjall::{KeyspaceCreateOptions, OptimisticTxDatabase, OptimisticTxKeyspace};
use redis::AsyncCommands;
use redis::Commands;
use std::sync::Arc;

use rslock::LockManager;

pub enum Mount<T> {
    Memory(Arc<HashMap<SmolStr, Node<T>>>),
    Redis {
        client: redis::Client,
        rl: LockManager,
    },
    Fjall {
        values: OptimisticTxKeyspace,
        write_lock: Arc<std::sync::Mutex<()>>,
    },
}

impl<T: std::fmt::Debug + MsgPack + MsgUnpack + Default + Send> Mount<T> {
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
        Ok(Self::Fjall { values, write_lock: Arc::new(std::sync::Mutex::new(())) })
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
            Self::Fjall { values, write_lock } => {
                let mut buf = Vec::new();
                value.encode(&mut buf);
                let Ok(_guard) = write_lock.lock() else {
                    return false;
                };
                fjall_clear_node(values, name).is_ok() && values.insert(fjall_object_key(name), buf).is_ok()
            }
        }
    }

    pub fn contains(&self, name: &str) -> bool {
        match self {
            Self::Memory(m) => m.contains_sync(name),
            Self::Redis { client, rl: _ } => client.get_connection().and_then(|mut conn| conn.exists::<&str, bool>(name)).unwrap_or(false),
            Self::Fjall { values, .. } => values.contains_key(fjall_object_key(name)).unwrap_or(false) || values.contains_key(fjall_type_key(name)).unwrap_or(false),
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
                        let time_out = random_range(0..1000); //等待随机时间
                        if let Ok(lock) = rl.lock(name.as_str(), std::time::Duration::from_millis(time_out)).await {
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
                    }
                })
            }
            Self::Fjall { values, write_lock } => {
                let _guard = write_lock.lock().map_err(|e| anyhow!("无法获取 fjall 写锁: {}", e))?;
                let mut v = fjall_get_object::<T>(values, name)?;
                let r = f(&mut v);
                fjall_insert_object(values, name, &v)?;
                Ok(r)
            }
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
                        let time_out = random_range(0..1000); //等待随机时间
                        let lock_name = format!("{}::{}", name, key); //为这个 name 里面的 key 单独上锁
                        if let Ok(lock) = rl.lock(lock_name.as_str(), std::time::Duration::from_millis(time_out)).await {
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
                    }
                })
            }
            Self::Fjall { values, write_lock } => {
                let _guard = write_lock.lock().map_err(|e| anyhow!("无法获取 fjall 写锁: {}", e))?;
                let mut v = fjall_get_map_item::<T>(values, name, key)?;
                let r = f(&mut v);
                fjall_insert_map_item(values, name, key, &v)?;
                Ok(r)
            }
        }
    }

    pub fn dir(&self, name: &str) -> Result<Vec<SmolStr>> {
        let prefix = if name.is_empty() || name.ends_with('/') { name.to_string() } else { format!("{name}/") };
        if let Self::Redis { client, rl: _ } = self {
            let mut conn = client.get_connection()?;
            return directory::children(&mut conn, name);
        }

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
        }
    }

    pub fn remove(&self, name: &str) -> Result<T> {
        match self {
            Self::Memory(m) => m.remove_sync(name).and_then(|(_, v)| v.into_object()).ok_or(anyhow!("{} 不存在", name)),
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
            Self::Fjall { values, .. } => {
                let v = fjall_get_object(values, name)?;
                values.remove(fjall_object_key(name))?;
                Ok(v)
            }
        }
    }

    pub fn add_list(&self, name: &str) {
        match self {
            Self::Memory(m) => {
                m.upsert_sync(name.into(), Node::<T>::list());
            } // 强制插入 肯定成功
            Self::Redis { client: _, rl: _ } => {}
            Self::Fjall { values, write_lock } => {
                if let Ok(_guard) = write_lock.lock() {
                    let _ = fjall_clear_node(values, name).and_then(|_| fjall_set_node_type(values, name, FjallNodeType::List));
                }
            }
        }
    }

    pub fn add_map(&self, name: &str) {
        match self {
            Self::Memory(m) => {
                m.upsert_sync(name.into(), Node::<T>::map());
            } // 强制插入 肯定成功
            Self::Redis { client: _, rl: _ } => {}
            Self::Fjall { values, write_lock } => {
                if let Ok(_guard) = write_lock.lock() {
                    let _ = fjall_clear_node(values, name).and_then(|_| fjall_set_node_type(values, name, FjallNodeType::Map));
                }
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
            Self::Fjall { values, write_lock } => {
                let _guard = write_lock.lock().map_err(|e| anyhow!("无法获取 fjall 写锁: {}", e))?;
                match fjall_node_type(values, name)? {
                    Some(FjallNodeType::List) => {}
                    Some(FjallNodeType::Map) => return Err(anyhow!("push {} 失败", name)),
                    None => fjall_set_node_type(values, name, FjallNodeType::List)?,
                }
                let idx = fjall_next_list_idx(values, name)?;
                fjall_insert_list_item(values, name, idx, &value)?;
                Ok(idx)
            }
        }
    }

    pub fn get_idx<R, F: FnOnce(&T) -> R>(&self, name: &str, idx: usize, f: F) -> Result<R> {
        match self {
            Self::Memory(m) => m.read_sync(name, |_, v| v.get_idx(idx, f)).flatten().ok_or(anyhow!("get_idx {} 失败", name)),
            Self::Redis { client, rl: _ } => {
                let mut conn = client.get_connection()?;
                let buf: Vec<u8> = conn.lindex(name, idx as isize)?;
                let (v, _) = T::decode(buf.as_slice())?;
                Ok(f(&v))
            }
            Self::Fjall { values, .. } => fjall_get_list_item(values, name, idx).map(|v| f(&v)),
        }
    }

    pub fn get_idx_mut<R, F: FnMut(&mut T) -> R>(&self, name: &str, idx: usize, mut f: F) -> Result<R> {
        match self {
            Self::Memory(m) => m.update_sync(name, |_, v| v.get_idx_mut(idx, f)).flatten().ok_or(anyhow!("get_idx {} 失败", name)),
            Self::Redis { client, rl: _ } => {
                let mut conn = client.get_connection()?;
                let buf: Vec<u8> = conn.lindex(name, idx as isize)?;
                let (mut v, _) = T::decode(buf.as_slice())?;
                Ok(f(&mut v))
            }
            Self::Fjall { values, write_lock } => {
                let _guard = write_lock.lock().map_err(|e| anyhow!("无法获取 fjall 写锁: {}", e))?;
                let mut v = fjall_get_list_item::<T>(values, name, idx)?;
                let r = f(&mut v);
                fjall_insert_list_item(values, name, idx, &v)?;
                Ok(r)
            }
        }
    }

    pub fn remove_idx(&self, name: &str, idx: usize) -> Result<T> {
        match self {
            Self::Memory(m) => m.update_sync(name, |_, v| v.remove_idx(idx)).flatten().ok_or(anyhow!("remove_idx {} 失败", name)),
            Self::Redis { client, rl: _ } => {
                let mut conn = client.get_connection()?;
                let buf: Vec<u8> = conn.lindex(name, idx as isize)?;
                let v = T::decode(buf.as_slice()).map(|(v, _)| v).unwrap_or(T::default());
                let _: () = conn.lset(name, idx as isize, Vec::new())?;
                Ok(v)
            }
            Self::Fjall { values, .. } => {
                let key = fjall_list_item_key(name, idx);
                let buf = values.get(&key)?.ok_or(anyhow!("remove_idx {} 失败", name))?;
                let (v, _) = T::decode(buf.as_ref())?;
                values.remove(key)?;
                Ok(v)
            }
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
            Self::Fjall { values, write_lock } => {
                let _guard = write_lock.lock().ok()?;
                match fjall_node_type(values, name).ok()? {
                    Some(FjallNodeType::Map) => {}
                    Some(FjallNodeType::List) => return None,
                    None => fjall_set_node_type(values, name, FjallNodeType::Map).ok()?,
                }
                let old = fjall_get_map_item(values, name, key).ok();
                fjall_insert_map_item(values, name, key, &value).ok()?;
                old
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
        }
    }

    pub fn keys(&self, name: &str) -> Result<Vec<SmolStr>> {
        match self {
            Self::Memory(m) => m.read_sync(name, |_, v| v.keys()).flatten().ok_or(anyhow!("get_key {} 失败", name)),
            Self::Redis { client, rl: _ } => {
                let mut conn = client.get_connection()?;
                let keys: Vec<String> = conn.hkeys(name)?;
                Ok(keys.into_iter().map(|k| k.into()).collect())
            }
            Self::Fjall { values, .. } => fjall_map_keys(values, name),
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
            Self::Fjall { values, .. } => {
                let item_key = fjall_map_item_key(name, key);
                let buf = values.get(&item_key)?.ok_or(anyhow!("remove_key {} 失败", name))?;
                let (v, _) = T::decode(buf.as_ref())?;
                values.remove(item_key)?;
                Ok(v)
            }
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

fn fjall_decode_value<T: MsgUnpack>(buf: &[u8]) -> Result<T> {
    T::decode(buf).map(|(value, _)| value)
}

fn fjall_encode_value<T: MsgPack>(value: &T) -> Vec<u8> {
    let mut buf = Vec::new();
    value.encode(&mut buf);
    buf
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
        }
    }
}

impl<T> Clone for Mount<T> {
    fn clone(&self) -> Self {
        match self {
            Self::Memory(m) => Self::Memory(m.clone()),
            Self::Redis { client, rl } => Self::Redis { client: client.clone(), rl: rl.clone() },
            Self::Fjall { values, write_lock } => Self::Fjall { values: values.clone(), write_lock: write_lock.clone() },
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

    pub fn get_mount<'a>(&self, name: &'a str) -> Result<(Mount<T>, &'a str)> {
        let (mount, name) = name.split_once('/').ok_or(anyhow!("{} 没有 root 路径", name))?;
        let mounts = self.mounts.read().map_err(|e| anyhow!("无法获取读锁: {}", e))?;
        let m = mounts.iter().find_map(|m| if m.0 == mount { Some(m.1.clone()) } else { None }).ok_or(anyhow!("没有找到 {}", name))?;
        Ok((m, name))
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
}
