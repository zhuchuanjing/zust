use super::FnVariant;
use crate::JITRunTime;
use crate::RwLock;
use crate::memory::{alloc_dynamic, alloc_struct_bytes, take_dynamic_return};
use anyhow::{Result, anyhow};
use cranelift::prelude::AbiParam;
use cranelift_module::{Linkage, Module};
use dynamic::{Dynamic, FromJson, ToJson, ToYaml, Type};
use parser::{BinaryOp, Expr, ExprKind, Span};
use rand::RngExt;
use std::borrow::Cow;
use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::sync::Weak;

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
        if matches!(self.explicit_arg_len, usize::MAX | 0) {
            return self.call_with_arg_ptrs(&[]);
        }
        self.call(Vec::new())
    }

    pub fn call1(&self, arg: Dynamic) -> Result<Dynamic> {
        match self.explicit_arg_len {
            0 => self.call0(),
            usize::MAX | 1 => {
                let mut ptrs = Vec::with_capacity(1 + self.captures.len());
                ptrs.push(&arg as *const Dynamic);
                self.call_with_arg_ptrs(&ptrs)
            }
            _ => self.call(vec![arg]),
        }
    }

    pub fn call(&self, mut args: Vec<Dynamic>) -> Result<Dynamic> {
        if self.explicit_arg_len != usize::MAX {
            args.truncate(self.explicit_arg_len);
            while args.len() < self.explicit_arg_len {
                args.push(Dynamic::Null);
            }
        }
        let ptrs: Vec<*const Dynamic> = args.iter().map(|arg| arg as *const Dynamic).collect();
        self.call_with_arg_ptrs(&ptrs)
    }

    fn call_with_arg_ptrs(&self, args: &[*const Dynamic]) -> Result<Dynamic> {
        let mut ptrs = Vec::with_capacity(args.len() + self.captures.len());
        ptrs.extend_from_slice(args);
        ptrs.extend(self.captures.iter().map(|value| value as *const Dynamic));
        call_jit_isolated(|| unsafe { call_callback(self.fn_ptr as *const u8, &self.ret_ty, &ptrs) })
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

extern "C" fn sleep(ms: i64) {
    std::thread::sleep(std::time::Duration::from_millis(ms.max(0) as u64));
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

extern "C" fn any_is_number(addr: *const Dynamic) -> bool {
    !addr.is_null() && unsafe { (*addr).is_int() || (*addr).is_uint() || (*addr).is_float() }
}

extern "C" fn any_is_integer(addr: *const Dynamic) -> bool {
    !addr.is_null() && unsafe { (*addr).is_int() || (*addr).is_uint() }
}

extern "C" fn any_is_bool(addr: *const Dynamic) -> bool {
    !addr.is_null() && unsafe { matches!(*addr, Dynamic::Bool(_)) }
}

extern "C" fn any_is_int(addr: *const Dynamic) -> bool {
    !addr.is_null() && unsafe { matches!(*addr, Dynamic::I8(_) | Dynamic::I16(_) | Dynamic::I32(_) | Dynamic::I64(_) | Dynamic::U8(_) | Dynamic::U16(_) | Dynamic::U32(_) | Dynamic::U64(_)) }
}

extern "C" fn any_is_float(addr: *const Dynamic) -> bool {
    !addr.is_null() && unsafe { matches!(*addr, Dynamic::F16(_) | Dynamic::F32(_) | Dynamic::F64(_)) }
}

extern "C" fn random(start: *const Dynamic, stop: *const Dynamic) -> *const Dynamic {
    if !start.is_null() && !stop.is_null() {
        let mut rng = rand::rng();
        unsafe {
            if (&*start).is_int() || (&*start).is_uint() {
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

/// 读取进程环境变量。name 是字符串;变量不存在(或值不是合法 unicode)时返回 Null。
/// 跟宿主 `std::env::var` 一致:不区分「不存在」和「非 unicode」,都视为空,避免把
/// 内部错误升级成脚本可观察的 panic。
extern "C" fn env(addr: *const Dynamic) -> *const Dynamic {
    let name = if addr.is_null() { "" } else { unsafe { (&*addr).as_str() } };
    match std::env::var(name) {
        Ok(value) => alloc_dynamic(Dynamic::from(value)),
        Err(_) => alloc_dynamic(Dynamic::Null),
    }
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
    alloc_dynamic(Dynamic::owned_struct_from_ptr(addr as usize, ty))
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
    let name = if addr.is_null() || path.is_null() {
        return false;
    } else {
        unsafe { (&*addr).as_str().to_string() }
    };
    let path = unsafe { (&*path).as_str().to_string() };
    super::with_native_context(context, |jit| jit.import(name.as_str(), path.as_str())).map_err(|e| log::error!("import {name} 失败: {e:#}")).is_ok()
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
    let thread_name = format!("zust:{fn_name}");
    // spawn 返回 bool:true=线程已启动(不代表任务执行成功),false=线程启动失败(资源耗尽等)。
    // 任务执行错误在新线程内 log::error,调用方通过返回值感知启动失败。
    match std::thread::Builder::new().name(thread_name).spawn(move || {
        if let Err(err) = spawn_run(context, fn_name.as_str(), args) {
            log::error!("spawn {fn_name} failed: {err:#}");
        }
    }) {
        Ok(_) => true,
        Err(e) => {
            log::error!("spawn 线程启动失败: {e:#}");
            false
        }
    }
}

fn spawn_args(args: Dynamic) -> Vec<Dynamic> {
    match args {
        Dynamic::Null => Vec::new(),
        Dynamic::List(values) => values.read().iter().map(Dynamic::deep_clone).collect(),
        value => vec![value],
    }
}

fn spawn_run(context: Weak<RwLock<JITRunTime>>, fn_name: &str, args: Dynamic) -> Result<()> {
    let args = spawn_args(args);
    if args.len() > 16 {
        anyhow::bail!("spawn supports at most 16 args, got {}", args.len());
    }
    let arg_tys = vec![Type::Any; args.len()];
    let (ptr, ret_ty) = super::with_native_context(&context as *const Weak<RwLock<JITRunTime>>, |vm| vm.jit.write().get_fn_ptr(fn_name, &arg_tys))?;
    let args: Vec<Box<Dynamic>> = args.into_iter().map(Box::new).collect();
    let ptrs: Vec<*const Dynamic> = args.iter().map(|arg| arg.as_ref() as *const Dynamic).collect();
    call_jit_isolated(|| unsafe { call_spawned(ptr, &ret_ty, &ptrs) })
}

pub(crate) extern "C" fn spawn_ptr(fn_ptr: i64, ret_ty: i64, args: *const Dynamic) -> bool {
    if fn_ptr == 0 || ret_ty == 0 {
        return false;
    }
    let fn_ptr = fn_ptr as usize;
    let ret_ty = unsafe { (&*(ret_ty as *const Type)).clone() };
    let args = if args.is_null() { Dynamic::Null } else { unsafe { (&*args).deep_clone() } };
    match std::thread::Builder::new().name("zust:closure".to_string()).spawn(move || {
        if let Err(err) = spawn_run_ptr(fn_ptr, ret_ty, args) {
            log::error!("spawn closure failed: {err:#}");
        }
    }) {
        Ok(_) => true,
        Err(e) => {
            log::error!("spawn closure 线程启动失败: {e:#}");
            false
        }
    }
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
        // 闭包捕获按引用共享:浅 clone 只复制 Arc 句柄,多个闭包捕获同一个
        // Map/List 时读写同一份数据(spawn 跨线程才需要 deep_clone 隔离)。
        match unsafe { &*captures } {
            Dynamic::List(values) => values.read().to_vec(),
            value => vec![value.clone()],
        }
    };
    alloc_dynamic(Dynamic::custom(ZustCallback::new_with_arg_len(fn_ptr as usize, ret_ty, explicit_arg_len, captures)))
}

pub(crate) extern "C" fn callback_call(callback: *const Dynamic, args: *const Dynamic) -> *const Dynamic {
    if callback.is_null() {
        return alloc_dynamic(Dynamic::Null);
    }
    let Some(callback) = (unsafe { &*callback }).as_custom::<ZustCallback>().cloned() else {
        return alloc_dynamic(Dynamic::Null);
    };
    let args = if args.is_null() {
        Vec::new()
    } else {
        match unsafe { &*args } {
            Dynamic::List(values) => values.read().to_vec(),
            Dynamic::Null => Vec::new(),
            value => vec![value.clone()],
        }
    };
    match callback.call(args) {
        Ok(value) => alloc_dynamic(value),
        Err(err) => {
            log::error!("callback call failed: {err:?}");
            alloc_dynamic(Dynamic::Null)
        }
    }
}

fn spawn_run_ptr(fn_ptr: usize, ret_ty: Type, args: Dynamic) -> Result<()> {
    let args = spawn_args(args);
    if args.len() > 16 {
        anyhow::bail!("spawn supports at most 16 args, got {}", args.len());
    }
    let args: Vec<Box<Dynamic>> = args.into_iter().map(Box::new).collect();
    let ptrs: Vec<*const Dynamic> = args.iter().map(|arg| arg.as_ref() as *const Dynamic).collect();
    call_jit_isolated(|| unsafe { call_spawned(fn_ptr as *const u8, &ret_ty, &ptrs) })
}

/// 脚本执行的隔离边界。
///
/// JIT 代码无法返回 `Result`,运行期错误(整数除零等)通过 [`dynamic`] 的线程
/// 局部 fault 标志上报(由 `__vm_arith_fault` 与 `Dynamic` 的除法守卫设置)。这里
/// 在调用前清掉陈旧标志,调用后读取它,把"脚本出错"降级为一次失败的 `Result`,
/// 而不是让进程崩溃。`catch_unwind` 额外兜住宿主侧 Rust 代码(参数编组、返回值
/// 取出)的 panic。
///
/// 注意:若 panic 发生在 JIT 的 `extern "C"` 帧之内再向外穿越,Rust 默认会 abort,
/// `catch_unwind` 无法拦截——这类路径靠的是各 native 助手不 panic(除零已改为置
/// fault 标志)。
pub(crate) fn call_jit_isolated<T>(f: impl FnOnce() -> Result<T>) -> Result<T> {
    let _ = dynamic::take_fault();
    let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(f));
    match outcome {
        Ok(inner) => match dynamic::take_fault() {
            Some(reason) => Err(anyhow!("脚本运行期错误: {}", reason)),
            None => inner,
        },
        Err(_) => Err(anyhow!("脚本执行 panic,已隔离")),
    }
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
                    std::mem::transmute::<_, extern "C" fn(*const Dynamic, *const Dynamic, *const Dynamic, *const Dynamic, *const Dynamic, *const Dynamic, *const Dynamic, *const Dynamic, *const Dynamic, *const Dynamic)>(
                        ptr,
                    )(*a, *b, *c, *d, *e, *f, *g, *h, *i, *j)
                },
                [a, b, c, d, e, f, g, h, i, j, k] => unsafe {
                    std::mem::transmute::<
                        _,
                        extern "C" fn(*const Dynamic, *const Dynamic, *const Dynamic, *const Dynamic, *const Dynamic, *const Dynamic, *const Dynamic, *const Dynamic, *const Dynamic, *const Dynamic, *const Dynamic),
                    >(ptr)(*a, *b, *c, *d, *e, *f, *g, *h, *i, *j, *k)
                },
                [a, b, c, d, e, f, g, h, i, j, k, l] => unsafe {
                    std::mem::transmute::<
                        _,
                        extern "C" fn(
                            *const Dynamic,
                            *const Dynamic,
                            *const Dynamic,
                            *const Dynamic,
                            *const Dynamic,
                            *const Dynamic,
                            *const Dynamic,
                            *const Dynamic,
                            *const Dynamic,
                            *const Dynamic,
                            *const Dynamic,
                            *const Dynamic,
                        ),
                    >(ptr)(*a, *b, *c, *d, *e, *f, *g, *h, *i, *j, *k, *l)
                },
                [a, b, c, d, e, f, g, h, i, j, k, l, m] => unsafe {
                    std::mem::transmute::<
                        _,
                        extern "C" fn(
                            *const Dynamic,
                            *const Dynamic,
                            *const Dynamic,
                            *const Dynamic,
                            *const Dynamic,
                            *const Dynamic,
                            *const Dynamic,
                            *const Dynamic,
                            *const Dynamic,
                            *const Dynamic,
                            *const Dynamic,
                            *const Dynamic,
                            *const Dynamic,
                        ),
                    >(ptr)(*a, *b, *c, *d, *e, *f, *g, *h, *i, *j, *k, *l, *m)
                },
                [a, b, c, d, e, f, g, h, i, j, k, l, m, n] => unsafe {
                    std::mem::transmute::<
                        _,
                        extern "C" fn(
                            *const Dynamic,
                            *const Dynamic,
                            *const Dynamic,
                            *const Dynamic,
                            *const Dynamic,
                            *const Dynamic,
                            *const Dynamic,
                            *const Dynamic,
                            *const Dynamic,
                            *const Dynamic,
                            *const Dynamic,
                            *const Dynamic,
                            *const Dynamic,
                            *const Dynamic,
                        ),
                    >(ptr)(*a, *b, *c, *d, *e, *f, *g, *h, *i, *j, *k, *l, *m, *n)
                },
                [a, b, c, d, e, f, g, h, i, j, k, l, m, n, o] => unsafe {
                    std::mem::transmute::<
                        _,
                        extern "C" fn(
                            *const Dynamic,
                            *const Dynamic,
                            *const Dynamic,
                            *const Dynamic,
                            *const Dynamic,
                            *const Dynamic,
                            *const Dynamic,
                            *const Dynamic,
                            *const Dynamic,
                            *const Dynamic,
                            *const Dynamic,
                            *const Dynamic,
                            *const Dynamic,
                            *const Dynamic,
                            *const Dynamic,
                        ),
                    >(ptr)(*a, *b, *c, *d, *e, *f, *g, *h, *i, *j, *k, *l, *m, *n, *o)
                },
                [a, b, c, d, e, f, g, h, i, j, k, l, m, n, o, p] => unsafe {
                    std::mem::transmute::<
                        _,
                        extern "C" fn(
                            *const Dynamic,
                            *const Dynamic,
                            *const Dynamic,
                            *const Dynamic,
                            *const Dynamic,
                            *const Dynamic,
                            *const Dynamic,
                            *const Dynamic,
                            *const Dynamic,
                            *const Dynamic,
                            *const Dynamic,
                            *const Dynamic,
                            *const Dynamic,
                            *const Dynamic,
                            *const Dynamic,
                            *const Dynamic,
                        ),
                    >(ptr)(*a, *b, *c, *d, *e, *f, *g, *h, *i, *j, *k, *l, *m, *n, *o, *p)
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
                    let fn_ptr: extern "C" fn(*const Dynamic, *const Dynamic, *const Dynamic, *const Dynamic, *const Dynamic, *const Dynamic, *const Dynamic, *const Dynamic, *const Dynamic) -> $ret =
                        unsafe { std::mem::transmute(ptr) };
                    $drop_result(fn_ptr(*a, *b, *c, *d, *e, *f, *g, *h, *i));
                }
                [a, b, c, d, e, f, g, h, i, j] => {
                    let fn_ptr: extern "C" fn(*const Dynamic, *const Dynamic, *const Dynamic, *const Dynamic, *const Dynamic, *const Dynamic, *const Dynamic, *const Dynamic, *const Dynamic, *const Dynamic) -> $ret =
                        unsafe { std::mem::transmute(ptr) };
                    $drop_result(fn_ptr(*a, *b, *c, *d, *e, *f, *g, *h, *i, *j));
                }
                [a, b, c, d, e, f, g, h, i, j, k] => {
                    let fn_ptr: extern "C" fn(
                        *const Dynamic,
                        *const Dynamic,
                        *const Dynamic,
                        *const Dynamic,
                        *const Dynamic,
                        *const Dynamic,
                        *const Dynamic,
                        *const Dynamic,
                        *const Dynamic,
                        *const Dynamic,
                        *const Dynamic,
                    ) -> $ret = unsafe { std::mem::transmute(ptr) };
                    $drop_result(fn_ptr(*a, *b, *c, *d, *e, *f, *g, *h, *i, *j, *k));
                }
                [a, b, c, d, e, f, g, h, i, j, k, l] => {
                    let fn_ptr: extern "C" fn(
                        *const Dynamic,
                        *const Dynamic,
                        *const Dynamic,
                        *const Dynamic,
                        *const Dynamic,
                        *const Dynamic,
                        *const Dynamic,
                        *const Dynamic,
                        *const Dynamic,
                        *const Dynamic,
                        *const Dynamic,
                        *const Dynamic,
                    ) -> $ret = unsafe { std::mem::transmute(ptr) };
                    $drop_result(fn_ptr(*a, *b, *c, *d, *e, *f, *g, *h, *i, *j, *k, *l));
                }
                [a, b, c, d, e, f, g, h, i, j, k, l, m] => {
                    let fn_ptr: extern "C" fn(
                        *const Dynamic,
                        *const Dynamic,
                        *const Dynamic,
                        *const Dynamic,
                        *const Dynamic,
                        *const Dynamic,
                        *const Dynamic,
                        *const Dynamic,
                        *const Dynamic,
                        *const Dynamic,
                        *const Dynamic,
                        *const Dynamic,
                        *const Dynamic,
                    ) -> $ret = unsafe { std::mem::transmute(ptr) };
                    $drop_result(fn_ptr(*a, *b, *c, *d, *e, *f, *g, *h, *i, *j, *k, *l, *m));
                }
                [a, b, c, d, e, f, g, h, i, j, k, l, m, n] => {
                    let fn_ptr: extern "C" fn(
                        *const Dynamic,
                        *const Dynamic,
                        *const Dynamic,
                        *const Dynamic,
                        *const Dynamic,
                        *const Dynamic,
                        *const Dynamic,
                        *const Dynamic,
                        *const Dynamic,
                        *const Dynamic,
                        *const Dynamic,
                        *const Dynamic,
                        *const Dynamic,
                        *const Dynamic,
                    ) -> $ret = unsafe { std::mem::transmute(ptr) };
                    $drop_result(fn_ptr(*a, *b, *c, *d, *e, *f, *g, *h, *i, *j, *k, *l, *m, *n));
                }
                [a, b, c, d, e, f, g, h, i, j, k, l, m, n, o] => {
                    let fn_ptr: extern "C" fn(
                        *const Dynamic,
                        *const Dynamic,
                        *const Dynamic,
                        *const Dynamic,
                        *const Dynamic,
                        *const Dynamic,
                        *const Dynamic,
                        *const Dynamic,
                        *const Dynamic,
                        *const Dynamic,
                        *const Dynamic,
                        *const Dynamic,
                        *const Dynamic,
                        *const Dynamic,
                        *const Dynamic,
                    ) -> $ret = unsafe { std::mem::transmute(ptr) };
                    $drop_result(fn_ptr(*a, *b, *c, *d, *e, *f, *g, *h, *i, *j, *k, *l, *m, *n, *o));
                }
                [a, b, c, d, e, f, g, h, i, j, k, l, m, n, o, p] => {
                    let fn_ptr: extern "C" fn(
                        *const Dynamic,
                        *const Dynamic,
                        *const Dynamic,
                        *const Dynamic,
                        *const Dynamic,
                        *const Dynamic,
                        *const Dynamic,
                        *const Dynamic,
                        *const Dynamic,
                        *const Dynamic,
                        *const Dynamic,
                        *const Dynamic,
                        *const Dynamic,
                        *const Dynamic,
                        *const Dynamic,
                        *const Dynamic,
                    ) -> $ret = unsafe { std::mem::transmute(ptr) };
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

extern "C" fn any_is_empty(addr: *const Dynamic) -> bool {
    addr.is_null() || unsafe { (&*addr).len() == 0 }
}

extern "C" fn any_keys(addr: *const Dynamic) -> *const Dynamic {
    if addr.is_null() {
        return alloc_dynamic(Dynamic::list(Vec::new()));
    }
    let keys = match unsafe { &*addr } {
        Dynamic::Map(map) => map.read().keys().map(|key| Dynamic::from(key.as_str())).collect(),
        _ => Vec::new(),
    };
    alloc_dynamic(Dynamic::list(keys))
}

extern "C" fn any_iter(addr: *const Dynamic) -> *const Dynamic {
    if addr.is_null() { any_null() } else { alloc_dynamic(unsafe { (*addr).clone().into_iter() }) }
}

extern "C" fn any_next(addr: *const Dynamic) -> *const Dynamic {
    if addr.is_null() { any_null() } else { alloc_dynamic(unsafe { (&mut *(addr as *mut Dynamic)).next().unwrap_or(Dynamic::Null) }) }
}

extern "C" fn any_next_pair(addr: *const Dynamic) -> *const Dynamic {
    if addr.is_null() { any_null() } else { alloc_dynamic(unsafe { (&mut *(addr as *mut Dynamic)).next_pair().unwrap_or(Dynamic::Null) }) }
}

extern "C" fn any_push(addr: *mut Dynamic, value: *mut Dynamic) {
    if !addr.is_null() && !value.is_null() {
        unsafe {
            (&mut *addr).push_dynamic((&*value).clone());
        }
    }
}

/// 函数式 `append(list, value)` / 动态 `list.append(value)`：原地追加后返回同一
/// 动态集合。模型常把结果重新赋给原变量，返回集合可同时兼容函数式与命令式写法。
extern "C" fn any_append(addr: *mut Dynamic, value: *mut Dynamic) -> *const Dynamic {
    any_push(addr, value);
    if addr.is_null() { any_null() } else { alloc_dynamic(unsafe { (&*addr).clone() }) }
}

extern "C" fn any_pop(addr: *mut Dynamic) -> *const Dynamic {
    if addr.is_null() { any_null() } else { alloc_dynamic(unsafe { (*addr).pop().unwrap_or(Dynamic::Null) }) }
}

/// Rust/JavaScript 都常见的 `list.insert(index, value)` 便利写法；map 也接受
/// `insert(key, value)`，便于模型生成的结构化代码直接运行。
extern "C" fn any_insert(addr: *mut Dynamic, key: *const Dynamic, value: *const Dynamic) -> bool {
    if addr.is_null() || key.is_null() || value.is_null() {
        return false;
    }
    unsafe {
        match &mut *addr {
            Dynamic::List(list) => {
                let Some(index) = (&*key).as_int().and_then(|value| usize::try_from(value).ok()) else {
                    return false;
                };
                let mut list = list.write();
                if index > list.len() {
                    return false;
                }
                list.insert(index, (&*value).clone());
                true
            }
            Dynamic::Map(map) if (&*key).is_str() => {
                map.write().insert((&*key).as_str().into(), (&*value).clone());
                true
            }
            _ => false,
        }
    }
}

extern "C" fn get_key(addr: *const Dynamic, key: *const Dynamic) -> *const Dynamic {
    if addr.is_null() || key.is_null() {
        any_null()
    } else {
        let key: &str = unsafe { &*key }.as_str();
        alloc_dynamic(unsafe { (*addr).get_dynamic(key).unwrap_or(Dynamic::Null) })
    }
}

extern "C" fn any_get(addr: *const Dynamic, key: *const Dynamic) -> *const Dynamic {
    if addr.is_null() || key.is_null() {
        return any_null();
    }
    if let Some(index) = unsafe { (&*key).as_int() } {
        return get_idx(addr, index);
    }
    get_key(addr, key)
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

extern "C" fn ends_with(addr: *const Dynamic, suffix: *const Dynamic) -> bool {
    if addr.is_null() || suffix.is_null() {
        false
    } else {
        let suffix: &str = unsafe { &*suffix }.as_str();
        unsafe { (*addr).ends_with(suffix) }
    }
}

// Any::trim / to_lower / to_upper / replace / find / substring —— 字符串清洗常用操作。
// 与 starts_with / ends_with 一样,非字符串类型(数字/列表/map)按 as_str() 的
// 兜底语义(返回 "" )处理,调用方拿到空串不会 panic。

extern "C" fn any_trim(addr: *const Dynamic) -> *const Dynamic {
    if addr.is_null() {
        return alloc_dynamic(Dynamic::from(""));
    }
    alloc_dynamic(Dynamic::from(unsafe { (&*addr).as_str().trim().to_string() }))
}

extern "C" fn any_trim_start(addr: *const Dynamic) -> *const Dynamic {
    if addr.is_null() {
        return alloc_dynamic(Dynamic::from(""));
    }
    alloc_dynamic(Dynamic::from(unsafe { (&*addr).as_str().trim_start().to_string() }))
}

extern "C" fn any_trim_end(addr: *const Dynamic) -> *const Dynamic {
    if addr.is_null() {
        return alloc_dynamic(Dynamic::from(""));
    }
    alloc_dynamic(Dynamic::from(unsafe { (&*addr).as_str().trim_end().to_string() }))
}

extern "C" fn any_trim_matches(addr: *const Dynamic, pattern: *const Dynamic) -> *const Dynamic {
    if addr.is_null() || pattern.is_null() {
        return alloc_dynamic(Dynamic::from(""));
    }
    let text = unsafe { (&*addr).as_str() };
    let pattern = unsafe { (&*pattern).as_str() };
    if pattern.is_empty() {
        return alloc_dynamic(Dynamic::from(text.to_string()));
    }
    let mut trimmed = text;
    while let Some(rest) = trimmed.strip_prefix(pattern) {
        trimmed = rest;
    }
    while let Some(rest) = trimmed.strip_suffix(pattern) {
        trimmed = rest;
    }
    alloc_dynamic(Dynamic::from(trimmed.to_string()))
}

extern "C" fn any_trim_start_matches(addr: *const Dynamic, pattern: *const Dynamic) -> *const Dynamic {
    if addr.is_null() || pattern.is_null() {
        return alloc_dynamic(Dynamic::from(""));
    }
    let text = unsafe { (&*addr).as_str() };
    let pattern = unsafe { (&*pattern).as_str() };
    if pattern.is_empty() {
        return alloc_dynamic(Dynamic::from(text.to_string()));
    }
    alloc_dynamic(Dynamic::from(text.trim_start_matches(pattern).to_string()))
}

extern "C" fn any_trim_end_matches(addr: *const Dynamic, pattern: *const Dynamic) -> *const Dynamic {
    if addr.is_null() || pattern.is_null() {
        return alloc_dynamic(Dynamic::from(""));
    }
    let text = unsafe { (&*addr).as_str() };
    let pattern = unsafe { (&*pattern).as_str() };
    if pattern.is_empty() {
        return alloc_dynamic(Dynamic::from(text.to_string()));
    }
    alloc_dynamic(Dynamic::from(text.trim_end_matches(pattern).to_string()))
}

extern "C" fn any_to_lower(addr: *const Dynamic) -> *const Dynamic {
    if addr.is_null() {
        return alloc_dynamic(Dynamic::from(""));
    }
    alloc_dynamic(Dynamic::from(unsafe { (&*addr).as_str().to_lowercase() }))
}

extern "C" fn any_to_upper(addr: *const Dynamic) -> *const Dynamic {
    if addr.is_null() {
        return alloc_dynamic(Dynamic::from(""));
    }
    alloc_dynamic(Dynamic::from(unsafe { (&*addr).as_str().to_uppercase() }))
}

extern "C" fn any_replace(addr: *const Dynamic, from: *const Dynamic, to: *const Dynamic) -> *const Dynamic {
    if addr.is_null() || from.is_null() || to.is_null() {
        return alloc_dynamic(Dynamic::from(""));
    }
    let from = unsafe { (&*from).as_str() };
    let to = unsafe { (&*to).as_str() };
    // from 为空时 str::replace 会做奇怪的事(每个字符边界都插入 to),这里直接返回原串,
    // 跟大多数脚本语言的 replace 语义不一致但能避免意外膨胀。
    if from.is_empty() {
        return alloc_dynamic(unsafe { (&*addr).clone() });
    }
    alloc_dynamic(Dynamic::from(unsafe { (&*addr).as_str().replace(from, to) }))
}

extern "C" fn any_find(addr: *const Dynamic, sub: *const Dynamic, from: *const Dynamic) -> i64 {
    if addr.is_null() || sub.is_null() {
        return -1;
    }
    let text = unsafe { (&*addr).as_str() };
    let sub = unsafe { (&*sub).as_str() };
    // 可选起始字符下标（null/缺省从 0 开始）；返回值仍是整个文本的字符
    // 下标，与 substring 的字符索引语义一致。找不到返回 -1(而不是 Option),
    // 与 zust 其它 find 类操作一致。
    let mut start = 0i64;
    if !from.is_null() {
        let raw = unsafe { &*from };
        if !raw.is_null() {
            start = raw.as_int().unwrap_or(0).max(0);
        }
    }
    let skip: usize = text.chars().take(start as usize).map(|c| c.len_utf8()).sum();
    let skip = skip.min(text.len());
    match text[skip..].find(sub) {
        Some(byte_idx) => start + text[skip..skip + byte_idx].chars().count() as i64,
        None => -1,
    }
}

extern "C" fn any_rfind(addr: *const Dynamic, sub: *const Dynamic) -> i64 {
    if addr.is_null() || sub.is_null() {
        return -1;
    }
    let text = unsafe { (&*addr).as_str() };
    let sub = unsafe { (&*sub).as_str() };
    match text.rfind(sub) {
        Some(byte_idx) => text[..byte_idx].chars().count() as i64,
        None => -1,
    }
}

/// UTF-8 文本的原始字节长度。普通 `len` 仍返回字符数；协议 framing 需要明确
/// 区分这两个概念，不能用字符数伪装 Content-Length。
extern "C" fn any_byte_len(addr: *const Dynamic) -> i64 {
    if addr.is_null() {
        return 0;
    }
    unsafe { (&*addr).as_str().len() as i64 }
}

/// 按 UTF-8 字节偏移切片。起止位置必须都落在字符边界，非法范围返回 Null，
/// 让协议解析器能把损坏的 framing 与合法空串区分开。
extern "C" fn any_byte_slice(addr: *const Dynamic, start: i64, stop: *const Dynamic) -> *const Dynamic {
    if addr.is_null() || start < 0 {
        return any_null();
    }
    let text = unsafe { (&*addr).as_str() };
    let Ok(start) = usize::try_from(start) else {
        return any_null();
    };
    let stop = if stop.is_null() || unsafe { (&*stop).is_null() } {
        text.len()
    } else {
        let value = unsafe { &*stop };
        let Some(value) = value.as_int() else {
            return any_null();
        };
        let Ok(value) = usize::try_from(value) else {
            return any_null();
        };
        value
    };
    if start > stop || stop > text.len() || !text.is_char_boundary(start) || !text.is_char_boundary(stop) {
        return any_null();
    }
    alloc_dynamic(Dynamic::from(text[start..stop].to_string()))
}

extern "C" fn any_sort(addr: *mut Dynamic) {
    if addr.is_null() {
        return;
    }
    if let Dynamic::List(list) = unsafe { &mut *addr } {
        list.write().sort_by(|left, right| {
            let numeric = left.as_float().zip(right.as_float()).and_then(|(left, right)| left.partial_cmp(&right));
            numeric.unwrap_or_else(|| left.to_string().cmp(&right.to_string()))
        });
    }
}

extern "C" fn any_substring(addr: *const Dynamic, start: i64, stop: *const Dynamic) -> *const Dynamic {
    if addr.is_null() {
        return alloc_dynamic(Dynamic::from(""));
    }
    let text = unsafe { (&*addr).as_str() };
    let char_count = text.chars().count() as i64;
    // start/stop 都按字符索引(不是字节),负值或越界 clamp 到 [0, len]。
    // stop=null 表示取到末尾,与 Any::slice 的 stop 语义对齐。
    let start = start.clamp(0, char_count) as usize;
    let stop = if stop.is_null() {
        char_count
    } else {
        let raw = unsafe { &*stop };
        if raw.is_null() { char_count } else { raw.as_int().unwrap_or(char_count) }
    };
    let stop = stop.clamp(start as i64, char_count) as usize;
    let result: String = text.chars().skip(start).take(stop - start).collect();
    alloc_dynamic(Dynamic::from(result))
}

/// `list.join(sep)`：字符串列表拼接（Rust 的 slice::join 惯例）。
extern "C" fn any_join(addr: *const Dynamic, sep: *const Dynamic) -> *const Dynamic {
    if addr.is_null() || sep.is_null() {
        return alloc_dynamic(Dynamic::from(""));
    }
    let sep = unsafe { (&*sep).as_str() };
    if let Dynamic::List(list) = unsafe { &*addr } {
        let parts: Vec<String> = list.read().iter().map(|v| v.as_str().to_string()).collect();
        return alloc_dynamic(Dynamic::from(parts.join(sep)));
    }
    alloc_dynamic(Dynamic::from(""))
}

extern "C" fn get_idx(addr: *const Dynamic, idx: i64) -> *const Dynamic {
    if addr.is_null() {
        any_null()
    } else {
        // 负索引用 usize::try_from 拒绝(返回 Null),避免 idx as usize 把 -1 变成 usize::MAX 的越界隐患。
        // 与 typed-vec 路径(list_get_idx_value)行为一致。
        match usize::try_from(idx) {
            Ok(idx) => alloc_dynamic(unsafe { (*addr).get_idx(idx).unwrap_or(Dynamic::Null) }),
            Err(_) => any_null(),
        }
    }
}

fn list_get_idx_value(addr: *const Dynamic, idx: i64) -> Option<Dynamic> {
    if addr.is_null() {
        return None;
    }
    let Ok(idx) = usize::try_from(idx) else {
        return None;
    };
    unsafe { (&*addr).get_idx(idx) }
}

fn dynamic_as_int(value: Dynamic) -> Option<i64> {
    value.as_int().or_else(|| value.as_uint().and_then(|value| i64::try_from(value).ok()))
}

fn dynamic_as_uint(value: Dynamic) -> Option<u64> {
    value.as_uint().or_else(|| value.as_int().and_then(|value| u64::try_from(value).ok()))
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

myvec_list_native!(list_i8_push, list_i8_get_idx, list_i8_set_idx, VecI8, I8, i8, |value: Dynamic| dynamic_as_int(value).map(|value| value as i8));
myvec_list_native!(list_u16_push, list_u16_get_idx, list_u16_set_idx, VecU16, U16, u16, |value: Dynamic| dynamic_as_uint(value).map(|value| value as u16));
myvec_list_native!(list_i16_push, list_i16_get_idx, list_i16_set_idx, VecI16, I16, i16, |value: Dynamic| dynamic_as_int(value).map(|value| value as i16));
myvec_list_native!(list_u32_push, list_u32_get_idx, list_u32_set_idx, VecU32, U32, u32, |value: Dynamic| dynamic_as_uint(value).map(|value| value as u32));
myvec_list_native!(list_i32_push, list_i32_get_idx, list_i32_set_idx, VecI32, I32, i32, |value: Dynamic| dynamic_as_int(value).map(|value| value as i32));
myvec_list_native!(list_f32_push, list_f32_get_idx, list_f32_set_idx, VecF32, F32, f32, |value: Dynamic| value.as_float().map(|value| value as f32));
stdvec_list_native!(list_u64_push, list_u64_get_idx, list_u64_set_idx, VecU64, U64, u64, dynamic_as_uint);
stdvec_list_native!(list_i64_push, list_i64_get_idx, list_i64_set_idx, VecI64, I64, i64, dynamic_as_int);
stdvec_list_native!(list_f64_push, list_f64_get_idx, list_f64_set_idx, VecF64, F64, f64, |value: Dynamic| value.as_float());

extern "C" fn list_u64_data_ptr(addr: *const Dynamic) -> *const u64 {
    if addr.is_null() {
        return std::ptr::null();
    }
    unsafe {
        match &*addr {
            Dynamic::VecU64(values) => values.as_ptr(),
            _ => std::ptr::null(),
        }
    }
}

extern "C" fn list_i64_data_ptr(addr: *const Dynamic) -> *const i64 {
    if addr.is_null() {
        return std::ptr::null();
    }
    unsafe {
        match &*addr {
            Dynamic::VecI64(values) => values.as_ptr(),
            _ => std::ptr::null(),
        }
    }
}

extern "C" fn list_f64_data_ptr(addr: *const Dynamic) -> *const f64 {
    if addr.is_null() {
        return std::ptr::null();
    }
    unsafe {
        match &*addr {
            Dynamic::VecF64(values) => values.as_ptr(),
            _ => std::ptr::null(),
        }
    }
}

extern "C" fn list_i8_get_idx_i64(addr: *const Dynamic, idx: i64) -> i64 {
    list_get_idx_value(addr, idx).and_then(dynamic_as_int).map(|value| value as i8 as i64).unwrap_or_default()
}

extern "C" fn list_u16_get_idx_i64(addr: *const Dynamic, idx: i64) -> i64 {
    list_get_idx_value(addr, idx).and_then(dynamic_as_uint).map(|value| value as u16 as i64).unwrap_or_default()
}

extern "C" fn list_i16_get_idx_i64(addr: *const Dynamic, idx: i64) -> i64 {
    list_get_idx_value(addr, idx).and_then(dynamic_as_int).map(|value| value as i16 as i64).unwrap_or_default()
}

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
    unsafe { (&*addr).get_idx(idx).is_some_and(|value| value.is_true() || value.as_int().is_some_and(|value| value != 0) || value.as_uint().is_some_and(|value| value != 0)) }
}

extern "C" fn list_bool_get_idx_i64(addr: *const Dynamic, idx: i64) -> i64 {
    list_get_idx_value(addr, idx).map(|value| i64::from(value.is_true() || value.as_int().is_some_and(|value| value != 0) || value.as_uint().is_some_and(|value| value != 0))).unwrap_or_default()
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
    unsafe { (&*addr).get_idx(idx).and_then(dynamic_as_uint).map(|value| value as u8).unwrap_or(0) }
}

extern "C" fn list_u8_get_idx_i64(addr: *const Dynamic, idx: i64) -> i64 {
    list_get_idx_value(addr, idx).and_then(dynamic_as_uint).map(|value| value as u8 as i64).unwrap_or_default()
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
            Dynamic::List(list) => Dynamic::list(list.read()[start..stop].to_vec()),
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

extern "C" fn any_set(addr: *mut Dynamic, key: *const Dynamic, value: *const Dynamic) {
    if addr.is_null() || key.is_null() || value.is_null() {
        return;
    }
    if let Some(index) = unsafe { (&*key).as_int() } {
        set_idx(addr, index, value);
    } else {
        set_key(addr, key, value);
    }
}

extern "C" fn any_set_return(addr: *mut Dynamic, key: *const Dynamic, value: *const Dynamic) -> *const Dynamic {
    any_set(addr, key, value);
    if addr.is_null() { any_null() } else { alloc_dynamic(unsafe { (&*addr).clone() }) }
}

extern "C" fn set_idx(addr: *mut Dynamic, idx: i64, value: *const Dynamic) {
    if addr.is_null() {
        return;
    }
    // 负索引用 usize::try_from 拒绝(静默不写入),避免 idx as usize 的越界隐患。
    // 与 typed-vec 路径行为一致。
    if let Ok(idx) = usize::try_from(idx) {
        unsafe { (&mut *addr).set_idx(idx, (&*value).clone()) }
    }
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

extern "C" fn any_to_yaml(addr: *const Dynamic) -> *const Dynamic {
    if addr.is_null() {
        return alloc_dynamic(Dynamic::from(""));
    }
    let mut buf = String::new();
    unsafe { &*addr }.to_yaml(&mut buf);
    alloc_dynamic(Dynamic::from(buf))
}

extern "C" fn any_from_yaml(addr: *const Dynamic) -> *const Dynamic {
    if addr.is_null() {
        return alloc_dynamic(Dynamic::Null);
    }
    let text = unsafe { &*addr }.as_str();
    match Dynamic::from_yaml_buf(text.as_bytes()) {
        Ok(value) => alloc_dynamic(value),
        Err(_) => alloc_dynamic(Dynamic::Null),
    }
}

extern "C" fn any_to_json(addr: *const Dynamic) -> *const Dynamic {
    if addr.is_null() {
        return alloc_dynamic(Dynamic::from("null"));
    }
    let mut buf = String::new();
    unsafe { &*addr }.to_json(&mut buf);
    alloc_dynamic(Dynamic::from(buf))
}

extern "C" fn any_from_json(addr: *const Dynamic) -> *const Dynamic {
    if addr.is_null() {
        return any_null();
    }
    match Dynamic::from_json(unsafe { (&*addr).as_str().as_bytes() }) {
        Ok((value, _)) => alloc_dynamic(value),
        Err(_) => any_null(),
    }
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
    let op = BinaryOp::try_from(op).unwrap_or(BinaryOp::Unknow);
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

extern "C" fn any_logic(left: *const Dynamic, op: i32, right: *const Dynamic) -> bool {
    let op = BinaryOp::try_from(op).unwrap_or(BinaryOp::Unknow);
    unsafe {
        let expr = Expr::new(
            ExprKind::Binary { left: Box::new(Expr::new(ExprKind::Value((&*left).clone()), Span::default())), op, right: Box::new(Expr::new(ExprKind::Value((&*right).clone()), Span::default())) },
            Span::default(),
        );
        expr.compact().and_then(|r| r.as_bool()).unwrap_or(false)
    }
}

pub const STD: [(&str, &[Type], Type, *const u8); 34] = [
    ("print", &[Type::Any], Type::Void, print as *const u8),
    ("sqrt", &[Type::F64], Type::F64, sqrt as *const u8),
    ("sleep", &[Type::I64], Type::Void, sleep as *const u8),
    ("log", &[Type::Any], Type::Void, log_any as *const u8),
    ("uuid", &[], Type::Any, uuid as *const u8),
    ("rand", &[Type::Any, Type::Any], Type::Any, random as *const u8),
    ("env", &[Type::Str], Type::Any, env as *const u8),
    ("len", &[Type::Any], Type::I32, any_len as *const u8),
    ("to_string", &[Type::Any], Type::Str, any_to_string as *const u8),
    ("str", &[Type::Any], Type::Str, any_to_string as *const u8),
    ("to_int", &[Type::Any], Type::I64, any_to_i64 as *const u8),
    ("int", &[Type::Any], Type::I64, any_to_i64 as *const u8),
    ("parse_int", &[Type::Any], Type::I64, any_to_i64 as *const u8),
    ("to_number", &[Type::Any], Type::F64, any_to_f64 as *const u8),
    ("parse_number", &[Type::Any], Type::F64, any_to_f64 as *const u8),
    ("num", &[Type::Any], Type::F64, any_to_f64 as *const u8),
    ("format_number", &[Type::Any], Type::Str, any_to_string as *const u8),
    ("join", &[Type::Any, Type::Any], Type::Any, any_join as *const u8),
    ("push", &[Type::Any, Type::Any], Type::Any, any_append as *const u8),
    ("append", &[Type::Any, Type::Any], Type::Any, any_append as *const u8),
    ("get", &[Type::Any, Type::Any], Type::Any, any_get as *const u8),
    ("set", &[Type::Any, Type::Any, Type::Any], Type::Any, any_set_return as *const u8),
    ("contains_key", &[Type::Any, Type::Any], Type::Bool, contains as *const u8),
    ("replace_all", &[Type::Any, Type::Any, Type::Any], Type::Any, any_replace as *const u8),
    ("rfind", &[Type::Any, Type::Any], Type::I64, any_rfind as *const u8),
    ("substring", &[Type::Any, Type::I64, Type::Any], Type::Any, any_substring as *const u8),
    ("byte_len", &[Type::Any], Type::I64, any_byte_len as *const u8),
    ("byte_slice", &[Type::Any, Type::I64, Type::Any], Type::Any, any_byte_slice as *const u8),
    ("parse_json", &[Type::Any], Type::Any, any_from_json as *const u8),
    ("from_json", &[Type::Any], Type::Any, any_from_json as *const u8),
    ("to_json", &[Type::Any], Type::Str, any_to_json as *const u8),
    ("json_dump", &[Type::Any], Type::Str, any_to_json as *const u8),
    ("is_number", &[Type::Any], Type::Bool, any_is_number as *const u8),
    ("is_integer", &[Type::Any], Type::Bool, any_is_integer as *const u8),
];

pub const ANY: [(&str, &[Type], Type, *const u8); 118] = [
    ("Any::null", &[], Type::Any, any_null as *const u8),
    ("Any::is_map", &[Type::Any], Type::Bool, any_is_map as *const u8),
    ("Any::is_list", &[Type::Any], Type::Bool, any_is_list as *const u8),
    ("Any::is_string", &[Type::Any], Type::Bool, any_is_string as *const u8),
    ("Any::is_null", &[Type::Any], Type::Bool, any_is_null as *const u8),
    ("Any::is_number", &[Type::Any], Type::Bool, any_is_number as *const u8),
    ("Any::is_integer", &[Type::Any], Type::Bool, any_is_integer as *const u8),
    ("Any::is_bool", &[Type::Any], Type::Bool, any_is_bool as *const u8),
    ("Any::is_int", &[Type::Any], Type::Bool, any_is_int as *const u8),
    ("Any::is_float", &[Type::Any], Type::Bool, any_is_float as *const u8),
    ("Any::is_bool", &[Type::Any], Type::Bool, any_is_bool as *const u8),
    ("Any::is_int", &[Type::Any], Type::Bool, any_is_int as *const u8),
    ("Any::is_float", &[Type::Any], Type::Bool, any_is_float as *const u8),
    ("Any::is_empty", &[Type::Any], Type::Bool, any_is_empty as *const u8),
    ("Any::clone", &[Type::Any], Type::Any, any_clone as *const u8),
    ("Any::len", &[Type::Any], Type::I32, any_len as *const u8),
    ("Any::length", &[Type::Any], Type::I32, any_len as *const u8),
    ("Any::keys", &[Type::Any], Type::Any, any_keys as *const u8),
    ("Any::split", &[Type::Any, Type::Any], Type::Any, any_split as *const u8),
    ("Any::join", &[Type::Any, Type::Any], Type::Any, any_join as *const u8),
    ("Any::push", &[Type::Any, Type::Any], Type::Void, any_push as *const u8),
    ("Any::append", &[Type::Any, Type::Any], Type::Any, any_append as *const u8),
    ("Any::add", &[Type::Any, Type::Any], Type::Any, any_append as *const u8),
    ("Any::pop", &[Type::Any], Type::Any, any_pop as *const u8),
    ("Any::insert", &[Type::Any, Type::Any, Type::Any], Type::Bool, any_insert as *const u8),
    ("Any::get_idx", &[Type::Any, Type::I64], Type::Any, get_idx as *const u8),
    ("Any::push_bool", &[Type::Any, Type::Bool], Type::Void, list_bool_push as *const u8),
    ("Any::get_idx_bool", &[Type::Any, Type::I64], Type::Bool, list_bool_get_idx as *const u8),
    ("Any::get_idx_bool_i64", &[Type::Any, Type::I64], Type::I64, list_bool_get_idx_i64 as *const u8),
    ("Any::set_idx_bool", &[Type::Any, Type::I64, Type::Bool], Type::Void, list_bool_set_idx as *const u8),
    ("Any::push_u8", &[Type::Any, Type::U8], Type::Void, list_u8_push as *const u8),
    ("Any::get_idx_u8", &[Type::Any, Type::I64], Type::U8, list_u8_get_idx as *const u8),
    ("Any::get_idx_u8_i64", &[Type::Any, Type::I64], Type::I64, list_u8_get_idx_i64 as *const u8),
    ("Any::set_idx_u8", &[Type::Any, Type::I64, Type::U8], Type::Void, list_u8_set_idx as *const u8),
    ("Any::push_i8", &[Type::Any, Type::I8], Type::Void, list_i8_push as *const u8),
    ("Any::get_idx_i8", &[Type::Any, Type::I64], Type::I8, list_i8_get_idx as *const u8),
    ("Any::get_idx_i8_i64", &[Type::Any, Type::I64], Type::I64, list_i8_get_idx_i64 as *const u8),
    ("Any::set_idx_i8", &[Type::Any, Type::I64, Type::I8], Type::Void, list_i8_set_idx as *const u8),
    ("Any::push_u16", &[Type::Any, Type::U16], Type::Void, list_u16_push as *const u8),
    ("Any::get_idx_u16", &[Type::Any, Type::I64], Type::U16, list_u16_get_idx as *const u8),
    ("Any::get_idx_u16_i64", &[Type::Any, Type::I64], Type::I64, list_u16_get_idx_i64 as *const u8),
    ("Any::set_idx_u16", &[Type::Any, Type::I64, Type::U16], Type::Void, list_u16_set_idx as *const u8),
    ("Any::push_i16", &[Type::Any, Type::I16], Type::Void, list_i16_push as *const u8),
    ("Any::get_idx_i16", &[Type::Any, Type::I64], Type::I16, list_i16_get_idx as *const u8),
    ("Any::get_idx_i16_i64", &[Type::Any, Type::I64], Type::I64, list_i16_get_idx_i64 as *const u8),
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
    ("Any::data_ptr_u64", &[Type::Any], Type::Any, list_u64_data_ptr as *const u8),
    ("Any::get_idx_u64", &[Type::Any, Type::I64], Type::U64, list_u64_get_idx as *const u8),
    ("Any::set_idx_u64", &[Type::Any, Type::I64, Type::U64], Type::Void, list_u64_set_idx as *const u8),
    ("Any::push_i64", &[Type::Any, Type::I64], Type::Void, list_i64_push as *const u8),
    ("Any::data_ptr_i64", &[Type::Any], Type::Any, list_i64_data_ptr as *const u8),
    ("Any::get_idx_i64", &[Type::Any, Type::I64], Type::I64, list_i64_get_idx as *const u8),
    ("Any::set_idx_i64", &[Type::Any, Type::I64, Type::I64], Type::Void, list_i64_set_idx as *const u8),
    ("Any::push_f64", &[Type::Any, Type::F64], Type::Void, list_f64_push as *const u8),
    ("Any::data_ptr_f64", &[Type::Any], Type::Any, list_f64_data_ptr as *const u8),
    ("Any::get_idx_f64", &[Type::Any, Type::I64], Type::F64, list_f64_get_idx as *const u8),
    ("Any::set_idx_f64", &[Type::Any, Type::I64, Type::F64], Type::Void, list_f64_set_idx as *const u8),
    ("Any::push_str", &[Type::Any, Type::Str], Type::Void, list_str_push as *const u8),
    ("Any::get_idx_str", &[Type::Any, Type::I64], Type::Str, list_str_get_idx as *const u8),
    ("Any::set_idx_str", &[Type::Any, Type::I64, Type::Str], Type::Void, list_str_set_idx as *const u8),
    ("Any::slice", &[Type::Any, Type::I64, Type::Any, Type::Bool], Type::Any, slice as *const u8),
    ("Any::contains", &[Type::Any, Type::Any], Type::Bool, contains as *const u8),
    ("Any::contains_key", &[Type::Any, Type::Any], Type::Bool, contains as *const u8),
    ("Any::starts_with", &[Type::Any, Type::Any], Type::Bool, starts_with as *const u8),
    ("Any::ends_with", &[Type::Any, Type::Any], Type::Bool, ends_with as *const u8),
    ("Any::trim", &[Type::Any], Type::Any, any_trim as *const u8),
    ("Any::trim_start", &[Type::Any], Type::Any, any_trim_start as *const u8),
    ("Any::trim_end", &[Type::Any], Type::Any, any_trim_end as *const u8),
    ("Any::trim_matches", &[Type::Any, Type::Any], Type::Any, any_trim_matches as *const u8),
    ("Any::trim_start_matches", &[Type::Any, Type::Any], Type::Any, any_trim_start_matches as *const u8),
    ("Any::trim_end_matches", &[Type::Any, Type::Any], Type::Any, any_trim_end_matches as *const u8),
    ("Any::to_lower", &[Type::Any], Type::Any, any_to_lower as *const u8),
    ("Any::to_upper", &[Type::Any], Type::Any, any_to_upper as *const u8),
    ("Any::replace", &[Type::Any, Type::Any, Type::Any], Type::Any, any_replace as *const u8),
    ("Any::replace_all", &[Type::Any, Type::Any, Type::Any], Type::Any, any_replace as *const u8),
    ("Any::find", &[Type::Any, Type::Any, Type::Any], Type::I64, any_find as *const u8),
    ("Any::rfind", &[Type::Any, Type::Any], Type::I64, any_rfind as *const u8),
    ("Any::substring", &[Type::Any, Type::I64, Type::Any], Type::Any, any_substring as *const u8),
    ("Any::byte_len", &[Type::Any], Type::I64, any_byte_len as *const u8),
    ("Any::byte_slice", &[Type::Any, Type::I64, Type::Any], Type::Any, any_byte_slice as *const u8),
    ("Any::sort", &[Type::Any], Type::Void, any_sort as *const u8),
    ("Any::get", &[Type::Any, Type::Any], Type::Any, any_get as *const u8),
    ("Any::get_key", &[Type::Any, Type::Any], Type::Any, get_key as *const u8),
    ("Any::del_key", &[Type::Any, Type::Any], Type::Any, del_key as *const u8),
    ("Any::set_idx", &[Type::Any, Type::I64, Type::Any], Type::Void, set_idx as *const u8),
    ("Any::set_key", &[Type::Any, Type::Any, Type::Any], Type::Void, set_key as *const u8),
    ("Any::set", &[Type::Any, Type::Any, Type::Any], Type::Void, any_set as *const u8),
    ("Any::from_i64", &[Type::I64], Type::Any, any_from_i64 as *const u8),
    ("Any::from_u64", &[Type::U64], Type::Any, any_from_u64 as *const u8),
    ("Any::from_bool", &[Type::Bool], Type::Any, any_from_bool as *const u8),
    ("Any::to_i64", &[Type::Any], Type::I64, any_to_i64 as *const u8),
    ("Any::to_int", &[Type::Any], Type::I64, any_to_i64 as *const u8),
    ("Any::parse_int", &[Type::Any], Type::I64, any_to_i64 as *const u8),
    ("Any::to_bool", &[Type::Any], Type::Bool, any_to_bool as *const u8),
    ("Any::from_f64", &[Type::F64], Type::Any, any_from_f64 as *const u8),
    ("Any::to_f64", &[Type::Any], Type::F64, any_to_f64 as *const u8),
    ("Any::to_number", &[Type::Any], Type::F64, any_to_f64 as *const u8),
    ("Any::parse_number", &[Type::Any], Type::F64, any_to_f64 as *const u8),
    ("Any::to_string", &[Type::Any], Type::Str, any_to_string as *const u8),
    ("Any::to_yaml", &[Type::Any], Type::Str, any_to_yaml as *const u8),
    ("Any::from_yaml", &[Type::Any], Type::Any, any_from_yaml as *const u8),
    ("Any::to_json", &[Type::Any], Type::Str, any_to_json as *const u8),
    ("Any::from_json", &[Type::Any], Type::Any, any_from_json as *const u8),
    ("Any::binary", &[Type::Any, Type::I32, Type::Any], Type::Any, any_binary as *const u8),
    ("Any::logic", &[Type::Any, Type::I32, Type::Any], Type::Bool, any_logic as *const u8),
    ("Any::iter", &[Type::Any], Type::Any, any_iter as *const u8),
    ("Any::next", &[Type::Any], Type::Any, any_next as *const u8),
    ("Any::next_pair", &[Type::Any], Type::Any, any_next_pair as *const u8),
];

use std::rc::Rc;
impl JITRunTime {
    pub fn add_native_ptr(&mut self, full_name: &str, name: &str, arg_tys: &[Type], ret_ty: Type, fn_ptr: *const u8) -> Result<u32> {
        self.native_symbols.write().insert(full_name.to_string(), fn_ptr as usize);
        self.add_native(full_name, name, arg_tys, ret_ty)
    }

    pub(crate) fn add_context_native_ptr(&mut self, full_name: &str, name: &str, arg_tys: &[Type], ret_ty: Type, fn_ptr: *const u8) -> Result<u32> {
        self.native_symbols.write().insert(full_name.to_string(), fn_ptr as usize);
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
