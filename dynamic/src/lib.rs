use bytemuck::{AnyBitPattern, NoUninit, cast_slice, cast_slice_mut};
use half::f16;
use indexmap::IndexMap;
use smol_str::SmolStr;
use std::any::Any;
use std::collections::BTreeMap;
use std::mem;
use tinyvec::TinyVec;
const TINY_SIZE: usize = 28;
pub mod json;

/// IEEE 754 half-precision bits -> f64. Delegates to `half::f16` for proper
/// signaling NaN, subnormal, and rounding semantics.
#[inline]
pub fn f16_to_f64(bits: u16) -> f64 {
    f16::from_bits(bits).to_f64()
}

/// f64 -> IEEE 754 half-precision bits, via `half::f16`.
#[inline]
pub fn f64_to_f16(value: f64) -> u16 {
    f16::from_f64(value).to_bits()
}
#[derive(Debug, Default, Clone, PartialEq)]
pub struct MyVec<T> {
    pub(crate) data: TinyVec<[u8; TINY_SIZE]>,
    phantom: std::marker::PhantomData<T>,
}

impl<T> MyVec<T> {
    pub fn len(&self) -> usize {
        self.data.len() / mem::size_of::<T>()
    }

    pub fn is_empty(&self) -> bool {
        self.data.is_empty()
    }

    pub fn as_slice(&self) -> &[u8] {
        self.data.as_slice()
    }
}

impl<T: NoUninit + AnyBitPattern> MyVec<T> {
    pub fn push(&mut self, value: T) {
        let binding = [value];
        let bytes = cast_slice(&binding);
        self.data.extend_from_slice(bytes);
    }

    pub fn pop(&mut self) -> Option<T>
    where
        T: AnyBitPattern,
    {
        if self.data.len() < mem::size_of::<T>() {
            return None;
        }
        let start = self.data.len() - mem::size_of::<T>();
        let slice = &self.data[start..];
        let value = cast_slice::<u8, T>(slice)[0];
        self.data.truncate(start);
        Some(value)
    }

    pub fn get(&self, idx: usize) -> Option<T> {
        if idx >= self.len() {
            return None;
        }
        let start = idx * mem::size_of::<T>();
        let slice = &self.data[start..start + mem::size_of::<T>()];
        Some(cast_slice::<u8, T>(slice)[0])
    }

    pub fn set(&mut self, idx: usize, value: T) {
        if idx < self.len() {
            let start = idx * mem::size_of::<T>();
            let slice = &mut self.data[start..start + mem::size_of::<T>()];
            cast_slice_mut::<u8, T>(slice)[0] = value;
        }
    }

    pub fn iter(&self) -> Iter<'_, T> {
        Iter { data: self.data.as_slice(), index: 0, phantom: std::marker::PhantomData }
    }
    pub fn extend_from_slice(&mut self, slice: &[T]) {
        self.data.extend_from_slice(cast_slice(slice));
    }
}

impl<T: NoUninit> From<&[T]> for MyVec<T> {
    fn from(vec: &[T]) -> Self {
        let mut data: TinyVec<[u8; TINY_SIZE]> = TinyVec::new();
        data.extend_from_slice(cast_slice(vec));
        Self { data, phantom: std::marker::PhantomData }
    }
}

impl<T: NoUninit, const N: usize> From<[T; N]> for MyVec<T> {
    fn from(arr: [T; N]) -> Self {
        Self::from(&arr[..])
    }
}

impl<T: AnyBitPattern> From<MyVec<T>> for Vec<T> {
    fn from(my_vec: MyVec<T>) -> Self {
        cast_slice(my_vec.data.as_slice()).to_vec()
    }
}

pub struct Iter<'a, T> {
    data: &'a [u8],
    index: usize,
    phantom: std::marker::PhantomData<T>,
}

impl<'a, T: AnyBitPattern> Iterator for Iter<'a, T> {
    type Item = &'a T;
    fn next(&mut self) -> Option<Self::Item> {
        let size = std::mem::size_of::<T>();
        let start = self.index * size;

        if start + size > self.data.len() {
            return None;
        }

        let slice = &self.data[start..start + size];
        let value = &cast_slice::<u8, T>(slice)[0];
        self.index += 1;
        Some(value)
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        let remaining = self.data.len() / std::mem::size_of::<T>() - self.index;
        (remaining, Some(remaining))
    }
}

impl<'a, T: AnyBitPattern> ExactSizeIterator for Iter<'a, T> {
    fn len(&self) -> usize {
        self.data.len() / std::mem::size_of::<T>() - self.index
    }
}

#[derive(Debug, thiserror::Error)]
pub enum DynamicErr {
    #[error("type mismatch")]
    TypeMismatch,
    #[error("range error: {0}")]
    Range(i64),
    #[error("没有成员: {0}")]
    NoField(SmolStr),
    #[error("out of range")]
    OutOfRange,
}

pub use parking_lot::RwLock;
use std::sync::Arc;

pub trait CustomProperty: Any + Send + Sync {
    fn get_key(&self, key: &str) -> Option<Dynamic>;

    fn set_key(&self, key: &str, value: Dynamic) -> bool;

    fn contains_key(&self, key: &str) -> bool {
        self.get_key(key).is_some()
    }
}

#[derive(Clone)]
pub struct CustomValue {
    type_name: &'static str,
    value: Arc<dyn Any + Send + Sync>,
    get_key: Option<fn(&(dyn Any + Send + Sync), &str) -> Option<Dynamic>>,
    set_key: Option<fn(&(dyn Any + Send + Sync), &str, Dynamic) -> bool>,
    contains_key: Option<fn(&(dyn Any + Send + Sync), &str) -> bool>,
}

impl CustomValue {
    pub fn new<T>(value: T) -> Self
    where
        T: Any + Send + Sync + 'static,
    {
        Self { type_name: std::any::type_name::<T>(), value: Arc::new(value), get_key: None, set_key: None, contains_key: None }
    }

    pub fn from_arc<T>(value: Arc<T>) -> Self
    where
        T: Any + Send + Sync + 'static,
    {
        Self { type_name: std::any::type_name::<T>(), value, get_key: None, set_key: None, contains_key: None }
    }

    pub fn new_with_properties<T>(value: T) -> Self
    where
        T: CustomProperty + 'static,
    {
        Self::from_property_arc(Arc::new(value))
    }

    pub fn from_property_arc<T>(value: Arc<T>) -> Self
    where
        T: CustomProperty + 'static,
    {
        fn get_key<T: CustomProperty + 'static>(value: &(dyn Any + Send + Sync), key: &str) -> Option<Dynamic> {
            value.downcast_ref::<T>()?.get_key(key)
        }

        fn set_key<T: CustomProperty + 'static>(value: &(dyn Any + Send + Sync), key: &str, next: Dynamic) -> bool {
            value.downcast_ref::<T>().is_some_and(|value| value.set_key(key, next))
        }

        fn contains_key<T: CustomProperty + 'static>(value: &(dyn Any + Send + Sync), key: &str) -> bool {
            value.downcast_ref::<T>().is_some_and(|value| value.contains_key(key))
        }

        Self { type_name: std::any::type_name::<T>(), value, get_key: Some(get_key::<T>), set_key: Some(set_key::<T>), contains_key: Some(contains_key::<T>) }
    }

    pub fn as_any(&self) -> &(dyn Any + Send + Sync) {
        self.value.as_ref()
    }

    pub fn custom_type_name(&self) -> &'static str {
        self.type_name
    }

    fn ptr_eq(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.value, &other.value)
    }

    fn get_key(&self, key: &str) -> Option<Dynamic> {
        self.get_key.and_then(|get_key| get_key(self.as_any(), key))
    }

    fn set_key(&self, key: &str, value: Dynamic) -> bool {
        self.set_key.is_some_and(|set_key| set_key(self.as_any(), key, value))
    }

    fn contains_key(&self, key: &str) -> bool {
        self.contains_key.is_some_and(|contains_key| contains_key(self.as_any(), key))
    }
}

impl std::fmt::Debug for CustomValue {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CustomValue").field("type_name", &self.type_name).finish()
    }
}

#[derive(Debug)]
pub struct StructBytes {
    bytes: RwLock<Vec<u8>>,
    dynamic_fields: RwLock<BTreeMap<usize, Box<Dynamic>>>,
}

impl StructBytes {
    fn new(size: usize) -> Arc<Self> {
        Arc::new(Self { bytes: RwLock::new(vec![0; size]), dynamic_fields: RwLock::new(BTreeMap::new()) })
    }

    fn addr(&self) -> usize {
        self.bytes.read().as_ptr() as usize
    }

    fn copy_from_ptr(addr: usize, ty: &Type) -> Arc<Self> {
        let size = ty.storage_width() as usize;
        let storage = Self::new(size);
        if addr != 0 && size > 0 {
            unsafe {
                std::ptr::copy_nonoverlapping(addr as *const u8, storage.addr() as *mut u8, size);
            }
            storage.clone_dynamic_fields_from(addr, ty, 0);
        }
        storage
    }

    fn clone_dynamic_fields_from(&self, src_addr: usize, ty: &Type, dst_offset: usize) {
        match ty {
            Type::Bool | Type::I8 | Type::U8 | Type::I16 | Type::U16 | Type::I32 | Type::U32 | Type::I64 | Type::U64 | Type::F16 | Type::F32 | Type::F64 | Type::Void => {}
            Type::Struct { fields, .. } => {
                let (_, offsets) = Type::struct_layout(fields);
                for ((_, field_ty), offset) in fields.iter().zip(offsets) {
                    self.clone_dynamic_fields_from(src_addr + offset as usize, field_ty, dst_offset + offset as usize);
                }
            }
            Type::Array(elem_ty, len) | Type::Vec(elem_ty, len) => {
                let width = elem_ty.storage_width() as usize;
                for idx in 0..*len as usize {
                    self.clone_dynamic_fields_from(src_addr + idx * width, elem_ty, dst_offset + idx * width);
                }
            }
            _ => {
                let ptr = unsafe { std::ptr::read_unaligned(src_addr as *const usize) };
                if ptr != 0 {
                    let value = unsafe { (&*(ptr as *const Dynamic)).deep_clone() };
                    self.write_dynamic_ptr_at(dst_offset, value);
                }
            }
        }
    }

    fn clear_dynamic_fields_in(&self, start: usize, width: usize) {
        let end = start.saturating_add(width);
        self.dynamic_fields.write().retain(|offset, _| *offset < start || *offset >= end);
    }

    fn read_dynamic_ptr_at(&self, offset: usize) -> Option<Dynamic> {
        if let Some(value) = self.dynamic_fields.read().get(&offset) {
            return Some(value.as_ref().clone());
        }
        let ptr = unsafe { std::ptr::read_unaligned((self.addr() + offset) as *const usize) };
        if ptr == 0 { None } else { Some(unsafe { (&*(ptr as *const Dynamic)).clone() }) }
    }

    fn write_dynamic_ptr_at(&self, offset: usize, value: Dynamic) {
        let mut boxed = Box::new(value);
        let ptr = boxed.as_mut() as *mut Dynamic as usize;
        self.dynamic_fields.write().insert(offset, boxed);
        unsafe {
            std::ptr::write_unaligned((self.addr() + offset) as *mut usize, ptr);
        }
    }
}

#[derive(Debug, Default, Clone)]
pub enum Dynamic {
    #[default]
    Null,
    Bool(bool),
    U8(u8),
    I8(i8),
    U16(u16),
    I16(i16),
    U32(u32),
    I32(i32), //默认整数类型
    U64(u64),
    I64(i64),
    F16(u16), //IEEE 754 half-precision bits
    F32(f32), //默认浮点类型
    F64(f64),
    String(SmolStr),
    StringBuf(String),
    Bytes(Vec<u8>),
    VecI8(MyVec<i8>),
    VecU16(MyVec<u16>),
    VecI16(MyVec<i16>),
    VecU32(MyVec<u32>),
    VecI32(MyVec<i32>),
    VecF32(MyVec<f32>),
    VecU64(Vec<u64>),
    VecI64(Vec<i64>),
    VecF64(Vec<f64>),
    List(Arc<RwLock<Vec<Dynamic>>>),
    Map(Arc<RwLock<IndexMap<SmolStr, Dynamic>>>),
    StructView {
        addr: usize,
        ty: Arc<Type>,
    },
    StructOwned {
        storage: Arc<StructBytes>,
        ty: Arc<Type>,
    },
    Custom(Box<CustomValue>),
    Iter {
        idx: usize,
        keys: Vec<SmolStr>,
        value: Box<Dynamic>,
    },
}

unsafe impl Send for Dynamic {}
unsafe impl Sync for Dynamic {}

impl PartialEq for Dynamic {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::Null, Self::Null) => true,
            (Self::Bool(a), Self::Bool(b)) => a == b,
            (a, b) if a.is_str() && b.is_str() => a.as_str() == b.as_str(),
            (Self::Bytes(a), Self::Bytes(b)) => a == b,
            // Integer types - compare as i64
            (Self::U8(a), Self::U8(b)) => a == b,
            (Self::I8(a), Self::I8(b)) => a == b,
            (Self::U16(a), Self::U16(b)) => a == b,
            (Self::I16(a), Self::I16(b)) => a == b,
            (Self::U32(a), Self::U32(b)) => a == b,
            (Self::I32(a), Self::I32(b)) => a == b,
            (Self::U64(a), Self::U64(b)) => a == b,
            (Self::I64(a), Self::I64(b)) => a == b,
            // Mixed integer types - compare as i64
            (a, b) if a.is_int() && b.is_int() => a.as_int() == b.as_int(),
            // Float types
            (Self::F16(a), Self::F16(b)) => a == b,
            (Self::F32(a), Self::F32(b)) => a.to_bits() == b.to_bits(),
            (Self::F64(a), Self::F64(b)) => a.to_bits() == b.to_bits(),
            (a, b) if (a.is_f16() || a.is_f32() || a.is_f64()) && (b.is_f16() || b.is_f32() || b.is_f64()) => a.as_float() == b.as_float(),
            // Typed vectors
            (Self::VecI8(a), Self::VecI8(b)) => a.data == b.data,
            (Self::VecU16(a), Self::VecU16(b)) => a.data == b.data,
            (Self::VecI16(a), Self::VecI16(b)) => a.data == b.data,
            (Self::VecU32(a), Self::VecU32(b)) => a.data == b.data,
            (Self::VecI32(a), Self::VecI32(b)) => a.data == b.data,
            (Self::VecF32(a), Self::VecF32(b)) => a.data == b.data,
            (Self::VecU64(a), Self::VecU64(b)) => a == b,
            (Self::VecI64(a), Self::VecI64(b)) => a == b,
            (Self::VecF64(a), Self::VecF64(b)) => a == b,
            // List - compare inner values
            (Self::List(a), Self::List(b)) => {
                let a_guard = a.read();
                let b_guard = b.read();
                if a_guard.len() != b_guard.len() {
                    return false;
                }
                a_guard.iter().zip(b_guard.iter()).all(|(x, y)| x == y)
            }
            // Map - compare key-value pairs
            (Self::Map(a), Self::Map(b)) => {
                let a_guard = a.read();
                let b_guard = b.read();
                if a_guard.len() != b_guard.len() {
                    return false;
                }
                for (k, v) in a_guard.iter() {
                    if let Some(other_v) = b_guard.get(k) {
                        if v != other_v {
                            return false;
                        }
                    } else {
                        return false;
                    }
                }
                true
            }
            // StructView compares by address, StructOwned by bytes.
            (Self::StructView { addr: a_addr, ty: a_ty }, Self::StructView { addr: b_addr, ty: b_ty }) => a_addr == b_addr && a_ty == b_ty,
            (Self::StructOwned { storage: a, ty: a_ty }, Self::StructOwned { storage: b, ty: b_ty }) => a_ty == b_ty && *a.bytes.read() == *b.bytes.read(),
            (Self::Custom(a), Self::Custom(b)) => a.ptr_eq(b),
            _ => false,
        }
    }
}

impl Eq for Dynamic {}

use std::cmp::Ordering;

impl PartialOrd for Dynamic {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for Dynamic {
    fn cmp(&self, other: &Self) -> Ordering {
        if self.is_f32() || self.is_f64() || other.is_f32() || other.is_f64() {
            self.as_float().unwrap_or(0.0).total_cmp(&other.as_float().unwrap_or(0.0))
        } else if self.is_int() || other.is_int() {
            self.as_int().unwrap_or(0).cmp(&other.as_int().unwrap_or(0))
        } else if self.is_uint() || other.is_uint() {
            self.as_uint().unwrap_or(0).cmp(&other.as_uint().unwrap_or(0))
        } else if self.is_false() && other.is_true() {
            Ordering::Less // false < true
        } else if self.is_true() && other.is_false() {
            Ordering::Greater // true > false
        } else if self.is_null() && other.is_null() {
            Ordering::Equal
        } else if self.is_str() && other.is_str() {
            self.as_str().cmp(other.as_str())
        } else {
            Ordering::Equal
        }
    }
}

macro_rules! impl_dynamic_scalar {
    ($variant:ident, $ty:ty) => {
        impl From<$ty> for Dynamic {
            fn from(value: $ty) -> Self {
                Dynamic::$variant(value)
            }
        }
    };
}

impl_dynamic_scalar!(Bool, bool);

impl_dynamic_scalar!(I8, i8);
impl_dynamic_scalar!(U16, u16);
impl_dynamic_scalar!(I16, i16);
impl_dynamic_scalar!(U32, u32);
impl_dynamic_scalar!(I32, i32);
impl_dynamic_scalar!(F32, f32);
impl_dynamic_scalar!(I64, i64);
impl_dynamic_scalar!(U64, u64);
impl_dynamic_scalar!(F64, f64);
impl_dynamic_scalar!(String, SmolStr);
impl From<&str> for Dynamic {
    fn from(s: &str) -> Self {
        Dynamic::String(s.into())
    }
}

macro_rules! impl_try_from_dynamic_int {
    ($($target:ty),+ $(,)?) => {
        $(
            impl TryFrom<Dynamic> for $target {
                type Error = DynamicErr;
                fn try_from(value: Dynamic) -> Result<Self, Self::Error> {
                    match value {
                        Dynamic::F32(v) => Ok(v as $target),
                        Dynamic::F64(v) => Ok(v as $target),
                        Dynamic::String(v) => v.trim().parse::<$target>().or_else(|_| v.trim().parse::<f64>().map(|value| value as $target)).map_err(|_| DynamicErr::TypeMismatch),
                        Dynamic::StringBuf(v) => v.trim().parse::<$target>().or_else(|_| v.trim().parse::<f64>().map(|value| value as $target)).map_err(|_| DynamicErr::TypeMismatch),
                        Dynamic::U8(v)  => v.try_into().map_err(|_| DynamicErr::OutOfRange),
                        Dynamic::U16(v) => v.try_into().map_err(|_| DynamicErr::OutOfRange),
                        Dynamic::U32(v) => v.try_into().map_err(|_| DynamicErr::OutOfRange),
                        Dynamic::U64(v) => v.try_into().map_err(|_| DynamicErr::OutOfRange),
                        Dynamic::I8(v)  => v.try_into().map_err(|_| DynamicErr::OutOfRange),
                        Dynamic::I16(v) => v.try_into().map_err(|_| DynamicErr::OutOfRange),
                        Dynamic::I32(v) => v.try_into().map_err(|_| DynamicErr::OutOfRange),
                        Dynamic::I64(v) => v.try_into().map_err(|_| DynamicErr::OutOfRange),
                        _ => Err(DynamicErr::TypeMismatch),
                    }
                }
            }
        )+
    };
}
impl_try_from_dynamic_int!(u8, u16, u32, u64, i8, i16, i32, i64);

impl TryFrom<Dynamic> for f64 {
    type Error = DynamicErr;
    fn try_from(value: Dynamic) -> Result<Self, Self::Error> {
        match value {
            Dynamic::F32(v) => Ok(v as f64),
            Dynamic::F64(v) => Ok(v),
            Dynamic::U8(v) => Ok(v as f64),
            Dynamic::U16(v) => Ok(v as f64),
            Dynamic::U32(v) => Ok(v as f64),
            Dynamic::U64(v) => Ok(v as f64),
            Dynamic::I8(v) => Ok(v as f64),
            Dynamic::I16(v) => Ok(v as f64),
            Dynamic::I32(v) => Ok(v as f64),
            Dynamic::I64(v) => Ok(v as f64),
            Dynamic::String(v) => v.trim().parse::<f64>().map_err(|_| DynamicErr::TypeMismatch),
            Dynamic::StringBuf(v) => v.trim().parse::<f64>().map_err(|_| DynamicErr::TypeMismatch),
            _ => Err(DynamicErr::TypeMismatch),
        }
    }
}

impl TryFrom<Dynamic> for f32 {
    type Error = DynamicErr;
    fn try_from(value: Dynamic) -> Result<Self, Self::Error> {
        match value {
            Dynamic::F32(v) => Ok(v),
            Dynamic::F64(v) => Ok(v as f32),
            Dynamic::U8(v) => Ok(v as f32),
            Dynamic::U16(v) => Ok(v as f32),
            Dynamic::U32(v) => Ok(v as f32),
            Dynamic::U64(v) => Ok(v as f32),
            Dynamic::I8(v) => Ok(v as f32),
            Dynamic::I16(v) => Ok(v as f32),
            Dynamic::I32(v) => Ok(v as f32),
            Dynamic::I64(v) => Ok(v as f32),
            Dynamic::String(v) => v.trim().parse::<f32>().map_err(|_| DynamicErr::TypeMismatch),
            Dynamic::StringBuf(v) => v.trim().parse::<f32>().map_err(|_| DynamicErr::TypeMismatch),
            _ => Err(DynamicErr::TypeMismatch),
        }
    }
}

impl TryFrom<Dynamic> for bool {
    type Error = DynamicErr;
    fn try_from(value: Dynamic) -> Result<Self, Self::Error> {
        match value {
            Dynamic::Bool(v) => Ok(v),
            Dynamic::U8(v) => Ok(v != 0),
            Dynamic::U16(v) => Ok(v != 0),
            Dynamic::U32(v) => Ok(v != 0),
            Dynamic::U64(v) => Ok(v != 0),
            Dynamic::I8(v) => Ok(v != 0),
            Dynamic::I16(v) => Ok(v != 0),
            Dynamic::I32(v) => Ok(v != 0),
            Dynamic::I64(v) => Ok(v != 0),
            _ => Err(DynamicErr::TypeMismatch),
        }
    }
}

impl TryFrom<Dynamic> for SmolStr {
    type Error = DynamicErr;
    fn try_from(value: Dynamic) -> Result<Self, Self::Error> {
        match value {
            Dynamic::String(s) => Ok(s),
            Dynamic::StringBuf(s) => Ok(s.into()),
            _ => Err(DynamicErr::TypeMismatch),
        }
    }
}

macro_rules! impl_dynamic_vec_from_slice {
    ($variant:ident, $ty:ty) => {
        impl From<&[$ty]> for Dynamic {
            fn from(vec: &[$ty]) -> Self {
                Dynamic::$variant(MyVec::from(vec))
            }
        }

        impl<const N: usize> From<[$ty; N]> for Dynamic {
            fn from(vec: [$ty; N]) -> Self {
                Dynamic::$variant(MyVec::from(vec))
            }
        }
    };
}

impl_dynamic_vec_from_slice!(VecI8, i8);
impl_dynamic_vec_from_slice!(VecU16, u16);
impl_dynamic_vec_from_slice!(VecI16, i16);
impl_dynamic_vec_from_slice!(VecU32, u32);
impl_dynamic_vec_from_slice!(VecI32, i32);
impl_dynamic_vec_from_slice!(VecF32, f32);

impl From<&[u8]> for Dynamic {
    fn from(vec: &[u8]) -> Self {
        Dynamic::Bytes(vec.to_vec())
    }
}

impl From<Vec<u8>> for Dynamic {
    fn from(vec: Vec<u8>) -> Self {
        Dynamic::Bytes(vec)
    }
}

impl From<&[u64]> for Dynamic {
    fn from(vec: &[u64]) -> Self {
        Dynamic::VecU64(vec.to_vec())
    }
}

impl<const N: usize> From<[u64; N]> for Dynamic {
    fn from(vec: [u64; N]) -> Self {
        Dynamic::VecU64(vec.to_vec())
    }
}

impl From<&[i64]> for Dynamic {
    fn from(vec: &[i64]) -> Self {
        Dynamic::VecI64(vec.to_vec())
    }
}
impl<const N: usize> From<[i64; N]> for Dynamic {
    fn from(vec: [i64; N]) -> Self {
        Dynamic::VecI64(vec.to_vec())
    }
}

impl From<&[f64]> for Dynamic {
    fn from(vec: &[f64]) -> Self {
        Dynamic::VecF64(vec.to_vec())
    }
}
impl<const N: usize> From<[f64; N]> for Dynamic {
    fn from(vec: [f64; N]) -> Self {
        Dynamic::VecF64(vec.to_vec())
    }
}

impl<T: Into<Dynamic>> From<Vec<T>> for Dynamic {
    fn from(vec: Vec<T>) -> Self {
        let vec = vec.into_iter().map(|v| v.into()).collect();
        Dynamic::List(Arc::new(RwLock::new(vec)))
    }
}

impl From<String> for Dynamic {
    fn from(s: String) -> Self {
        Dynamic::String(s.into())
    }
}

impl ToString for Dynamic {
    fn to_string(&self) -> String {
        match self {
            Self::Null => "()".into(),
            Self::Bool(b) => {
                if *b {
                    "true".into()
                } else {
                    "false".into()
                }
            }
            Self::U8(u) => u.to_string(),
            Self::U16(u) => u.to_string(),
            Self::U32(u) => u.to_string(),
            Self::U64(u) => u.to_string(),
            Self::I8(u) => u.to_string(),
            Self::I16(u) => u.to_string(),
            Self::I32(u) => u.to_string(),
            Self::I64(u) => u.to_string(),
            Self::F32(u) => u.to_string(),
            Self::F64(u) => u.to_string(),
            Self::String(s) => s.to_string(),
            Self::StringBuf(s) => s.clone(),
            _ => {
                let mut buf = String::new();
                self.to_json(&mut buf);
                if buf.is_empty() { format!("{:?}", self) } else { buf }
            }
        }
    }
}

use anyhow::Result;
impl Dynamic {
    pub fn custom<T>(value: T) -> Self
    where
        T: Any + Send + Sync + 'static,
    {
        Self::Custom(Box::new(CustomValue::new(value)))
    }

    pub fn custom_arc<T>(value: Arc<T>) -> Self
    where
        T: Any + Send + Sync + 'static,
    {
        Self::Custom(Box::new(CustomValue::from_arc(value)))
    }

    pub fn custom_with_properties<T>(value: T) -> Self
    where
        T: CustomProperty + 'static,
    {
        Self::Custom(Box::new(CustomValue::new_with_properties(value)))
    }

    pub fn custom_property_arc<T>(value: Arc<T>) -> Self
    where
        T: CustomProperty + 'static,
    {
        Self::Custom(Box::new(CustomValue::from_property_arc(value)))
    }

    pub fn is_custom(&self) -> bool {
        matches!(self, Self::Custom(_))
    }

    pub fn custom_type_name(&self) -> Option<&'static str> {
        if let Self::Custom(value) = self { Some(value.custom_type_name()) } else { None }
    }

    pub fn as_custom<T>(&self) -> Option<&T>
    where
        T: Any + Send + Sync,
    {
        if let Self::Custom(value) = self { value.as_any().downcast_ref::<T>() } else { None }
    }

    pub fn deep_clone(&self) -> Self {
        match self {
            Self::Map(m) => {
                let m = m.read().iter().map(|(k, v)| (k.clone(), v.deep_clone())).collect();
                Self::map(m)
            }
            Self::List(l) => {
                let l = l.read().iter().map(|item| item.deep_clone()).collect();
                Self::list(l)
            }
            Self::StructView { addr, ty } => Self::owned_struct_from_ptr(*addr, ty.as_ref().clone()),
            Self::StructOwned { storage, ty } => Self::owned_struct_from_ptr(storage.addr(), ty.as_ref().clone()),
            _ => self.clone(),
        }
    }

    pub fn struct_view(addr: usize, ty: Type) -> Self {
        Self::StructView { addr, ty: Arc::new(ty) }
    }

    pub fn owned_struct_from_ptr(addr: usize, ty: Type) -> Self {
        Self::StructOwned { storage: StructBytes::copy_from_ptr(addr, &ty), ty: Arc::new(ty) }
    }

    fn struct_addr_ty(&self) -> Option<(usize, &Type)> {
        match self {
            Self::StructView { addr, ty } => Some((*addr, ty.as_ref())),
            Self::StructOwned { storage, ty } => Some((storage.addr(), ty.as_ref())),
            _ => None,
        }
    }

    fn struct_storage(&self) -> Option<&StructBytes> {
        match self {
            Self::StructOwned { storage, .. } => Some(storage),
            _ => None,
        }
    }

    pub fn add(&mut self, val: i64) -> Option<i64> {
        // 如果是 整数类型 增加指定值 并返回新的值 不考虑溢出
        // 关键不变量:返回 Some(n) 时,*self 的实际值 == n(没有"写入截断但返回溢出值"的不一致 bug)。
        // 原代码 v: i64 = *u as i64 + val 后 *u = v as u8 会静默截断 + 返回不一致;
        // 这里改用 checked_add_signed:失败时返回 None,不修改 *self。
        match self {
            Self::U8(u)  => u.checked_add_signed(val as i8).map(|v| { *u = v; v as i64 }),
            Self::U16(u) => u.checked_add_signed(val as i16).map(|v| { *u = v; v as i64 }),
            Self::U32(u) => u.checked_add_signed(val as i32).map(|v| { *u = v; v as i64 }),
            Self::U64(u) => u.checked_add(val as u64).map(|v| { *u = v; v as i64 }),
            Self::I8(i)  => i.checked_add(val as i8).map(|v| { *i = v; v as i64 }),
            Self::I16(i) => i.checked_add(val as i16).map(|v| { *i = v; v as i64 }),
            Self::I32(i) => i.checked_add(val as i32).map(|v| { *i = v; v as i64 }),
            Self::I64(i) => i.checked_add(val).map(|v| { *i = v; v }),
            _ => None,
        }
    }

    pub fn is_vec(&self) -> bool {
        use Dynamic::*;
        match self {
            VecI8(_) | VecU16(_) | Self::VecI16(_) | VecU32(_) | VecI32(_) | VecF32(_) | VecU64(_) | VecI64(_) | VecF64(_) => true,
            _ => false,
        }
    }

    pub fn as_bytes(&self) -> Option<&[u8]> {
        match self {
            Self::Bytes(b) => Some(b.as_slice()),
            _ => None,
        }
    }

    pub fn as_str(&self) -> &str {
        match self {
            Dynamic::String(s) => s.as_str(),
            Dynamic::StringBuf(s) => s.as_str(),
            _ => "",
        }
    }

    pub fn is_native(&self) -> bool {
        if self.is_f64() || self.is_f32() || self.is_int() || self.is_true() || self.is_false() { true } else { false }
    }

    pub fn from_utf8(buf: &[u8]) -> Result<Self> {
        Ok(Dynamic::from(SmolStr::new(std::str::from_utf8(buf)?)))
    }

    pub fn append(&self, other: Self) {
        match (self, other) {
            (Self::List(left), rhs) => {
                if let Self::List(right) = rhs {
                    left.write().append(&mut right.write());
                } else {
                    left.write().push(rhs);
                }
            }
            (Self::Map(left), Self::Map(right)) => {
                left.write().append(&mut right.write());
            }
            (_, _) => {}
        }
    }

    pub fn into_vec<T: TryFrom<Self> + 'static>(self) -> Option<Vec<T>> {
        if std::any::TypeId::of::<T>() == std::any::TypeId::of::<Dynamic>() {
            match self {
                Dynamic::List(list) => match Arc::try_unwrap(list) {
                    Ok(vec) => Some(unsafe { mem::transmute::<Vec<Dynamic>, Vec<T>>(vec.into_inner()) }),
                    Err(_) => None,
                },
                _ => {
                    let mut vec = Vec::with_capacity(self.len());
                    for idx in 0..self.len() {
                        if let Some(item) = self.get_idx(idx) {
                            vec.push(item);
                        }
                    }
                    Some(unsafe { mem::transmute(vec) })
                }
            }
        } else {
            match self {
                Dynamic::List(list) => Arc::try_unwrap(list).ok().map(|l| l.into_inner().into_iter().filter_map(|l| T::try_from(l).ok()).collect()),
                Dynamic::Bytes(vec) => {
                    if std::any::TypeId::of::<T>() == std::any::TypeId::of::<u8>() {
                        let bytes_vec: Vec<u8> = Vec::from(vec);
                        Some(unsafe { mem::transmute(bytes_vec) })
                    } else {
                        None
                    }
                }
                Dynamic::VecI8(vec) => {
                    if std::any::TypeId::of::<T>() == std::any::TypeId::of::<i8>() {
                        let vec_i8: Vec<i8> = Vec::from(vec);
                        Some(unsafe { mem::transmute(vec_i8) })
                    } else {
                        None
                    }
                }
                Dynamic::VecU16(vec) => {
                    if std::any::TypeId::of::<T>() == std::any::TypeId::of::<u16>() {
                        let vec_u16: Vec<u16> = Vec::from(vec);
                        Some(unsafe { mem::transmute(vec_u16) })
                    } else {
                        None
                    }
                }
                Dynamic::VecI16(vec) => {
                    if std::any::TypeId::of::<T>() == std::any::TypeId::of::<i16>() {
                        let vec_i16: Vec<i16> = Vec::from(vec);
                        Some(unsafe { mem::transmute(vec_i16) })
                    } else {
                        None
                    }
                }
                Dynamic::VecU32(vec) => {
                    if std::any::TypeId::of::<T>() == std::any::TypeId::of::<u32>() {
                        let vec_u32: Vec<u32> = Vec::from(vec);
                        Some(unsafe { mem::transmute(vec_u32) })
                    } else {
                        None
                    }
                }
                Dynamic::VecI32(vec) => {
                    if std::any::TypeId::of::<T>() == std::any::TypeId::of::<i32>() {
                        let vec_i32: Vec<i32> = Vec::from(vec);
                        Some(unsafe { mem::transmute(vec_i32) })
                    } else {
                        None
                    }
                }
                Dynamic::VecF32(vec) => {
                    if std::any::TypeId::of::<T>() == std::any::TypeId::of::<f32>() {
                        let vec_f32: Vec<f32> = Vec::from(vec);
                        Some(unsafe { mem::transmute(vec_f32) })
                    } else {
                        None
                    }
                }
                Dynamic::VecU64(vec) => {
                    if std::any::TypeId::of::<T>() == std::any::TypeId::of::<u64>() {
                        Some(unsafe { mem::transmute(vec) })
                    } else {
                        None
                    }
                }
                Dynamic::VecI64(vec) => {
                    if std::any::TypeId::of::<T>() == std::any::TypeId::of::<i64>() {
                        Some(unsafe { mem::transmute(vec) })
                    } else {
                        None
                    }
                }
                Dynamic::VecF64(vec) => {
                    if std::any::TypeId::of::<T>() == std::any::TypeId::of::<f64>() {
                        Some(unsafe { mem::transmute(vec) })
                    } else {
                        None
                    }
                }
                _ => None,
            }
        }
    }

    pub fn push<T: Into<Dynamic> + 'static>(&mut self, value: T) -> bool {
        match self {
            Self::List(list) => {
                list.write().push(value.into());
                true
            }
            Self::Bytes(vec) => {
                if std::any::TypeId::of::<T>() == std::any::TypeId::of::<u8>() {
                    vec.push(unsafe { mem::transmute_copy(&value) });
                    true
                } else {
                    false
                }
            }
            Self::VecI8(vec) => {
                if std::any::TypeId::of::<T>() == std::any::TypeId::of::<i8>() {
                    vec.push(unsafe { mem::transmute_copy(&value) });
                    true
                } else {
                    false
                }
            }
            Self::VecU16(vec) => {
                if std::any::TypeId::of::<T>() == std::any::TypeId::of::<u16>() {
                    vec.push(unsafe { mem::transmute_copy(&value) });
                    true
                } else {
                    false
                }
            }
            Self::VecI16(vec) => {
                if std::any::TypeId::of::<T>() == std::any::TypeId::of::<i16>() {
                    vec.push(unsafe { mem::transmute_copy(&value) });
                    true
                } else {
                    false
                }
            }
            Self::VecU32(vec) => {
                if std::any::TypeId::of::<T>() == std::any::TypeId::of::<u32>() {
                    vec.push(unsafe { mem::transmute_copy(&value) });
                    true
                } else {
                    false
                }
            }
            Self::VecI32(vec) => {
                if std::any::TypeId::of::<T>() == std::any::TypeId::of::<i32>() {
                    vec.push(unsafe { mem::transmute_copy(&value) });
                    true
                } else {
                    false
                }
            }
            Self::VecF32(vec) => {
                if std::any::TypeId::of::<T>() == std::any::TypeId::of::<f32>() {
                    vec.push(unsafe { mem::transmute_copy(&value) });
                    true
                } else {
                    false
                }
            }
            Self::VecU64(vec) => {
                if std::any::TypeId::of::<T>() == std::any::TypeId::of::<u64>() {
                    vec.push(unsafe { mem::transmute_copy(&value) });
                    true
                } else {
                    false
                }
            }
            Self::VecI64(vec) => {
                if std::any::TypeId::of::<T>() == std::any::TypeId::of::<i64>() {
                    vec.push(unsafe { mem::transmute_copy(&value) });
                    true
                } else {
                    false
                }
            }
            Self::VecF64(vec) => {
                if std::any::TypeId::of::<T>() == std::any::TypeId::of::<f64>() {
                    vec.push(unsafe { mem::transmute_copy(&value) });
                    true
                } else {
                    false
                }
            }
            _ => false,
        }
    }

    pub fn push_dynamic(&mut self, value: Dynamic) -> bool {
        match self {
            Self::List(list) => {
                list.write().push(value);
                true
            }
            Self::Bytes(vec) => value.try_into().map(|value| vec.push(value)).is_ok(),
            Self::VecI8(vec) => value.try_into().map(|value| vec.push(value)).is_ok(),
            Self::VecU16(vec) => value.try_into().map(|value| vec.push(value)).is_ok(),
            Self::VecI16(vec) => value.try_into().map(|value| vec.push(value)).is_ok(),
            Self::VecU32(vec) => value.try_into().map(|value| vec.push(value)).is_ok(),
            Self::VecI32(vec) => value.try_into().map(|value| vec.push(value)).is_ok(),
            Self::VecF32(vec) => value.try_into().map(|value| vec.push(value)).is_ok(),
            Self::VecU64(vec) => value.try_into().map(|value| vec.push(value)).is_ok(),
            Self::VecI64(vec) => value.try_into().map(|value| vec.push(value)).is_ok(),
            Self::VecF64(vec) => value.try_into().map(|value| vec.push(value)).is_ok(),
            _ => false,
        }
    }

    pub fn pop(&mut self) -> Option<Dynamic> {
        match self {
            Self::List(list) => list.write().pop(),
            Self::Bytes(vec) => vec.pop().map(Dynamic::U8),
            Self::VecI8(vec) => vec.pop().map(Dynamic::I8),
            Self::VecU16(vec) => vec.pop().map(Dynamic::U16),
            Self::VecI16(vec) => vec.pop().map(Dynamic::I16),
            Self::VecU32(vec) => vec.pop().map(Dynamic::U32),
            Self::VecI32(vec) => vec.pop().map(Dynamic::I32),
            Self::VecF32(vec) => vec.pop().map(Dynamic::F32),
            Self::VecU64(vec) => vec.pop().map(Dynamic::U64),
            Self::VecI64(vec) => vec.pop().map(Dynamic::I64),
            Self::VecF64(vec) => vec.pop().map(Dynamic::F64),
            _ => None,
        }
    }

    pub fn is_null(&self) -> bool {
        match self {
            Self::Null => true,
            _ => false,
        }
    }

    pub fn as_bool(&self) -> Option<bool> {
        if let Self::Bool(b) = self { Some(*b) } else { None }
    }

    pub fn is_true(&self) -> bool {
        match self {
            Self::Bool(b) => *b,
            _ => false,
        }
    }

    pub fn is_false(&self) -> bool {
        match self {
            Self::Bool(b) => !*b,
            _ => false,
        }
    }

    pub fn is_int(&self) -> bool {
        match self {
            Self::I8(_) | Self::I16(_) | Self::I32(_) | Self::I64(_) => true,
            Self::U8(_) | Self::U16(_) | Self::U32(_) | Self::U64(_) => true,
            _ => false,
        }
    }

    pub fn as_int(&self) -> Option<i64> {
        match self {
            Self::U8(u) => Some(*u as i64),
            Self::U16(u) => Some(*u as i64),
            Self::U32(u) => Some(*u as i64),
            Self::U64(u) => i64::try_from(*u).ok(),
            Self::I8(i) => Some(*i as i64),
            Self::I16(i) => Some(*i as i64),
            Self::I32(i) => Some(*i as i64),
            Self::I64(i) => Some(*i as i64),
            _ => None,
        }
    }

    pub fn is_uint(&self) -> bool {
        match self {
            Self::U8(_) | Self::U16(_) | Self::U32(_) | Self::U64(_) => true,
            _ => false,
        }
    }

    pub fn as_uint(&self) -> Option<u64> {
        match self {
            Self::U8(i) => Some(*i as u64),
            Self::U16(i) => Some(*i as u64),
            Self::U32(i) => Some(*i as u64),
            Self::U64(i) => Some(*i as u64),
            _ => None,
        }
    }

    pub fn is_f32(&self) -> bool {
        if let Self::F32(_) = self { true } else { false }
    }

    pub fn is_f16(&self) -> bool {
        if let Self::F16(_) = self { true } else { false }
    }

    pub fn is_str(&self) -> bool {
        if let Self::String(_) | Self::StringBuf(_) = self { true } else { false }
    }

    pub fn is_f64(&self) -> bool {
        if let Self::F64(_) = self { true } else { false }
    }

    pub fn as_float(&self) -> Option<f64> {
        match self {
            Self::U8(u) => Some(*u as f64),
            Self::U16(u) => Some(*u as f64),
            Self::U32(u) => Some(*u as f64),
            Self::U64(u) => Some(*u as f64),
            Self::I8(i) => Some(*i as f64),
            Self::I16(i) => Some(*i as f64),
            Self::I32(i) => Some(*i as f64),
            Self::I64(i) => Some(*i as f64),
            Self::F16(bits) => Some(f16_to_f64(*bits)),
            Self::F32(f) => Some(*f as f64),
            Self::F64(f) => Some(*f),
            _ => None,
        }
    }

    pub fn is_signed(&self) -> bool {
        match self {
            Self::I8(_) | Self::I16(_) | Self::I32(_) | Self::I64(_) | Self::F16(_) | Self::F32(_) | Self::F64(_) => true,
            _ => false,
        }
    }

    pub fn size_of(&self) -> usize {
        match self {
            Self::I8(_) | Self::U8(_) => 1,
            Self::I16(_) | Self::U16(_) => 2,
            Self::I32(_) | Self::U32(_) | Self::F32(_) => 4,
            Self::I64(_) | Self::U64(_) | Self::F64(_) => 8,
            Self::F16(_) => 2,
            Self::String(s) => s.len(),
            Self::StringBuf(s) => s.len(),
            Self::Bytes(bytes) => bytes.len(),
            Self::VecI8(vec) => vec.len(),
            Self::VecU16(vec) => vec.len(),
            Self::VecI16(vec) => vec.len(),
            Self::VecU32(vec) => vec.len(),
            Self::VecI32(vec) => vec.len(),
            Self::VecF32(vec) => vec.len(),
            Self::VecI64(vec) => vec.len(),
            Self::VecU64(vec) => vec.len(),
            Self::VecF64(vec) => vec.len(),
            Self::List(list) => list.read().len(),
            Self::Map(obj) => obj.read().len(),
            Self::StructView { ty, .. } | Self::StructOwned { ty, .. } => ty.len(),
            Self::Custom(_) => 0,
            _ => 1,
        }
    }

    pub fn list(v: Vec<Dynamic>) -> Self {
        Dynamic::List(Arc::new(RwLock::new(v)))
    }

    pub fn is_list(&self) -> bool {
        match self {
            Self::List(_) | Self::VecF32(_) | Self::VecF64(_) | Self::VecI16(_) | Self::VecI32(_) | Self::VecI64(_) | Self::VecU16(_) | Self::VecU32(_) | Self::VecU64(_) => true,
            Self::StructView { ty, .. } | Self::StructOwned { ty, .. } => ty.is_array() || ty.is_vec(),
            _ => false,
        }
    }

    pub fn split(self, tag: &str) -> Self {
        match self {
            Self::String(s) => Self::list(s.split(tag).map(|p| Dynamic::from(p)).collect()),
            Self::StringBuf(s) => Self::list(s.split(tag).map(|p| Dynamic::from(p)).collect()),
            _ => self,
        }
    }

    pub fn map(m: BTreeMap<SmolStr, Dynamic>) -> Self {
        // 入参保持 BTreeMap 以兼容已有调用点;底层用 IndexMap(O(1) 访问)。
        // BTreeMap 已按 key 排序,转入后即为初始(有序)插入序。
        Dynamic::Map(Arc::new(RwLock::new(m.into_iter().collect())))
    }

    pub fn into_map(self) -> Option<IndexMap<SmolStr, Dynamic>> {
        if let Self::Map(map) = self { Arc::try_unwrap(map).ok().map(|m| m.into_inner()) } else { None }
    }

    pub fn is_map(&self) -> bool {
        if let Self::Map(_) | Self::StructView { .. } | Self::StructOwned { .. } = self { true } else { false }
    }

    pub fn insert<K: Into<SmolStr>, T: Into<Self>>(&self, key: K, value: T) {
        match self {
            Self::Map(obj) => {
                obj.write().insert(key.into(), value.into());
            }
            _ => {}
        }
    }

    pub fn len(&self) -> usize {
        match self {
            Self::String(value) => value.len(),
            Self::StringBuf(value) => value.len(),
            Self::List(list) => list.read().len(),
            Self::Bytes(bytes) => bytes.len(),
            Self::VecI8(vec) => vec.len(),
            Self::VecU16(vec) => vec.len(),
            Self::VecI16(vec) => vec.len(),
            Self::VecU32(vec) => vec.len(),
            Self::VecI32(vec) => vec.len(),
            Self::VecF32(vec) => vec.len(),
            Self::VecI64(vec) => vec.len(),
            Self::VecU64(vec) => vec.len(),
            Self::VecF64(vec) => vec.len(),
            Self::Map(obj) => obj.read().len(),
            Self::Custom(_) => 0,
            _ => 0,
        }
    }

    pub fn keys(&self) -> Vec<SmolStr> {
        if let Self::Map(map) = self {
            map.read().keys().cloned().collect()
        } else if let Some((_, Type::Struct { params: _, fields })) = self.struct_addr_ty() {
            fields.iter().map(|(name, _)| name.clone()).collect()
        } else {
            Vec::new()
        }
    }

    pub fn contains(&self, key: &str) -> bool {
        if let Self::Map(map) = self {
            map.read().get(key).is_some_and(|value| !value.is_null())
        } else if let Self::StructView { ty, .. } | Self::StructOwned { ty, .. } = self {
            ty.get_field(key).is_ok()
        } else if let Self::List(list) = self {
            list.read().iter().find(|l| l.as_str() == key).is_some()
        } else if let Self::String(s) = self {
            s.contains(key)
        } else if let Self::StringBuf(s) = self {
            s.contains(key)
        } else if let Self::Custom(value) = self {
            value.contains_key(key)
        } else {
            false
        }
    }

    pub fn starts_with(&self, prefix: &str) -> bool {
        if let Self::String(s) = self {
            s.starts_with(prefix)
        } else if let Self::StringBuf(s) = self {
            s.starts_with(prefix)
        } else {
            false
        }
    }

    pub fn ends_with(&self, suffix: &str) -> bool {
        if let Self::String(s) = self {
            s.ends_with(suffix)
        } else if let Self::StringBuf(s) = self {
            s.ends_with(suffix)
        } else {
            false
        }
    }

    pub fn get_dynamic(&self, key: &str) -> Option<Dynamic> {
        if let Self::Map(map) = self {
            map.read().get(key).cloned()
        } else if let Some((addr, ty)) = self.struct_addr_ty() {
            let (idx, field_ty) = ty.get_field(key).ok()?;
            Self::read_struct_field(addr, idx, field_ty, ty, self.struct_storage())
        } else if let Self::Custom(value) = self {
            value.get_key(key)
        } else {
            None
        }
    }

    pub fn set_dynamic(&self, key: SmolStr, value: impl Into<Dynamic>) {
        if let Self::Map(map) = self {
            map.write().insert(key, value.into());
        } else if let Some((addr, ty)) = self.struct_addr_ty()
            && let Ok((idx, field_ty)) = ty.get_field(key.as_str())
        {
            Self::write_struct_field(addr, idx, field_ty, ty, value.into(), self.struct_storage());
        } else if let Self::Custom(custom) = self {
            custom.set_key(key.as_str(), value.into());
        }
    }

    fn field_addr(addr: usize, idx: usize, struct_ty: &Type) -> Option<usize> {
        struct_ty.field_offset(idx).map(|offset| addr + offset as usize)
    }

    fn read_dynamic_ptr(addr: usize, storage: Option<&StructBytes>, offset: usize) -> Option<Dynamic> {
        if let Some(storage) = storage {
            return storage.read_dynamic_ptr_at(offset);
        }
        let ptr = unsafe { std::ptr::read_unaligned(addr as *const usize) };
        if ptr == 0 { None } else { Some(unsafe { (&*(ptr as *const Dynamic)).clone() }) }
    }

    fn write_dynamic_ptr(addr: usize, value: Dynamic, storage: Option<&StructBytes>, offset: usize) {
        if let Some(storage) = storage {
            storage.write_dynamic_ptr_at(offset, value);
        } else {
            let ptr = Box::into_raw(Box::new(value)) as usize;
            unsafe {
                std::ptr::write_unaligned(addr as *mut usize, ptr);
            }
        }
    }

    fn read_struct_field(addr: usize, idx: usize, field_ty: &Type, struct_ty: &Type, storage: Option<&StructBytes>) -> Option<Dynamic> {
        let field_addr = Self::field_addr(addr, idx, struct_ty)?;
        let offset = field_addr.saturating_sub(addr);
        match field_ty {
            Type::Bool => Some(Dynamic::Bool(unsafe { std::ptr::read_unaligned(field_addr as *const u8) } != 0)),
            Type::I8 => Some(Dynamic::I8(unsafe { std::ptr::read_unaligned(field_addr as *const i8) })),
            Type::U8 => Some(Dynamic::U8(unsafe { std::ptr::read_unaligned(field_addr as *const u8) })),
            Type::I16 => Some(Dynamic::I16(unsafe { std::ptr::read_unaligned(field_addr as *const i16) })),
            Type::U16 => Some(Dynamic::U16(unsafe { std::ptr::read_unaligned(field_addr as *const u16) })),
            Type::I32 => Some(Dynamic::I32(unsafe { std::ptr::read_unaligned(field_addr as *const i32) })),
            Type::U32 => Some(Dynamic::U32(unsafe { std::ptr::read_unaligned(field_addr as *const u32) })),
            Type::I64 => Some(Dynamic::I64(unsafe { std::ptr::read_unaligned(field_addr as *const i64) })),
            Type::U64 => Some(Dynamic::U64(unsafe { std::ptr::read_unaligned(field_addr as *const u64) })),
            Type::F32 => Some(Dynamic::F32(unsafe { std::ptr::read_unaligned(field_addr as *const f32) })),
            Type::F64 => Some(Dynamic::F64(unsafe { std::ptr::read_unaligned(field_addr as *const f64) })),
            ty if ty.is_struct() || ty.is_array() || ty.is_vec() => {
                if storage.is_some() {
                    Some(Dynamic::owned_struct_from_ptr(field_addr, field_ty.clone()))
                } else {
                    Some(Dynamic::struct_view(field_addr, field_ty.clone()))
                }
            }
            _ => Self::read_dynamic_ptr(field_addr, storage, offset),
        }
    }

    fn write_struct_field(addr: usize, idx: usize, field_ty: &Type, struct_ty: &Type, value: Dynamic, storage: Option<&StructBytes>) {
        let Some(field_addr) = Self::field_addr(addr, idx, struct_ty) else {
            return;
        };
        let offset = field_addr.saturating_sub(addr);
        if let Some(storage) = storage {
            storage.clear_dynamic_fields_in(offset, field_ty.storage_width() as usize);
        }
        match field_ty {
            Type::Bool => unsafe {
                std::ptr::write_unaligned(field_addr as *mut u8, if value.is_true() { 1 } else { 0 });
            },
            Type::I8 => unsafe {
                std::ptr::write_unaligned(field_addr as *mut i8, value.try_into().unwrap_or_default());
            },
            Type::U8 => unsafe {
                std::ptr::write_unaligned(field_addr as *mut u8, value.try_into().unwrap_or_default());
            },
            Type::I16 => unsafe {
                std::ptr::write_unaligned(field_addr as *mut i16, value.try_into().unwrap_or_default());
            },
            Type::U16 => unsafe {
                std::ptr::write_unaligned(field_addr as *mut u16, value.try_into().unwrap_or_default());
            },
            Type::I32 => unsafe {
                std::ptr::write_unaligned(field_addr as *mut i32, value.try_into().unwrap_or_default());
            },
            Type::U32 => unsafe {
                std::ptr::write_unaligned(field_addr as *mut u32, value.try_into().unwrap_or_default());
            },
            Type::I64 => unsafe {
                std::ptr::write_unaligned(field_addr as *mut i64, value.try_into().unwrap_or_default());
            },
            Type::U64 => unsafe {
                std::ptr::write_unaligned(field_addr as *mut u64, value.try_into().unwrap_or_default());
            },
            Type::F32 => unsafe {
                std::ptr::write_unaligned(field_addr as *mut f32, f32::try_from(value).unwrap_or_default());
            },
            Type::F64 => unsafe {
                std::ptr::write_unaligned(field_addr as *mut f64, f64::try_from(value).unwrap_or_default());
            },
            ty if ty.is_struct() || ty.is_array() || ty.is_vec() => {
                if let Some((src_addr, _)) = value.struct_addr_ty() {
                    if let Some(storage) = storage {
                        unsafe {
                            std::ptr::copy_nonoverlapping(src_addr as *const u8, field_addr as *mut u8, field_ty.storage_width() as usize);
                        }
                        storage.clone_dynamic_fields_from(src_addr, field_ty, offset);
                    } else {
                        unsafe {
                            std::ptr::copy_nonoverlapping(src_addr as *const u8, field_addr as *mut u8, field_ty.storage_width() as usize);
                        }
                    }
                }
            }
            _ => Self::write_dynamic_ptr(field_addr, value, storage, offset),
        }
    }

    pub fn remove_dynamic(&self, key: &str) -> Option<Dynamic> {
        // shift_remove 保留插入顺序(swap_remove 会打乱),与原 BTreeMap 删除后仍有序的语义最接近
        if let Self::Map(map) = self { map.write().shift_remove(key) } else { None }
    }

    pub fn get_idx(&self, idx: usize) -> Option<Self> {
        match self {
            Self::List(list) => list.read().get(idx).cloned(),
            Self::VecI8(vec) => vec.get(idx).map(Self::I8),
            Self::VecU16(vec) => vec.get(idx).map(Self::U16),
            Self::VecI16(vec) => vec.get(idx).map(Self::I16),
            Self::VecU32(vec) => vec.get(idx).map(Self::U32),
            Self::VecI32(vec) => vec.get(idx).map(Self::I32),
            Self::VecF32(vec) => vec.get(idx).map(Self::F32),
            Self::VecI64(vec) => vec.get(idx).cloned().map(Self::I64),
            Self::VecU64(vec) => vec.get(idx).cloned().map(Self::U64),
            Self::VecF64(vec) => vec.get(idx).cloned().map(Self::F64),
            Self::StructView { addr, ty } => {
                if let Type::Struct { params: _, fields } = ty.as_ref() {
                    fields.get(idx).and_then(|(_, field_ty)| Self::read_struct_field(*addr, idx, field_ty, ty.as_ref(), None))
                } else {
                    Self::read_aggregate_index(*addr, idx, ty.as_ref(), None)
                }
            }
            Self::StructOwned { storage, ty } => Self::read_aggregate_index(storage.addr(), idx, ty.as_ref(), Some(storage)),
            _ => None,
        }
    }

    fn read_aggregate_index(addr: usize, idx: usize, ty: &Type, storage: Option<&StructBytes>) -> Option<Self> {
        match ty {
            Type::Struct { fields, .. } => fields.get(idx).and_then(|(_, field_ty)| Self::read_struct_field(addr, idx, field_ty, ty, storage)),
            Type::Array(elem_ty, len) | Type::Vec(elem_ty, len) => {
                if idx >= *len as usize {
                    return None;
                }
                let elem_addr = addr + idx * elem_ty.storage_width() as usize;
                Some(Self::read_aggregate_value(elem_addr, elem_ty, storage, elem_addr.saturating_sub(addr)))
            }
            _ => None,
        }
    }

    fn read_aggregate_value(addr: usize, ty: &Type, storage: Option<&StructBytes>, offset: usize) -> Self {
        match ty {
            Type::Bool => Dynamic::Bool(unsafe { std::ptr::read_unaligned(addr as *const u8) } != 0),
            Type::I8 => Dynamic::I8(unsafe { std::ptr::read_unaligned(addr as *const i8) }),
            Type::U8 => Dynamic::U8(unsafe { std::ptr::read_unaligned(addr as *const u8) }),
            Type::I16 => Dynamic::I16(unsafe { std::ptr::read_unaligned(addr as *const i16) }),
            Type::U16 => Dynamic::U16(unsafe { std::ptr::read_unaligned(addr as *const u16) }),
            Type::I32 => Dynamic::I32(unsafe { std::ptr::read_unaligned(addr as *const i32) }),
            Type::U32 => Dynamic::U32(unsafe { std::ptr::read_unaligned(addr as *const u32) }),
            Type::I64 => Dynamic::I64(unsafe { std::ptr::read_unaligned(addr as *const i64) }),
            Type::U64 => Dynamic::U64(unsafe { std::ptr::read_unaligned(addr as *const u64) }),
            Type::F32 => Dynamic::F32(unsafe { std::ptr::read_unaligned(addr as *const f32) }),
            Type::F64 => Dynamic::F64(unsafe { std::ptr::read_unaligned(addr as *const f64) }),
            ty if ty.is_struct() || ty.is_array() || ty.is_vec() => {
                if storage.is_some() {
                    Dynamic::owned_struct_from_ptr(addr, ty.clone())
                } else {
                    Dynamic::struct_view(addr, ty.clone())
                }
            }
            _ => Self::read_dynamic_ptr(addr, storage, offset).unwrap_or(Dynamic::Null),
        }
    }

    pub fn into_iter(self) -> Self {
        if self.is_map() {
            let keys = self.keys();
            Self::Iter { idx: 0, keys, value: Box::new(self) }
        } else {
            Self::Iter { idx: 0, keys: Vec::new(), value: Box::new(self) }
        }
    }

    pub fn next(&mut self) -> Option<Self> {
        if let Self::Iter { idx, keys, value } = self {
            if !keys.is_empty() {
                if *idx < keys.len() {
                    let k = keys[*idx].clone();
                    let v = value.get_dynamic(k.as_str()).unwrap();
                    *idx += 1;
                    return Some(v);
                }
            } else {
                if let Some(v) = value.get_idx(*idx) {
                    *idx += 1;
                    return Some(v);
                }
            }
        }
        None
    }

    pub fn next_pair(&mut self) -> Option<Self> {
        if let Self::Iter { idx, keys, value } = self {
            if !keys.is_empty() {
                if *idx < keys.len() {
                    let k = keys[*idx].clone();
                    let v = value.get_dynamic(k.as_str()).unwrap();
                    *idx += 1;
                    return Some(list!(k, v));
                }
            } else {
                if let Some(v) = value.get_idx(*idx) {
                    *idx += 1;
                    return Some(v);
                }
            }
        }
        None
    }

    pub fn set_idx(&mut self, idx: usize, val: Dynamic) {
        match self {
            Self::List(list) => {
                list.write().get_mut(idx).map(|l| *l = val);
            }
            Self::VecI8(vec) => {
                if let Ok(value) = val.try_into() {
                    vec.set(idx, value);
                }
            }
            Self::VecU16(vec) => {
                if let Ok(value) = val.try_into() {
                    vec.set(idx, value);
                }
            }
            Self::VecI16(vec) => {
                if let Ok(value) = val.try_into() {
                    vec.set(idx, value);
                }
            }
            Self::VecU32(vec) => {
                if let Ok(value) = val.try_into() {
                    vec.set(idx, value);
                }
            }
            Self::VecI32(vec) => {
                if let Ok(value) = val.try_into() {
                    vec.set(idx, value);
                }
            }
            Self::VecF32(vec) => {
                if let Ok(value) = val.try_into() {
                    vec.set(idx, value);
                }
            }
            Self::VecI64(vec) => {
                if let Some(slot) = vec.get_mut(idx)
                    && let Ok(value) = val.try_into()
                {
                    *slot = value;
                }
            }
            Self::VecU64(vec) => {
                if let Some(slot) = vec.get_mut(idx)
                    && let Ok(value) = val.try_into()
                {
                    *slot = value;
                }
            }
            Self::VecF64(vec) => {
                if let Some(slot) = vec.get_mut(idx)
                    && let Ok(value) = val.try_into()
                {
                    *slot = value;
                }
            }
            Self::StructView { addr, ty } => {
                if let Type::Struct { params: _, fields } = ty.as_ref()
                    && let Some((_, field_ty)) = fields.get(idx)
                {
                    Self::write_struct_field(*addr, idx, field_ty, ty.as_ref(), val, None);
                } else {
                    Self::write_aggregate_index(*addr, idx, ty.as_ref(), val, None);
                }
            }
            Self::StructOwned { storage, ty } => {
                if let Type::Struct { params: _, fields } = ty.as_ref()
                    && let Some((_, field_ty)) = fields.get(idx)
                {
                    Self::write_struct_field(storage.addr(), idx, field_ty, ty.as_ref(), val, Some(storage));
                } else {
                    Self::write_aggregate_index(storage.addr(), idx, ty.as_ref(), val, Some(storage));
                }
            }
            _ => {}
        }
    }

    fn write_aggregate_index(addr: usize, idx: usize, ty: &Type, val: Dynamic, storage: Option<&StructBytes>) {
        let (elem_ty, len) = match ty {
            Type::Array(elem_ty, len) | Type::Vec(elem_ty, len) => (elem_ty.as_ref(), *len as usize),
            _ => return,
        };
        if idx >= len {
            return;
        }
        let offset = idx * elem_ty.storage_width() as usize;
        let elem_addr = addr + offset;
        if let Some(storage) = storage {
            storage.clear_dynamic_fields_in(offset, elem_ty.storage_width() as usize);
        }
        match elem_ty {
            Type::Bool => unsafe { std::ptr::write_unaligned(elem_addr as *mut u8, if val.is_true() { 1 } else { 0 }) },
            Type::I8 => unsafe { std::ptr::write_unaligned(elem_addr as *mut i8, val.try_into().unwrap_or_default()) },
            Type::U8 => unsafe { std::ptr::write_unaligned(elem_addr as *mut u8, val.try_into().unwrap_or_default()) },
            Type::I16 => unsafe { std::ptr::write_unaligned(elem_addr as *mut i16, val.try_into().unwrap_or_default()) },
            Type::U16 => unsafe { std::ptr::write_unaligned(elem_addr as *mut u16, val.try_into().unwrap_or_default()) },
            Type::I32 => unsafe { std::ptr::write_unaligned(elem_addr as *mut i32, val.try_into().unwrap_or_default()) },
            Type::U32 => unsafe { std::ptr::write_unaligned(elem_addr as *mut u32, val.try_into().unwrap_or_default()) },
            Type::I64 => unsafe { std::ptr::write_unaligned(elem_addr as *mut i64, val.try_into().unwrap_or_default()) },
            Type::U64 => unsafe { std::ptr::write_unaligned(elem_addr as *mut u64, val.try_into().unwrap_or_default()) },
            Type::F32 => unsafe { std::ptr::write_unaligned(elem_addr as *mut f32, f32::try_from(val).unwrap_or_default()) },
            Type::F64 => unsafe { std::ptr::write_unaligned(elem_addr as *mut f64, f64::try_from(val).unwrap_or_default()) },
            ty if ty.is_struct() || ty.is_array() || ty.is_vec() => {
                if let Some((src_addr, _)) = val.struct_addr_ty() {
                    unsafe {
                        std::ptr::copy_nonoverlapping(src_addr as *const u8, elem_addr as *mut u8, elem_ty.storage_width() as usize);
                    }
                    if let Some(storage) = storage {
                        storage.clone_dynamic_fields_from(src_addr, elem_ty, offset);
                    }
                }
            }
            _ => Self::write_dynamic_ptr(elem_addr, val, storage, offset),
        }
    }

    pub fn to_markdown(&self) -> String {
        let mut s = String::new();
        if let Self::Map(m) = self {
            for (key, v) in m.read().iter() {
                s.push_str(&format!("#### ```{}```\n", key));
                s.push_str(&v.to_markdown());
                s.push('\n');
            }
        } else if let Self::Bytes(bytes) = self {
            s = format!("[{}...]", hex::encode(&bytes[..8]));
        } else {
            let len = self.len();
            if len > 0 {
                for idx in 0..len {
                    s.push_str(&format!("- {}\n", self.get_idx(idx).unwrap().to_markdown()));
                }
            } else {
                s = self.to_string();
            }
        }
        s
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use parking_lot::RwLock;

    #[derive(Debug, PartialEq)]
    struct CustomCounter {
        value: i64,
    }

    #[test]
    fn type_add_promotion_rules() {
        use crate::Type;
        // 相同类型保持
        assert_eq!(Type::I32 + Type::I32, Type::I32);
        // 字符串吸收一切
        assert_eq!(Type::I32 + Type::Str, Type::Str);
        // Any 退化
        assert_eq!(Type::I32 + Type::Any, Type::Any);
        // 浮点优先,取较宽
        assert_eq!(Type::I64 + Type::F32, Type::F32);
        assert_eq!(Type::F32 + Type::F64, Type::F64);
        // 整数取较大宽度
        assert_eq!(Type::I8 + Type::I32, Type::I32);
        assert_eq!(Type::I32 + Type::I64, Type::I64);
        // 有符号先于无符号被处理:i32 + u32 -> i32
        assert_eq!(Type::I32 + Type::U32, Type::I32);
        // 无符号同宽
        assert_eq!(Type::U8 + Type::U32, Type::U32);
    }

    #[test]
    fn dynamic_enum_stays_compact() {
        assert_eq!(std::mem::size_of::<Dynamic>(), 40);
    }

    #[test]
    fn custom_values_can_be_downcast_and_shared_by_clone() {
        let value = Dynamic::custom(RwLock::new(CustomCounter { value: 7 }));
        assert!(value.is_custom());
        assert!(value.custom_type_name().is_some());

        let cloned = value.clone();
        assert_eq!(cloned.as_custom::<RwLock<CustomCounter>>().unwrap().read().value, 7);

        cloned.as_custom::<RwLock<CustomCounter>>().unwrap().write().value = 9;
        assert_eq!(value.as_custom::<RwLock<CustomCounter>>().unwrap().read().value, 9);
        assert_eq!(value, cloned);
    }

    #[derive(Debug, Default)]
    struct CustomPropertyBag {
        values: RwLock<BTreeMap<SmolStr, Dynamic>>,
    }

    impl CustomProperty for CustomPropertyBag {
        fn get_key(&self, key: &str) -> Option<Dynamic> {
            self.values.read().get(key).cloned()
        }

        fn set_key(&self, key: &str, value: Dynamic) -> bool {
            self.values.write().insert(key.into(), value);
            true
        }
    }

    #[test]
    fn custom_values_can_forward_dynamic_properties() {
        let value = Dynamic::custom_with_properties(CustomPropertyBag::default());

        value.set_dynamic("file_mode".into(), 2i64);

        assert!(value.contains("file_mode"));
        assert_eq!(value.get_dynamic("file_mode").and_then(|value| value.as_int()), Some(2));
    }

    #[test]
    fn deep_clone_recursively_copies_maps_and_lists() {
        let nested = Dynamic::map(Default::default());
        nested.insert("score", 1);

        let value = Dynamic::map(Default::default());
        value.insert("nested", nested.clone());
        value.insert("items", Dynamic::list(vec![nested.clone()]));

        let cloned = value.deep_clone();
        cloned.get_dynamic("nested").unwrap().insert("score", 2);
        cloned.get_dynamic("items").unwrap().get_idx(0).unwrap().insert("score", 3);

        assert_eq!(value.get_dynamic("nested").unwrap().get_dynamic("score").and_then(|v| v.as_int()), Some(1));
        assert_eq!(value.get_dynamic("items").unwrap().get_idx(0).unwrap().get_dynamic("score").and_then(|v| v.as_int()), Some(1));
    }

    #[test]
    fn string_add_keeps_concat_semantics() {
        let left = Dynamic::from("hello");
        let right = Dynamic::from(" world");
        let joined = left + right;
        assert!(matches!(joined, Dynamic::StringBuf(_)));
        assert_eq!(joined.as_str(), "hello world");

        assert_eq!((Dynamic::from("level ") + Dynamic::I64(7)).as_str(), "level 7");
        assert_eq!((Dynamic::I64(7) + Dynamic::from(" days")).as_str(), "7 days");
    }

    #[test]
    fn string_add_reuses_string_buf_after_first_concat() {
        let mut value = Dynamic::from("a") + Dynamic::from("b");
        assert!(matches!(value, Dynamic::StringBuf(_)));

        value = value + Dynamic::from("c");
        assert!(matches!(value, Dynamic::StringBuf(_)));
        assert_eq!(value.as_str(), "abc");
    }

    #[test]
    fn u64_as_int_does_not_wrap() {
        assert_eq!(Dynamic::U64(i64::MAX as u64).as_int(), Some(i64::MAX));
        assert_eq!(Dynamic::U64(i64::MAX as u64 + 1).as_int(), None);
    }

    #[test]
    fn dynamic_integer_ops_report_fault_instead_of_panicking() {
        let _ = take_fault();
        assert_eq!(Dynamic::U64(u64::MAX) + Dynamic::U64(1), Dynamic::Null);
        assert!(take_fault().is_some());

        assert_eq!(Dynamic::I64(i64::MAX) + Dynamic::I64(1), Dynamic::Null);
        assert!(take_fault().is_some());

        assert_eq!(Dynamic::I32(1) << Dynamic::I32(64), Dynamic::Null);
        assert!(take_fault().is_some());
    }

    #[test]
    fn typed_vec_set_idx_ignores_bad_index_or_value_without_panicking() {
        let mut values = Dynamic::VecI64(vec![1, 2, 3]);
        values.set_idx(10, Dynamic::I64(99));
        assert_eq!(values.get_idx(2).and_then(|value| value.as_int()), Some(3));

        values.set_idx(1, Dynamic::from("bad"));
        assert_eq!(values.get_idx(1).and_then(|value| value.as_int()), Some(2));

        values.set_idx(1, Dynamic::I64(7));
        assert_eq!(values.get_idx(1).and_then(|value| value.as_int()), Some(7));
    }

    #[test]
    fn nested_struct_fields_use_inline_storage() {
        let inner_ty = Type::Struct { params: vec![], fields: vec![("value".into(), Type::I64)] };
        let outer_ty = Type::Struct { params: vec![], fields: vec![("inner".into(), inner_ty.clone()), ("tag".into(), Type::I64)] };

        let mut inner_bytes = vec![0u8; inner_ty.storage_width() as usize];
        let mut outer_bytes = vec![0u8; outer_ty.storage_width() as usize];
        let inner = Dynamic::struct_view(inner_bytes.as_mut_ptr() as usize, inner_ty);
        let outer = Dynamic::struct_view(outer_bytes.as_mut_ptr() as usize, outer_ty);

        inner.set_dynamic("value".into(), Dynamic::I64(17));
        outer.set_dynamic("inner".into(), inner);
        outer.set_dynamic("tag".into(), Dynamic::I64(3));

        let read_inner = outer.get_dynamic("inner").expect("inner field");
        assert_eq!(read_inner.get_dynamic("value").and_then(|value| value.as_int()), Some(17));
        assert_eq!(outer.get_dynamic("tag").and_then(|value| value.as_int()), Some(3));
    }

    #[test]
    fn owned_struct_clones_dynamic_pointer_fields() {
        let ty = Type::Struct { params: vec![], fields: vec![("name".into(), Type::Str)] };
        let mut bytes = vec![0u8; ty.storage_width() as usize];
        let original = Box::into_raw(Box::new(Dynamic::from("alpha"))) as usize;
        unsafe {
            std::ptr::write_unaligned(bytes.as_mut_ptr() as *mut usize, original);
        }

        let owned = Dynamic::owned_struct_from_ptr(bytes.as_ptr() as usize, ty);
        unsafe {
            drop(Box::from_raw(original as *mut Dynamic));
        }

        assert_eq!(owned.get_dynamic("name").map(|value| value.as_str().to_string()), Some("alpha".to_string()));
        owned.set_dynamic("name".into(), Dynamic::from("beta"));
        assert_eq!(owned.get_dynamic("name").map(|value| value.as_str().to_string()), Some("beta".to_string()));
    }

    #[test]
    fn aggregate_array_fields_support_dynamic_index_access() {
        let ty = Type::Array(std::rc::Rc::new(Type::I64), 3);
        let mut bytes = vec![0u8; ty.storage_width() as usize];
        for (idx, value) in [3i64, 5, 7].into_iter().enumerate() {
            unsafe {
                std::ptr::write_unaligned(bytes.as_mut_ptr().add(idx * 8) as *mut i64, value);
            }
        }

        let mut array = Dynamic::owned_struct_from_ptr(bytes.as_ptr() as usize, ty);
        assert_eq!(array.get_idx(1).and_then(|value| value.as_int()), Some(5));
        array.set_idx(1, Dynamic::I64(11));
        assert_eq!(array.get_idx(1).and_then(|value| value.as_int()), Some(11));
    }

    #[test]
    fn f16_roundtrip_via_helpers() {
        let bits = f64_to_f16(1.0);
        assert_eq!(bits, 0x3C00);
        assert_eq!(f16_to_f64(bits), 1.0);
        let bits = f64_to_f16(0.5);
        assert_eq!(bits, 0x3800);
        assert_eq!(f16_to_f64(bits), 0.5);
    }

    #[test]
    fn f16_dynamic_get_type_and_is_float() {
        let v = Dynamic::F16(0x3C00);
        assert_eq!(v.get_type(), Type::F16);
        assert!(v.is_f16());
        assert!(v.is_signed());
        assert_eq!(v.size_of(), 2);
        assert_eq!(v.as_float(), Some(1.0));
    }

    #[test]
    fn f16_force_from_f64_preserves_value() {
        let d = Type::F16.force(Dynamic::F64(2.0)).unwrap();
        let Dynamic::F16(bits) = d else {
            panic!("expected F16");
        };
        assert_eq!(bits, 0x4000);
        assert_eq!(f16_to_f64(bits), 2.0);
    }

    #[test]
    fn f16_compare_equal_by_bits() {
        assert_eq!(Dynamic::F16(0x3C00), Dynamic::F16(0x3C00));
        assert_ne!(Dynamic::F16(0x3C00), Dynamic::F16(0x4000));
    }

    #[test]
    fn f16_subnormal_roundtrip() {
        // 最小 subnormal (0x0001) ≈ 5.960464477539063e-8
        let bits = f64_to_f16(5.96e-8);
        assert_eq!(bits, 0x0001);
        let back = f16_to_f64(bits);
        let expected = half::f16::from_bits(0x0001).to_f64();
        assert_eq!(back, expected, "got {back}");
    }

    #[test]
    fn f16_infinity_roundtrip() {
        let bits = f64_to_f16(f64::INFINITY);
        assert_eq!(bits, 0x7C00);
        assert!(f16_to_f64(bits).is_infinite());

        let bits = f64_to_f16(f64::NEG_INFINITY);
        assert_eq!(bits, 0xFC00);
        assert!(f16_to_f64(bits).is_sign_negative());
    }

    #[test]
    fn fn_type_partial_eq_with_diff_ret_returns_false_not_panic() {
        use std::rc::Rc;
        let a = Type::Fn { tys: vec![Type::I32], ret: Rc::new(Type::I32) };
        let b = Type::Fn { tys: vec![Type::I32], ret: Rc::new(Type::F32) };
        assert!(a != b);
        assert!(!(a == b));
    }

    #[test]
    fn fn_type_partial_eq_same_args_same_ret_is_true() {
        use std::rc::Rc;
        let a = Type::Fn { tys: vec![Type::I32], ret: Rc::new(Type::I32) };
        let b = Type::Fn { tys: vec![Type::I32], ret: Rc::new(Type::I32) };
        assert!(a == b);
    }

    #[test]
    fn fn_type_partial_eq_diff_args_returns_false() {
        use std::rc::Rc;
        let a = Type::Fn { tys: vec![Type::I32], ret: Rc::new(Type::Void) };
        let b = Type::Fn { tys: vec![Type::I64], ret: Rc::new(Type::Void) };
        assert!(a != b);
    }

    #[test]
    fn fn_type_partial_eq_with_any_ret_is_false() {
        use std::rc::Rc;
        let a = Type::Fn { tys: vec![Type::I32], ret: Rc::new(Type::Any) };
        let b = Type::Fn { tys: vec![Type::I32], ret: Rc::new(Type::I32) };
        assert!(a != b);
    }
}

#[macro_export]
macro_rules! assert_ok {
    ( $x: expr, $ok: expr) => {
        if $x {
            return Ok($ok);
        }
    };
}

#[macro_export]
macro_rules! assert_err {
    ( $x: expr, $err: expr) => {
        if $x {
            return Err($err);
        }
    };
}

pub struct ZOnce {
    first: Option<&'static str>,
    other: &'static str,
}

impl ZOnce {
    pub fn new(first: &'static str, other: &'static str) -> Self {
        Self { first: Some(first), other }
    }
    pub fn take(&mut self) -> &'static str {
        self.first.take().unwrap_or(self.other)
    }
}

mod fixvec;
pub use fixvec::FixVec;
mod msgpack;
pub use msgpack::{MsgPack, MsgUnpack};

pub use json::{FromJson, ToJson};

mod fault;
pub use fault::{has_fault, set_fault, take_fault};
mod ops;
mod types;
pub use types::{ConstIntOp, Type, call_fn, set_dynamic_return_handler};

#[macro_export]
macro_rules! list {
    ($($v:expr),+ $(,)?) => {{
        let mut list = Vec::new();
        $( let _ = list.push(Dynamic::from($v)); )*
        Dynamic::List(::std::sync::Arc::new($crate::RwLock::new(list)))
    }};
}

#[macro_export]
macro_rules! map {
    ($($k:expr => $v:expr), *) => {{
        let mut obj = std::collections::BTreeMap::new();
        $( let _ = obj.insert(smol_str::SmolStr::from($k), Dynamic::from($v)); )*
        Dynamic::map(obj)
    }};
}
