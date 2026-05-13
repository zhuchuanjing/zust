use dynamic::{MsgPack, MsgUnpack};
use rand::random_range;
use scc::HashMap;
use smol_str::SmolStr;

use anyhow::{Result, anyhow};

use super::sync_await;
use crate::directory;
use crate::node::Node;
use redis::AsyncCommands;
use redis::Commands;
use std::sync::Arc;

use rslock::LockManager;

pub enum Mount<T> {
    Memory(Arc<HashMap<SmolStr, Node<T>>>),
    Redis { client: redis::Client, rl: LockManager },
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
        }
    }

    pub fn contains(&self, name: &str) -> bool {
        match self {
            Self::Memory(m) => m.contains_sync(name),
            Self::Redis { client, rl: _ } => client.get_connection().and_then(|mut conn| conn.exists::<&str, bool>(name)).unwrap_or(false),
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
        }
    }

    pub fn dir(&self, name: &str) -> Result<Vec<SmolStr>> {
        let prefix = if name.is_empty() || name.ends_with('/') { name.to_string() } else { format!("{name}/") };
        if let Self::Redis { client, rl: _ } = self {
            let mut conn = client.get_connection()?;
            return Ok(Self::dir_entries_from_children(&prefix, directory::children(&mut conn, name)?));
        }

        let raw = self.dir_raw(&prefix)?;
        Ok(Self::dir_entries_from_raw(&prefix, raw))
    }

    fn dir_entries_from_children(prefix: &str, children: Vec<SmolStr>) -> Vec<SmolStr> {
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
                names.push(format!("{prefix}{entry}").into());
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
                names.append(&mut Self::dir_entries_from_children(&prefix, directory::children(&mut conn, name)?));
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
        }
    }

    pub fn add_list(&self, name: &str) {
        match self {
            Self::Memory(m) => {
                m.upsert_sync(name.into(), Node::<T>::list());
            } // 强制插入 肯定成功
            Self::Redis { client: _, rl: _ } => {}
        }
    }

    pub fn add_map(&self, name: &str) {
        match self {
            Self::Memory(m) => {
                m.upsert_sync(name.into(), Node::<T>::map());
            } // 强制插入 肯定成功
            Self::Redis { client: _, rl: _ } => {}
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
        }
    }

    pub fn keys(&self, name: &str) -> Result<Vec<SmolStr>> {
        match self {
            Self::Memory(m) => m.read_sync(name, |_, v| v.keys()).flatten().ok_or(anyhow!("get_key {} 失败", name)),
            Self::Redis { client, rl: _ } => {
                let mut conn = client.get_connection()?;
                let keys: Vec<String> = conn.hkeys(name).unwrap_or(Vec::new());
                Ok(keys.into_iter().map(|k| k.into()).collect())
            }
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
        }
    }
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
        }
    }
}

impl<T> Clone for Mount<T> {
    fn clone(&self) -> Self {
        match self {
            Self::Memory(m) => Self::Memory(m.clone()),
            Self::Redis { client, rl } => Self::Redis { client: client.clone(), rl: rl.clone() },
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

        assert_eq!(entries, vec![SmolStr::new("test/dir/a"), SmolStr::new("test/dir/sub")]);
    }

    #[test]
    fn dir_accepts_trailing_slash() {
        let root = Root::<Dynamic>::new();
        let (mount, name) = root.get_mount("local/test/slash/a").unwrap();
        assert!(mount.add(name, 1.into()));

        let (mount, name) = root.get_mount("local/test/slash/").unwrap();
        assert_eq!(mount.dir(name).unwrap(), vec![SmolStr::new("test/slash/a")]);
    }
}
