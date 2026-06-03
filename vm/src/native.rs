use super::FnVariant;
use crate::JITRunTime;
use crate::memory::{alloc_dynamic, alloc_struct_bytes};
use anyhow::Result;
use cranelift::prelude::AbiParam;
use cranelift_module::{Linkage, Module};
use dynamic::{Dynamic, Type};
use parser::{BinaryOp, Expr, ExprKind, Span};
use rand::RngExt;
use std::sync::{Mutex, Weak};

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

extern "C" fn log_any(addr: *const Dynamic) {
    if addr.is_null() {
        log::info!("{:?}", Dynamic::Null);
    } else {
        log::info!("{:?}", unsafe { &*addr });
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

pub(crate) extern "C" fn struct_from_ptr(addr: i64, ty: i64) -> *const Dynamic {
    let ty = unsafe { (&*(ty as *const Type)).clone() };
    alloc_dynamic(Dynamic::Struct { addr: addr as usize, ty })
}

pub(crate) extern "C" fn import_with_vm(context: *const Weak<Mutex<JITRunTime>>, addr: *const Dynamic, path: *const Dynamic) -> bool {
    if addr.is_null() || path.is_null() {
        return false;
    }
    super::with_vm_context(context, |vm| vm.import(unsafe { &*addr }.as_str(), unsafe { &*path }.as_str())).map_err(|e| println!("import {:?}", e)).is_ok()
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

extern "C" fn any_push(addr: *mut Dynamic, value: *mut Dynamic) {
    if !addr.is_null() && !value.is_null() {
        unsafe {
            (&mut *addr).push((&*value).clone());
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

    let sliced = match value {
        Dynamic::String(text) => Dynamic::from(text.chars().skip(start).take(stop.saturating_sub(start)).collect::<String>()),
        Dynamic::List(list) => Dynamic::list(list.read().unwrap()[start..stop].to_vec()),
        _ => Dynamic::Null,
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

extern "C" fn any_from_bool(v: bool) -> *const Dynamic {
    alloc_dynamic(Dynamic::Bool(v))
}

extern "C" fn any_to_i64(addr: *const Dynamic) -> i64 {
    if addr.is_null() {
        return 0;
    }
    unsafe {
        let value = &*addr;
        value.as_int().or_else(|| value.as_float().map(|value| value as i64)).unwrap_or(0)
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
    unsafe { (&*addr).as_float().unwrap_or(0.0) }
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

pub const STD: [(&str, &[Type], Type, *const u8); 4] = [
    ("print", &[Type::Any], Type::Void, print as *const u8),
    ("log", &[Type::Any], Type::Void, log_any as *const u8),
    ("uuid", &[], Type::Any, uuid as *const u8),
    ("rand", &[Type::Any, Type::Any], Type::Any, random as *const u8),
];

pub const ANY: [(&str, &[Type], Type, *const u8); 30] = [
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
    ("Any::slice", &[Type::Any, Type::I64, Type::Any, Type::Bool], Type::Any, slice as *const u8),
    ("Any::contains", &[Type::Any, Type::Any], Type::Bool, contains as *const u8),
    ("Any::starts_with", &[Type::Any, Type::Any], Type::Bool, starts_with as *const u8),
    ("Any::get_key", &[Type::Any, Type::Any], Type::Any, get_key as *const u8),
    ("Any::del_key", &[Type::Any, Type::Any], Type::Any, del_key as *const u8),
    ("Any::set_idx", &[Type::Any, Type::I64, Type::Any], Type::Void, set_idx as *const u8),
    ("Any::set_key", &[Type::Any, Type::Any, Type::Any], Type::Void, set_key as *const u8),
    ("Any::from_i64", &[Type::I64], Type::Any, any_from_i64 as *const u8),
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
