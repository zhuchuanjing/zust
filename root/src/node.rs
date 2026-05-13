use dynamic::FixVec;
use scc::HashMap;
use smol_str::SmolStr;

#[derive(Debug)]
pub enum Node<T> {
    Object(T),                //对象
    List(FixVec<T>),          //索引稳定的列表
    Map(HashMap<SmolStr, T>), //KV Map
}

impl<T: std::fmt::Debug + Default> Node<T> {
    pub fn into_object(self) -> Option<T> {
        if let Self::Object(v) = self { Some(v) } else { None }
    }

    pub fn list() -> Self {
        Self::List(FixVec::default())
    }

    pub fn map() -> Self {
        Self::Map(HashMap::new())
    }

    pub fn len(&self) -> usize {
        match self {
            Self::List(l) => l.len(),
            Self::Map(m) => m.len(),
            _ => 1,
        }
    }

    pub fn push(&mut self, value: T) -> Option<usize> {
        match self {
            Self::List(l) => Some(l.push(value)),
            _ => None,
        }
    }

    pub fn get_idx<R, F: FnOnce(&T) -> R>(&self, idx: usize, f: F) -> Option<R> {
        match self {
            Self::List(l) => l.get(idx).map(|v| f(&v)),
            _ => None,
        }
    }

    pub fn get_idx_mut<R, F: FnMut(&mut T) -> R>(&mut self, idx: usize, mut f: F) -> Option<R> {
        match self {
            Self::List(l) => l.get_mut(idx).map(|v| f(v)),
            _ => None,
        }
    }

    pub fn remove_idx(&mut self, idx: usize) -> Option<T> {
        match self {
            Self::List(l) => l.remove(idx),
            _ => None,
        }
    }

    pub fn keys(&self) -> Option<Vec<SmolStr>> {
        match self {
            Self::Map(m) => {
                let mut keys = Vec::new();
                m.iter_sync(|key, _| {
                    keys.push(key.clone());
                    true
                });
                Some(keys)
            }
            _ => None,
        }
    }

    pub fn get_key<R, F: FnOnce(&T) -> R>(&self, key: &str, f: F) -> Option<R> {
        match self {
            Self::Map(m) => m.read_sync(key, |_, v| f(v)),
            _ => None,
        }
    }

    pub fn remove_key(&self, key: &str) -> Option<T> {
        match self {
            Self::Map(m) => m.remove_sync(key).map(|(_, v)| v),
            _ => None,
        }
    }

    pub fn insert(&self, key: SmolStr, value: T) -> Option<T> {
        match self {
            Self::Map(m) => m.upsert_sync(key, value),
            _ => None,
        }
    }
}
