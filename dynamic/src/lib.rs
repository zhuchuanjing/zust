use bytemuck::{AnyBitPattern, NoUninit, cast_slice, cast_slice_mut};
use smol_str::SmolStr;
use std::any::Any;
use std::collections::BTreeMap;
use std::mem;
use tinyvec::TinyVec;
const TINY_SIZE: usize = 28;
pub mod json;
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

use std::sync::{Arc, RwLock};

#[derive(Clone)]
pub struct CustomValue {
    type_name: &'static str,
    value: Arc<dyn Any + Send + Sync>,
}

impl CustomValue {
    pub fn new<T>(value: T) -> Self
    where
        T: Any + Send + Sync + 'static,
    {
        Self { type_name: std::any::type_name::<T>(), value: Arc::new(value) }
    }

    pub fn from_arc<T>(value: Arc<T>) -> Self
    where
        T: Any + Send + Sync + 'static,
    {
        Self { type_name: std::any::type_name::<T>(), value }
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
}

impl std::fmt::Debug for CustomValue {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CustomValue").field("type_name", &self.type_name).finish()
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
    F32(f32), //默认浮点类型
    F64(f64),
    String(SmolStr),
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
    Map(Arc<RwLock<BTreeMap<SmolStr, Dynamic>>>),
    Struct {
        addr: usize,
        ty: Type,
    },
    Custom(CustomValue),
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
            (Self::String(a), Self::String(b)) => a == b,
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
            (Self::F32(a), Self::F32(b)) => a.to_bits() == b.to_bits(),
            (Self::F64(a), Self::F64(b)) => a.to_bits() == b.to_bits(),
            (a, b) if (a.is_f32() || a.is_f64()) && (b.is_f32() || b.is_f64()) => a.as_float() == b.as_float(),
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
                let a_guard = a.read().unwrap();
                let b_guard = b.read().unwrap();
                if a_guard.len() != b_guard.len() {
                    return false;
                }
                a_guard.iter().zip(b_guard.iter()).all(|(x, y)| x == y)
            }
            // Map - compare key-value pairs
            (Self::Map(a), Self::Map(b)) => {
                let a_guard = a.read().unwrap();
                let b_guard = b.read().unwrap();
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
            // Struct - compare addresses and types
            (Self::Struct { addr: a_addr, ty: a_ty }, Self::Struct { addr: b_addr, ty: b_ty }) => a_addr == b_addr && a_ty == b_ty,
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
        } else if (self.is_true() && other.is_false()) || (self.is_false() && other.is_true()) {
            Ordering::Less
        } else if self.is_null() && other.is_null() {
            Ordering::Equal
        } else if let Self::String(s1) = self
            && let Self::String(s2) = other
        {
            s1.cmp(s2)
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
        Self::Custom(CustomValue::new(value))
    }

    pub fn custom_arc<T>(value: Arc<T>) -> Self
    where
        T: Any + Send + Sync + 'static,
    {
        Self::Custom(CustomValue::from_arc(value))
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
                let m = m.read().unwrap().iter().map(|(k, v)| (k.clone(), v.deep_clone())).collect();
                Self::map(m)
            }
            Self::List(l) => {
                let l = l.read().unwrap().iter().map(|item| item.deep_clone()).collect();
                Self::list(l)
            }
            Self::Struct { addr, ty } => Self::Struct { addr: *addr, ty: ty.clone() },
            _ => self.clone(),
        }
    }

    pub fn add(&mut self, val: i64) -> Option<i64> {
        //如果是 整数类型 增加指定值 并返回新的值 不考虑溢出
        match self {
            Self::U8(u) => {
                let v = (*u as i64) + val;
                *u = v as u8;
                Some(v)
            }
            Self::U16(u) => {
                let v = (*u as i64) + val;
                *u = v as u16;
                Some(v)
            }
            Self::U32(u) => {
                let v = (*u as i64) + val;
                *u = v as u32;
                Some(v)
            }
            Self::U64(u) => {
                let v = (*u as i64) + val;
                *u = v as u64;
                Some(v)
            }
            Self::I8(i) => {
                let v = (*i as i64) + val;
                *i = v as i8;
                Some(v)
            }
            Self::I16(i) => {
                let v = (*i as i64) + val;
                *i = v as i16;
                Some(v)
            }
            Self::I32(i) => {
                let v = (*i as i64) + val;
                *i = v as i32;
                Some(v)
            }
            Self::I64(i) => {
                let v = (*i as i64) + val;
                *i = v;
                Some(v)
            }
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
                    left.write().unwrap().append(&mut right.write().unwrap());
                } else {
                    left.write().unwrap().push(rhs);
                }
            }
            (Self::Map(left), Self::Map(right)) => {
                left.write().unwrap().append(&mut right.write().unwrap());
            }
            (_, _) => {}
        }
    }

    pub fn into_vec<T: TryFrom<Self> + 'static>(self) -> Option<Vec<T>> {
        if std::any::TypeId::of::<T>() == std::any::TypeId::of::<Dynamic>() {
            match self {
                Dynamic::List(list) => {
                    match Arc::try_unwrap(list) {
                        Ok(vec) => vec.into_inner().map(|v| unsafe { mem::transmute::<Vec<Dynamic>, Vec<T>>(v) }).ok(), // 成功：直接返回 Vec
                        Err(_) => None,                                                                                 // 失败：有其他引用，无法移动所有权
                    }
                }
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
                Dynamic::List(list) => Arc::try_unwrap(list).ok().and_then(|l| l.into_inner().map(|l| l.into_iter().filter_map(|l| T::try_from(l).ok()).collect()).ok()),
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
                list.write().unwrap().push(value.into());
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

    pub fn pop(&mut self) -> Option<Dynamic> {
        match self {
            Self::List(list) => list.write().unwrap().pop(),
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
            Self::U64(u) => Some(*u as i64),
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

    pub fn is_str(&self) -> bool {
        if let Self::String(_) = self { true } else { false }
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
            Self::F32(f) => Some(*f as f64),
            Self::F64(f) => Some(*f),
            _ => None,
        }
    }

    pub fn is_signed(&self) -> bool {
        match self {
            Self::I8(_) | Self::I16(_) | Self::I32(_) | Self::I64(_) | Self::F32(_) | Self::F64(_) => true,
            _ => false,
        }
    }

    pub fn size_of(&self) -> usize {
        match self {
            Self::I8(_) | Self::U8(_) => 1,
            Self::I16(_) | Self::U16(_) => 2,
            Self::I32(_) | Self::U32(_) | Self::F32(_) => 4,
            Self::I64(_) | Self::U64(_) | Self::F64(_) => 8,
            Self::String(s) => s.len(),
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
            Self::List(list) => list.read().unwrap().len(),
            Self::Map(obj) => obj.read().unwrap().len(),
            Self::Struct { ty, .. } => ty.len(),
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
            _ => false,
        }
    }

    pub fn split(self, tag: &str) -> Self {
        match self {
            Self::String(s) => Self::list(s.split(tag).map(|p| Dynamic::from(p)).collect()),
            _ => self,
        }
    }

    pub fn map(m: BTreeMap<SmolStr, Dynamic>) -> Self {
        Dynamic::Map(Arc::new(RwLock::new(m)))
    }

    pub fn into_map(self) -> Option<BTreeMap<SmolStr, Dynamic>> {
        if let Self::Map(map) = self { Arc::try_unwrap(map).ok().and_then(|m| m.into_inner().ok()) } else { None }
    }

    pub fn is_map(&self) -> bool {
        if let Self::Map(_) | Self::Struct { .. } = self { true } else { false }
    }

    pub fn insert<K: Into<SmolStr>, T: Into<Self>>(&self, key: K, value: T) {
        match self {
            Self::Map(obj) => {
                obj.write().unwrap().insert(key.into(), value.into());
            }
            _ => {}
        }
    }

    pub fn len(&self) -> usize {
        match self {
            Self::String(value) => value.len(),
            Self::List(list) => list.read().unwrap().len(),
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
            Self::Map(obj) => obj.read().unwrap().len(),
            Self::Custom(_) => 0,
            _ => 0,
        }
    }

    pub fn keys(&self) -> Vec<SmolStr> {
        if let Self::Map(map) = self {
            map.read().unwrap().keys().cloned().collect()
        } else if let Self::Struct { ty: Type::Struct { params: _, fields }, .. } = self {
            fields.iter().map(|(name, _)| name.clone()).collect()
        } else {
            Vec::new()
        }
    }

    pub fn contains(&self, key: &str) -> bool {
        if let Self::Map(map) = self {
            map.read().unwrap().get(key).is_some_and(|value| !value.is_null())
        } else if let Self::Struct { ty, .. } = self {
            ty.get_field(key).is_ok()
        } else if let Self::List(list) = self {
            list.read().unwrap().iter().find(|l| l.as_str() == key).is_some()
        } else if let Self::String(s) = self {
            s.contains(key)
        } else {
            false
        }
    }

    pub fn starts_with(&self, prefix: &str) -> bool {
        if let Self::String(s) = self { s.starts_with(prefix) } else { false }
    }

    pub fn get_dynamic(&self, key: &str) -> Option<Dynamic> {
        if let Self::Map(map) = self {
            map.read().unwrap().get(key).cloned()
        } else if let Self::Struct { addr, ty } = self {
            let (idx, field_ty) = ty.get_field(key).ok()?;
            Self::read_struct_field(*addr, idx, field_ty, ty)
        } else {
            None
        }
    }

    pub fn set_dynamic(&self, key: SmolStr, value: impl Into<Dynamic>) {
        if let Self::Map(map) = self {
            map.write().unwrap().insert(key, value.into());
        } else if let Self::Struct { addr, ty } = self
            && let Ok((idx, field_ty)) = ty.get_field(key.as_str())
        {
            Self::write_struct_field(*addr, idx, field_ty, ty, value.into());
        }
    }

    fn field_addr(addr: usize, idx: usize, struct_ty: &Type) -> Option<usize> {
        struct_ty.field_offset(idx).map(|offset| addr + offset as usize)
    }

    fn read_dynamic_ptr(addr: usize) -> Option<Dynamic> {
        let ptr = unsafe { std::ptr::read_unaligned(addr as *const usize) };
        if ptr == 0 { None } else { Some(unsafe { (&*(ptr as *const Dynamic)).clone() }) }
    }

    fn write_dynamic_ptr(addr: usize, value: Dynamic) {
        let ptr = Box::into_raw(Box::new(value)) as usize;
        unsafe {
            std::ptr::write_unaligned(addr as *mut usize, ptr);
        }
    }

    fn read_struct_field(addr: usize, idx: usize, field_ty: &Type, struct_ty: &Type) -> Option<Dynamic> {
        let field_addr = Self::field_addr(addr, idx, struct_ty)?;
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
            Type::Struct { .. } => {
                let ptr = unsafe { std::ptr::read_unaligned(field_addr as *const usize) };
                Some(Dynamic::Struct { addr: ptr, ty: field_ty.clone() })
            }
            _ => Self::read_dynamic_ptr(field_addr),
        }
    }

    fn write_struct_field(addr: usize, idx: usize, field_ty: &Type, struct_ty: &Type, value: Dynamic) {
        let Some(field_addr) = Self::field_addr(addr, idx, struct_ty) else {
            return;
        };
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
            Type::Struct { .. } => {
                if let Dynamic::Struct { addr, ty: _ } = value {
                    unsafe {
                        std::ptr::write_unaligned(field_addr as *mut usize, addr);
                    }
                }
            }
            _ => Self::write_dynamic_ptr(field_addr, value),
        }
    }

    pub fn remove_dynamic(&self, key: &str) -> Option<Dynamic> {
        if let Self::Map(map) = self { map.write().unwrap().remove(key) } else { None }
    }

    pub fn get_idx(&self, idx: usize) -> Option<Self> {
        match self {
            Self::List(list) => list.read().unwrap().get(idx).cloned(),
            Self::VecI8(vec) => vec.get(idx).map(Self::I8),
            Self::VecU16(vec) => vec.get(idx).map(Self::U16),
            Self::VecI16(vec) => vec.get(idx).map(Self::I16),
            Self::VecU32(vec) => vec.get(idx).map(Self::U32),
            Self::VecI32(vec) => vec.get(idx).map(Self::I32),
            Self::VecF32(vec) => vec.get(idx).map(Self::F32),
            Self::VecI64(vec) => vec.get(idx).cloned().map(Self::I64),
            Self::VecU64(vec) => vec.get(idx).cloned().map(Self::U64),
            Self::VecF64(vec) => vec.get(idx).cloned().map(Self::F64),
            Self::Struct { addr, ty } => {
                if let Type::Struct { params: _, fields } = ty {
                    fields.get(idx).and_then(|(_, field_ty)| Self::read_struct_field(*addr, idx, field_ty, ty))
                } else {
                    None
                }
            }
            _ => None,
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
                list.write().unwrap().get_mut(idx).map(|l| *l = val);
            }
            Self::VecI8(vec) => vec.set(idx, val.try_into().unwrap()),
            Self::VecU16(vec) => vec.set(idx, val.try_into().unwrap()),
            Self::VecI16(vec) => vec.set(idx, val.try_into().unwrap()),
            Self::VecU32(vec) => vec.set(idx, val.try_into().unwrap()),
            Self::VecI32(vec) => vec.set(idx, val.try_into().unwrap()),
            Self::VecF32(vec) => vec.set(idx, val.try_into().unwrap()),
            Self::VecI64(vec) => vec[idx] = val.try_into().unwrap(),
            Self::VecU64(vec) => vec[idx] = val.try_into().unwrap(),
            Self::VecF64(vec) => vec[idx] = val.try_into().unwrap(),
            Self::Struct { addr, ty } => {
                if let Type::Struct { params: _, fields } = ty.clone()
                    && let Some((_, field_ty)) = fields.get(idx)
                {
                    Self::write_struct_field(*addr, idx, field_ty, &ty, val);
                }
            }
            _ => {}
        }
    }

    pub fn to_markdown(&self) -> String {
        let mut s = String::new();
        if let Self::Map(m) = self {
            for (key, v) in m.read().unwrap().iter() {
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
    use std::sync::RwLock;

    #[derive(Debug, PartialEq)]
    struct CustomCounter {
        value: i64,
    }

    #[test]
    fn custom_values_can_be_downcast_and_shared_by_clone() {
        let value = Dynamic::custom(RwLock::new(CustomCounter { value: 7 }));
        assert!(value.is_custom());
        assert!(value.custom_type_name().is_some());

        let cloned = value.clone();
        assert_eq!(cloned.as_custom::<RwLock<CustomCounter>>().unwrap().read().unwrap().value, 7);

        cloned.as_custom::<RwLock<CustomCounter>>().unwrap().write().unwrap().value = 9;
        assert_eq!(value.as_custom::<RwLock<CustomCounter>>().unwrap().read().unwrap().value, 9);
        assert_eq!(value, cloned);
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

mod ops;
mod types;
pub use types::{ConstIntOp, Type, call_fn, set_dynamic_return_handler};

#[macro_export]
macro_rules! list {
    ($($v:expr),+ $(,)?) => {{
        let mut list = Vec::new();
        $( let _ = list.push(Dynamic::from($v)); )*
        Dynamic::List(std::sync::Arc::new(std::sync::RwLock::new(list)))
    }};
}

#[macro_export]
macro_rules! map {
    ($($k:expr => $v:expr), *) => {{
        let mut obj = std::collections::BTreeMap::new();
        $( let _ = obj.insert(smol_str::SmolStr::from($k), Dynamic::from($v)); )*
        Dynamic::Map(std::sync::Arc::new(std::sync::RwLock::new(obj)))
    }};
}
