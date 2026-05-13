mod kv;
pub use kv::KVStore;

mod hnsw;
mod layer;
mod order_id;
pub mod query;
mod unique_id;

pub(crate) const ID_BITS: usize = 64 - 4; //2 的 4 次方层 最大 0-15 已经足够了
pub(crate) const ID_MASK: u64 = 0xfffffffffffffffu64;

use anndists::dist::*;
#[derive(Clone, Debug)]
pub enum Dist {
    L1,
    L2,
    Cosine,
}

impl Dist {
    pub fn eval(&self, va: &[f32], vb: &[f32]) -> f32 {
        match self {
            Self::L1 => DistL1 {}.eval(va, vb),
            Self::L2 => DistL2 {}.eval(va, vb),
            Self::Cosine => DistCosine {}.eval(va, vb),
        }
    }
}

use std::mem;
pub(crate) fn u8_to_vec<T: Clone>(u8_vec: Vec<u8>) -> Vec<T> {
    let len = u8_vec.len() / mem::size_of::<T>();
    let ptr = u8_vec.as_ptr() as *const T;
    mem::forget(u8_vec);
    unsafe { Vec::from_raw_parts(ptr as *mut T, len, len) } //.clone()
}

pub(crate) fn vec_to_u8<T>(vec: Vec<T>) -> Vec<u8> {
    let len = vec.len() * mem::size_of::<T>();
    let ptr = vec.as_ptr() as *const u8;
    mem::forget(vec);
    unsafe { Vec::from_raw_parts(ptr as *mut u8, len, len) }
}

pub(crate) trait PersistID {
    fn size(&self) -> u64;
    fn get_id(&self) -> u64;
    fn entry(&self) -> (usize, u64);
    fn set_entry(&self, level: usize, id: u64);
}
