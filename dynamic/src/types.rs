use crate::{Dynamic, DynamicErr};
use smol_str::SmolStr;

use anyhow::{Result, anyhow};
use parking_lot::RwLock;
use std::rc::Rc;

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum ConstIntOp {
    Add,
    Sub,
    Mul,
    Div,
    Mod,
}

#[derive(Debug, Default, Clone, Eq)]
pub enum Type {
    #[default]
    Any, //这个是任何类型 none 动态类型 可以是任何类型
    Void, //整个是空类型  void()
    Bool,
    U8,
    I8,
    U16,
    I16,
    U32,
    I32,
    U64,
    I64,
    F16,
    F32,
    F64,
    Str,
    Map,
    List(Rc<Type>),
    Iter,
    Ident {
        name: SmolStr,
        params: Vec<Type>,
    },
    ConstInt(i64),
    ConstBinary {
        op: ConstIntOp,
        left: Rc<Type>,
        right: Rc<Type>,
    },
    Tuple(Vec<Type>),
    Struct {
        params: Vec<Type>,
        fields: Vec<(SmolStr, Type)>,
    },
    Vec(Rc<Type>, u32),   //spirv 特有 表示向量 一般在四个元素以下
    Array(Rc<Type>, u32), //这是通常意义上的 类型数组，没有大小的限制
    ArrayParam(Rc<Type>, Rc<Type>),
    Fn {
        tys: Vec<Type>,
        ret: Rc<Type>,
    }, //调用参数 和返回参数的类型 注意
    Symbol {
        id: u32,
        params: Vec<Type>,
    }, //自定义的类型 仅在有符号表的情况下有意义 可能是结构 有可能是函数 支持泛型参数
}

unsafe impl Send for Type {}
unsafe impl Sync for Type {}

/// 两个类型相加得到的"公共类型",也是 VM 里混合宽度算术结果类型的推断规则。
///
/// 优先级(从高到低):
/// 1. 完全相同的类型 → 自身;
/// 2. 任一为字符串 → `Str`(支持 `任意 + 字符串` 拼接);
/// 3. 任一为 `Any` → `Any`(动态值参与即退化为动态);
/// 4. 任一为浮点 → 取较宽的浮点(有 f64 则 f64,否则 f32);
/// 5. 任一为有符号整数 → 取双方较大宽度的有符号整数(有符号优先于无符号);
/// 6. 任一为无符号整数 → 取双方较大宽度的无符号整数;
/// 7. 其它 → `Any`。
///
/// 注意:有符号在第 5 步先于无符号被处理,因此 `i32 + u32` 结果是 `i32`(按宽度,
/// 不做无符号回绕检查)。宽度按 `Type::width` 计算。
impl std::ops::Add for Type {
    type Output = Self;
    fn add(self, rhs: Self) -> Self::Output {
        if self == rhs {
            self
        } else if self.is_str() || rhs.is_str() {
            Type::Str
        } else if self.is_any() || rhs.is_any() {
            Type::Any
        } else if self.is_float() || rhs.is_float() {
            if self.is_f64() || rhs.is_f64() { Type::F64 } else { Type::F32 }
        } else if self.is_int() || rhs.is_int() {
            match self.width().max(rhs.width()) {
                1 => Type::I8,
                2 => Type::I16,
                4 => Type::I32,
                _ => Type::I64,
            }
        } else if self.is_uint() || rhs.is_uint() {
            match self.width().max(rhs.width()) {
                1 => Type::U8,
                2 => Type::U16,
                4 => Type::U32,
                _ => Type::U64,
            }
        } else {
            Type::Any
        }
    }
}

impl PartialEq for Type {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (Type::Any, Type::Any) => true,
            (Type::Void, Type::Void)
            | (Type::Bool, Type::Bool)
            | (Type::U8, Type::U8)
            | (Type::I8, Type::I8)
            | (Type::U16, Type::U16)
            | (Type::I16, Type::I16)
            | (Type::U32, Type::U32)
            | (Type::I32, Type::I32)
            | (Type::U64, Type::U64)
            | (Type::I64, Type::I64)
            | (Type::F16, Type::F16)
            | (Type::F32, Type::F32)
            | (Type::F64, Type::F64)
            | (Type::Str, Type::Str)
            | (Type::Map, Type::Map) => true,
            (Type::List(left), Type::List(right)) => left == right,
            (Type::Ident { name: name1, params: params1 }, Type::Ident { name: name2, params: params2 }) => name1 == name2 && params1 == params2,
            (Type::ConstInt(left), Type::ConstInt(right)) => left == right,
            (Type::ConstBinary { op: op1, left: left1, right: right1 }, Type::ConstBinary { op: op2, left: left2, right: right2 }) => op1 == op2 && left1 == left2 && right1 == right2,
            (Type::Symbol { id: id1, params: p1 }, Type::Symbol { id: id2, params: p2 }) => id1 == id2 && p1 == p2,
            (Type::Struct { params: p1, fields: f1 }, Type::Struct { params: p2, fields: f2 }) => {
                p1.len() == p2.len() && f1.len() == f2.len() && p1.iter().zip(p2.iter()).position(|(t1, t2)| t1 != t2).is_none() && f1.iter().zip(f2.iter()).position(|(item1, item2)| item1 != item2).is_none()
            }
            (Type::Vec(elem_type1, len1), Type::Vec(elem_type2, len2)) => elem_type1 == elem_type2 && len1 == len2,
            (Type::Array(elem_type1, len1), Type::Array(elem_type2, len2)) => elem_type1 == elem_type2 && len1 == len2,
            (Type::ArrayParam(elem_type1, len1), Type::ArrayParam(elem_type2, len2)) => elem_type1 == elem_type2 && len1 == len2,
            (Type::Fn { tys: t1, ret: r1 }, Type::Fn { tys: t2, ret: r2 }) => t1 == t2 && r1 == r2,
            _ => false,
        }
    }
}

impl Type {
    pub fn list_any() -> Self {
        Self::List(Rc::new(Self::Any))
    }

    fn align_up(value: u32, align: u32) -> u32 {
        if align <= 1 { value } else { (value + align - 1) & !(align - 1) }
    }

    pub fn align(&self) -> u32 {
        self.storage_width().min(8).max(1)
    }

    pub fn storage_width(&self) -> u32 {
        match self {
            Self::Void => 0,
            Self::Bool => 1,
            Self::U8 | Self::I8 => 1,
            Self::U16 | Self::I16 | Self::F16 => 2,
            Self::U32 | Self::I32 | Self::F32 => 4,
            Self::U64 | Self::I64 | Self::F64 => 8,
            Self::Struct { params: _, fields } => Self::struct_layout(fields).0,
            Self::Vec(ty, num) => num * ty.storage_width(),
            Self::Array(ty, num) => num * ty.storage_width(),
            Self::ArrayParam(ty, len) => {
                if let Self::ConstInt(num) = len.as_ref() {
                    if *num >= 0 { *num as u32 * ty.storage_width() } else { 8 }
                } else {
                    8
                }
            }
            Self::ConstBinary { .. } => 8,
            _ => 8,
        }
    }

    pub fn struct_layout(fields: &[(SmolStr, Type)]) -> (u32, Vec<u32>) {
        let mut offset = 0;
        let mut offsets = Vec::with_capacity(fields.len());
        let mut struct_align = 8;
        for (_, ty) in fields {
            let align = ty.align().min(8);
            struct_align = struct_align.max(align);
            offset = Self::align_up(offset, align);
            offsets.push(offset);
            offset += ty.storage_width();
        }
        (Self::align_up(offset, struct_align), offsets)
    }

    pub fn field_offset(&self, idx: usize) -> Option<u32> {
        if let Self::Struct { params: _, fields } = self { Self::struct_layout(fields).1.get(idx).cloned() } else { None }
    }

    pub fn len(&self) -> usize {
        match self {
            Self::Struct { params: _, fields } => fields.len(),
            Self::Tuple(items) => items.len(),
            Self::Vec(_, num) | Self::Array(_, num) => *num as usize,
            Self::ArrayParam(_, len) => {
                if let Self::ConstInt(num) = len.as_ref() {
                    if *num >= 0 { *num as usize } else { 0 }
                } else {
                    0
                }
            }
            Self::ConstBinary { .. } => 0,
            _ => 0,
        }
    }

    pub fn compare_args(left: &[Type], right: &[Type]) -> Option<Vec<Type>> {
        let mut tys = Vec::new();
        for (left, right) in left.iter().zip(right.iter()) {
            if left == right || right.is_any() {
                tys.push(left.clone());
            } else if left.is_any() {
                tys.push(right.clone());
            } else {
                return None;
            }
        }
        Some(tys)
    }

    pub fn force(&self, src: Dynamic) -> Result<Dynamic, DynamicErr> {
        match self {
            Self::Bool => src.try_into().map(Dynamic::Bool),
            Self::I8 => src.try_into().map(Dynamic::I8),
            Self::I16 => src.try_into().map(Dynamic::I16),
            Self::I32 => src.try_into().map(Dynamic::I32),
            Self::I64 => src.try_into().map(Dynamic::I64),
            Self::U8 => src.try_into().map(Dynamic::U8),
            Self::U16 => src.try_into().map(Dynamic::U16),
            Self::U32 => src.try_into().map(Dynamic::U32),
            Self::U64 => src.try_into().map(Dynamic::U64),
            Self::F16 => {
                let f: f64 = src.try_into()?;
                Ok(Dynamic::F16(crate::f64_to_f16(f)))
            }
            Self::F32 => src.try_into().map(Dynamic::F32),
            Self::F64 => src.try_into().map(Dynamic::F64),
            Self::Str => Ok(Dynamic::from(src.to_string())),
            _ => Ok(src),
        }
    }

    pub fn width(&self) -> u32 {
        //所占字节数
        self.storage_width()
    }

    pub fn is_void(&self) -> bool {
        if let Self::Void = self { true } else { false }
    }

    pub fn is_bool(&self) -> bool {
        if let Self::Bool = self { true } else { false }
    }

    pub fn is_str(&self) -> bool {
        if let Self::Str = self { true } else { false }
    }

    pub fn is_native(&self) -> bool {
        match self {
            Self::F16 | Self::F32 | Self::F64 | Self::U8 | Self::I8 | Self::U16 | Self::I16 | Self::U32 | Self::I32 | Self::U64 | Self::I64 => true,
            _ => false,
        }
    }

    pub fn is_any(&self) -> bool {
        match self {
            Self::Any => true,
            Self::Fn { tys: _, ret } => ret.is_any(),
            _ => false,
        }
    }

    pub fn is_ident(&self) -> bool {
        if let Self::Ident { name: _, params: _ } = self { true } else { false }
    }

    pub fn is_struct(&self) -> bool {
        if let Self::Struct { .. } = self { true } else { false }
    }

    pub fn get_field(&self, name: &str) -> Result<(usize, &Type)> {
        if let Self::Struct { params: _, fields } = self {
            fields.iter().enumerate().find(|(_, (field_name, _))| field_name == name).map(|(index, (_, ty))| (index, ty)).ok_or(anyhow!("{:?} 未发现属性 {}", self, name))
        } else {
            Err(anyhow!("不是结构体"))
        }
    }

    pub fn add_field(&mut self, name: SmolStr, ty: Type) -> Result<u32> {
        if let Self::Struct { params: _, fields } = self {
            fields.push((name, ty));
            Ok(fields.len() as u32 - 1)
        } else {
            Err(anyhow!("不是结构体"))
        }
    }

    pub fn is_vec(&self) -> bool {
        if let Self::Vec(_, _) = self { true } else { false }
    }

    pub fn is_array(&self) -> bool {
        if let Self::Array(_, _) = self { true } else { false }
    }

    pub fn is_int(&self) -> bool {
        match self {
            Self::I8 | Self::I16 | Self::I32 | Self::I64 => true,
            _ => false,
        }
    }

    pub fn is_uint(&self) -> bool {
        match self {
            Self::U8 | Self::U16 | Self::U32 | Self::U64 => true,
            _ => false,
        }
    }

    pub fn sign(self) -> Self {
        match self {
            Self::U8 => Self::I8,
            Self::U16 => Self::I16,
            Self::U32 => Self::I32,
            Self::U64 => Self::I64,
            _ => self,
        }
    }

    pub fn is_float(&self) -> bool {
        match self {
            Self::F16 | Self::F32 | Self::F64 => true,
            _ => false,
        }
    }

    pub fn is_f64(&self) -> bool {
        match self {
            Self::F64 => true,
            _ => false,
        }
    }

    pub fn is_f32(&self) -> bool {
        match self {
            Self::F32 => true,
            _ => false,
        }
    }

    pub fn is_fn(&self) -> bool {
        if let Self::Fn { .. } = self { true } else { false }
    }

    pub fn from_args(args: Vec<(SmolStr, Type)>) -> (Self, Vec<SmolStr>) {
        let (args, tys) = args.into_iter().fold((Vec::new(), Vec::new()), |mut v, a| {
            v.0.push(a.0);
            v.1.push(a.1);
            v
        });
        (Self::Fn { tys, ret: Rc::new(Type::Any) }, args)
    }
}

impl Dynamic {
    pub fn get_type(&self) -> Type {
        let len = self.len() as u32;
        match self {
            Self::Bool(_) => Type::Bool,
            Self::I8(_) => Type::I8,
            Self::I16(_) => Type::I16,
            Self::I32(_) => Type::I32,
            Self::I64(_) => Type::I64,
            Self::U8(_) => Type::U8,
            Self::U16(_) => Type::U16,
            Self::U32(_) => Type::U32,
            Self::U64(_) => Type::U64,
            Self::F16(_) => Type::F16,
            Self::F32(_) => Type::F32,
            Self::F64(_) => Type::F64,
            Self::Bytes(_) => Type::Vec(Rc::new(Type::U8), len),
            Self::VecI8(_) => Type::Vec(Rc::new(Type::I8), len),
            Self::VecI16(_) => Type::Vec(Rc::new(Type::I16), len),
            Self::VecI32(_) => Type::Vec(Rc::new(Type::I32), len),
            Self::VecI64(_) => Type::Vec(Rc::new(Type::I64), len),
            Self::VecU16(_) => Type::Vec(Rc::new(Type::U16), len),
            Self::VecU32(_) => Type::Vec(Rc::new(Type::U32), len),
            Self::VecU64(_) => Type::Vec(Rc::new(Type::U64), len),
            Self::VecF32(_) => Type::Vec(Rc::new(Type::F32), len),
            Self::VecF64(_) => Type::Vec(Rc::new(Type::F64), len),
            Self::String(_) | Self::StringBuf(_) => Type::Str,
            Self::Map(_) => Type::Map,
            Self::StructView { ty, .. } | Self::StructOwned { ty, .. } => ty.as_ref().clone(),
            Self::Custom(_) => Type::Any,
            Self::Null => Type::Void,
            Self::List(items) => {
                let tys: Vec<Type> = items.read().iter().map(|v| v.get_type()).collect();
                if let Some(first) = tys.first() {
                    if tys.iter().all(|x| x == first) {
                        return Type::Array(Rc::new(first.clone()), len);
                    }
                }
                Type::list_any()
            }
            Self::Iter { idx: _, keys: _, value: _ } => Type::Iter,
        }
    }
}

type DynamicReturnHandler = unsafe fn(*const Dynamic) -> Box<Dynamic>;

static DYNAMIC_RETURN_HANDLER: RwLock<Option<DynamicReturnHandler>> = RwLock::new(None);

pub fn set_dynamic_return_handler(handler: DynamicReturnHandler) {
    *DYNAMIC_RETURN_HANDLER.write() = Some(handler);
}

unsafe fn take_dynamic_return(ptr: *const Dynamic) -> Box<Dynamic> {
    if let Some(handler) = *DYNAMIC_RETURN_HANDLER.read() {
        unsafe { handler(ptr) }
    } else if ptr.is_null() {
        Box::new(Dynamic::Null)
    } else {
        unsafe { Box::from_raw(ptr as *mut Dynamic) }
    }
}

pub fn call_fn(ptr: i64, ret_ty: Type, param: Box<Dynamic>) -> Result<Box<Dynamic>> {
    let param = Box::into_raw(param);
    match ret_ty {
        Type::Any => {
            let fn_ptr: extern "C" fn(*const Dynamic) -> *mut Dynamic = unsafe { std::mem::transmute(ptr) };
            let r = fn_ptr(param);
            unsafe {
                drop(Box::from_raw(param));
            }
            Ok(unsafe { take_dynamic_return(r) })
        }
        Type::Bool => {
            let fn_ptr: extern "C" fn(*const Dynamic) -> i8 = unsafe { std::mem::transmute(ptr) };
            let r = fn_ptr(param);
            unsafe {
                drop(Box::from_raw(param));
            }
            Ok(Box::new(Dynamic::Bool(r != 0)))
        }
        Type::Void => {
            let fn_ptr: extern "C" fn(*const Dynamic) = unsafe { std::mem::transmute(ptr) };
            fn_ptr(param);
            unsafe {
                drop(Box::from_raw(param));
            }
            Ok(Box::new(Dynamic::Null))
        }
        Type::F32 => {
            let fn_ptr: extern "C" fn(*const Dynamic) -> f32 = unsafe { std::mem::transmute(ptr) };
            let r = fn_ptr(param);
            unsafe {
                drop(Box::from_raw(param));
            }
            Ok(Box::new(Dynamic::F32(r)))
        }
        Type::F64 => {
            let fn_ptr: extern "C" fn(*const Dynamic) -> f64 = unsafe { std::mem::transmute(ptr) };
            let r = fn_ptr(param);
            unsafe {
                drop(Box::from_raw(param));
            }
            Ok(Box::new(Dynamic::F64(r)))
        }
        _ => {
            let fn_ptr: extern "C" fn(*const Dynamic) -> i64 = unsafe { std::mem::transmute(ptr) };
            let r = fn_ptr(param);
            unsafe {
                drop(Box::from_raw(param));
            }
            Ok(Box::new(Dynamic::I64(r)))
        }
    }
}
