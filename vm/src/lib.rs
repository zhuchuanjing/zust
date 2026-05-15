//使用 cranelift 作为后端 直接 jit 解释脚本
mod binary;
mod native;
pub use native::{ANY, STD};

mod fns;
use anyhow::{Result, anyhow};
pub use fns::{FnInfo, FnVariant};
mod context;
use context::BuildContext;

mod rt;
use cranelift::prelude::types;
use dynamic::Type;
pub use rt::JITRunTime;
use smol_str::SmolStr;
mod http_module;
mod llm_module;
mod root_module;

use std::sync::{OnceLock, RwLock};
static PTR_TYPE: OnceLock<types::Type> = OnceLock::new();
pub fn ptr_type() -> types::Type {
    PTR_TYPE.get().cloned().unwrap()
}

pub fn get_type(ty: &Type) -> Result<types::Type> {
    if ty.is_f64() {
        Ok(types::F64)
    } else if ty.is_f32() {
        Ok(types::F32)
    } else if ty.is_int() | ty.is_uint() {
        match ty.width() {
            1 => Ok(types::I8),
            2 => Ok(types::I16),
            4 => Ok(types::I32),
            8 => Ok(types::I64),
            _ => Err(anyhow!("非法类型 {:?}", ty)),
        }
    } else if let Type::Bool = ty {
        Ok(types::I8)
    } else {
        Ok(ptr_type())
    }
}

use compiler::Symbol;
use cranelift::prelude::*;

pub fn init_jit(mut jit: JITRunTime) -> Result<JITRunTime> {
    jit.compiler.symbols.add_module("std".into()); //开始导入标准库,可以直接访问
    for std in STD {
        jit.add_native(std.0, std.0, std.1, std.2)?;
    }

    let mut fields = Vec::new();
    for (name, arg_tys, ret_ty, _) in ANY {
        let id = jit.add_native(name, name, arg_tys, ret_ty)?;
        let (_, field_name) = name.split_once("::").unwrap();
        fields.push((field_name.into(), Type::Symbol { id, params: Vec::new() }));
    }
    jit.compiler.add_symbol("Any", Symbol::Struct(Type::Struct { params: Vec::new(), fields }, true));

    jit.compiler.add_symbol("Vec", Symbol::Struct(Type::Struct { params: Vec::new(), fields: Vec::new() }, true));
    let vec_def = Type::Symbol { id: jit.get_id("Vec")?, params: Vec::new() };
    jit.add_inline("Vec::swap", vec![vec_def.clone(), Type::I64, Type::I64], Type::Void, |ctx: Option<&mut BuildContext>, args: Vec<Value>| {
        if let Some(ctx) = ctx {
            let width = ctx.builder.ins().iconst(types::I64, 4);
            let offset_val = ctx.builder.ins().imul(args[1], width); // i * 4 i32大小四字节
            let final_addr = ctx.builder.ins().iadd(args[0], offset_val); // base + (i*4)
            let dest = ctx.builder.ins().imul(args[2], width);
            let dest_addr = ctx.builder.ins().iadd(args[0], dest); // base + (i*4)
            let dest_val = ctx.builder.ins().load(types::I32, MemFlags::trusted(), dest_addr, 0);
            let v = ctx.builder.ins().load(types::I32, MemFlags::trusted(), final_addr, 0);
            ctx.builder.ins().store(MemFlags::trusted(), v, dest_addr, 0);
            ctx.builder.ins().store(MemFlags::trusted(), dest_val, final_addr, 0);
        }
        Err(anyhow!("无返回值"))
    })?;

    jit.add_inline("Vec::get_idx", vec![vec_def.clone(), Type::I64], Type::I32, |ctx: Option<&mut BuildContext>, args: Vec<Value>| {
        if let Some(ctx) = ctx {
            let width = ctx.builder.ins().iconst(types::I64, 4);
            let offset_val = ctx.builder.ins().imul(args[1], width); // i * 4 i32大小四字节
            let final_addr = ctx.builder.ins().iadd(args[0], offset_val);
            Ok((Some(ctx.builder.ins().load(types::I32, MemFlags::trusted(), final_addr, 0)), Type::I32))
        } else {
            Ok((None, Type::I32))
        }
    })?;
    Ok(jit)
}

use std::sync::Arc;

use std::sync::LazyLock;
unsafe impl Send for JITRunTime {}
unsafe impl Sync for JITRunTime {}

//直接在这里增加一行 就可以导入一个模块
static mut MODULES: &[(&str, &[(&str, &[Type], Type, *const u8)])] = &[("llm", &llm_module::LLM_NATIVE), ("root", &root_module::ROOT_NATIVE), ("http", &http_module::HTTP_NATIVE)];

pub static JIT: LazyLock<Arc<RwLock<JITRunTime>>> = LazyLock::new(|| {
    let jit = JITRunTime::new(|b| {
        //这里注册所有的外部符号
        for (name, _, _, fn_ptr) in STD {
            b.symbol(name, fn_ptr);
        }
        for (name, _, _, fn_ptr) in ANY {
            b.symbol(name, fn_ptr);
        }
        for (name, fns) in unsafe { MODULES.into_iter() } {
            for (fn_name, _, _, fn_ptr) in *fns {
                let full_name = format!("{}::{}", *name, *fn_name);
                b.symbol(&full_name, *fn_ptr);
            }
        }
    });
    let mut jit = init_jit(jit).unwrap();
    for (name, fns) in unsafe { MODULES.into_iter() } {
        jit.compiler.symbols.add_module((*name).into());
        for r in fns.into_iter() {
            let full_name = format!("{}::{}", *name, r.0);
            jit.add_native(&&full_name, r.0, r.1, r.2.clone()).unwrap();
        }
        jit.compiler.symbols.pop_module();
    }
    Arc::new(RwLock::new(jit))
});

pub fn import_code(name: &str, code: Vec<u8>) -> Result<()> {
    JIT.write().unwrap().import_code(name, code)
}

pub fn import(name: &str, path: &str) -> Result<()> {
    if root::contains(path) {
        //优先从 root 文件系统里面 import
        let code = root::get(path).unwrap();
        if code.is_str() {
            JIT.write().unwrap().import_code(name, code.as_str().as_bytes().to_vec())
        } else {
            JIT.write().unwrap().import_code(name, code.get_dynamic("code").ok_or(anyhow!("{:?} 没有 code 成员", code))?.as_str().as_bytes().to_vec())
        }
    } else {
        JIT.write().unwrap().compiler.import_file(name, path)?;
        Ok(())
    }
}

pub fn infer(name: &str, arg_tys: &[Type]) -> Result<Type> {
    JIT.write().unwrap().get_type(name, arg_tys)
}

pub fn get_fn(name: &str, arg_tys: &[Type]) -> Result<(*const u8, Type)> {
    JIT.write().unwrap().get_fn_ptr(name, arg_tys)
}

pub fn load(code: Vec<u8>, arg_name: SmolStr) -> Result<(i64, Type)> {
    JIT.write().unwrap().load(code, arg_name)
}

pub fn get_symbol(name: &str, params: Vec<Type>) -> Result<Type> {
    Ok(Type::Symbol { id: JIT.read().unwrap().get_id(name)?, params })
}

pub fn disassemble(name: &str) -> Result<String> {
    JIT.read().unwrap().compiler.symbols.disassemble(name)
}

#[cfg(feature = "ir-disassembly")]
pub fn disassemble_ir(name: &str) -> Result<String> {
    JIT.write().unwrap().disassemble_ir(name)
}

#[cfg(test)]
mod tests {
    use super::{get_fn, import_code};
    use dynamic::{Dynamic, Type};

    #[test]
    fn compares_any_with_string_literal_as_string() -> anyhow::Result<()> {
        import_code(
            "vm_string_compare_any",
            br#"
            pub fn any_ne_empty(chat_path) {
                chat_path != ""
            }
            "#
            .to_vec(),
        )?;

        let (fn_ptr, ret_ty) = get_fn("vm_string_compare_any::any_ne_empty", &[Type::Any])?;
        assert_eq!(ret_ty, Type::Bool);

        let any_ne_empty: extern "C" fn(*const Dynamic) -> bool = unsafe { std::mem::transmute(fn_ptr) };
        let empty = Dynamic::from("");
        let non_empty = Dynamic::from("chat");

        assert!(!any_ne_empty(&empty));
        assert!(any_ne_empty(&non_empty));
        Ok(())
    }

    #[test]
    fn compares_concrete_value_with_string_literal_as_string() -> anyhow::Result<()> {
        import_code(
            "vm_string_compare_imm",
            br#"
            pub fn int_eq_str(value: i64) {
                value == "42"
            }

            pub fn int_to_str(value: i64) {
                value + ""
            }
            "#
            .to_vec(),
        )?;

        let (fn_ptr, ret_ty) = get_fn("vm_string_compare_imm::int_eq_str", &[Type::I64])?;
        assert_eq!(ret_ty, Type::Bool);

        let int_eq_str: extern "C" fn(i64) -> bool = unsafe { std::mem::transmute(fn_ptr) };

        let (fn_ptr, ret_ty) = get_fn("vm_string_compare_imm::int_to_str", &[Type::I64])?;
        assert_eq!(ret_ty, Type::Any);
        let int_to_str: extern "C" fn(i64) -> *const Dynamic = unsafe { std::mem::transmute(fn_ptr) };
        let text = int_to_str(42);
        assert_eq!(unsafe { &*text }.as_str(), "42");

        assert!(int_eq_str(42));
        assert!(!int_eq_str(7));
        Ok(())
    }
}
