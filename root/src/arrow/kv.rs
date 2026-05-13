//需要提供一个底层的 KV Store
use super::{ID_BITS, ID_MASK, PersistID};
use bytes::Bytes;

//需要定义一个
pub trait KVStore {
    fn get(&self, key: Bytes) -> Result<Bytes>;
    fn set(&self, key: Bytes, value: Bytes) -> Result<()>;
    fn remove(&self, key: Bytes) -> Result<()>;
    fn update<F: Fn(Bytes) -> Bytes>(&self, key: Bytes, f: F) -> Bytes;
}

fn bytes_to_u64(buf: Bytes) -> u64 {
    buf.as_ref().try_into().map(|buf| u64::from_le_bytes(buf)).unwrap_or(0)
}

fn u64_to_bytes(value: u64) -> Bytes {
    Bytes::copy_from_slice(value.to_le_bytes().as_ref())
}

const ID_KEY: Bytes = Bytes::from_static(b"__id__");
const ENTRY_KEY: Bytes = Bytes::from_static(b"__entry__");

impl<T: KVStore> PersistID for T {
    fn size(&self) -> u64 {
        //获取总的 ID 数目 不精确包括了已删除的
        self.get(ID_KEY).map(|buf| bytes_to_u64(buf)).unwrap_or(0)
    }

    fn get_id(&self) -> u64 {
        //获取一个新的 ID
        bytes_to_u64(self.update(ID_KEY, |old| {
            let id = bytes_to_u64(old);
            u64_to_bytes(id + 1)
        }))
    }

    fn entry(&self) -> (usize, u64) {
        //获取入口点
        let id_level = self.get(ENTRY_KEY).map(|buf| bytes_to_u64(buf)).unwrap_or(0);
        ((id_level >> ID_BITS) as usize, id_level & ID_MASK)
    }

    fn set_entry(&self, level: usize, id: u64) {
        //设置入口点
        self.update(ENTRY_KEY, |old| {
            let old_val = bytes_to_u64(old);
            if ((old_val >> ID_BITS) as usize) < level { u64_to_bytes(super::order_id::level_id(id, level)) } else { u64_to_bytes(old_val) }
        });
    }
}

impl super::KVStore for FjallStore {
    fn get(&self, key: Bytes) -> Result<Bytes> {
        let value = Bytes::copy_from_slice(self.space.get(&key)?.ok_or(anyhow!("no key {:?}", key))?.as_ref());
        Ok(value)
    }

    fn set(&self, key: Bytes, value: Bytes) -> Result<()> {
        Ok(self.space.insert(key.as_ref(), value.as_ref())?)
    }

    fn remove(&self, key: Bytes) -> Result<()> {
        Ok(self.space.remove(key.as_ref())?)
    }

    fn update<F: Fn(Bytes) -> Bytes>(&self, key: Bytes, f: F) -> Bytes {
        self.space
            .fetch_update(key.as_ref(), |old| {
                let old = if let Some(old) = old { Bytes::copy_from_slice(old) } else { Bytes::default() };
                let val = f(old);
                if val.is_empty() { None } else { Some(val.as_ref().into()) }
            })
            .unwrap()
            .map(|old| Bytes::copy_from_slice(&old))
            .unwrap_or(Bytes::default())
    }
}

use anyhow::{Result, anyhow};
use fjall::{KeyspaceCreateOptions, OptimisticTxDatabase, OptimisticTxKeyspace};

#[derive(Clone)]
pub struct FjallStore {
    pub(crate) space: OptimisticTxKeyspace,
}

impl FjallStore {
    pub fn open(db: &OptimisticTxDatabase, name: &str) -> Result<Self> {
        let space = db.keyspace(&format!("#{}", name), KeyspaceCreateOptions::default)?;
        Ok(Self { space })
    }
}
