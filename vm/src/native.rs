use super::FnVariant;
use crate::JITRunTime;
use crate::memory::{alloc_dynamic, alloc_struct_bytes, take_dynamic_return};
use anyhow::Result;
use cranelift::prelude::AbiParam;
use cranelift_module::{Linkage, Module};
use dynamic::{Dynamic, Type};
use parser::{BinaryOp, Expr, ExprKind, Span};
use rand::RngExt;
use std::borrow::Cow;
use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::sync::{RwLock, Weak};

#[derive(Clone, Debug)]
pub struct ZustCallback {
    pub fn_ptr: usize,
    pub ret_ty: Type,
    pub explicit_arg_len: usize,
    pub captures: Vec<Dynamic>,
}

impl ZustCallback {
    pub fn new(fn_ptr: usize, ret_ty: Type, captures: Vec<Dynamic>) -> Self {
        Self { fn_ptr, ret_ty, explicit_arg_len: usize::MAX, captures }
    }

    pub fn new_with_arg_len(fn_ptr: usize, ret_ty: Type, explicit_arg_len: usize, captures: Vec<Dynamic>) -> Self {
        Self { fn_ptr, ret_ty, explicit_arg_len, captures }
    }

    pub fn call0(&self) -> Result<Dynamic> {
        self.call(Vec::new())
    }

    pub fn call1(&self, arg: Dynamic) -> Result<Dynamic> {
        self.call(vec![arg])
    }

    pub fn call(&self, mut args: Vec<Dynamic>) -> Result<Dynamic> {
        if self.explicit_arg_len != usize::MAX {
            args.truncate(self.explicit_arg_len);
            while args.len() < self.explicit_arg_len {
                args.push(Dynamic::Null);
            }
        }
        let args: Vec<Box<Dynamic>> = args.into_iter().map(Box::new).collect();
        let mut ptrs: Vec<*const Dynamic> = args.iter().map(|arg| arg.as_ref() as *const Dynamic).collect();
        ptrs.extend(self.captures.iter().map(|value| value as *const Dynamic));
        unsafe { call_callback(self.fn_ptr as *const u8, &self.ret_ty, &ptrs) }
    }
}

extern "C" fn any_clone(addr: *const Dynamic) -> *const Dynamic {
    //在堆上分配内存 复制 addr 到内存中
    unsafe {
        let cloned_value = (*addr).deep_clone();
        alloc_dynamic(cloned_value)
    }
}

extern "C" fn any_null() -> *const Dynamic {
    //在堆上分配内存 复制 addr 到内存中
    alloc_dynamic(Dynamic::Null)
}

extern "C" fn print(addr: *const Dynamic) {
    if !addr.is_null() {
        unsafe {
            println!("{}", (*addr).to_string());
        }
    }
}

extern "C" fn sqrt(value: f64) -> f64 {
    value.sqrt()
}

extern "C" fn log_any(addr: *const Dynamic) {
    if addr.is_null() {
        log::debug!("{:?}", Dynamic::Null);
    } else {
        log::debug!("{:?}", unsafe { &*addr });
    }
}

extern "C" fn any_is_map(addr: *const Dynamic) -> bool {
    !addr.is_null() && unsafe { (*addr).is_map() }
}

extern "C" fn any_is_list(addr: *const Dynamic) -> bool {
    !addr.is_null() && unsafe { (*addr).is_list() }
}

extern "C" fn any_is_string(addr: *const Dynamic) -> bool {
    !addr.is_null() && unsafe { (*addr).is_str() }
}

extern "C" fn any_is_null(addr: *const Dynamic) -> bool {
    addr.is_null() || unsafe { (*addr).is_null() }
}

extern "C" fn random(start: *const Dynamic, stop: *const Dynamic) -> *const Dynamic {
    if !start.is_null() && !stop.is_null() {
        let mut rng = rand::rng();
        unsafe {
            if (&*start).is_int() {
                let start = (*start).as_int().unwrap_or(0);
                let stop = (*stop).as_int().unwrap_or(100);
                return alloc_dynamic(Dynamic::I64(rng.random_range(start..stop)));
            } else if (&*start).is_f32() || (&*start).is_f64() {
                let start = (*start).as_float().unwrap_or(0.0);
                let stop = (*stop).as_float().unwrap_or(1.0);
                return alloc_dynamic(Dynamic::F64(rng.random_range(start..stop)));
            }
        }
    }
    alloc_dynamic(Dynamic::Null)
}

extern "C" fn uuid() -> *const Dynamic {
    alloc_dynamic(uuid::Uuid::new_v4().to_string().into())
}

pub(crate) extern "C" fn struct_alloc(size: i64) -> *mut u8 {
    let size = size.max(0) as usize;
    let ptr = alloc_struct_bytes(size);
    unsafe {
        std::ptr::write_bytes(ptr, 0, size);
    }
    ptr
}

pub(crate) extern "C" fn repeat_fill(dst: *mut u8, pattern: u64, width: i64, len: i64) {
    if dst.is_null() || width <= 0 || len <= 0 {
        return;
    }
    let width = width as usize;
    let len = len as usize;
    let bytes = pattern.to_le_bytes();
    unsafe {
        if width == 1 {
            std::ptr::write_bytes(dst, bytes[0], len);
            return;
        }
        if !matches!(width, 2 | 4 | 8) {
            return;
        }
        for idx in 0..len {
            std::ptr::copy_nonoverlapping(bytes.as_ptr(), dst.add(idx * width), width);
        }
    }
}

pub(crate) extern "C" fn strcat(left: *const Dynamic, right: *const Dynamic) -> *const Dynamic {
    let left = if left.is_null() { "" } else { unsafe { (&*left).as_str() } };
    let right = if right.is_null() { "" } else { unsafe { (&*right).as_str() } };
    let mut out = String::with_capacity(left.len() + right.len());
    out.push_str(left);
    out.push_str(right);
    alloc_dynamic(Dynamic::StringBuf(out))
}

pub(crate) extern "C" fn strcat_i64(left: *const Dynamic, right: i64) -> *const Dynamic {
    let left = if left.is_null() { "" } else { unsafe { (&*left).as_str() } };
    let mut out = String::with_capacity(left.len() + 20);
    out.push_str(left);
    let _ = write!(&mut out, "{right}");
    alloc_dynamic(Dynamic::StringBuf(out))
}

pub(crate) extern "C" fn strcat_assign(left: *mut Dynamic, right: *const Dynamic) -> *const Dynamic {
    if left.is_null() {
        return strcat(left, right);
    }
    let suffix = if right.is_null() {
        Cow::Borrowed("")
    } else if std::ptr::eq(left as *const Dynamic, right) {
        Cow::Owned(unsafe { (&*right).to_string() })
    } else {
        let right = unsafe { &*right };
        if right.is_str() { Cow::Borrowed(right.as_str()) } else { Cow::Owned(right.to_string()) }
    };
    unsafe {
        match &mut *left {
            Dynamic::StringBuf(text) => text.push_str(suffix.as_ref()),
            Dynamic::String(text) => {
                let mut out = String::with_capacity(text.len() + suffix.len());
                out.push_str(text.as_str());
                out.push_str(suffix.as_ref());
                *left = Dynamic::StringBuf(out);
            }
            value => {
                let prefix = value.to_string();
                let mut out = String::with_capacity(prefix.len() + suffix.len());
                out.push_str(&prefix);
                out.push_str(suffix.as_ref());
                *value = Dynamic::StringBuf(out);
            }
        }
    }
    left
}

pub(crate) extern "C" fn struct_from_ptr(addr: i64, ty: i64) -> *const Dynamic {
    let ty = unsafe { (&*(ty as *const Type)).clone() };
    alloc_dynamic(Dynamic::Struct { addr: addr as usize, ty })
}

pub(crate) extern "C" fn array_from_ptr(addr: i64, ty: i64) -> *const Dynamic {
    if addr == 0 || ty == 0 {
        return alloc_dynamic(Dynamic::Null);
    }
    let ty = unsafe { &*(ty as *const Type) };
    alloc_dynamic(dynamic_from_ptr(addr as *const u8, ty))
}

pub(crate) extern "C" fn array_to_ptr(dst: *mut u8, value: *const Dynamic, ty: i64) {
    if dst.is_null() || value.is_null() || ty == 0 {
        return;
    }
    let ty = unsafe { &*(ty as *const Type) };
    write_dynamic_to_ptr(dst, unsafe { &*value }, ty);
}

fn dynamic_from_ptr(addr: *const u8, ty: &Type) -> Dynamic {
    if addr.is_null() {
        return Dynamic::Null;
    }
    match ty {
        Type::Bool => Dynamic::Bool(unsafe { std::ptr::read_unaligned(addr) } != 0),
        Type::I8 => Dynamic::I8(unsafe { std::ptr::read_unaligned(addr as *const i8) }),
        Type::U8 => Dynamic::U8(unsafe { std::ptr::read_unaligned(addr) }),
        Type::I16 => Dynamic::I16(unsafe { std::ptr::read_unaligned(addr as *const i16) }),
        Type::U16 => Dynamic::U16(unsafe { std::ptr::read_unaligned(addr as *const u16) }),
        Type::I32 => Dynamic::I32(unsafe { std::ptr::read_unaligned(addr as *const i32) }),
        Type::U32 => Dynamic::U32(unsafe { std::ptr::read_unaligned(addr as *const u32) }),
        Type::I64 => Dynamic::I64(unsafe { std::ptr::read_unaligned(addr as *const i64) }),
        Type::U64 => Dynamic::U64(unsafe { std::ptr::read_unaligned(addr as *const u64) }),
        Type::F32 => Dynamic::F32(unsafe { std::ptr::read_unaligned(addr as *const f32) }),
        Type::F64 => Dynamic::F64(unsafe { std::ptr::read_unaligned(addr as *const f64) }),
        Type::Array(elem_ty, len) => {
            let width = elem_ty.storage_width() as usize;
            let values = (0..*len as usize).map(|idx| unsafe { dynamic_from_ptr(addr.add(idx * width), elem_ty) }).collect();
            Dynamic::list(values)
        }
        Type::Struct { fields, .. } => {
            let mut map = BTreeMap::new();
            let (_, offsets) = Type::struct_layout(fields);
            for ((name, field_ty), offset) in fields.iter().zip(offsets) {
                let value = unsafe { dynamic_from_ptr(addr.add(offset as usize), field_ty) };
                map.insert(name.clone(), value);
            }
            Dynamic::map(map)
        }
        _ => {
            let ptr = unsafe { std::ptr::read_unaligned(addr as *const *const Dynamic) };
            if ptr.is_null() { Dynamic::Null } else { unsafe { (&*ptr).deep_clone() } }
        }
    }
}

fn write_dynamic_to_ptr(dst: *mut u8, value: &Dynamic, ty: &Type) {
    if dst.is_null() {
        return;
    }
    match ty {
        Type::Bool => unsafe { std::ptr::write_unaligned(dst, if value.is_true() { 1 } else { 0 }) },
        Type::I8 => unsafe { std::ptr::write_unaligned(dst as *mut i8, value.clone().try_into().unwrap_or_default()) },
        Type::U8 => unsafe { std::ptr::write_unaligned(dst, value.clone().try_into().unwrap_or_default()) },
        Type::I16 => unsafe { std::ptr::write_unaligned(dst as *mut i16, value.clone().try_into().unwrap_or_default()) },
        Type::U16 => unsafe { std::ptr::write_unaligned(dst as *mut u16, value.clone().try_into().unwrap_or_default()) },
        Type::I32 => unsafe { std::ptr::write_unaligned(dst as *mut i32, value.clone().try_into().unwrap_or_default()) },
        Type::U32 => unsafe { std::ptr::write_unaligned(dst as *mut u32, value.clone().try_into().unwrap_or_default()) },
        Type::I64 => unsafe { std::ptr::write_unaligned(dst as *mut i64, value.clone().try_into().unwrap_or_default()) },
        Type::U64 => unsafe { std::ptr::write_unaligned(dst as *mut u64, value.clone().try_into().unwrap_or_default()) },
        Type::F32 => unsafe { std::ptr::write_unaligned(dst as *mut f32, f32::try_from(value.clone()).unwrap_or_default()) },
        Type::F64 => unsafe { std::ptr::write_unaligned(dst as *mut f64, value.clone().try_into().unwrap_or_default()) },
        Type::Array(elem_ty, len) => {
            let width = elem_ty.storage_width() as usize;
            for idx in 0..*len as usize {
                let item = value.get_idx(idx).unwrap_or(Dynamic::Null);
                unsafe { write_dynamic_to_ptr(dst.add(idx * width), &item, elem_ty) };
            }
        }
        Type::Struct { fields, .. } => {
            let (_, offsets) = Type::struct_layout(fields);
            for ((name, field_ty), offset) in fields.iter().zip(offsets) {
                let item = value.get_dynamic(name.as_str()).unwrap_or(Dynamic::Null);
                unsafe { write_dynamic_to_ptr(dst.add(offset as usize), &item, field_ty) };
            }
        }
        _ => {
            let ptr = alloc_dynamic(value.deep_clone());
            unsafe { std::ptr::write_unaligned(dst as *mut usize, ptr as usize) };
        }
    }
}

pub(crate) extern "C" fn import_with_vm(context: *const Weak<RwLock<JITRunTime>>, addr: *const Dynamic, path: *const Dynamic) -> bool {
    if addr.is_null() || path.is_null() {
        return false;
    }
    super::with_vm_context(context, |jit| {
        let name = unsafe { &*addr }.as_str();
        let path = unsafe { &*path }.as_str();
        jit.import(name, path)
    }).map_err(|e| log::error!("import failed: {e:#}")).is_ok()
}

pub(crate) extern "C" fn spawn_with_vm(context: *const Weak<RwLock<JITRunTime>>, fn_name: *const Dynamic, args: *const Dynamic) -> bool {
    if context.is_null() || fn_name.is_null() {
        return false;
    }
    let fn_name = unsafe { (&*fn_name).as_str().to_string() };
    if fn_name.is_empty() {
        return false;
    }
    let args = if args.is_null() { Dynamic::Null } else { unsafe { (&*args).deep_clone() } };
    let context = unsafe { (&*context).clone() };
    std::thread::Builder::new()
        .name(format!("zust:{fn_name}"))
        .spawn(move || {
            if let Err(err) = spawn_run(context, fn_name.as_str(), args) {
                log::error!("spawn {fn_name} failed: {err:?}");
            }
        })
        .is_ok()
}

fn spawn_args(args: Dynamic) -> Vec<Dynamic> {
    match args {
        Dynamic::Null => Vec::new(),
        Dynamic::List(values) => values.read().unwrap().iter().map(Dynamic::deep_clone).collect(),
        value => vec![value],
    }
}

fn spawn_run(context: Weak<RwLock<JITRunTime>>, fn_name: &str, args: Dynamic) -> Result<()> {
    let args = spawn_args(args);
    if args.len() > 16 {
        anyhow::bail!("spawn supports at most 16 args, got {}", args.len());
    }
    let arg_tys = vec![Type::Any; args.len()];
    let (ptr, ret_ty) = super::with_vm_context(&context as *const Weak<RwLock<JITRunTime>>, |vm| vm.jit.write().unwrap().get_fn_ptr(fn_name, &arg_tys))?;
    let args: Vec<Box<Dynamic>> = args.into_iter().map(Box::new).collect();
    let ptrs: Vec<*const Dynamic> = args.iter().map(|arg| arg.as_ref() as *const Dynamic).collect();
    unsafe {
        call_spawned(ptr, &ret_ty, &ptrs)?;
    }
    Ok(())
}

pub(crate) extern "C" fn spawn_ptr(fn_ptr: i64, ret_ty: i64, args: *const Dynamic) -> bool {
    if fn_ptr == 0 || ret_ty == 0 {
        return false;
    }
    let fn_ptr = fn_ptr as usize;
    let ret_ty = unsafe { (&*(ret_ty as *const Type)).clone() };
    let args = if args.is_null() { Dynamic::Null } else { unsafe { (&*args).deep_clone() } };
    std::thread::Builder::new()
        .name("zust:closure".to_string())
        .spawn(move || {
            if let Err(err) = spawn_run_ptr(fn_ptr, ret_ty, args) {
                log::error!("spawn closure failed: {err:?}");
            }
        })
        .is_ok()
}

pub(crate) extern "C" fn callback_new(fn_ptr: i64, ret_ty: i64, explicit_arg_len: i64, captures: *const Dynamic) -> *const Dynamic {
    if fn_ptr == 0 || ret_ty == 0 {
        return alloc_dynamic(Dynamic::Null);
    }
    let ret_ty = unsafe { (&*(ret_ty as *const Type)).clone() };
    let explicit_arg_len = usize::try_from(explicit_arg_len).unwrap_or(usize::MAX);
    let captures = if captures.is_null() {
        Vec::new()
    } else {
        match unsafe { &*captures } {
            Dynamic::List(values) => values.read().unwrap().iter().map(Dynamic::deep_clone).collect(),
            value => vec![value.deep_clone()],
        }
    };
    alloc_dynamic(Dynamic::custom(ZustCallback::new_with_arg_len(fn_ptr as usize, ret_ty, explicit_arg_len, captures)))
}

fn spawn_run_ptr(fn_ptr: usize, ret_ty: Type, args: Dynamic) -> Result<()> {
    let args = spawn_args(args);
    if args.len() > 16 {
        anyhow::bail!("spawn supports at most 16 args, got {}", args.len());
    }
    let args: Vec<Box<Dynamic>> = args.into_iter().map(Box::new).collect();
    let ptrs: Vec<*const Dynamic> = args.iter().map(|arg| arg.as_ref() as *const Dynamic).collect();
    unsafe {
        call_spawned(fn_ptr as *const u8, &ret_ty, &ptrs)?;
    }
    Ok(())
}

unsafe fn call_callback(ptr: *const u8, ret_ty: &Type, args: &[*const Dynamic]) -> Result<Dynamic> {
    macro_rules! callback_arg_ty {
        ($arg:ident) => {
            *const Dynamic
        };
    }

    macro_rules! callback_args {
        ($body:ident $(, $extra:tt)*) => {
            match args {
                [] => $body!($($extra),*;),
                [a] => $body!($($extra),*; a),
                [a, b] => $body!($($extra),*; a, b),
                [a, b, c] => $body!($($extra),*; a, b, c),
                [a, b, c, d] => $body!($($extra),*; a, b, c, d),
                [a, b, c, d, e] => $body!($($extra),*; a, b, c, d, e),
                [a, b, c, d, e, f] => $body!($($extra),*; a, b, c, d, e, f),
                [a, b, c, d, e, f, g] => $body!($($extra),*; a, b, c, d, e, f, g),
                [a, b, c, d, e, f, g, h] => $body!($($extra),*; a, b, c, d, e, f, g, h),
                [a, b, c, d, e, f, g, h, i] => $body!($($extra),*; a, b, c, d, e, f, g, h, i),
                [a, b, c, d, e, f, g, h, i, j] => $body!($($extra),*; a, b, c, d, e, f, g, h, i, j),
                [a, b, c, d, e, f, g, h, i, j, k] => $body!($($extra),*; a, b, c, d, e, f, g, h, i, j, k),
                [a, b, c, d, e, f, g, h, i, j, k, l] => $body!($($extra),*; a, b, c, d, e, f, g, h, i, j, k, l),
                [a, b, c, d, e, f, g, h, i, j, k, l, m] => $body!($($extra),*; a, b, c, d, e, f, g, h, i, j, k, l, m),
                [a, b, c, d, e, f, g, h, i, j, k, l, m, n] => $body!($($extra),*; a, b, c, d, e, f, g, h, i, j, k, l, m, n),
                [a, b, c, d, e, f, g, h, i, j, k, l, m, n, o] => $body!($($extra),*; a, b, c, d, e, f, g, h, i, j, k, l, m, n, o),
                [a, b, c, d, e, f, g, h, i, j, k, l, m, n, o, p] => $body!($($extra),*; a, b, c, d, e, f, g, h, i, j, k, l, m, n, o, p),
                [a, b, c, d, e, f, g, h, i, j, k, l, m, n, o, p, q] => $body!($($extra),*; a, b, c, d, e, f, g, h, i, j, k, l, m, n, o, p, q),
                [a, b, c, d, e, f, g, h, i, j, k, l, m, n, o, p, q, r] => $body!($($extra),*; a, b, c, d, e, f, g, h, i, j, k, l, m, n, o, p, q, r),
                [a, b, c, d, e, f, g, h, i, j, k, l, m, n, o, p, q, r, s] => $body!($($extra),*; a, b, c, d, e, f, g, h, i, j, k, l, m, n, o, p, q, r, s),
                [a, b, c, d, e, f, g, h, i, j, k, l, m, n, o, p, q, r, s, t] => $body!($($extra),*; a, b, c, d, e, f, g, h, i, j, k, l, m, n, o, p, q, r, s, t),
                [a, b, c, d, e, f, g, h, i, j, k, l, m, n, o, p, q, r, s, t, u] => $body!($($extra),*; a, b, c, d, e, f, g, h, i, j, k, l, m, n, o, p, q, r, s, t, u),
                [a, b, c, d, e, f, g, h, i, j, k, l, m, n, o, p, q, r, s, t, u, v] => $body!($($extra),*; a, b, c, d, e, f, g, h, i, j, k, l, m, n, o, p, q, r, s, t, u, v),
                [a, b, c, d, e, f, g, h, i, j, k, l, m, n, o, p, q, r, s, t, u, v, w] => $body!($($extra),*; a, b, c, d, e, f, g, h, i, j, k, l, m, n, o, p, q, r, s, t, u, v, w),
                [a, b, c, d, e, f, g, h, i, j, k, l, m, n, o, p, q, r, s, t, u, v, w, x] => $body!($($extra),*; a, b, c, d, e, f, g, h, i, j, k, l, m, n, o, p, q, r, s, t, u, v, w, x),
                _ => anyhow::bail!("callback supports at most 24 args including captures, got {}", args.len()),
            }
        };
    }

    macro_rules! call_void_body {
        (; $($arg:ident),*) => {{
            let fn_ptr: extern "C" fn($(callback_arg_ty!($arg)),*) = unsafe { std::mem::transmute(ptr) };
            fn_ptr($(*$arg),*)
        }};
    }

    macro_rules! call_void {
        () => {
            callback_args!(call_void_body)
        };
    }

    macro_rules! call_ret_body {
        ($ret:ty, $dynamic:expr; $($arg:ident),*) => {{
            let fn_ptr: extern "C" fn($(callback_arg_ty!($arg)),*) -> $ret = unsafe { std::mem::transmute(ptr) };
            $dynamic(fn_ptr($(*$arg),*))
        }};
    }

    macro_rules! call_ret {
        ($ret:ty, $dynamic:expr) => {{ callback_args!(call_ret_body, $ret, $dynamic) }};
    }

    if ret_ty.is_void() {
        call_void!();
        Ok(Dynamic::Null)
    } else if ret_ty.is_bool() {
        call_ret!(bool, |value| Ok(Dynamic::Bool(value)))
    } else if ret_ty.is_float() {
        if ret_ty.is_f64() { call_ret!(f64, |value| Ok(Dynamic::F64(value))) } else { call_ret!(f32, |value| Ok(Dynamic::F32(value))) }
    } else if ret_ty.is_int() || ret_ty.is_uint() {
        match ret_ty {
            Type::I8 => call_ret!(i8, |value| Ok(Dynamic::I8(value))),
            Type::U8 => call_ret!(u8, |value| Ok(Dynamic::U8(value))),
            Type::I16 => call_ret!(i16, |value| Ok(Dynamic::I16(value))),
            Type::U16 => call_ret!(u16, |value| Ok(Dynamic::U16(value))),
            Type::I32 => call_ret!(i32, |value| Ok(Dynamic::I32(value))),
            Type::U32 => call_ret!(u32, |value| Ok(Dynamic::U32(value))),
            Type::I64 => call_ret!(i64, |value| Ok(Dynamic::I64(value))),
            Type::U64 => call_ret!(u64, |value| Ok(Dynamic::U64(value))),
            _ => unreachable!(),
        }
    } else if ret_ty.is_struct() || ret_ty.is_array() || ret_ty.is_vec() {
        log::warn!("callback returns {ret_ty:?} — not supported, discarding");
        call_ret!(*const u8, |_| Ok(Dynamic::Null))
    } else {
        call_ret!(*const Dynamic, |ptr| unsafe { Ok((*take_dynamic_return(ptr)).deep_clone()) })
    }
}

unsafe fn call_spawned(ptr: *const u8, ret_ty: &Type, args: &[*const Dynamic]) -> Result<()> {
    macro_rules! call_void {
        () => {
            match args {
                [] => unsafe { std::mem::transmute::<_, extern "C" fn()>(ptr)() },
                [a] => unsafe { std::mem::transmute::<_, extern "C" fn(*const Dynamic)>(ptr)(*a) },
                [a, b] => unsafe { std::mem::transmute::<_, extern "C" fn(*const Dynamic, *const Dynamic)>(ptr)(*a, *b) },
                [a, b, c] => unsafe { std::mem::transmute::<_, extern "C" fn(*const Dynamic, *const Dynamic, *const Dynamic)>(ptr)(*a, *b, *c) },
                [a, b, c, d] => unsafe { std::mem::transmute::<_, extern "C" fn(*const Dynamic, *const Dynamic, *const Dynamic, *const Dynamic)>(ptr)(*a, *b, *c, *d) },
                [a, b, c, d, e] => unsafe { std::mem::transmute::<_, extern "C" fn(*const Dynamic, *const Dynamic, *const Dynamic, *const Dynamic, *const Dynamic)>(ptr)(*a, *b, *c, *d, *e) },
                [a, b, c, d, e, f] => unsafe { std::mem::transmute::<_, extern "C" fn(*const Dynamic, *const Dynamic, *const Dynamic, *const Dynamic, *const Dynamic, *const Dynamic)>(ptr)(*a, *b, *c, *d, *e, *f) },
                [a, b, c, d, e, f, g] => unsafe {
                    std::mem::transmute::<_, extern "C" fn(*const Dynamic, *const Dynamic, *const Dynamic, *const Dynamic, *const Dynamic, *const Dynamic, *const Dynamic)>(ptr)(*a, *b, *c, *d, *e, *f, *g)
                },
                [a, b, c, d, e, f, g, h] => unsafe {
                    std::mem::transmute::<_, extern "C" fn(*const Dynamic, *const Dynamic, *const Dynamic, *const Dynamic, *const Dynamic, *const Dynamic, *const Dynamic, *const Dynamic)>(ptr)(
                        *a, *b, *c, *d, *e, *f, *g, *h,
                    )
                },
                [a, b, c, d, e, f, g, h, i] => unsafe {
                    std::mem::transmute::<_, extern "C" fn(*const Dynamic, *const Dynamic, *const Dynamic, *const Dynamic, *const Dynamic, *const Dynamic, *const Dynamic, *const Dynamic, *const Dynamic)>(ptr)(
                        *a, *b, *c, *d, *e, *f, *g, *h, *i,
                    )
                },
                [a, b, c, d, e, f, g, h, i, j] => unsafe {
                    std::mem::transmute::<_, extern "C" fn(*const Dynamic, *const Dynamic, *const Dynamic, *const Dynamic, *const Dynamic, *const Dynamic, *const Dynamic, *const Dynamic, *const Dynamic, *const Dynamic)>(ptr)(
                        *a, *b, *c, *d, *e, *f, *g, *h, *i, *j,
                    )
                },
                [a, b, c, d, e, f, g, h, i, j, k] => unsafe {
                    std::mem::transmute::<_, extern "C" fn(*const Dynamic, *const Dynamic, *const Dynamic, *const Dynamic, *const Dynamic, *const Dynamic, *const Dynamic, *const Dynamic, *const Dynamic, *const Dynamic, *const Dynamic)>(ptr)(
                        *a, *b, *c, *d, *e, *f, *g, *h, *i, *j, *k,
                    )
                },
                [a, b, c, d, e, f, g, h, i, j, k, l] => unsafe {
                    std::mem::transmute::<_, extern "C" fn(*const Dynamic, *const Dynamic, *const Dynamic, *const Dynamic, *const Dynamic, *const Dynamic, *const Dynamic, *const Dynamic, *const Dynamic, *const Dynamic, *const Dynamic, *const Dynamic)>(ptr)(
                        *a, *b, *c, *d, *e, *f, *g, *h, *i, *j, *k, *l,
                    )
                },
                [a, b, c, d, e, f, g, h, i, j, k, l, m] => unsafe {
                    std::mem::transmute::<_, extern "C" fn(*const Dynamic, *const Dynamic, *const Dynamic, *const Dynamic, *const Dynamic, *const Dynamic, *const Dynamic, *const Dynamic, *const Dynamic, *const Dynamic, *const Dynamic, *const Dynamic, *const Dynamic)>(ptr)(
                        *a, *b, *c, *d, *e, *f, *g, *h, *i, *j, *k, *l, *m,
                    )
                },
                [a, b, c, d, e, f, g, h, i, j, k, l, m, n] => unsafe {
                    std::mem::transmute::<_, extern "C" fn(*const Dynamic, *const Dynamic, *const Dynamic, *const Dynamic, *const Dynamic, *const Dynamic, *const Dynamic, *const Dynamic, *const Dynamic, *const Dynamic, *const Dynamic, *const Dynamic, *const Dynamic, *const Dynamic)>(ptr)(
                        *a, *b, *c, *d, *e, *f, *g, *h, *i, *j, *k, *l, *m, *n,
                    )
                },
                [a, b, c, d, e, f, g, h, i, j, k, l, m, n, o] => unsafe {
                    std::mem::transmute::<_, extern "C" fn(*const Dynamic, *const Dynamic, *const Dynamic, *const Dynamic, *const Dynamic, *const Dynamic, *const Dynamic, *const Dynamic, *const Dynamic, *const Dynamic, *const Dynamic, *const Dynamic, *const Dynamic, *const Dynamic, *const Dynamic)>(ptr)(
                        *a, *b, *c, *d, *e, *f, *g, *h, *i, *j, *k, *l, *m, *n, *o,
                    )
                },
                [a, b, c, d, e, f, g, h, i, j, k, l, m, n, o, p] => unsafe {
                    std::mem::transmute::<_, extern "C" fn(*const Dynamic, *const Dynamic, *const Dynamic, *const Dynamic, *const Dynamic, *const Dynamic, *const Dynamic, *const Dynamic, *const Dynamic, *const Dynamic, *const Dynamic, *const Dynamic, *const Dynamic, *const Dynamic, *const Dynamic, *const Dynamic)>(ptr)(
                        *a, *b, *c, *d, *e, *f, *g, *h, *i, *j, *k, *l, *m, *n, *o, *p,
                    )
                },
                _ => unreachable!(),
            }
        };
    }

    if ret_ty.is_void() {
        call_void!();
        return Ok(());
    }

    macro_rules! call_ret {
        ($ret:ty, $drop_result:expr) => {
            match args {
                [] => {
                    let fn_ptr: extern "C" fn() -> $ret = unsafe { std::mem::transmute(ptr) };
                    $drop_result(fn_ptr());
                }
                [a] => {
                    let fn_ptr: extern "C" fn(*const Dynamic) -> $ret = unsafe { std::mem::transmute(ptr) };
                    $drop_result(fn_ptr(*a));
                }
                [a, b] => {
                    let fn_ptr: extern "C" fn(*const Dynamic, *const Dynamic) -> $ret = unsafe { std::mem::transmute(ptr) };
                    $drop_result(fn_ptr(*a, *b));
                }
                [a, b, c] => {
                    let fn_ptr: extern "C" fn(*const Dynamic, *const Dynamic, *const Dynamic) -> $ret = unsafe { std::mem::transmute(ptr) };
                    $drop_result(fn_ptr(*a, *b, *c));
                }
                [a, b, c, d] => {
                    let fn_ptr: extern "C" fn(*const Dynamic, *const Dynamic, *const Dynamic, *const Dynamic) -> $ret = unsafe { std::mem::transmute(ptr) };
                    $drop_result(fn_ptr(*a, *b, *c, *d));
                }
                [a, b, c, d, e] => {
                    let fn_ptr: extern "C" fn(*const Dynamic, *const Dynamic, *const Dynamic, *const Dynamic, *const Dynamic) -> $ret = unsafe { std::mem::transmute(ptr) };
                    $drop_result(fn_ptr(*a, *b, *c, *d, *e));
                }
                [a, b, c, d, e, f] => {
                    let fn_ptr: extern "C" fn(*const Dynamic, *const Dynamic, *const Dynamic, *const Dynamic, *const Dynamic, *const Dynamic) -> $ret = unsafe { std::mem::transmute(ptr) };
                    $drop_result(fn_ptr(*a, *b, *c, *d, *e, *f));
                }
                [a, b, c, d, e, f, g] => {
                    let fn_ptr: extern "C" fn(*const Dynamic, *const Dynamic, *const Dynamic, *const Dynamic, *const Dynamic, *const Dynamic, *const Dynamic) -> $ret = unsafe { std::mem::transmute(ptr) };
                    $drop_result(fn_ptr(*a, *b, *c, *d, *e, *f, *g));
                }
                [a, b, c, d, e, f, g, h] => {
                    let fn_ptr: extern "C" fn(*const Dynamic, *const Dynamic, *const Dynamic, *const Dynamic, *const Dynamic, *const Dynamic, *const Dynamic, *const Dynamic) -> $ret = unsafe { std::mem::transmute(ptr) };
                    $drop_result(fn_ptr(*a, *b, *c, *d, *e, *f, *g, *h));
                }
                [a, b, c, d, e, f, g, h, i] => {
                    let fn_ptr: extern "C" fn(*const Dynamic, *const Dynamic, *const Dynamic, *const Dynamic, *const Dynamic, *const Dynamic, *const Dynamic, *const Dynamic, *const Dynamic) -> $ret = unsafe { std::mem::transmute(ptr) };
                    $drop_result(fn_ptr(*a, *b, *c, *d, *e, *f, *g, *h, *i));
                }
                [a, b, c, d, e, f, g, h, i, j] => {
                    let fn_ptr: extern "C" fn(*const Dynamic, *const Dynamic, *const Dynamic, *const Dynamic, *const Dynamic, *const Dynamic, *const Dynamic, *const Dynamic, *const Dynamic, *const Dynamic) -> $ret = unsafe { std::mem::transmute(ptr) };
                    $drop_result(fn_ptr(*a, *b, *c, *d, *e, *f, *g, *h, *i, *j));
                }
                [a, b, c, d, e, f, g, h, i, j, k] => {
                    let fn_ptr: extern "C" fn(*const Dynamic, *const Dynamic, *const Dynamic, *const Dynamic, *const Dynamic, *const Dynamic, *const Dynamic, *const Dynamic, *const Dynamic, *const Dynamic, *const Dynamic) -> $ret = unsafe { std::mem::transmute(ptr) };
                    $drop_result(fn_ptr(*a, *b, *c, *d, *e, *f, *g, *h, *i, *j, *k));
                }
                [a, b, c, d, e, f, g, h, i, j, k, l] => {
                    let fn_ptr: extern "C" fn(*const Dynamic, *const Dynamic, *const Dynamic, *const Dynamic, *const Dynamic, *const Dynamic, *const Dynamic, *const Dynamic, *const Dynamic, *const Dynamic, *const Dynamic, *const Dynamic) -> $ret = unsafe { std::mem::transmute(ptr) };
                    $drop_result(fn_ptr(*a, *b, *c, *d, *e, *f, *g, *h, *i, *j, *k, *l));
                }
                [a, b, c, d, e, f, g, h, i, j, k, l, m] => {
                    let fn_ptr: extern "C" fn(*const Dynamic, *const Dynamic, *const Dynamic, *const Dynamic, *const Dynamic, *const Dynamic, *const Dynamic, *const Dynamic, *const Dynamic, *const Dynamic, *const Dynamic, *const Dynamic, *const Dynamic) -> $ret = unsafe { std::mem::transmute(ptr) };
                    $drop_result(fn_ptr(*a, *b, *c, *d, *e, *f, *g, *h, *i, *j, *k, *l, *m));
                }
                [a, b, c, d, e, f, g, h, i, j, k, l, m, n] => {
                    let fn_ptr: extern "C" fn(*const Dynamic, *const Dynamic, *const Dynamic, *const Dynamic, *const Dynamic, *const Dynamic, *const Dynamic, *const Dynamic, *const Dynamic, *const Dynamic, *const Dynamic, *const Dynamic, *const Dynamic, *const Dynamic) -> $ret = unsafe { std::mem::transmute(ptr) };
                    $drop_result(fn_ptr(*a, *b, *c, *d, *e, *f, *g, *h, *i, *j, *k, *l, *m, *n));
                }
                [a, b, c, d, e, f, g, h, i, j, k, l, m, n, o] => {
                    let fn_ptr: extern "C" fn(*const Dynamic, *const Dynamic, *const Dynamic, *const Dynamic, *const Dynamic, *const Dynamic, *const Dynamic, *const Dynamic, *const Dynamic, *const Dynamic, *const Dynamic, *const Dynamic, *const Dynamic, *const Dynamic, *const Dynamic) -> $ret = unsafe { std::mem::transmute(ptr) };
                    $drop_result(fn_ptr(*a, *b, *c, *d, *e, *f, *g, *h, *i, *j, *k, *l, *m, *n, *o));
                }
                [a, b, c, d, e, f, g, h, i, j, k, l, m, n, o, p] => {
                    let fn_ptr: extern "C" fn(*const Dynamic, *const Dynamic, *const Dynamic, *const Dynamic, *const Dynamic, *const Dynamic, *const Dynamic, *const Dynamic, *const Dynamic, *const Dynamic, *const Dynamic, *const Dynamic, *const Dynamic, *const Dynamic, *const Dynamic, *const Dynamic) -> $ret = unsafe { std::mem::transmute(ptr) };
                    $drop_result(fn_ptr(*a, *b, *c, *d, *e, *f, *g, *h, *i, *j, *k, *l, *m, *n, *o, *p));
                }
                _ => unreachable!(),
            }
        };
    }

    if ret_ty.is_bool() {
        call_ret!(bool, |_| {});
    } else if ret_ty.is_float() {
        if ret_ty.is_f64() {
            call_ret!(f64, |_| {});
        } else {
            call_ret!(f32, |_| {});
        }
    } else if ret_ty.is_int() || ret_ty.is_uint() {
        match ret_ty {
            Type::I8 => call_ret!(i8, |_| {}),
            Type::U8 => call_ret!(u8, |_| {}),
            Type::I16 => call_ret!(i16, |_| {}),
            Type::U16 => call_ret!(u16, |_| {}),
            Type::I32 => call_ret!(i32, |_| {}),
            Type::U32 => call_ret!(u32, |_| {}),
            Type::I64 => call_ret!(i64, |_| {}),
            Type::U64 => call_ret!(u64, |_| {}),
            _ => unreachable!(),
        }
    } else if ret_ty.is_struct() || ret_ty.is_array() || ret_ty.is_vec() {
        log::warn!("spawned fn returns {ret_ty:?} — not supported, discarding");
        call_ret!(*const u8, |_| {});
    } else {
        call_ret!(*const Dynamic, |ptr| drop(unsafe { take_dynamic_return(ptr) }));
    }
    Ok(())
}

extern "C" fn any_len(addr: *const Dynamic) -> i64 {
    if addr.is_null() { 0 } else { unsafe { (&*addr).len() as i64 } }
}

extern "C" fn any_keys(addr: *const Dynamic) -> *const Dynamic {
    if addr.is_null() {
        return alloc_dynamic(Dynamic::list(Vec::new()));
    }
    let keys = match unsafe { &*addr } {
        Dynamic::Map(map) => map.read().unwrap().keys().map(|key| Dynamic::from(key.as_str())).collect(),
        _ => Vec::new(),
    };
    alloc_dynamic(Dynamic::list(keys))
}

extern "C" fn any_iter(addr: *const Dynamic) -> *const Dynamic {
    if addr.is_null() { any_null() } else { alloc_dynamic(unsafe { (*addr).clone().into_iter() }) }
}

extern "C" fn any_next(addr: *mut Dynamic) -> *const Dynamic {
    alloc_dynamic(unsafe { (*addr).next().unwrap_or(Dynamic::Null) })
}

extern "C" fn any_next_pair(addr: *mut Dynamic) -> *const Dynamic {
    alloc_dynamic(unsafe { (*addr).next_pair().unwrap_or(Dynamic::Null) })
}

extern "C" fn any_push(addr: *mut Dynamic, value: *mut Dynamic) {
    if !addr.is_null() && !value.is_null() {
        unsafe {
            (&mut *addr).push_dynamic((&*value).clone());
        }
    }
}

extern "C" fn any_pop(addr: *mut Dynamic) -> *const Dynamic {
    if addr.is_null() { any_null() } else { alloc_dynamic(unsafe { (*addr).pop().unwrap_or(Dynamic::Null) }) }
}

extern "C" fn get_key(addr: *const Dynamic, key: *const Dynamic) -> *const Dynamic {
    if addr.is_null() || key.is_null() {
        any_null()
    } else {
        let key: &str = unsafe { &*key }.as_str();
        alloc_dynamic(unsafe { (*addr).get_dynamic(key).unwrap_or(Dynamic::Null) })
    }
}

extern "C" fn del_key(addr: *const Dynamic, key: *const Dynamic) -> *const Dynamic {
    if addr.is_null() || key.is_null() {
        any_null()
    } else {
        let key: &str = unsafe { &*key }.as_str();
        alloc_dynamic(unsafe { (*addr).remove_dynamic(key).unwrap_or(Dynamic::Null) })
    }
}

extern "C" fn contains(addr: *const Dynamic, key: *const Dynamic) -> bool {
    if addr.is_null() || key.is_null() {
        false
    } else {
        let key: &str = unsafe { &*key }.as_str();
        unsafe { (*addr).contains(key) }
    }
}

extern "C" fn starts_with(addr: *const Dynamic, prefix: *const Dynamic) -> bool {
    if addr.is_null() || prefix.is_null() {
        false
    } else {
        let prefix: &str = unsafe { &*prefix }.as_str();
        unsafe { (*addr).starts_with(prefix) }
    }
}

extern "C" fn get_idx(addr: *const Dynamic, idx: i64) -> *const Dynamic {
    if addr.is_null() { any_null() } else { alloc_dynamic(unsafe { (*addr).get_idx(idx as usize).unwrap_or(Dynamic::Null) }) }
}

macro_rules! myvec_list_native {
    ($push:ident, $get_idx:ident, $set_idx:ident, $vec:ident, $dynamic:ident, $ty:ty, $fallback:expr) => {
        extern "C" fn $push(addr: *mut Dynamic, value: $ty) {
            if addr.is_null() {
                return;
            }
            unsafe {
                match &mut *addr {
                    Dynamic::$vec(values) => values.push(value),
                    list => {
                        list.push_dynamic(Dynamic::$dynamic(value));
                    }
                }
            }
        }

        extern "C" fn $get_idx(addr: *const Dynamic, idx: i64) -> $ty {
            if addr.is_null() {
                return <$ty>::default();
            }
            let Ok(idx) = usize::try_from(idx) else {
                return <$ty>::default();
            };
            unsafe {
                match &*addr {
                    Dynamic::$vec(values) => values.get(idx).unwrap_or_default(),
                    values => values.get_idx(idx).and_then($fallback).unwrap_or_default(),
                }
            }
        }

        extern "C" fn $set_idx(addr: *mut Dynamic, idx: i64, value: $ty) {
            if addr.is_null() {
                return;
            }
            let Ok(idx) = usize::try_from(idx) else {
                return;
            };
            unsafe {
                match &mut *addr {
                    Dynamic::$vec(values) => values.set(idx, value),
                    list => list.set_idx(idx, Dynamic::$dynamic(value)),
                }
            }
        }
    };
}

macro_rules! stdvec_list_native {
    ($push:ident, $get_idx:ident, $set_idx:ident, $vec:ident, $dynamic:ident, $ty:ty, $fallback:expr) => {
        extern "C" fn $push(addr: *mut Dynamic, value: $ty) {
            if addr.is_null() {
                return;
            }
            unsafe {
                match &mut *addr {
                    Dynamic::$vec(values) => values.push(value),
                    list => {
                        list.push_dynamic(Dynamic::$dynamic(value));
                    }
                }
            }
        }

        extern "C" fn $get_idx(addr: *const Dynamic, idx: i64) -> $ty {
            if addr.is_null() {
                return <$ty>::default();
            }
            let Ok(idx) = usize::try_from(idx) else {
                return <$ty>::default();
            };
            unsafe {
                match &*addr {
                    Dynamic::$vec(values) => values.get(idx).copied().unwrap_or_default(),
                    values => values.get_idx(idx).and_then($fallback).unwrap_or_default(),
                }
            }
        }

        extern "C" fn $set_idx(addr: *mut Dynamic, idx: i64, value: $ty) {
            if addr.is_null() {
                return;
            }
            let Ok(idx) = usize::try_from(idx) else {
                return;
            };
            unsafe {
                match &mut *addr {
                    Dynamic::$vec(values) => {
                        if let Some(slot) = values.get_mut(idx) {
                            *slot = value;
                        }
                    }
                    list => list.set_idx(idx, Dynamic::$dynamic(value)),
                }
            }
        }
    };
}

myvec_list_native!(list_i8_push, list_i8_get_idx, list_i8_set_idx, VecI8, I8, i8, |value: Dynamic| value.as_int().map(|value| value as i8));
myvec_list_native!(list_u16_push, list_u16_get_idx, list_u16_set_idx, VecU16, U16, u16, |value: Dynamic| value.as_uint().map(|value| value as u16));
myvec_list_native!(list_i16_push, list_i16_get_idx, list_i16_set_idx, VecI16, I16, i16, |value: Dynamic| value.as_int().map(|value| value as i16));
myvec_list_native!(list_u32_push, list_u32_get_idx, list_u32_set_idx, VecU32, U32, u32, |value: Dynamic| value.as_uint().map(|value| value as u32));
myvec_list_native!(list_i32_push, list_i32_get_idx, list_i32_set_idx, VecI32, I32, i32, |value: Dynamic| value.as_int().map(|value| value as i32));
myvec_list_native!(list_f32_push, list_f32_get_idx, list_f32_set_idx, VecF32, F32, f32, |value: Dynamic| value.as_float().map(|value| value as f32));
stdvec_list_native!(list_u64_push, list_u64_get_idx, list_u64_set_idx, VecU64, U64, u64, |value: Dynamic| value.as_uint());
stdvec_list_native!(list_i64_push, list_i64_get_idx, list_i64_set_idx, VecI64, I64, i64, |value: Dynamic| value.as_int());
stdvec_list_native!(list_f64_push, list_f64_get_idx, list_f64_set_idx, VecF64, F64, f64, |value: Dynamic| value.as_float());

extern "C" fn list_bool_push(addr: *mut Dynamic, value: bool) {
    if !addr.is_null() {
        unsafe {
            (&mut *addr).push_dynamic(Dynamic::Bool(value));
        }
    }
}

extern "C" fn list_bool_get_idx(addr: *const Dynamic, idx: i64) -> bool {
    if addr.is_null() {
        return false;
    }
    let Ok(idx) = usize::try_from(idx) else {
        return false;
    };
    unsafe { (&*addr).get_idx(idx).and_then(|value| value.as_bool()).unwrap_or(false) }
}

extern "C" fn list_bool_set_idx(addr: *mut Dynamic, idx: i64, value: bool) {
    if addr.is_null() {
        return;
    }
    let Ok(idx) = usize::try_from(idx) else {
        return;
    };
    unsafe {
        (&mut *addr).set_idx(idx, Dynamic::Bool(value));
    }
}

extern "C" fn list_u8_push(addr: *mut Dynamic, value: u8) {
    if !addr.is_null() {
        unsafe {
            (&mut *addr).push_dynamic(Dynamic::U8(value));
        }
    }
}

extern "C" fn list_u8_get_idx(addr: *const Dynamic, idx: i64) -> u8 {
    if addr.is_null() {
        return 0;
    }
    let Ok(idx) = usize::try_from(idx) else {
        return 0;
    };
    unsafe { (&*addr).get_idx(idx).and_then(|value| value.as_uint()).map(|value| value as u8).unwrap_or(0) }
}

extern "C" fn list_u8_set_idx(addr: *mut Dynamic, idx: i64, value: u8) {
    if addr.is_null() {
        return;
    }
    let Ok(idx) = usize::try_from(idx) else {
        return;
    };
    unsafe {
        (&mut *addr).set_idx(idx, Dynamic::U8(value));
    }
}

extern "C" fn list_str_push(addr: *mut Dynamic, value: *const Dynamic) {
    if addr.is_null() || value.is_null() {
        return;
    }
    unsafe {
        (&mut *addr).push_dynamic((&*value).clone());
    }
}

extern "C" fn list_str_get_idx(addr: *const Dynamic, idx: i64) -> *const Dynamic {
    if addr.is_null() {
        return any_null();
    };
    let Ok(idx) = usize::try_from(idx) else {
        return any_null();
    };
    if let Some(value) = unsafe { (&*addr).get_idx(idx) } { alloc_dynamic(value) } else { any_null() }
}

extern "C" fn list_str_set_idx(addr: *mut Dynamic, idx: i64, value: *const Dynamic) {
    if addr.is_null() || value.is_null() {
        return;
    }
    let Ok(idx) = usize::try_from(idx) else {
        return;
    };
    unsafe {
        (&mut *addr).set_idx(idx, (&*value).clone());
    }
}

extern "C" fn slice(addr: *const Dynamic, start: i64, stop: *const Dynamic, inclusive: bool) -> *const Dynamic {
    if addr.is_null() {
        return any_null();
    }

    let value = unsafe { &*addr };
    let len = value.len() as i64;
    let start = start.clamp(0, len) as usize;
    let mut stop = if stop.is_null() {
        len
    } else {
        let raw = unsafe { &*stop };
        if raw.is_null() { len } else { raw.as_int().unwrap_or(len) }
    };
    if inclusive && stop < len {
        stop += 1;
    }
    let stop = stop.clamp(start as i64, len) as usize;

    let sliced = if value.is_str() {
        Dynamic::from(value.as_str().chars().skip(start).take(stop.saturating_sub(start)).collect::<String>())
    } else {
        match value {
            Dynamic::List(list) => Dynamic::list(list.read().unwrap()[start..stop].to_vec()),
            _ => Dynamic::Null,
        }
    };
    alloc_dynamic(sliced)
}

extern "C" fn set_key(addr: *mut Dynamic, key: *const Dynamic, value: *const Dynamic) {
    if addr.is_null() || key.is_null() {
        return;
    }
    let key: &str = unsafe { &*key }.as_str();
    unsafe { (&mut *addr).set_dynamic(key.into(), (&*value).clone()) }
}

extern "C" fn set_idx(addr: *mut Dynamic, idx: i64, value: *const Dynamic) {
    if addr.is_null() {
        return;
    }
    unsafe { (&mut *addr).set_idx(idx as usize, (&*value).clone()) }
}

extern "C" fn any_from_i64(v: i64) -> *const Dynamic {
    alloc_dynamic(Dynamic::I64(v))
}

extern "C" fn any_from_u64(v: u64) -> *const Dynamic {
    alloc_dynamic(Dynamic::U64(v))
}

extern "C" fn any_from_bool(v: bool) -> *const Dynamic {
    alloc_dynamic(Dynamic::Bool(v))
}

extern "C" fn any_to_i64(addr: *const Dynamic) -> i64 {
    if addr.is_null() {
        return 0;
    }
    unsafe {
        let value = &*addr;
        value
            .as_int()
            .or_else(|| value.as_float().map(|value| value as i64))
            .or_else(|| {
                let text = value.as_str();
                let text = text.trim();
                if text.is_empty() { None } else { text.parse::<i64>().ok().or_else(|| text.parse::<f64>().ok().map(|value| value as i64)) }
            })
            .unwrap_or(0)
    }
}

extern "C" fn any_to_bool(addr: *const Dynamic) -> bool {
    if addr.is_null() {
        return false;
    }
    unsafe {
        let value = &*addr;
        if let Some(v) = value.as_bool() {
            v
        } else if let Some(v) = value.as_int() {
            v != 0
        } else if let Some(v) = value.as_float() {
            v != 0.0
        } else {
            !value.is_null()
        }
    }
}

extern "C" fn any_from_f64(v: f64) -> *const Dynamic {
    alloc_dynamic(Dynamic::F64(v))
}

extern "C" fn any_split(addr: *mut Dynamic, s: *const Dynamic) -> *const Dynamic {
    if addr.is_null() || s.is_null() {
        return any_null();
    }
    let s: &str = unsafe { &*s }.as_str();
    alloc_dynamic(unsafe { (&*addr).clone() }.split(s))
}

extern "C" fn any_to_f64(addr: *const Dynamic) -> f64 {
    if addr.is_null() {
        return 0.0;
    }
    unsafe {
        let value = &*addr;
        value.as_float().or_else(|| value.as_str().trim().parse::<f64>().ok()).unwrap_or(0.0)
    }
}

extern "C" fn any_to_string(addr: *const Dynamic) -> *const Dynamic {
    if addr.is_null() {
        return alloc_dynamic(Dynamic::from(""));
    }
    alloc_dynamic(Dynamic::from(unsafe { &*addr }.to_string()))
}

extern "C" fn any_binary(left: *const Dynamic, op: i32, right: *const Dynamic) -> *const Dynamic {
    if left.is_null() {
        if right.is_null() {
            return any_null();
        }
        return alloc_dynamic(unsafe { (&*right).clone() });
    }
    if right.is_null() {
        return alloc_dynamic(unsafe { (&*left).clone() });
    }
    let op = BinaryOp::try_from(op).unwrap();
    if op == BinaryOp::Add {
        let (left_value, right_value) = unsafe { (&*left, &*right) };
        if left_value.is_str() || right_value.is_str() {
            return alloc_dynamic(left_value.clone() + right_value.clone());
        }
    }
    unsafe {
        let expr = Expr::new(
            ExprKind::Binary { left: Box::new(Expr::new(ExprKind::Value((&*left).clone()), Span::default())), op, right: Box::new(Expr::new(ExprKind::Value((&*right).clone()), Span::default())) },
            Span::default(),
        );
        alloc_dynamic(expr.compact().unwrap_or(Dynamic::Null))
    }
}

extern "C" fn any_logic(left: *const Dynamic, op: i32, right: *const Dynamic) -> i32 {
    let op = BinaryOp::try_from(op).unwrap();
    unsafe {
        let expr = Expr::new(
            ExprKind::Binary { left: Box::new(Expr::new(ExprKind::Value((&*left).clone()), Span::default())), op, right: Box::new(Expr::new(ExprKind::Value((&*right).clone()), Span::default())) },
            Span::default(),
        );
        if expr.compact().and_then(|r| r.as_bool()).unwrap_or(false) { 1 } else { 0 }
    }
}

pub const STD: [(&str, &[Type], Type, *const u8); 5] = [
    ("print", &[Type::Any], Type::Void, print as *const u8),
    ("sqrt", &[Type::F64], Type::F64, sqrt as *const u8),
    ("log", &[Type::Any], Type::Void, log_any as *const u8),
    ("uuid", &[], Type::Any, uuid as *const u8),
    ("rand", &[Type::Any, Type::Any], Type::Any, random as *const u8),
];

pub const ANY: [(&str, &[Type], Type, *const u8); 68] = [
    ("Any::null", &[], Type::Any, any_null as *const u8),
    ("Any::is_map", &[Type::Any], Type::Bool, any_is_map as *const u8),
    ("Any::is_list", &[Type::Any], Type::Bool, any_is_list as *const u8),
    ("Any::is_string", &[Type::Any], Type::Bool, any_is_string as *const u8),
    ("Any::is_null", &[Type::Any], Type::Bool, any_is_null as *const u8),
    ("Any::clone", &[Type::Any], Type::Any, any_clone as *const u8),
    ("Any::len", &[Type::Any], Type::I32, any_len as *const u8),
    ("Any::keys", &[Type::Any], Type::Any, any_keys as *const u8),
    ("Any::split", &[Type::Any, Type::Any], Type::Any, any_split as *const u8),
    ("Any::push", &[Type::Any, Type::Any], Type::Void, any_push as *const u8),
    ("Any::pop", &[Type::Any], Type::Any, any_pop as *const u8),
    ("Any::get_idx", &[Type::Any, Type::I64], Type::Any, get_idx as *const u8),
    ("Any::push_bool", &[Type::Any, Type::Bool], Type::Void, list_bool_push as *const u8),
    ("Any::get_idx_bool", &[Type::Any, Type::I64], Type::Bool, list_bool_get_idx as *const u8),
    ("Any::set_idx_bool", &[Type::Any, Type::I64, Type::Bool], Type::Void, list_bool_set_idx as *const u8),
    ("Any::push_u8", &[Type::Any, Type::U8], Type::Void, list_u8_push as *const u8),
    ("Any::get_idx_u8", &[Type::Any, Type::I64], Type::U8, list_u8_get_idx as *const u8),
    ("Any::set_idx_u8", &[Type::Any, Type::I64, Type::U8], Type::Void, list_u8_set_idx as *const u8),
    ("Any::push_i8", &[Type::Any, Type::I8], Type::Void, list_i8_push as *const u8),
    ("Any::get_idx_i8", &[Type::Any, Type::I64], Type::I8, list_i8_get_idx as *const u8),
    ("Any::set_idx_i8", &[Type::Any, Type::I64, Type::I8], Type::Void, list_i8_set_idx as *const u8),
    ("Any::push_u16", &[Type::Any, Type::U16], Type::Void, list_u16_push as *const u8),
    ("Any::get_idx_u16", &[Type::Any, Type::I64], Type::U16, list_u16_get_idx as *const u8),
    ("Any::set_idx_u16", &[Type::Any, Type::I64, Type::U16], Type::Void, list_u16_set_idx as *const u8),
    ("Any::push_i16", &[Type::Any, Type::I16], Type::Void, list_i16_push as *const u8),
    ("Any::get_idx_i16", &[Type::Any, Type::I64], Type::I16, list_i16_get_idx as *const u8),
    ("Any::set_idx_i16", &[Type::Any, Type::I64, Type::I16], Type::Void, list_i16_set_idx as *const u8),
    ("Any::push_u32", &[Type::Any, Type::U32], Type::Void, list_u32_push as *const u8),
    ("Any::get_idx_u32", &[Type::Any, Type::I64], Type::U32, list_u32_get_idx as *const u8),
    ("Any::set_idx_u32", &[Type::Any, Type::I64, Type::U32], Type::Void, list_u32_set_idx as *const u8),
    ("Any::push_i32", &[Type::Any, Type::I32], Type::Void, list_i32_push as *const u8),
    ("Any::get_idx_i32", &[Type::Any, Type::I64], Type::I32, list_i32_get_idx as *const u8),
    ("Any::set_idx_i32", &[Type::Any, Type::I64, Type::I32], Type::Void, list_i32_set_idx as *const u8),
    ("Any::push_f32", &[Type::Any, Type::F32], Type::Void, list_f32_push as *const u8),
    ("Any::get_idx_f32", &[Type::Any, Type::I64], Type::F32, list_f32_get_idx as *const u8),
    ("Any::set_idx_f32", &[Type::Any, Type::I64, Type::F32], Type::Void, list_f32_set_idx as *const u8),
    ("Any::push_u64", &[Type::Any, Type::U64], Type::Void, list_u64_push as *const u8),
    ("Any::get_idx_u64", &[Type::Any, Type::I64], Type::U64, list_u64_get_idx as *const u8),
    ("Any::set_idx_u64", &[Type::Any, Type::I64, Type::U64], Type::Void, list_u64_set_idx as *const u8),
    ("Any::push_i64", &[Type::Any, Type::I64], Type::Void, list_i64_push as *const u8),
    ("Any::get_idx_i64", &[Type::Any, Type::I64], Type::I64, list_i64_get_idx as *const u8),
    ("Any::set_idx_i64", &[Type::Any, Type::I64, Type::I64], Type::Void, list_i64_set_idx as *const u8),
    ("Any::push_f64", &[Type::Any, Type::F64], Type::Void, list_f64_push as *const u8),
    ("Any::get_idx_f64", &[Type::Any, Type::I64], Type::F64, list_f64_get_idx as *const u8),
    ("Any::set_idx_f64", &[Type::Any, Type::I64, Type::F64], Type::Void, list_f64_set_idx as *const u8),
    ("Any::push_str", &[Type::Any, Type::Str], Type::Void, list_str_push as *const u8),
    ("Any::get_idx_str", &[Type::Any, Type::I64], Type::Str, list_str_get_idx as *const u8),
    ("Any::set_idx_str", &[Type::Any, Type::I64, Type::Str], Type::Void, list_str_set_idx as *const u8),
    ("Any::slice", &[Type::Any, Type::I64, Type::Any, Type::Bool], Type::Any, slice as *const u8),
    ("Any::contains", &[Type::Any, Type::Any], Type::Bool, contains as *const u8),
    ("Any::starts_with", &[Type::Any, Type::Any], Type::Bool, starts_with as *const u8),
    ("Any::get_key", &[Type::Any, Type::Any], Type::Any, get_key as *const u8),
    ("Any::del_key", &[Type::Any, Type::Any], Type::Any, del_key as *const u8),
    ("Any::set_idx", &[Type::Any, Type::I64, Type::Any], Type::Void, set_idx as *const u8),
    ("Any::set_key", &[Type::Any, Type::Any, Type::Any], Type::Void, set_key as *const u8),
    ("Any::from_i64", &[Type::I64], Type::Any, any_from_i64 as *const u8),
    ("Any::from_u64", &[Type::U64], Type::Any, any_from_u64 as *const u8),
    ("Any::from_bool", &[Type::Bool], Type::Any, any_from_bool as *const u8),
    ("Any::to_i64", &[Type::Any], Type::I64, any_to_i64 as *const u8),
    ("Any::to_bool", &[Type::Any], Type::Bool, any_to_bool as *const u8),
    ("Any::from_f64", &[Type::F64], Type::Any, any_from_f64 as *const u8),
    ("Any::to_f64", &[Type::Any], Type::F64, any_to_f64 as *const u8),
    ("Any::to_string", &[Type::Any], Type::Str, any_to_string as *const u8),
    ("Any::binary", &[Type::Any, Type::I32, Type::Any], Type::Any, any_binary as *const u8),
    ("Any::logic", &[Type::Any, Type::I32, Type::Any], Type::Bool, any_logic as *const u8),
    ("Any::iter", &[Type::Any], Type::Any, any_iter as *const u8),
    ("Any::next", &[Type::Any], Type::Any, any_next as *const u8),
    ("Any::next_pair", &[Type::Any], Type::Any, any_next_pair as *const u8),
];

use std::rc::Rc;
impl JITRunTime {
    pub fn add_native_ptr(&mut self, full_name: &str, name: &str, arg_tys: &[Type], ret_ty: Type, fn_ptr: *const u8) -> Result<u32> {
        self.native_symbols.write().unwrap().insert(full_name.to_string(), fn_ptr as usize);
        self.add_native(full_name, name, arg_tys, ret_ty)
    }

    pub(crate) fn add_context_native_ptr(&mut self, full_name: &str, name: &str, arg_tys: &[Type], ret_ty: Type, fn_ptr: *const u8) -> Result<u32> {
        self.native_symbols.write().unwrap().insert(full_name.to_string(), fn_ptr as usize);
        self.add_context_native(full_name, name, arg_tys, ret_ty)
    }

    pub fn add_native(&mut self, full_name: &str, name: &str, arg_tys: &[Type], ret_ty: Type) -> Result<u32> {
        let fn_ty = Type::Fn { tys: arg_tys.to_vec(), ret: Rc::new(ret_ty.clone()) };
        let id = self.compiler.add_symbol(name, compiler::Symbol::Native(fn_ty.clone()));
        let sig = self.get_sig(arg_tys, ret_ty)?;
        let fn_id = self.module.declare_function(full_name, Linkage::Import, &sig)?;
        self.fns.insert(id, FnVariant::Native { ty: fn_ty, fn_id, context: None });
        Ok(id)
    }

    pub(crate) fn add_context_native(&mut self, full_name: &str, name: &str, arg_tys: &[Type], ret_ty: Type) -> Result<u32> {
        let fn_ty = Type::Fn { tys: arg_tys.to_vec(), ret: Rc::new(ret_ty.clone()) };
        let id = self.compiler.add_symbol(name, compiler::Symbol::Native(fn_ty.clone()));
        let mut sig = self.module.make_signature();
        sig.params.push(AbiParam::new(crate::ptr_type()));
        for arg in arg_tys.iter() {
            sig.params.push(AbiParam::new(crate::get_type(arg)?));
        }
        if !ret_ty.is_void() {
            sig.returns.push(AbiParam::new(crate::get_type(&ret_ty)?));
        }
        let fn_id = self.module.declare_function(full_name, Linkage::Import, &sig)?;
        self.fns.insert(id, FnVariant::Native { ty: fn_ty, fn_id, context: Some(self.owner_context_ptr()) });
        Ok(id)
    }
}
