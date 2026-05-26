//使用 cranelift 作为后端 直接 jit 解释脚本
mod binary;
mod memory;
mod native;
pub use native::{ANY, STD};

mod fns;
use anyhow::{Result, anyhow};
pub use fns::{FnInfo, FnVariant};
mod context;
pub use context::BuildContext;

mod rt;
use cranelift::prelude::types;
use dynamic::Type;
pub use rt::JITRunTime;
use smol_str::SmolStr;
mod db_module;
mod gpu_layout;
mod gpu_module;
mod http_module;
mod llm_module;
mod root_module;
pub use gpu_layout::{GpuFieldLayout, GpuStructLayout};

use std::sync::{Mutex, OnceLock, Weak};
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
use cranelift_module::Module;

pub fn init_jit(mut jit: JITRunTime) -> Result<JITRunTime> {
    jit.add_all()?;
    Ok(jit)
}

use std::sync::Arc;
unsafe impl Send for JITRunTime {}
unsafe impl Sync for JITRunTime {}

pub(crate) fn with_vm_context<T>(context: *const Weak<Mutex<JITRunTime>>, f: impl FnOnce(&Vm) -> Result<T>) -> Result<T> {
    if context.is_null() {
        return Err(anyhow!("VM context is null"));
    }
    let jit = unsafe { &*context }.upgrade().ok_or_else(|| anyhow!("VM context has expired"))?;
    let vm = Vm { jit };
    f(&vm)
}

fn add_method_field(jit: &mut JITRunTime, def: &str, method: &str, id: u32) -> Result<()> {
    let def_id = jit.get_id(def)?;
    if let Some((_, define)) = jit.compiler.symbols.get_symbol_mut(def_id) {
        if let Symbol::Struct(Type::Struct { params, fields }, _) = define {
            fields.push((method.into(), Type::Symbol { id, params: params.clone() }));
        }
    }
    Ok(())
}

fn add_native_module_fns(jit: &mut JITRunTime, module: &str, fns: &[(&str, &[Type], Type, *const u8)]) -> Result<()> {
    jit.add_module(module);
    for (name, arg_tys, ret_ty, fn_ptr) in fns {
        let full_name = format!("{}::{}", module, name);
        jit.add_native_ptr(&full_name, name, arg_tys, ret_ty.clone(), *fn_ptr)?;
    }
    jit.pop_module();
    Ok(())
}

impl JITRunTime {
    fn add_memory_runtime(&mut self) -> Result<()> {
        self.native_symbols.write().unwrap().insert("__vm_scope_enter".to_string(), memory::scope_enter as *const () as usize);
        self.native_symbols.write().unwrap().insert("__vm_scope_exit_void".to_string(), memory::scope_exit_void as *const () as usize);
        self.native_symbols.write().unwrap().insert("__vm_scope_exit_dynamic".to_string(), memory::scope_exit_dynamic as *const () as usize);
        self.native_symbols.write().unwrap().insert("__vm_scope_exit_bytes".to_string(), memory::scope_exit_bytes as *const () as usize);
        self.native_symbols.write().unwrap().insert("__vm_struct_alloc".to_string(), native::struct_alloc as *const () as usize);
        self.native_symbols.write().unwrap().insert("__vm_struct_from_ptr".to_string(), native::struct_from_ptr as *const () as usize);

        let void_sig = self.get_sig(&[], Type::Void)?;
        self.scope_enter_fn = Some(self.module.declare_function("__vm_scope_enter", cranelift_module::Linkage::Import, &void_sig)?);
        self.scope_exit_void_fn = Some(self.module.declare_function("__vm_scope_exit_void", cranelift_module::Linkage::Import, &void_sig)?);

        let dynamic_sig = self.get_sig(&[Type::Any], Type::Any)?;
        self.scope_exit_dynamic_fn = Some(self.module.declare_function("__vm_scope_exit_dynamic", cranelift_module::Linkage::Import, &dynamic_sig)?);

        let bytes_sig = self.get_sig(&[Type::Any, Type::I64], Type::Any)?;
        self.scope_exit_bytes_fn = Some(self.module.declare_function("__vm_scope_exit_bytes", cranelift_module::Linkage::Import, &bytes_sig)?);

        let struct_alloc_sig = self.get_sig(&[Type::I64], Type::Any)?;
        self.struct_alloc_fn = Some(self.module.declare_function("__vm_struct_alloc", cranelift_module::Linkage::Import, &struct_alloc_sig)?);

        let struct_from_ptr_sig = self.get_sig(&[Type::I64, Type::I64], Type::Any)?;
        self.struct_from_ptr_fn = Some(self.module.declare_function("__vm_struct_from_ptr", cranelift_module::Linkage::Import, &struct_from_ptr_sig)?);
        Ok(())
    }

    pub fn add_module(&mut self, name: &str) {
        self.compiler.symbols.add_module(name.into());
    }

    pub fn pop_module(&mut self) {
        self.compiler.symbols.pop_module();
    }

    pub fn add_type(&mut self, name: &str, ty: Type, is_pub: bool) -> u32 {
        self.compiler.add_symbol(name, Symbol::Struct(ty, is_pub))
    }

    pub fn add_empty_type(&mut self, name: &str) -> Result<u32> {
        match self.get_id(name) {
            Ok(id) => Ok(id),
            Err(_) => Ok(self.add_type(name, Type::Struct { params: Vec::new(), fields: Vec::new() }, true)),
        }
    }

    pub fn add_native_module_ptr(&mut self, module: &str, name: &str, arg_tys: &[Type], ret_ty: Type, fn_ptr: *const u8) -> Result<u32> {
        self.add_module(module);
        let full_name = format!("{}::{}", module, name);
        let result = self.add_native_ptr(&full_name, name, arg_tys, ret_ty, fn_ptr);
        self.pop_module();
        result
    }

    pub(crate) fn add_native_module_context_ptr(&mut self, module: &str, name: &str, arg_tys: &[Type], ret_ty: Type, fn_ptr: *const u8) -> Result<u32> {
        self.add_module(module);
        let full_name = format!("{}::{}", module, name);
        let result = self.add_context_native_ptr(&full_name, name, arg_tys, ret_ty, fn_ptr);
        self.pop_module();
        result
    }

    pub fn add_native_method_ptr(&mut self, def: &str, method: &str, arg_tys: &[Type], ret_ty: Type, fn_ptr: *const u8) -> Result<u32> {
        self.add_empty_type(def)?;
        let full_name = format!("{}::{}", def, method);
        let id = self.add_native_ptr(&full_name, &full_name, arg_tys, ret_ty, fn_ptr)?;
        add_method_field(self, def, method, id)?;
        Ok(id)
    }

    pub fn add_std(&mut self) -> Result<()> {
        if self.compiler.symbols.get_id("std::print").is_ok() {
            return Ok(());
        }
        self.add_module("std");
        for (name, arg_tys, ret_ty, fn_ptr) in STD {
            self.add_native_ptr(name, name, arg_tys, ret_ty, fn_ptr)?;
        }
        self.add_context_native_ptr("import", "import", &[Type::Any, Type::Any], Type::Bool, native::import_with_vm as *const u8)?;
        Ok(())
    }

    pub fn add_any(&mut self) -> Result<()> {
        if self.compiler.symbols.get_id("Any").is_ok() && self.compiler.symbols.get_id("Any::is_map").is_ok() {
            return Ok(());
        }
        for (name, arg_tys, ret_ty, fn_ptr) in ANY {
            let (_, method) = name.split_once("::").ok_or_else(|| anyhow!("非法 Any 方法名 {}", name))?;
            self.add_native_method_ptr("Any", method, arg_tys, ret_ty, fn_ptr)?;
        }
        Ok(())
    }

    pub fn add_vec(&mut self) -> Result<()> {
        self.add_empty_type("Vec")?;
        let vec_def = Type::Symbol { id: self.get_id("Vec")?, params: Vec::new() };
        self.add_inline("Vec::swap", vec![vec_def.clone(), Type::I64, Type::I64], Type::Void, |ctx: Option<&mut BuildContext>, args: Vec<Value>| {
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

        self.add_inline("Vec::get_idx", vec![vec_def.clone(), Type::I64], Type::I32, |ctx: Option<&mut BuildContext>, args: Vec<Value>| {
            if let Some(ctx) = ctx {
                let width = ctx.builder.ins().iconst(types::I64, 4);
                let offset_val = ctx.builder.ins().imul(args[1], width); // i * 4 i32大小四字节
                let final_addr = ctx.builder.ins().iadd(args[0], offset_val);
                Ok((Some(ctx.builder.ins().load(types::I32, MemFlags::trusted(), final_addr, 0)), Type::I32))
            } else {
                Ok((None, Type::I32))
            }
        })?;
        Ok(())
    }

    pub fn add_llm(&mut self) -> Result<()> {
        add_native_module_fns(self, "llm", &llm_module::LLM_NATIVE)
    }

    pub fn add_root(&mut self) -> Result<()> {
        add_native_module_fns(self, "root", &root_module::ROOT_NATIVE)?;
        self.add_native_module_context_ptr("root", "add_fn", &[Type::Any, Type::Any], Type::Bool, root_module::root_add_fn_with_vm as *const u8)?;
        Ok(())
    }

    pub fn add_http(&mut self) -> Result<()> {
        add_native_module_fns(self, "http", &http_module::HTTP_NATIVE)
    }

    pub fn add_db(&mut self) -> Result<()> {
        add_native_module_fns(self, "db", &db_module::DB_NATIVE)
    }

    pub fn add_gpu(&mut self) -> Result<()> {
        add_native_module_fns(self, "gpu", &gpu_module::GPU_NATIVE)
    }

    pub fn add_all(&mut self) -> Result<()> {
        self.add_std()?;
        self.add_any()?;
        self.add_vec()?;
        self.add_llm()?;
        self.add_root()?;
        self.add_http()?;
        self.add_db()?;
        self.add_gpu()?;
        Ok(())
    }
}

#[derive(Clone)]
pub struct Vm {
    jit: Arc<Mutex<JITRunTime>>,
}

#[derive(Clone)]
pub struct CompiledFn {
    ptr: usize,
    ret: Type,
    owner: Vm,
}

impl CompiledFn {
    pub fn ptr(&self) -> *const u8 {
        self.ptr as *const u8
    }

    pub fn ret_ty(&self) -> &Type {
        &self.ret
    }

    pub fn owner(&self) -> &Vm {
        &self.owner
    }
}

impl Vm {
    pub fn new() -> Self {
        dynamic::set_dynamic_return_handler(memory::take_dynamic_return);
        let jit = Arc::new(Mutex::new(JITRunTime::new(|_| {})));
        {
            let mut guard = jit.lock().unwrap();
            guard.set_owner(Arc::downgrade(&jit));
            guard.add_memory_runtime().expect("register VM memory runtime");
            guard.add_std().expect("register VM std runtime");
            guard.add_any().expect("register VM Any runtime");
        }
        Self { jit }
    }

    pub fn with_all() -> Result<Self> {
        let vm = Self::new();
        vm.add_all()?;
        Ok(vm)
    }

    pub fn add_module(&self, name: &str) {
        self.jit.lock().unwrap().add_module(name)
    }

    pub fn pop_module(&self) {
        self.jit.lock().unwrap().pop_module()
    }

    pub fn add_type(&self, name: &str, ty: Type, is_pub: bool) -> u32 {
        self.jit.lock().unwrap().add_type(name, ty, is_pub)
    }

    pub fn add_empty_type(&self, name: &str) -> Result<u32> {
        self.jit.lock().unwrap().add_empty_type(name)
    }

    pub fn add_std(&self) -> Result<()> {
        self.jit.lock().unwrap().add_std()
    }

    pub fn add_any(&self) -> Result<()> {
        self.jit.lock().unwrap().add_any()
    }

    pub fn add_vec(&self) -> Result<()> {
        self.jit.lock().unwrap().add_vec()
    }

    pub fn add_llm(&self) -> Result<()> {
        self.jit.lock().unwrap().add_llm()
    }

    pub fn add_root(&self) -> Result<()> {
        self.jit.lock().unwrap().add_root()
    }

    pub fn add_http(&self) -> Result<()> {
        self.jit.lock().unwrap().add_http()
    }

    pub fn add_db(&self) -> Result<()> {
        self.jit.lock().unwrap().add_db()
    }

    pub fn add_gpu(&self) -> Result<()> {
        self.jit.lock().unwrap().add_gpu()
    }

    pub fn add_all(&self) -> Result<()> {
        self.jit.lock().unwrap().add_all()
    }

    pub fn add_native_ptr(&self, full_name: &str, name: &str, arg_tys: &[Type], ret_ty: Type, fn_ptr: *const u8) -> Result<u32> {
        self.jit.lock().unwrap().add_native_ptr(full_name, name, arg_tys, ret_ty, fn_ptr)
    }

    pub fn add_native_module_ptr(&self, module: &str, name: &str, arg_tys: &[Type], ret_ty: Type, fn_ptr: *const u8) -> Result<u32> {
        self.jit.lock().unwrap().add_native_module_ptr(module, name, arg_tys, ret_ty, fn_ptr)
    }

    pub fn add_native_method_ptr(&self, def: &str, method: &str, arg_tys: &[Type], ret_ty: Type, fn_ptr: *const u8) -> Result<u32> {
        self.jit.lock().unwrap().add_native_method_ptr(def, method, arg_tys, ret_ty, fn_ptr)
    }

    pub fn add_inline(&self, name: &str, args: Vec<Type>, ret: Type, f: fn(Option<&mut BuildContext>, Vec<Value>) -> Result<(Option<Value>, Type)>) -> Result<u32> {
        self.jit.lock().unwrap().add_inline(name, args, ret, f)
    }

    pub fn import_code(&self, name: &str, code: Vec<u8>) -> Result<()> {
        self.jit.lock().unwrap().import_code(name, code)
    }

    pub fn import_file(&self, name: &str, path: &str) -> Result<()> {
        self.jit.lock().unwrap().compiler.import_file(name, path)?;
        Ok(())
    }

    pub fn import(&self, name: &str, path: &str) -> Result<()> {
        if root::contains(path) {
            let code = root::get(path).unwrap();
            if code.is_str() {
                self.import_code(name, code.as_str().as_bytes().to_vec())
            } else {
                self.import_code(name, code.get_dynamic("code").ok_or(anyhow!("{:?} 没有 code 成员", code))?.as_str().as_bytes().to_vec())
            }
        } else {
            self.import_file(name, path)
        }
    }

    pub fn infer(&self, name: &str, arg_tys: &[Type]) -> Result<Type> {
        self.jit.lock().unwrap().get_type(name, arg_tys)
    }

    pub fn get_fn_ptr(&self, name: &str, arg_tys: &[Type]) -> Result<(*const u8, Type)> {
        self.jit.lock().unwrap().get_fn_ptr(name, arg_tys)
    }

    pub fn get_fn(&self, name: &str, arg_tys: &[Type]) -> Result<CompiledFn> {
        let (ptr, ret) = self.get_fn_ptr(name, arg_tys)?;
        Ok(CompiledFn { ptr: ptr as usize, ret, owner: self.clone() })
    }

    pub fn load(&self, code: Vec<u8>, arg_name: SmolStr) -> Result<(i64, Type)> {
        self.jit.lock().unwrap().load(code, arg_name)
    }

    pub fn get_symbol(&self, name: &str, params: Vec<Type>) -> Result<Type> {
        Ok(Type::Symbol { id: self.jit.lock().unwrap().get_id(name)?, params })
    }

    pub fn gpu_struct_layout(&self, name: &str, params: &[Type]) -> Result<GpuStructLayout> {
        let jit = self.jit.lock().unwrap();
        GpuStructLayout::from_symbol_table(&jit.compiler.symbols, name, params)
    }

    pub fn disassemble(&self, name: &str) -> Result<String> {
        self.jit.lock().unwrap().compiler.symbols.disassemble(name)
    }

    #[cfg(feature = "ir-disassembly")]
    pub fn disassemble_ir(&self, name: &str) -> Result<String> {
        self.jit.lock().unwrap().disassemble_ir(name)
    }
}

impl Default for Vm {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::Vm;
    use dynamic::{Dynamic, ToJson, Type};

    extern "C" fn math_double(value: i64) -> i64 {
        value * 2
    }

    #[test]
    fn vm_can_add_native_after_jit_creation() -> anyhow::Result<()> {
        let vm = Vm::new();
        vm.add_native_module_ptr("math", "double", &[Type::I64], Type::I64, math_double as *const u8)?;
        vm.import_code(
            "vm_dynamic_native",
            br#"
            pub fn run(value: i64) {
                math::double(value)
            }
            "#
            .to_vec(),
        )?;

        let compiled = vm.get_fn("vm_dynamic_native::run", &[Type::I64])?;
        assert_eq!(compiled.ret_ty(), &Type::I64);
        let run: extern "C" fn(i64) -> i64 = unsafe { std::mem::transmute(compiled.ptr()) };
        assert_eq!(run(21), 42);
        Ok(())
    }

    #[test]
    fn vm_new_registers_std_and_any() -> anyhow::Result<()> {
        let vm = Vm::new();
        vm.add_std()?;
        vm.add_any()?;
        assert_eq!(vm.infer("std::print", &[Type::Any])?, Type::Void);

        vm.import_code(
            "vm_new_default_any",
            br#"
            pub fn has_items(content) {
                if content.is_map() {
                    if content.contains("items") {
                        return content.items.len() > 0;
                    }
                }
                false
            }
            "#
            .to_vec(),
        )?;

        assert_eq!(vm.infer("vm_new_default_any::has_items", &[Type::Any])?, Type::Bool);
        let compiled = vm.get_fn("vm_new_default_any::has_items", &[Type::Any])?;
        assert_eq!(compiled.ret_ty(), &Type::Bool);
        Ok(())
    }

    #[test]
    fn nested_struct_arg_return_struct_field_is_static_field_access() -> anyhow::Result<()> {
        let vm = Vm::with_all()?;
        vm.import_code(
            "vm_nested_struct_return_field",
            br#"
            pub struct Inner {
                value: i64,
            }

            pub struct RoleMini {
                inner: Inner,
                hp: i64,
            }

            pub struct TeamMini {
                role: RoleMini,
            }

            pub struct BigSummary {
                winner: i64,
                loser: i64,
            }

            pub fn make_big_with_team(team: TeamMini) {
                let score = team.role.inner.value;
                BigSummary{winner: score, loser: 0}
            }

            pub fn read_team_winner_direct() {
                let team = TeamMini{role: RoleMini{inner: Inner{value: 9}, hp: 1}};
                make_big_with_team(team).winner
            }

            pub fn read_team_winner_bound() {
                let team = TeamMini{role: RoleMini{inner: Inner{value: 9}, hp: 1}};
                let summary = make_big_with_team(team);
                summary.winner
            }
            "#
            .to_vec(),
        )?;

        let compiled = vm.get_fn("vm_nested_struct_return_field::read_team_winner_direct", &[])?;
        assert_eq!(compiled.ret_ty(), &Type::I64);
        let direct: extern "C" fn() -> i64 = unsafe { std::mem::transmute(compiled.ptr()) };
        assert_eq!(direct(), 9);

        let compiled = vm.get_fn("vm_nested_struct_return_field::read_team_winner_bound", &[])?;
        assert_eq!(compiled.ret_ty(), &Type::I64);
        let bound: extern "C" fn() -> i64 = unsafe { std::mem::transmute(compiled.ptr()) };
        assert_eq!(bound(), 9);
        Ok(())
    }

    #[test]
    fn any_push_does_not_consume_reused_value() -> anyhow::Result<()> {
        let vm = Vm::with_all()?;
        vm.import_code(
            "vm_any_push_reused_value",
            br#"
            pub fn run() {
                let role_id = "acct_role_2";
                let updated = [];
                updated.push(role_id);
                {
                    ok: true,
                    user_id: role_id,
                    first: updated.get_idx(0)
                }
            }
            "#
            .to_vec(),
        )?;

        let compiled = vm.get_fn("vm_any_push_reused_value::run", &[])?;
        assert_eq!(compiled.ret_ty(), &Type::Any);
        let run: extern "C" fn() -> *const Dynamic = unsafe { std::mem::transmute(compiled.ptr()) };
        let result = unsafe { &*run() };
        assert_eq!(result.get_dynamic("ok").and_then(|value| value.as_bool()), Some(true));
        assert_eq!(result.get_dynamic("user_id").map(|value| value.as_str().to_string()), Some("acct_role_2".to_string()));
        assert_eq!(result.get_dynamic("first").map(|value| value.as_str().to_string()), Some("acct_role_2".to_string()));
        Ok(())
    }

    #[test]
    fn compares_any_with_string_literal_as_string() -> anyhow::Result<()> {
        let vm = Vm::with_all()?;
        vm.import_code(
            "vm_string_compare_any",
            br#"
            pub fn any_ne_empty(chat_path) {
                chat_path != ""
            }
            "#
            .to_vec(),
        )?;

        let compiled = vm.get_fn("vm_string_compare_any::any_ne_empty", &[Type::Any])?;
        assert_eq!(compiled.ret_ty(), &Type::Bool);

        let any_ne_empty: extern "C" fn(*const Dynamic) -> bool = unsafe { std::mem::transmute(compiled.ptr()) };
        let empty = Dynamic::from("");
        let non_empty = Dynamic::from("chat");

        assert!(!any_ne_empty(&empty));
        assert!(any_ne_empty(&non_empty));
        Ok(())
    }

    #[test]
    fn compares_bool_values_and_bool_literals() -> anyhow::Result<()> {
        let vm = Vm::with_all()?;
        vm.import_code(
            "vm_bool_compare",
            br#"
            pub fn eq_true(value: bool) {
                value == true
            }

            pub fn ne_false(value: bool) {
                value != false
            }

            pub fn literal_left(value: bool) {
                true == value
            }

            pub fn eq_pair(left: bool, right: bool) {
                left == right
            }

            pub fn logic_pair(left: bool, right: bool) {
                (left && right) || (left == true && right != false)
            }
            "#
            .to_vec(),
        )?;

        let compiled = vm.get_fn("vm_bool_compare::eq_true", &[Type::Bool])?;
        assert_eq!(compiled.ret_ty(), &Type::Bool);
        let eq_true: extern "C" fn(bool) -> bool = unsafe { std::mem::transmute(compiled.ptr()) };
        assert!(eq_true(true));
        assert!(!eq_true(false));

        let compiled = vm.get_fn("vm_bool_compare::ne_false", &[Type::Bool])?;
        assert_eq!(compiled.ret_ty(), &Type::Bool);
        let ne_false: extern "C" fn(bool) -> bool = unsafe { std::mem::transmute(compiled.ptr()) };
        assert!(ne_false(true));
        assert!(!ne_false(false));

        let compiled = vm.get_fn("vm_bool_compare::literal_left", &[Type::Bool])?;
        assert_eq!(compiled.ret_ty(), &Type::Bool);
        let literal_left: extern "C" fn(bool) -> bool = unsafe { std::mem::transmute(compiled.ptr()) };
        assert!(literal_left(true));
        assert!(!literal_left(false));

        let compiled = vm.get_fn("vm_bool_compare::eq_pair", &[Type::Bool, Type::Bool])?;
        assert_eq!(compiled.ret_ty(), &Type::Bool);
        let eq_pair: extern "C" fn(bool, bool) -> bool = unsafe { std::mem::transmute(compiled.ptr()) };
        assert!(eq_pair(true, true));
        assert!(eq_pair(false, false));
        assert!(!eq_pair(true, false));
        assert!(!eq_pair(false, true));

        let compiled = vm.get_fn("vm_bool_compare::logic_pair", &[Type::Bool, Type::Bool])?;
        assert_eq!(compiled.ret_ty(), &Type::Bool);
        let logic_pair: extern "C" fn(bool, bool) -> bool = unsafe { std::mem::transmute(compiled.ptr()) };
        assert!(logic_pair(true, true));
        assert!(!logic_pair(true, false));
        assert!(!logic_pair(false, true));
        assert!(!logic_pair(false, false));
        Ok(())
    }

    #[test]
    fn parenthesized_expression_can_call_any_method() -> anyhow::Result<()> {
        let vm = Vm::with_all()?;
        vm.import_code(
            "vm_parenthesized_method_call",
            br#"
            pub fn run(value) {
                (value + 2).to_i64()
            }
            "#
            .to_vec(),
        )?;

        let compiled = vm.get_fn("vm_parenthesized_method_call::run", &[Type::Any])?;
        assert_eq!(compiled.ret_ty(), &Type::I64);
        let run: extern "C" fn(*const Dynamic) -> i64 = unsafe { std::mem::transmute(compiled.ptr()) };
        let value = Dynamic::from(40i64);

        assert_eq!(run(&value), 42);
        Ok(())
    }

    #[test]
    fn any_keys_returns_map_keys_and_empty_list_for_other_values() -> anyhow::Result<()> {
        let vm = Vm::with_all()?;
        vm.import_code(
            "vm_any_keys",
            br#"
            pub fn map_keys(value) {
                let keys = value.keys();
                keys.len() == 2 && keys.contains("alpha") && keys.contains("beta")
            }

            pub fn non_map_keys(value) {
                value.keys().len() == 0
            }
            "#
            .to_vec(),
        )?;

        let compiled = vm.get_fn("vm_any_keys::map_keys", &[Type::Any])?;
        assert_eq!(compiled.ret_ty(), &Type::Bool);
        let map_keys: extern "C" fn(*const Dynamic) -> bool = unsafe { std::mem::transmute(compiled.ptr()) };
        let value = dynamic::map!("alpha"=> 1i64, "beta"=> 2i64);
        assert!(map_keys(&value));

        let compiled = vm.get_fn("vm_any_keys::non_map_keys", &[Type::Any])?;
        assert_eq!(compiled.ret_ty(), &Type::Bool);
        let non_map_keys: extern "C" fn(*const Dynamic) -> bool = unsafe { std::mem::transmute(compiled.ptr()) };
        let value = Dynamic::from("alpha");
        assert!(non_map_keys(&value));
        Ok(())
    }

    #[test]
    fn compares_concrete_value_with_string_literal_as_string() -> anyhow::Result<()> {
        let vm = Vm::with_all()?;
        vm.import_code(
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

        let compiled = vm.get_fn("vm_string_compare_imm::int_eq_str", &[Type::I64])?;
        assert_eq!(compiled.ret_ty(), &Type::Bool);

        let int_eq_str: extern "C" fn(i64) -> bool = unsafe { std::mem::transmute(compiled.ptr()) };

        let compiled = vm.get_fn("vm_string_compare_imm::int_to_str", &[Type::I64])?;
        assert_eq!(compiled.ret_ty(), &Type::Any);
        let int_to_str: extern "C" fn(i64) -> *const Dynamic = unsafe { std::mem::transmute(compiled.ptr()) };
        let text = int_to_str(42);
        assert_eq!(unsafe { &*text }.as_str(), "42");

        assert!(int_eq_str(42));
        assert!(!int_eq_str(7));
        Ok(())
    }

    #[test]
    fn concatenates_string_with_integer_values() -> anyhow::Result<()> {
        let vm = Vm::with_all()?;
        vm.import_code(
            "vm_string_concat_integer",
            br#"
            pub fn idx_key(idx: i64) {
                "" + idx
            }

            pub fn level_text(level: i64) {
                "" + level + " level"
            }

            pub fn gold_text(currency) {
                "" + currency.gold
            }
            "#
            .to_vec(),
        )?;

        let compiled = vm.get_fn("vm_string_concat_integer::idx_key", &[Type::I64])?;
        let idx_key: extern "C" fn(i64) -> *const Dynamic = unsafe { std::mem::transmute(compiled.ptr()) };
        let result = unsafe { &*idx_key(7) };
        assert_eq!(result.as_str(), "7");

        let compiled = vm.get_fn("vm_string_concat_integer::level_text", &[Type::I64])?;
        let level_text: extern "C" fn(i64) -> *const Dynamic = unsafe { std::mem::transmute(compiled.ptr()) };
        let result = unsafe { &*level_text(12) };
        assert_eq!(result.as_str(), "12 level");

        let compiled = vm.get_fn("vm_string_concat_integer::gold_text", &[Type::Any])?;
        let gold_text: extern "C" fn(*const Dynamic) -> *const Dynamic = unsafe { std::mem::transmute(compiled.ptr()) };
        let currency = dynamic::map!("gold"=> 345i64);
        let result = unsafe { &*gold_text(&currency) };
        assert_eq!(result.as_str(), "345");
        Ok(())
    }

    #[test]
    fn coerces_string_concat_to_i64_without_unimplemented_log() -> anyhow::Result<()> {
        let vm = Vm::with_all()?;
        vm.import_code(
            "vm_string_concat_to_i64",
            br#"
            pub fn run(idx: i64) {
                ("" + idx) as i64
            }
            "#
            .to_vec(),
        )?;

        let compiled = vm.get_fn("vm_string_concat_to_i64::run", &[Type::I64])?;
        assert_eq!(compiled.ret_ty(), &Type::I64);
        let run: extern "C" fn(i64) -> i64 = unsafe { std::mem::transmute(compiled.ptr()) };
        assert_eq!(run(7), 0);
        Ok(())
    }

    #[test]
    fn unifies_explicit_return_and_tail_integer_widths() -> anyhow::Result<()> {
        let vm = Vm::with_all()?;
        vm.import_code(
            "vm_return_integer_widths",
            br#"
            pub fn selected(flag, slot) {
                if flag {
                    return slot;
                }
                0
            }
            "#
            .to_vec(),
        )?;

        let compiled = vm.get_fn("vm_return_integer_widths::selected", &[Type::Bool, Type::I64])?;
        assert_eq!(compiled.ret_ty(), &Type::I64);
        let selected: extern "C" fn(bool, i64) -> i64 = unsafe { std::mem::transmute(compiled.ptr()) };

        assert_eq!(selected(true, 7), 7);
        assert_eq!(selected(false, 7), 0);
        Ok(())
    }

    #[test]
    fn root_contains_string_concat_is_bool_condition() -> anyhow::Result<()> {
        let vm = Vm::with_all()?;
        vm.import_code(
            "vm_root_contains_condition",
            br#"
            pub fn exists(user_id) {
                if root::contains("redis/user/" + user_id) {
                    return 1;
                }
                0
            }
            "#
            .to_vec(),
        )?;

        assert_eq!(vm.infer("root::contains", &[Type::Any])?, Type::Bool);
        let compiled = vm.get_fn("vm_root_contains_condition::exists", &[Type::Any])?;
        assert_eq!(compiled.ret_ty(), &Type::I32);
        Ok(())
    }

    #[test]
    fn root_add_map_can_be_printed() -> anyhow::Result<()> {
        let vm = Vm::with_all()?;
        assert_eq!(vm.infer("root::add_map", &[Type::Any])?, Type::Bool);
        vm.import_code(
            "vm_root_add_map_print",
            br#"
            pub fn run() {
                print(root::add_map("local/world_handlers/til_map_novicevillage"));
            }
            "#
            .to_vec(),
        )?;

        let compiled = vm.get_fn("vm_root_add_map_print::run", &[])?;
        assert!(compiled.ret_ty().is_void());
        Ok(())
    }

    #[test]
    fn std_log_accepts_any_and_returns_void() -> anyhow::Result<()> {
        let vm = Vm::with_all()?;
        vm.import_code(
            "vm_std_log",
            br#"
            pub fn run(value) {
                log({ ok: true, value: value });
            }
            "#
            .to_vec(),
        )?;

        let compiled = vm.get_fn("vm_std_log::run", &[Type::Any])?;
        assert!(compiled.ret_ty().is_void());
        let run: extern "C" fn(*const Dynamic) = unsafe { std::mem::transmute(compiled.ptr()) };
        let value = Dynamic::from(7i64);
        run(&value);
        Ok(())
    }

    #[test]
    fn unary_not_any_loop_var_is_bool_condition() -> anyhow::Result<()> {
        let vm = Vm::with_all()?;
        vm.import_code(
            "vm_unary_not_any_loop_var",
            br#"
            pub fn count_missing(flags) {
                let missing = 0;
                for exists in flags {
                    if !exists {
                        missing = missing + 1;
                    }
                }
                missing
            }
            "#
            .to_vec(),
        )?;

        let compiled = vm.get_fn("vm_unary_not_any_loop_var::count_missing", &[Type::Any])?;
        assert_eq!(compiled.ret_ty(), &Type::I32);
        Ok(())
    }

    #[test]
    fn semicolon_tail_call_makes_function_void() -> anyhow::Result<()> {
        let vm = Vm::with_all()?;
        vm.import_code(
            "vm_semicolon_tail_void",
            br#"
            pub fn send_role_select(idx, account_id, selected_slot) {
                root::send("local/ui/send_dialog", {
                    idx: idx,
                    account_id: account_id,
                    selected_slot: selected_slot
                });
            }
            "#
            .to_vec(),
        )?;

        let compiled = vm.get_fn("vm_semicolon_tail_void::send_role_select", &[Type::Any, Type::Any, Type::Any])?;
        assert_eq!(compiled.ret_ty(), &Type::Void);
        Ok(())
    }

    #[test]
    fn bare_return_conflicts_with_non_void_return() -> anyhow::Result<()> {
        let vm = Vm::with_all()?;
        vm.import_code(
            "vm_bare_return_conflict",
            br#"
            pub fn run(flag) {
                if flag {
                    return;
                }
                1
            }
            "#
            .to_vec(),
        )?;

        let err = match vm.get_fn("vm_bare_return_conflict::run", &[Type::Bool]) {
            Ok(_) => panic!("expected mismatched return types to fail"),
            Err(err) => err,
        };
        assert!(format!("{err:#}").contains("返回类型不一致"));
        Ok(())
    }

    #[test]
    fn root_get_accepts_string_concat_with_dynamic_field() -> anyhow::Result<()> {
        let vm = Vm::with_all()?;
        vm.import_code(
            "vm_root_get_dynamic_concat",
            br#"
            pub fn get_action(req) {
                root::get("local/game/panel_actions/" + req.idx)
            }
            "#
            .to_vec(),
        )?;

        root::add("local/game/panel_actions/7", dynamic::map!("id"=> "action-7").into())?;
        let compiled = vm.get_fn("vm_root_get_dynamic_concat::get_action", &[Type::Any])?;
        assert_eq!(compiled.ret_ty(), &Type::Any);
        let get_action: extern "C" fn(*const Dynamic) -> *const Dynamic = unsafe { std::mem::transmute(compiled.ptr()) };
        let req = dynamic::map!("idx"=> 7i64);
        let result = unsafe { &*get_action(&req) };

        assert_eq!(result.get_dynamic("id").map(|value| value.as_str().to_string()), Some("action-7".to_string()));
        Ok(())
    }

    #[test]
    fn root_add_fn_registers_handler_with_dynamic_field_path_concat() -> anyhow::Result<()> {
        let vm = Vm::with_all()?;
        vm.import_code(
            "vm_registered_panel_action",
            br#"
            pub fn panel_action(req) {
                root::get("local/game/panel_actions/" + req.idx)
            }

            pub fn register() {
                root::add_fn("local/ui/panel_action", "vm_registered_panel_action::panel_action")
            }
            "#
            .to_vec(),
        )?;

        let compiled = vm.get_fn("vm_registered_panel_action::register", &[])?;
        assert_eq!(compiled.ret_ty(), &Type::Bool);
        let register: extern "C" fn() -> bool = unsafe { std::mem::transmute(compiled.ptr()) };
        assert!(register());
        Ok(())
    }

    #[test]
    fn root_add_fn_accepts_string_concat_in_registered_handler() -> anyhow::Result<()> {
        let vm = Vm::with_all()?;
        vm.import_code(
            "vm_registered_string_concat",
            br#"
            pub fn send_panel(idx: i64) {
                let idx_key = "" + idx;
                idx_key
            }
            "#
            .to_vec(),
        )?;

        assert!(vm.get_fn_ptr("vm_registered_string_concat::send_panel", &[Type::Any]).is_ok());
        Ok(())
    }

    #[test]
    fn compiles_public_hotspots_with_string_paths_and_keys() -> anyhow::Result<()> {
        let vm = Vm::with_all()?;
        vm.import_code(
            "vm_public_hotspots",
            br#"
            pub fn public_hotspot(action_map_path, panel_id, action_id, hotspot) {
                {
                    path: action_map_path,
                    panel_id: panel_id,
                    action_id: action_id,
                    id: hotspot.id
                }
            }

            pub fn public_hotspots(idx, panel_id, hotspots) {
                let idx_key = "" + idx;
                let action_map_path = "local/game/panel_actions/" + idx_key;

                let existing_action_map = root::get(action_map_path);
                if !existing_action_map.is_map() {
                    root::add_map(action_map_path);
                }

                if hotspots.is_map() {
                    let public_items = {};
                    for action_id in hotspots.keys() {
                        public_items[action_id] = public_hotspot(action_map_path, panel_id, action_id, hotspots[action_id]);
                    }
                    return public_items;
                }

                let public_items = [];
                let i = 0;
                while i < hotspots.len() {
                    let hotspot = hotspots.get_idx(i);
                    let item = public_hotspot(action_map_path, panel_id, hotspot.id, hotspot);
                    public_items.push(item);
                    i = i + 1;
                }

                public_items
            }
            "#
            .to_vec(),
        )?;

        assert!(vm.get_fn("vm_public_hotspots::public_hotspots", &[Type::I64, Type::Any, Type::Any]).is_ok());
        assert!(vm.get_fn("vm_public_hotspots::public_hotspots", &[Type::Any, Type::Any, Type::Any]).is_ok());
        Ok(())
    }

    #[test]
    fn send_panel_calls_public_hotspots_with_dynamic_request() -> anyhow::Result<()> {
        let vm = Vm::with_all()?;
        vm.import_code(
            "vm_send_panel_public_hotspots",
            br#"
            pub fn ok(value) {
                value
            }

            pub fn panel_from_node(req) {
                {
                    panel_id: req.panel_id,
                    hotspots: req.hotspots
                }
            }

            pub fn public_hotspot(action_map_path, panel_id, action_id, hotspot) {
                {
                    path: action_map_path,
                    panel_id: panel_id,
                    action_id: action_id,
                    id: hotspot.id
                }
            }

            pub fn public_hotspots(idx, panel_id, hotspots) {
                let idx_key = "" + idx;
                let action_map_path = "local/game/panel_actions/" + idx_key;

                let existing_action_map = root::get(action_map_path);
                if !existing_action_map.is_map() {
                    root::add_map(action_map_path);
                }

                if hotspots.is_map() {
                    let public_items = {};
                    for action_id in hotspots.keys() {
                        public_items[action_id] = public_hotspot(action_map_path, panel_id, action_id, hotspots[action_id]);
                    }
                    return public_items;
                }

                let public_items = [];
                let i = 0;
                while i < hotspots.len() {
                    let hotspot = hotspots.get_idx(i);
                    let item = public_hotspot(action_map_path, panel_id, hotspot.id, hotspot);
                    public_items.push(item);
                    i = i + 1;
                }

                public_items
            }

            pub fn send_panel(req) {
                let panel = req.panel;
                if !panel.is_map() {
                    panel = panel_from_node(req);
                }
                if !panel.is_map() {
                    return ok({
                        id: 4,
                        type: "panel_rejected",
                        reason: "invalid panel"
                    });
                }
                panel.id = 4;
                panel.idx = req.idx;
                if !panel.contains("type") {
                    panel.type = "panel";
                }
                if panel.contains("hotspots") {
                    panel.hotspots = public_hotspots(req.idx, panel.panel_id, panel.hotspots);
                }
                root::send_idx("local/ws", req.idx, panel);
                ok({
                    id: 4,
                    type: "panel",
                    panel_id: panel.panel_id
                })
            }
            "#
            .to_vec(),
        )?;

        let compiled = vm.get_fn("vm_send_panel_public_hotspots::send_panel", &[Type::Any])?;
        assert_eq!(compiled.ret_ty(), &Type::Any);
        let send_panel: extern "C" fn(*const Dynamic) -> *const Dynamic = unsafe { std::mem::transmute(compiled.ptr()) };
        let req = dynamic::map!(
            "idx"=> 7i64,
            "panel"=> dynamic::map!(
                "panel_id"=> "main",
                "hotspots"=> dynamic::map!(
                    "open"=> dynamic::map!("id"=> "open")
                )
            )
        );
        let result = unsafe { &*send_panel(&req) };

        assert_eq!(result.get_dynamic("type").map(|value| value.as_str().to_string()), Some("panel".to_string()));
        assert_eq!(result.get_dynamic("panel_id").map(|value| value.as_str().to_string()), Some("main".to_string()));
        Ok(())
    }

    #[test]
    fn map_assignment_accepts_string_concat_key() -> anyhow::Result<()> {
        let vm = Vm::with_all()?;
        vm.import_code(
            "vm_string_concat_map_key",
            br##"
            pub fn write_action(action_map, panel_id, action_id, action) {
                action_map[panel_id + "#" + action_id] = action;
                action_map[panel_id + "#" + action_id]
            }
            "##
            .to_vec(),
        )?;

        let compiled = vm.get_fn("vm_string_concat_map_key::write_action", &[Type::Any, Type::Any, Type::Any, Type::Any])?;
        let write_action: extern "C" fn(*const Dynamic, *const Dynamic, *const Dynamic, *const Dynamic) -> *const Dynamic = unsafe { std::mem::transmute(compiled.ptr()) };
        let action_map = dynamic::map!();
        let panel_id: Dynamic = "panel".into();
        let action_id: Dynamic = "open".into();
        let action = dynamic::map!("id"=> "open");

        let result = unsafe { &*write_action(&action_map, &panel_id, &action_id, &action) };

        assert_eq!(result.get_dynamic("id").map(|value| value.as_str().to_string()), Some("open".to_string()));
        assert_eq!(action_map.get_dynamic("panel#open").and_then(|value| value.get_dynamic("id")).map(|value| value.as_str().to_string()), Some("open".to_string()));
        Ok(())
    }

    #[test]
    fn map_get_key_accepts_string_concat_key_variable() -> anyhow::Result<()> {
        let vm = Vm::with_all()?;
        vm.import_code(
            "vm_get_key_string_concat_key",
            br##"
            pub fn read_action(action_map, panel_id, action_id) {
                let action_key = panel_id + "#" + action_id;
                action_map.get_key(action_key)
            }
            "##
            .to_vec(),
        )?;

        let compiled = vm.get_fn("vm_get_key_string_concat_key::read_action", &[Type::Any, Type::Any, Type::Any])?;
        let read_action: extern "C" fn(*const Dynamic, *const Dynamic, *const Dynamic) -> *const Dynamic = unsafe { std::mem::transmute(compiled.ptr()) };
        let action_map = dynamic::map!("panel#open"=> dynamic::map!("id"=> "open"));
        let panel_id: Dynamic = "panel".into();
        let action_id: Dynamic = "open".into();

        let result = unsafe { &*read_action(&action_map, &panel_id, &action_id) };

        assert_eq!(result.get_dynamic("id").map(|value| value.as_str().to_string()), Some("open".to_string()));
        Ok(())
    }

    #[test]
    fn map_get_key_accepts_helper_string_key() -> anyhow::Result<()> {
        let vm = Vm::with_all()?;
        vm.import_code(
            "vm_get_key_helper_string_key",
            br##"
            pub fn make_action_key(panel_id, action_id) {
                panel_id + "#" + action_id
            }

            pub fn read_action(action_map, panel_id, action_id) {
                let action_key = make_action_key(panel_id, action_id);
                let action = action_map.get_key(action_key);
                action
            }
            "##
            .to_vec(),
        )?;

        let compiled = vm.get_fn("vm_get_key_helper_string_key::read_action", &[Type::Any, Type::Any, Type::Any])?;
        let read_action: extern "C" fn(*const Dynamic, *const Dynamic, *const Dynamic) -> *const Dynamic = unsafe { std::mem::transmute(compiled.ptr()) };
        let action_map = dynamic::map!("panel#open"=> dynamic::map!("id"=> "open"));
        let panel_id: Dynamic = "panel".into();
        let action_id: Dynamic = "open".into();

        let result = unsafe { &*read_action(&action_map, &panel_id, &action_id) };

        assert_eq!(result.get_dynamic("id").map(|value| value.as_str().to_string()), Some("open".to_string()));
        Ok(())
    }

    #[test]
    fn map_del_key_removes_string_key_and_returns_removed_value() -> anyhow::Result<()> {
        let vm = Vm::with_all()?;
        vm.import_code(
            "vm_del_key_string_key",
            br##"
            pub fn remove_action(action_map, panel_id, action_id) {
                let action_key = panel_id + "#" + action_id;
                let removed = action_map.del_key(action_key);
                [removed, action_map.get_key(action_key)]
            }
            "##
            .to_vec(),
        )?;

        let compiled = vm.get_fn("vm_del_key_string_key::remove_action", &[Type::Any, Type::Any, Type::Any])?;
        let remove_action: extern "C" fn(*const Dynamic, *const Dynamic, *const Dynamic) -> *const Dynamic = unsafe { std::mem::transmute(compiled.ptr()) };
        let action_map = dynamic::map!("panel#open"=> dynamic::map!("id"=> "open"));
        let panel_id: Dynamic = "panel".into();
        let action_id: Dynamic = "open".into();

        let result = unsafe { &*remove_action(&action_map, &panel_id, &action_id) };

        assert_eq!(result.get_idx(0).and_then(|value| value.get_dynamic("id")).map(|value| value.as_str().to_string()), Some("open".to_string()));
        assert!(result.get_idx(1).is_some_and(|value| value.is_null()));
        assert!(action_map.get_dynamic("panel#open").is_none());
        Ok(())
    }

    #[test]
    fn dynamic_field_value_participates_in_or_expression() -> anyhow::Result<()> {
        let vm = Vm::with_all()?;
        vm.import_code(
            "vm_dynamic_field_or",
            r#"
            pub fn next_or_start() {
                let choice = {
                    label: "颜色",
                    next: "color"
                };
                choice.next || "start"
            }

            pub fn direct_next() {
                let choice = {
                    label: "颜色",
                    next: "color"
                };
                choice.next
            }

            pub fn bracket_next() {
                let choice = {
                    label: "颜色",
                    next: "color"
                };
                choice["next"]
            }

            pub fn assigned_preview() {
                let choice = {
                    next: "tax_free"
                };
                choice.preview = choice.next || "start";
                choice
            }
            "#
            .as_bytes()
            .to_vec(),
        )?;

        let compiled = vm.get_fn("vm_dynamic_field_or::direct_next", &[])?;
        assert_eq!(compiled.ret_ty(), &Type::Any);
        let direct_next: extern "C" fn() -> *const Dynamic = unsafe { std::mem::transmute(compiled.ptr()) };
        assert_eq!(unsafe { &*direct_next() }.as_str(), "color");

        let compiled = vm.get_fn("vm_dynamic_field_or::bracket_next", &[])?;
        assert_eq!(compiled.ret_ty(), &Type::Any);
        let bracket_next: extern "C" fn() -> *const Dynamic = unsafe { std::mem::transmute(compiled.ptr()) };
        assert_eq!(unsafe { &*bracket_next() }.as_str(), "color");

        let compiled = vm.get_fn("vm_dynamic_field_or::next_or_start", &[])?;
        assert_eq!(compiled.ret_ty(), &Type::Any);
        let next_or_start: extern "C" fn() -> *const Dynamic = unsafe { std::mem::transmute(compiled.ptr()) };
        assert_eq!(unsafe { &*next_or_start() }.as_str(), "color");

        let compiled = vm.get_fn("vm_dynamic_field_or::assigned_preview", &[])?;
        assert_eq!(compiled.ret_ty(), &Type::Any);
        let assigned_preview: extern "C" fn() -> *const Dynamic = unsafe { std::mem::transmute(compiled.ptr()) };
        let choice = unsafe { &*assigned_preview() };
        assert_eq!(choice.get_dynamic("preview").unwrap().as_str(), "tax_free");
        Ok(())
    }

    #[test]
    fn empty_object_literal_in_if_branch_stays_dynamic() -> anyhow::Result<()> {
        let vm = Vm::with_all()?;
        vm.import_code(
            "vm_if_empty_object_branch",
            r#"
            pub fn first_note(steps) {
                let first = if steps.len() > 0 { steps[0] } else { {} };
                let first_note = first.note || "fallback";
                first_note
            }

            pub fn first_ja(steps) {
                let first = if steps.len() > 0 { steps[0] } else { {} };
                first.ja || "すみません"
            }

            pub fn assign_first_note(steps) {
                let first = {};
                first = if steps.len() > 0 { steps[0] } else { {} };
                first.note || "fallback"
            }
            "#
            .as_bytes()
            .to_vec(),
        )?;

        let compiled = vm.get_fn("vm_if_empty_object_branch::first_note", &[Type::Any])?;
        assert_eq!(compiled.ret_ty(), &Type::Any);
        let first_note: extern "C" fn(*const Dynamic) -> *const Dynamic = unsafe { std::mem::transmute(compiled.ptr()) };

        let empty_steps = Dynamic::list(Vec::new());
        assert_eq!(unsafe { &*first_note(&empty_steps) }.as_str(), "fallback");

        let mut step = std::collections::BTreeMap::new();
        step.insert("note".into(), "hello".into());
        let steps = Dynamic::list(vec![Dynamic::map(step)]);
        assert_eq!(unsafe { &*first_note(&steps) }.as_str(), "hello");

        let compiled = vm.get_fn("vm_if_empty_object_branch::first_ja", &[Type::Any])?;
        assert_eq!(compiled.ret_ty(), &Type::Any);
        let first_ja: extern "C" fn(*const Dynamic) -> *const Dynamic = unsafe { std::mem::transmute(compiled.ptr()) };
        assert_eq!(unsafe { &*first_ja(&empty_steps) }.as_str(), "すみません");

        let compiled = vm.get_fn("vm_if_empty_object_branch::assign_first_note", &[Type::Any])?;
        assert_eq!(compiled.ret_ty(), &Type::Any);
        let assign_first_note: extern "C" fn(*const Dynamic) -> *const Dynamic = unsafe { std::mem::transmute(compiled.ptr()) };
        assert_eq!(unsafe { &*assign_first_note(&empty_steps) }.as_str(), "fallback");
        assert_eq!(unsafe { &*assign_first_note(&steps) }.as_str(), "hello");
        Ok(())
    }

    #[test]
    fn list_literal_can_be_function_tail_expression() -> anyhow::Result<()> {
        let vm = Vm::with_all()?;
        vm.import_code(
            "vm_tail_list_literal",
            r#"
            pub fn numbers() {
                [1, 2, 3]
            }

            pub fn maps() {
                [
                    {note: "first"},
                    {note: "second"}
                ]
            }

            pub fn object_with_maps() {
                {
                    steps: [
                        {note: "first"},
                        {note: "second"}
                    ]
                }
            }

            pub fn return_maps() {
                return [
                    {note: "first"},
                    {note: "second"}
                ];
            }

            pub fn return_maps_without_semicolon() {
                return [
                    {note: "first"},
                    {note: "second"}
                ]
            }

            pub fn tail_bare_variable() {
                let value = [
                    {note: "first"},
                    {note: "second"}
                ];
                value
            }

            pub fn return_bare_variable_without_semicolon() {
                let value = [
                    {note: "first"},
                    {note: "second"}
                ];
                return value
            }

            pub fn tail_object_variable() {
                let result = {
                    steps: [
                        {note: "first"},
                        {note: "second"}
                    ]
                };
                result
            }
            "#
            .as_bytes()
            .to_vec(),
        )?;

        let compiled = vm.get_fn("vm_tail_list_literal::numbers", &[])?;
        assert_eq!(compiled.ret_ty(), &Type::Any);
        let numbers: extern "C" fn() -> *const Dynamic = unsafe { std::mem::transmute(compiled.ptr()) };
        let result = unsafe { &*numbers() };
        assert_eq!(result.len(), 3);
        assert_eq!(result.get_idx(1).and_then(|value| value.as_int()), Some(2));

        let compiled = vm.get_fn("vm_tail_list_literal::maps", &[])?;
        assert_eq!(compiled.ret_ty(), &Type::Any);
        let maps: extern "C" fn() -> *const Dynamic = unsafe { std::mem::transmute(compiled.ptr()) };
        let result = unsafe { &*maps() };
        assert_eq!(result.len(), 2);
        assert_eq!(result.get_idx(1).and_then(|value| value.get_dynamic("note")).map(|value| value.as_str().to_string()), Some("second".to_string()));

        let compiled = vm.get_fn("vm_tail_list_literal::object_with_maps", &[])?;
        assert_eq!(compiled.ret_ty(), &Type::Any);
        let object_with_maps: extern "C" fn() -> *const Dynamic = unsafe { std::mem::transmute(compiled.ptr()) };
        let result = unsafe { &*object_with_maps() };
        let steps = result.get_dynamic("steps").expect("steps");
        assert_eq!(steps.len(), 2);
        assert_eq!(steps.get_idx(0).and_then(|value| value.get_dynamic("note")).map(|value| value.as_str().to_string()), Some("first".to_string()));

        let compiled = vm.get_fn("vm_tail_list_literal::return_maps", &[])?;
        assert_eq!(compiled.ret_ty(), &Type::Any);
        let return_maps: extern "C" fn() -> *const Dynamic = unsafe { std::mem::transmute(compiled.ptr()) };
        let result = unsafe { &*return_maps() };
        assert_eq!(result.len(), 2);
        assert_eq!(result.get_idx(1).and_then(|value| value.get_dynamic("note")).map(|value| value.as_str().to_string()), Some("second".to_string()));

        let compiled = vm.get_fn("vm_tail_list_literal::return_maps_without_semicolon", &[])?;
        assert_eq!(compiled.ret_ty(), &Type::Any);
        let return_maps_without_semicolon: extern "C" fn() -> *const Dynamic = unsafe { std::mem::transmute(compiled.ptr()) };
        let result = unsafe { &*return_maps_without_semicolon() };
        assert_eq!(result.len(), 2);
        assert_eq!(result.get_idx(0).and_then(|value| value.get_dynamic("note")).map(|value| value.as_str().to_string()), Some("first".to_string()));

        let compiled = vm.get_fn("vm_tail_list_literal::tail_bare_variable", &[])?;
        assert_eq!(compiled.ret_ty(), &Type::Any);
        let tail_bare_variable: extern "C" fn() -> *const Dynamic = unsafe { std::mem::transmute(compiled.ptr()) };
        let result = unsafe { &*tail_bare_variable() };
        assert_eq!(result.len(), 2);
        assert_eq!(result.get_idx(1).and_then(|value| value.get_dynamic("note")).map(|value| value.as_str().to_string()), Some("second".to_string()));

        let compiled = vm.get_fn("vm_tail_list_literal::return_bare_variable_without_semicolon", &[])?;
        assert_eq!(compiled.ret_ty(), &Type::Any);
        let return_bare_variable_without_semicolon: extern "C" fn() -> *const Dynamic = unsafe { std::mem::transmute(compiled.ptr()) };
        let result = unsafe { &*return_bare_variable_without_semicolon() };
        assert_eq!(result.len(), 2);
        assert_eq!(result.get_idx(0).and_then(|value| value.get_dynamic("note")).map(|value| value.as_str().to_string()), Some("first".to_string()));

        let compiled = vm.get_fn("vm_tail_list_literal::tail_object_variable", &[])?;
        assert_eq!(compiled.ret_ty(), &Type::Any);
        let tail_object_variable: extern "C" fn() -> *const Dynamic = unsafe { std::mem::transmute(compiled.ptr()) };
        let result = unsafe { &*tail_object_variable() };
        let steps = result.get_dynamic("steps").expect("steps");
        assert_eq!(steps.len(), 2);
        assert_eq!(steps.get_idx(1).and_then(|value| value.get_dynamic("note")).map(|value| value.as_str().to_string()), Some("second".to_string()));
        Ok(())
    }

    #[test]
    fn list_return_value_supports_get_idx_method_call() -> anyhow::Result<()> {
        let vm = Vm::with_all()?;
        vm.import_code(
            "vm_returned_list_get_idx",
            r#"
            pub fn ids() {
                [
                    "base",
                    "2",
                    "3"
                ]
            }

            pub fn combinations() {
                let result = [];
                let values = ids();
                let idx = 0;
                while idx < values.len() {
                    result.push(values.get_idx(idx));
                    idx = idx + 1;
                }
                result
            }
            "#
            .as_bytes()
            .to_vec(),
        )?;

        let compiled = vm.get_fn("vm_returned_list_get_idx::combinations", &[])?;
        assert_eq!(compiled.ret_ty(), &Type::Any);
        let combinations: extern "C" fn() -> *const Dynamic = unsafe { std::mem::transmute(compiled.ptr()) };
        let result = unsafe { &*combinations() };

        assert_eq!(result.len(), 3);
        assert_eq!(result.get_idx(0).map(|value| value.as_str().to_string()), Some("base".to_string()));
        assert_eq!(result.get_idx(2).map(|value| value.as_str().to_string()), Some("3".to_string()));
        Ok(())
    }

    #[test]
    fn repeated_deep_step_literals_import_successfully() -> anyhow::Result<()> {
        fn extra_page_literal(depth: usize) -> String {
            let mut value = "{leaf: \"done\"}".to_string();
            for idx in 0..depth {
                value = format!("{{kind: \"page\", idx: {idx}, children: [{value}], meta: {{title: \"extra\", visible: true}}}}");
            }
            value
        }

        let extra = extra_page_literal(48);
        let code = format!(
            r#"
            pub fn script() {{
                return [
                    {{ja: "一つ目", note: "first", extra: {extra}}},
                    {{ja: "二つ目", note: "second", extra: {extra}}},
                    {{ja: "三つ目", note: "third", extra: {extra}}}
                ]
            }}
            "#
        );

        let vm = Vm::with_all()?;
        vm.import_code("vm_repeated_deep_step_literals", code.into_bytes())?;
        let compiled = vm.get_fn("vm_repeated_deep_step_literals::script", &[])?;
        assert_eq!(compiled.ret_ty(), &Type::Any);
        let script: extern "C" fn() -> *const Dynamic = unsafe { std::mem::transmute(compiled.ptr()) };
        let result = unsafe { &*script() };
        assert_eq!(result.len(), 3);
        assert_eq!(result.get_idx(2).and_then(|value| value.get_dynamic("note")).map(|value| value.as_str().to_string()), Some("third".to_string()));
        Ok(())
    }

    #[test]
    fn native_import_uses_owning_vm() -> anyhow::Result<()> {
        let module_path = std::env::temp_dir().join(format!("zust_vm_import_owner_{}.zs", std::process::id()));
        std::fs::write(&module_path, "pub fn value() { 41 }")?;
        let module_path = module_path.to_string_lossy().replace('\\', "\\\\").replace('"', "\\\"");

        let vm1 = Vm::with_all()?;
        vm1.import_code(
            "vm_import_owner",
            format!(
                r#"
                pub fn run() {{
                    import("vm_imported_owner", "{module_path}");
                }}
                "#
            )
            .into_bytes(),
        )?;
        let compiled = vm1.get_fn("vm_import_owner::run", &[])?;

        let vm2 = Vm::with_all()?;
        vm2.import_code("vm_import_other", b"pub fn run() { 0 }".to_vec())?;
        let _ = vm2.get_fn("vm_import_other::run", &[])?;

        let run: extern "C" fn() = unsafe { std::mem::transmute(compiled.ptr()) };
        run();

        assert!(vm1.get_fn("vm_imported_owner::value", &[]).is_ok());
        assert!(vm2.get_fn("vm_imported_owner::value", &[]).is_err());
        Ok(())
    }

    #[test]
    fn object_last_field_call_does_not_need_trailing_comma() -> anyhow::Result<()> {
        let vm = Vm::with_all()?;
        vm.import_code(
            "vm_object_last_call_field",
            r#"
            pub fn extra_page() {
                {
                    title: "extra",
                    pages: [
                        {note: "nested"}
                    ]
                }
            }

            pub fn data() {
                return [
                    {
                        note: "first",
                        choices: ["a", "b"],
                        extras: extra_page()
                    },
                    {
                        note: "second",
                        choices: ["c"],
                        extras: extra_page()
                    }
                ]
            }
            "#
            .as_bytes()
            .to_vec(),
        )?;

        let compiled = vm.get_fn("vm_object_last_call_field::data", &[])?;
        assert_eq!(compiled.ret_ty(), &Type::Any);
        let data: extern "C" fn() -> *const Dynamic = unsafe { std::mem::transmute(compiled.ptr()) };
        let result = unsafe { &*data() };
        assert_eq!(result.len(), 2);
        let first = result.get_idx(0).expect("first step");
        assert_eq!(first.get_dynamic("extras").and_then(|extras| extras.get_dynamic("title")).map(|title| title.as_str().to_string()), Some("extra".to_string()));
        Ok(())
    }

    #[test]
    fn gpu_struct_layout_packs_and_unpacks_dynamic_maps() -> anyhow::Result<()> {
        let vm = Vm::with_all()?;
        vm.import_code(
            "vm_gpu_layout",
            br#"
            pub struct Params {
                a: u32,
                b: u32,
                c: u32,
            }
            "#
            .to_vec(),
        )?;

        let layout = vm.gpu_struct_layout("vm_gpu_layout::Params", &[])?;
        assert_eq!(layout.size, 16);
        assert_eq!(layout.fields.iter().map(|field| (field.name.as_str(), field.offset)).collect::<Vec<_>>(), vec![("a", 0), ("b", 4), ("c", 8)]);

        let value = dynamic::map!("a"=> 1u32, "b"=> 2u32, "c"=> 3u32);
        let bytes = layout.pack_map(&value)?;
        assert_eq!(bytes.len(), 16);
        assert_eq!(&bytes[0..4], &1u32.to_ne_bytes());
        assert_eq!(&bytes[4..8], &2u32.to_ne_bytes());
        assert_eq!(&bytes[8..12], &3u32.to_ne_bytes());

        let read = layout.unpack_map(&bytes)?;
        assert_eq!(read.get_dynamic("a").and_then(|value| value.as_uint()), Some(1));
        assert_eq!(read.get_dynamic("b").and_then(|value| value.as_uint()), Some(2));
        assert_eq!(read.get_dynamic("c").and_then(|value| value.as_uint()), Some(3));
        Ok(())
    }

    #[test]
    fn root_native_calls_do_not_take_ownership_of_dynamic_args() -> anyhow::Result<()> {
        let vm = Vm::with_all()?;
        vm.import_code(
            "vm_root_clone_bridge",
            br#"
            pub fn add_then_reuse(arg) {
                let user = {
                    address: "test-wallet",
                    points: 20
                };
                root::add("local/root-clone-bridge-user", user);
                user.points = user.points - 7;
                root::add("local/root-clone-bridge-user", user);
                {
                    user: user,
                    points: user.points
                }
            }

            pub fn clone_then_mutate(arg) {
                let user = {
                    profile: {
                        points: 20
                    }
                };
                let copied = user.clone();
                copied.profile.points = 13;
                user
            }
            "#
            .to_vec(),
        )?;

        let compiled = vm.get_fn("vm_root_clone_bridge::add_then_reuse", &[Type::Any])?;
        assert_eq!(compiled.ret_ty(), &Type::Any);
        let add_then_reuse: extern "C" fn(*const Dynamic) -> *const Dynamic = unsafe { std::mem::transmute(compiled.ptr()) };
        let arg = Dynamic::Null;
        let result = add_then_reuse(&arg);
        let result = unsafe { &*result };

        assert_eq!(result.get_dynamic("points").and_then(|value| value.as_int()), Some(13));
        let mut json = String::new();
        result.to_json(&mut json);
        assert!(json.contains("\"points\": 13"));

        let clone_then_mutate = vm.get_fn("vm_root_clone_bridge::clone_then_mutate", &[Type::Any])?;
        let clone_then_mutate: extern "C" fn(*const Dynamic) -> *const Dynamic = unsafe { std::mem::transmute(clone_then_mutate.ptr()) };
        let result = clone_then_mutate(&arg);
        let result = unsafe { &*result };
        assert_eq!(result.get_dynamic("profile").unwrap().get_dynamic("points").and_then(|value| value.as_int()), Some(20));
        Ok(())
    }

    struct CounterForTypedReceiver {
        value: i64,
    }

    extern "C" fn counter_for_typed_receiver_get(value: *const Dynamic) -> i64 {
        unsafe { &*value }.as_custom::<CounterForTypedReceiver>().map(|counter| counter.value).unwrap_or(-1)
    }

    struct NavMapForFunctionArg;

    extern "C" fn nav_map_for_function_arg_new() -> *const Dynamic {
        Box::into_raw(Box::new(Dynamic::custom(NavMapForFunctionArg)))
    }

    #[test]
    fn typed_receiver_method_call_dispatches_with_type_hint() -> anyhow::Result<()> {
        let vm = Vm::with_all()?;
        vm.add_empty_type("Counter")?;
        let counter_ty = vm.get_symbol("Counter", Vec::new())?;
        vm.add_native_method_ptr("Counter", "get", &[counter_ty], Type::I64, counter_for_typed_receiver_get as *const u8)?;
        vm.import_code(
            "vm_typed_receiver_method",
            br#"
            pub fn run(value) {
                value::<Counter>::get()
            }
            "#
            .to_vec(),
        )?;

        let compiled = vm.get_fn("vm_typed_receiver_method::run", &[Type::Any])?;
        assert_eq!(compiled.ret_ty(), &Type::I64);
        let run: extern "C" fn(*const Dynamic) -> i64 = unsafe { std::mem::transmute(compiled.ptr()) };
        let value = Dynamic::custom(CounterForTypedReceiver { value: 42 });

        assert_eq!(run(&value), 42);
        Ok(())
    }

    #[test]
    fn native_custom_object_can_be_passed_to_zs_function() -> anyhow::Result<()> {
        let vm = Vm::with_all()?;
        vm.add_empty_type("NavMap")?;
        vm.add_native_method_ptr("NavMap", "new", &[], Type::Any, nav_map_for_function_arg_new as *const u8)?;
        vm.import_code(
            "vm_native_custom_arg",
            br#"
            pub fn add_nav_spawns(world, navmap) {
                navmap
            }

            pub fn run(world) {
                let navmap = NavMap::new();
                let with_spawns = add_nav_spawns(world, navmap);
                with_spawns
            }
            "#
            .to_vec(),
        )?;

        let compiled = vm.get_fn("vm_native_custom_arg::run", &[Type::Any])?;
        assert_eq!(compiled.ret_ty(), &Type::Any);
        let run: extern "C" fn(*const Dynamic) -> *const Dynamic = unsafe { std::mem::transmute(compiled.ptr()) };
        let world = Dynamic::Null;
        let result = run(&world);
        let result = unsafe { &*result };

        assert!(result.as_custom::<NavMapForFunctionArg>().is_some());
        Ok(())
    }

    #[test]
    fn native_custom_object_typed_local_can_be_passed_to_zs_function() -> anyhow::Result<()> {
        let vm = Vm::with_all()?;
        vm.add_empty_type("NavMap")?;
        let _nav_map_ty = vm.get_symbol("NavMap", Vec::new())?;
        vm.add_native_method_ptr("NavMap", "new", &[], Type::Any, nav_map_for_function_arg_new as *const u8)?;
        vm.import_code(
            "vm_native_custom_typed_arg",
            br#"
            pub fn add_nav_spawns(world, navmap) {
                navmap
            }

            pub fn run(world) {
                let navmap: NavMap = NavMap::new();
                let with_spawns = add_nav_spawns(world, navmap);
                with_spawns
            }
            "#
            .to_vec(),
        )?;

        let compiled = vm.get_fn("vm_native_custom_typed_arg::run", &[Type::Any])?;
        let run: extern "C" fn(*const Dynamic) -> *const Dynamic = unsafe { std::mem::transmute(compiled.ptr()) };
        let world = Dynamic::Null;
        let result = run(&world);
        let result = unsafe { &*result };

        assert!(result.as_custom::<NavMapForFunctionArg>().is_some());
        Ok(())
    }
}
