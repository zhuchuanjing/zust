//使用 cranelift 作为后端 直接 jit 解释脚本
mod binary;
mod memory;
mod native;
pub use native::{ANY, STD, ZustCallback};

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
mod oss_module;
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
        self.native_symbols.write().unwrap().insert("__vm_repeat_fill".to_string(), native::repeat_fill as *const () as usize);
        self.native_symbols.write().unwrap().insert("__vm_strcat".to_string(), native::strcat as *const () as usize);
        self.native_symbols.write().unwrap().insert("__vm_strcat_i64".to_string(), native::strcat_i64 as *const () as usize);
        self.native_symbols.write().unwrap().insert("__vm_strcat_assign".to_string(), native::strcat_assign as *const () as usize);
        self.native_symbols.write().unwrap().insert("__vm_callback_new".to_string(), native::callback_new as *const () as usize);
        self.native_symbols.write().unwrap().insert("__vm_spawn_ptr".to_string(), native::spawn_ptr as *const () as usize);
        self.native_symbols.write().unwrap().insert("__vm_struct_from_ptr".to_string(), native::struct_from_ptr as *const () as usize);
        self.native_symbols.write().unwrap().insert("__vm_array_from_ptr".to_string(), native::array_from_ptr as *const () as usize);
        self.native_symbols.write().unwrap().insert("__vm_array_to_ptr".to_string(), native::array_to_ptr as *const () as usize);

        let void_sig = self.get_sig(&[], Type::Void)?;
        self.scope_enter_fn = Some(self.module.declare_function("__vm_scope_enter", cranelift_module::Linkage::Import, &void_sig)?);
        self.scope_exit_void_fn = Some(self.module.declare_function("__vm_scope_exit_void", cranelift_module::Linkage::Import, &void_sig)?);

        let dynamic_sig = self.get_sig(&[Type::Any], Type::Any)?;
        self.scope_exit_dynamic_fn = Some(self.module.declare_function("__vm_scope_exit_dynamic", cranelift_module::Linkage::Import, &dynamic_sig)?);

        let bytes_sig = self.get_sig(&[Type::Any, Type::I64], Type::Any)?;
        self.scope_exit_bytes_fn = Some(self.module.declare_function("__vm_scope_exit_bytes", cranelift_module::Linkage::Import, &bytes_sig)?);

        let struct_alloc_sig = self.get_sig(&[Type::I64], Type::Any)?;
        self.struct_alloc_fn = Some(self.module.declare_function("__vm_struct_alloc", cranelift_module::Linkage::Import, &struct_alloc_sig)?);

        let repeat_fill_sig = self.get_sig(&[Type::Any, Type::I64, Type::I64, Type::I64], Type::Void)?;
        self.repeat_fill_fn = Some(self.module.declare_function("__vm_repeat_fill", cranelift_module::Linkage::Import, &repeat_fill_sig)?);

        let strcat_sig = self.get_sig(&[Type::Str, Type::Str], Type::Str)?;
        self.strcat_fn = Some(self.module.declare_function("__vm_strcat", cranelift_module::Linkage::Import, &strcat_sig)?);

        let strcat_i64_sig = self.get_sig(&[Type::Str, Type::I64], Type::Str)?;
        self.strcat_i64_fn = Some(self.module.declare_function("__vm_strcat_i64", cranelift_module::Linkage::Import, &strcat_i64_sig)?);

        let strcat_assign_sig = self.get_sig(&[Type::Any, Type::Any], Type::Any)?;
        self.strcat_assign_fn = Some(self.module.declare_function("__vm_strcat_assign", cranelift_module::Linkage::Import, &strcat_assign_sig)?);

        let callback_new_sig = self.get_sig(&[Type::I64, Type::I64, Type::I64, Type::Any], Type::Any)?;
        self.callback_new_fn = Some(self.module.declare_function("__vm_callback_new", cranelift_module::Linkage::Import, &callback_new_sig)?);

        let spawn_ptr_sig = self.get_sig(&[Type::I64, Type::I64, Type::Any], Type::Bool)?;
        self.spawn_ptr_fn = Some(self.module.declare_function("__vm_spawn_ptr", cranelift_module::Linkage::Import, &spawn_ptr_sig)?);

        let struct_from_ptr_sig = self.get_sig(&[Type::I64, Type::I64], Type::Any)?;
        self.struct_from_ptr_fn = Some(self.module.declare_function("__vm_struct_from_ptr", cranelift_module::Linkage::Import, &struct_from_ptr_sig)?);
        self.array_from_ptr_fn = Some(self.module.declare_function("__vm_array_from_ptr", cranelift_module::Linkage::Import, &struct_from_ptr_sig)?);
        let array_to_ptr_sig = self.get_sig(&[Type::Any, Type::Any, Type::I64], Type::Void)?;
        self.array_to_ptr_fn = Some(self.module.declare_function("__vm_array_to_ptr", cranelift_module::Linkage::Import, &array_to_ptr_sig)?);
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
        self.add_context_native_ptr("spawn", "spawn", &[Type::Any, Type::Any], Type::Bool, native::spawn_with_vm as *const u8)?;
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
        add_native_module_fns(self, "http", &http_module::HTTP_NATIVE)?;
        http_module::add_root_handlers()
    }

    pub fn add_oss(&mut self) -> Result<()> {
        add_native_module_fns(self, "oss", &oss_module::OSS_NATIVE)
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
        self.add_oss()?;
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

    pub fn add_oss(&self) -> Result<()> {
        self.jit.lock().unwrap().add_oss()
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

    pub fn get_fn_ptr_with_params(&self, name: &str, arg_tys: &[Type], generic_args: &[Type]) -> Result<(*const u8, Type)> {
        self.jit.lock().unwrap().get_fn_ptr_with_params(name, arg_tys, generic_args)
    }

    pub fn get_fn(&self, name: &str, arg_tys: &[Type]) -> Result<CompiledFn> {
        let (ptr, ret) = self.get_fn_ptr(name, arg_tys)?;
        Ok(CompiledFn { ptr: ptr as usize, ret, owner: self.clone() })
    }

    pub fn get_fn_with_params(&self, name: &str, arg_tys: &[Type], generic_args: &[Type]) -> Result<CompiledFn> {
        let (ptr, ret) = self.get_fn_ptr_with_params(name, arg_tys, generic_args)?;
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
    use super::{Vm, ZustCallback};
    use dynamic::{CustomProperty, Dynamic, ToJson, Type};
    use std::collections::BTreeMap;
    use std::sync::{Mutex, RwLock};

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
        assert_eq!(vm.infer("std::sqrt", &[Type::F64])?, Type::F64);

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
    fn std_sqrt_is_available_as_top_level_function() -> anyhow::Result<()> {
        let vm = Vm::with_all()?;
        vm.import_code(
            "vm_std_sqrt",
            br#"
            pub fn run() {
                sqrt(9.0f64)
            }
            "#
            .to_vec(),
        )?;

        let compiled = vm.get_fn("vm_std_sqrt::run", &[])?;
        assert_eq!(compiled.ret_ty(), &Type::F64);
        let run: extern "C" fn() -> f64 = unsafe { std::mem::transmute(compiled.ptr()) };
        assert_eq!(run(), 3.0);
        Ok(())
    }

    #[test]
    fn tuple_assignment_uses_simultaneous_scalar_temps() -> anyhow::Result<()> {
        let vm = Vm::with_all()?;
        vm.import_code(
            "vm_tuple_assignment",
            br#"
            pub fn swap() {
                let a = 1i64;
                let b = 2i64;
                (a, b) = (b, a);
                a * 10i64 + b
            }

            pub fn fib(n: i64) {
                let a = 0i64;
                let b = 1i64;
                for _ in 0..n {
                    (a, b) = (b, (a + b) % 1000000007i64);
                }
                a
            }
            "#
            .to_vec(),
        )?;

        let swap = vm.get_fn("vm_tuple_assignment::swap", &[])?;
        let swap: extern "C" fn() -> i64 = unsafe { std::mem::transmute(swap.ptr()) };
        assert_eq!(swap(), 21);

        let fib = vm.get_fn("vm_tuple_assignment::fib", &[Type::I64])?;
        let fib: extern "C" fn(i64) -> i64 = unsafe { std::mem::transmute(fib.ptr()) };
        assert_eq!(fib(10), 55);
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
    fn casts_any_float_to_i32_without_zeroing() -> anyhow::Result<()> {
        let vm = Vm::with_all()?;
        vm.import_code(
            "vm_any_float_to_i32",
            br#"
            pub fn direct(value) {
                value as i32
            }

            pub fn map_field(value) {
                let field = value.v;
                field as i32
            }

            pub fn damage(attacker, def_rate) {
                let x = attacker.atk * (1.0 - def_rate);
                x as i32
            }
            "#
            .to_vec(),
        )?;

        let compiled = vm.get_fn("vm_any_float_to_i32::direct", &[Type::Any])?;
        assert_eq!(compiled.ret_ty(), &Type::I32);
        let direct: extern "C" fn(*const Dynamic) -> i32 = unsafe { std::mem::transmute(compiled.ptr()) };
        let value = Dynamic::from(9.5f64);
        assert_eq!(direct(&value), 9);

        let compiled = vm.get_fn("vm_any_float_to_i32::map_field", &[Type::Any])?;
        assert_eq!(compiled.ret_ty(), &Type::I32);
        let map_field: extern "C" fn(*const Dynamic) -> i32 = unsafe { std::mem::transmute(compiled.ptr()) };
        let value = dynamic::map!("v"=> 9.5f64);
        assert_eq!(map_field(&value), 9);

        let compiled = vm.get_fn("vm_any_float_to_i32::damage", &[Type::Any, Type::Any])?;
        assert_eq!(compiled.ret_ty(), &Type::I32);
        let damage: extern "C" fn(*const Dynamic, *const Dynamic) -> i32 = unsafe { std::mem::transmute(compiled.ptr()) };
        let attacker = dynamic::map!("atk"=> 64i64);
        let def_rate = Dynamic::from(0.17f64);
        assert_eq!(damage(&attacker, &def_rate), 53);
        Ok(())
    }

    #[test]
    fn binary_imm_promotes_integer_literals_for_float_left_values() -> anyhow::Result<()> {
        let vm = Vm::with_all()?;
        vm.import_code(
            "vm_float_binary_imm",
            br#"
            pub fn add_f32(value: f32) {
                value + 1i32
            }

            pub fn sub_f32(value: f32) {
                value - 1i32
            }

            pub fn mul_f32(value: f32) {
                value * 2i32
            }

            pub fn div_f32(value: f32) {
                value / 2i32
            }

            pub fn gt_f32(value: f32) {
                value > 2i32
            }
            "#
            .to_vec(),
        )?;

        let compiled = vm.get_fn("vm_float_binary_imm::add_f32", &[Type::F32])?;
        assert_eq!(compiled.ret_ty(), &Type::F32);
        let add_f32: extern "C" fn(f32) -> f32 = unsafe { std::mem::transmute(compiled.ptr()) };
        assert_eq!(add_f32(2.5), 3.5);

        let compiled = vm.get_fn("vm_float_binary_imm::sub_f32", &[Type::F32])?;
        assert_eq!(compiled.ret_ty(), &Type::F32);
        let sub_f32: extern "C" fn(f32) -> f32 = unsafe { std::mem::transmute(compiled.ptr()) };
        assert_eq!(sub_f32(2.5), 1.5);

        let compiled = vm.get_fn("vm_float_binary_imm::mul_f32", &[Type::F32])?;
        assert_eq!(compiled.ret_ty(), &Type::F32);
        let mul_f32: extern "C" fn(f32) -> f32 = unsafe { std::mem::transmute(compiled.ptr()) };
        assert_eq!(mul_f32(2.5), 5.0);

        let compiled = vm.get_fn("vm_float_binary_imm::div_f32", &[Type::F32])?;
        assert_eq!(compiled.ret_ty(), &Type::F32);
        let div_f32: extern "C" fn(f32) -> f32 = unsafe { std::mem::transmute(compiled.ptr()) };
        assert_eq!(div_f32(5.0), 2.5);

        let compiled = vm.get_fn("vm_float_binary_imm::gt_f32", &[Type::F32])?;
        assert_eq!(compiled.ret_ty(), &Type::Bool);
        let gt_f32: extern "C" fn(f32) -> bool = unsafe { std::mem::transmute(compiled.ptr()) };
        assert!(gt_f32(2.5));
        assert!(!gt_f32(1.5));
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
    fn string_methods_work_on_static_string_and_any_string_values() -> anyhow::Result<()> {
        let vm = Vm::with_all()?;
        vm.import_code(
            "vm_string_methods",
            br#"
            pub fn static_string_methods(text: string) {
                let parts = text.split(",");
                text.starts_with("alpha")
                    && text.is_string()
                    && !text.is_null()
                    && parts.len() == 2
                    && parts.get_idx(0) == "alpha"
                    && parts.get_idx(1) == "beta"
            }

            pub fn any_string_methods(value) {
                let parts = value.split(",");
                value.starts_with("alpha")
                    && value.is_string()
                    && !value.is_null()
                    && parts.len() == 2
                    && parts.get_idx(0) == "alpha"
                    && parts.get_idx(1) == "beta"
            }

            pub fn any_null_methods(value) {
                value.is_null() && !value.is_string()
            }
            "#
            .to_vec(),
        )?;

        let compiled = vm.get_fn("vm_string_methods::static_string_methods", &[Type::Str])?;
        assert_eq!(compiled.ret_ty(), &Type::Bool);
        let static_string_methods: extern "C" fn(*const Dynamic) -> bool = unsafe { std::mem::transmute(compiled.ptr()) };
        let text = Dynamic::from("alpha,beta");
        assert!(static_string_methods(&text));

        let compiled = vm.get_fn("vm_string_methods::any_string_methods", &[Type::Any])?;
        assert_eq!(compiled.ret_ty(), &Type::Bool);
        let any_string_methods: extern "C" fn(*const Dynamic) -> bool = unsafe { std::mem::transmute(compiled.ptr()) };
        assert!(any_string_methods(&text));

        let compiled = vm.get_fn("vm_string_methods::any_null_methods", &[Type::Any])?;
        assert_eq!(compiled.ret_ty(), &Type::Bool);
        let any_null_methods: extern "C" fn(*const Dynamic) -> bool = unsafe { std::mem::transmute(compiled.ptr()) };
        let value = Dynamic::Null;
        assert!(any_null_methods(&value));
        Ok(())
    }

    #[test]
    fn static_string_add_uses_direct_strcat() -> anyhow::Result<()> {
        let vm = Vm::with_all()?;
        vm.import_code(
            "vm_static_strcat",
            br#"
            pub fn join(left: string, right: string) {
                left + right
            }

            pub fn suffix(left: string) {
                left + "-tail"
            }

            pub fn append_local() {
                let text: string = "alpha";
                text += "-beta";
                text += "-tail";
                text
            }

            pub fn append_local_assign() {
                let text: string = "alpha";
                text = text + "-beta";
                text = text + "-tail";
                text
            }

            pub fn append_arg(text: string) {
                text += "-tail";
                text
            }

            pub fn append_arg_assign(text: string) {
                text = text + "-tail";
                text
            }

            pub fn append_any(value) {
                value += "-tail";
                value
            }

            pub fn add_sub_assign_form() {
                let x = 10i64;
                x = x + 1i64;
                x = x - 2i64;
                x
            }
            "#
            .to_vec(),
        )?;

        let compiled = vm.get_fn("vm_static_strcat::join", &[Type::Str, Type::Str])?;
        assert_eq!(compiled.ret_ty(), &Type::Str);
        let join: extern "C" fn(*const Dynamic, *const Dynamic) -> *const Dynamic = unsafe { std::mem::transmute(compiled.ptr()) };
        let left = Dynamic::from("alpha");
        let right = Dynamic::from("-beta");
        let result = unsafe { &*join(&left, &right) };
        assert!(matches!(result, Dynamic::StringBuf(_)));
        assert_eq!(result.as_str(), "alpha-beta");

        let compiled = vm.get_fn("vm_static_strcat::suffix", &[Type::Str])?;
        assert_eq!(compiled.ret_ty(), &Type::Str);
        let suffix: extern "C" fn(*const Dynamic) -> *const Dynamic = unsafe { std::mem::transmute(compiled.ptr()) };
        let result = unsafe { &*suffix(&left) };
        assert!(matches!(result, Dynamic::StringBuf(_)));
        assert_eq!(result.as_str(), "alpha-tail");

        let compiled = vm.get_fn("vm_static_strcat::append_local", &[])?;
        assert_eq!(compiled.ret_ty(), &Type::Str);
        let append_local: extern "C" fn() -> *const Dynamic = unsafe { std::mem::transmute(compiled.ptr()) };
        let result = unsafe { &*append_local() };
        assert!(matches!(result, Dynamic::StringBuf(_)));
        assert_eq!(result.as_str(), "alpha-beta-tail");

        let compiled = vm.get_fn("vm_static_strcat::append_local_assign", &[])?;
        assert_eq!(compiled.ret_ty(), &Type::Str);
        let append_local_assign: extern "C" fn() -> *const Dynamic = unsafe { std::mem::transmute(compiled.ptr()) };
        let result = unsafe { &*append_local_assign() };
        assert!(matches!(result, Dynamic::StringBuf(_)));
        assert_eq!(result.as_str(), "alpha-beta-tail");

        let compiled = vm.get_fn("vm_static_strcat::append_arg", &[Type::Str])?;
        assert_eq!(compiled.ret_ty(), &Type::Str);
        let append_arg: extern "C" fn(*const Dynamic) -> *const Dynamic = unsafe { std::mem::transmute(compiled.ptr()) };
        let input = Dynamic::from("alpha");
        let result = unsafe { &*append_arg(&input) };
        assert_eq!(result.as_str(), "alpha-tail");
        assert_eq!(input.as_str(), "alpha");

        let compiled = vm.get_fn("vm_static_strcat::append_arg_assign", &[Type::Str])?;
        assert_eq!(compiled.ret_ty(), &Type::Str);
        let append_arg_assign: extern "C" fn(*const Dynamic) -> *const Dynamic = unsafe { std::mem::transmute(compiled.ptr()) };
        let input = Dynamic::from("alpha");
        let result = unsafe { &*append_arg_assign(&input) };
        assert_eq!(result.as_str(), "alpha-tail");
        assert_eq!(input.as_str(), "alpha");

        let compiled = vm.get_fn("vm_static_strcat::append_any", &[Type::Any])?;
        assert_eq!(compiled.ret_ty(), &Type::Str);
        let append_any: extern "C" fn(*const Dynamic) -> *const Dynamic = unsafe { std::mem::transmute(compiled.ptr()) };
        let input = Dynamic::from("alpha");
        let result = unsafe { &*append_any(&input) };
        assert_eq!(result.as_str(), "alpha-tail");
        assert_eq!(input.as_str(), "alpha");

        let compiled = vm.get_fn("vm_static_strcat::add_sub_assign_form", &[])?;
        assert_eq!(compiled.ret_ty(), &Type::I64);
        let add_sub_assign_form: extern "C" fn() -> i64 = unsafe { std::mem::transmute(compiled.ptr()) };
        assert_eq!(add_sub_assign_form(), 9);
        Ok(())
    }

    #[test]
    fn primitive_type_check_methods_call_any_runtime() -> anyhow::Result<()> {
        let vm = Vm::with_all()?;
        vm.import_code(
            "vm_primitive_type_check_methods",
            br#"
            pub fn int_checks() {
                !42i64.is_list()
                    && !42i64.is_map()
                    && !42i64.is_string()
                    && !42i64.is_null()
            }

            pub fn bool_checks() {
                !true.is_list() && !true.is_map() && !true.is_null()
            }
            "#
            .to_vec(),
        )?;

        let compiled = vm.get_fn("vm_primitive_type_check_methods::int_checks", &[])?;
        assert_eq!(compiled.ret_ty(), &Type::Bool);
        let int_checks: extern "C" fn() -> bool = unsafe { std::mem::transmute(compiled.ptr()) };
        assert!(int_checks());

        let compiled = vm.get_fn("vm_primitive_type_check_methods::bool_checks", &[])?;
        assert_eq!(compiled.ret_ty(), &Type::Bool);
        let bool_checks: extern "C" fn() -> bool = unsafe { std::mem::transmute(compiled.ptr()) };
        assert!(bool_checks());
        Ok(())
    }

    #[test]
    fn for_loop_iterates_any_list_and_map_values() -> anyhow::Result<()> {
        let vm = Vm::with_all()?;
        vm.import_code(
            "vm_for_any_collections",
            br#"
            pub fn list_sum(items) {
                let total = 0i64;
                for item in items {
                    total += item;
                }
                total
            }

            pub fn map_sum(data) {
                let total = 0i64;
                for (key, value) in data {
                    total += value;
                }
                total
            }
            "#
            .to_vec(),
        )?;

        let compiled = vm.get_fn("vm_for_any_collections::list_sum", &[Type::Any])?;
        assert_eq!(compiled.ret_ty(), &Type::I64);
        let list_sum: extern "C" fn(*const Dynamic) -> i64 = unsafe { std::mem::transmute(compiled.ptr()) };
        let items = Dynamic::list(vec![1i64.into(), 2i64.into(), 3i64.into()]);
        assert_eq!(list_sum(&items), 6);

        let compiled = vm.get_fn("vm_for_any_collections::map_sum", &[Type::Any])?;
        assert_eq!(compiled.ret_ty(), &Type::I64);
        let map_sum: extern "C" fn(*const Dynamic) -> i64 = unsafe { std::mem::transmute(compiled.ptr()) };
        let data = dynamic::map!("a"=> 4i64, "b"=> 5i64);
        assert_eq!(map_sum(&data), 9);
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
        assert_eq!(compiled.ret_ty(), &Type::Str);
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
        assert!(matches!(result, Dynamic::StringBuf(_)));
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
        assert_eq!(run(7), 7);
        Ok(())
    }

    #[test]
    fn casts_dynamic_string_numbers_to_ints_and_floats() -> anyhow::Result<()> {
        let vm = Vm::with_all()?;
        vm.import_code(
            "vm_string_number_casts",
            br#"
            pub fn limit_i64(req) {
                req["@query"].limit as i64
            }

            pub fn limit_i32(req) {
                req["@query"].limit as i32
            }

            pub fn price_f64(req) {
                req["@query"].price as f64
            }

            pub fn price_f32(req) {
                req["@query"].price as f32
            }

            pub fn literal_i64() {
                "42" as i64
            }

            pub fn literal_f64() {
                "3.5" as f64
            }

            pub fn bad_number(req) {
                req["@query"].bad as i64
            }
            "#
            .to_vec(),
        )?;

        let req = dynamic::map!("@query"=> dynamic::map!("limit"=> "50", "price"=> "3.5", "bad"=> "nope"));

        let limit_i64 = vm.get_fn("vm_string_number_casts::limit_i64", &[Type::Any])?;
        assert_eq!(limit_i64.ret_ty(), &Type::I64);
        let limit_i64: extern "C" fn(*const Dynamic) -> i64 = unsafe { std::mem::transmute(limit_i64.ptr()) };
        assert_eq!(limit_i64(&req), 50);

        let limit_i32 = vm.get_fn("vm_string_number_casts::limit_i32", &[Type::Any])?;
        assert_eq!(limit_i32.ret_ty(), &Type::I32);
        let limit_i32: extern "C" fn(*const Dynamic) -> i32 = unsafe { std::mem::transmute(limit_i32.ptr()) };
        assert_eq!(limit_i32(&req), 50);

        let price_f64 = vm.get_fn("vm_string_number_casts::price_f64", &[Type::Any])?;
        assert_eq!(price_f64.ret_ty(), &Type::F64);
        let price_f64: extern "C" fn(*const Dynamic) -> f64 = unsafe { std::mem::transmute(price_f64.ptr()) };
        assert_eq!(price_f64(&req), 3.5);

        let price_f32 = vm.get_fn("vm_string_number_casts::price_f32", &[Type::Any])?;
        assert_eq!(price_f32.ret_ty(), &Type::F32);
        let price_f32: extern "C" fn(*const Dynamic) -> f32 = unsafe { std::mem::transmute(price_f32.ptr()) };
        assert_eq!(price_f32(&req), 3.5);

        let literal_i64 = vm.get_fn("vm_string_number_casts::literal_i64", &[])?;
        assert_eq!(literal_i64.ret_ty(), &Type::I64);
        let literal_i64: extern "C" fn() -> i64 = unsafe { std::mem::transmute(literal_i64.ptr()) };
        assert_eq!(literal_i64(), 42);

        let literal_f64 = vm.get_fn("vm_string_number_casts::literal_f64", &[])?;
        assert_eq!(literal_f64.ret_ty(), &Type::F64);
        let literal_f64: extern "C" fn() -> f64 = unsafe { std::mem::transmute(literal_f64.ptr()) };
        assert_eq!(literal_f64(), 3.5);

        let bad_number = vm.get_fn("vm_string_number_casts::bad_number", &[Type::Any])?;
        assert_eq!(bad_number.ret_ty(), &Type::I64);
        let bad_number: extern "C" fn(*const Dynamic) -> i64 = unsafe { std::mem::transmute(bad_number.ptr()) };
        assert_eq!(bad_number(&req), 0);
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
    fn closure_literal_can_be_called_immediately() -> anyhow::Result<()> {
        let vm = Vm::with_all()?;
        vm.import_code(
            "vm_closure_immediate_call",
            br#"
            pub fn no_args() {
                let r = || { 1i32 }();
                r
            }

            pub fn with_arg() {
                |value: i32| { value + 1i32 }(2i32)
            }
            "#
            .to_vec(),
        )?;

        let compiled = vm.get_fn("vm_closure_immediate_call::no_args", &[])?;
        assert_eq!(compiled.ret_ty(), &Type::I32);
        let no_args: extern "C" fn() -> i32 = unsafe { std::mem::transmute(compiled.ptr()) };
        assert_eq!(no_args(), 1);

        let compiled = vm.get_fn("vm_closure_immediate_call::with_arg", &[])?;
        assert_eq!(compiled.ret_ty(), &Type::I32);
        let with_arg: extern "C" fn() -> i32 = unsafe { std::mem::transmute(compiled.ptr()) };
        assert_eq!(with_arg(), 3);
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
    fn std_spawn_runs_named_function_with_tuple_args() -> anyhow::Result<()> {
        let zero_path = "local/vm_std_spawn/zero";
        let sum_path = "local/vm_std_spawn/sum";
        let closure_path = "local/vm_std_spawn/closure";
        let closure_vars_path = "local/vm_std_spawn/closure_vars";
        let _ = root::remove(zero_path);
        let _ = root::remove(sum_path);
        let _ = root::remove(closure_path);
        let _ = root::remove(closure_vars_path);
        let vm = Vm::with_all()?;
        vm.import_code(
            "vm_std_spawn",
            br#"
            pub fn zero() {
                root::add("local/vm_std_spawn/zero", 1);
            }

            pub fn job(left, right) {
                root::add("local/vm_std_spawn/sum", left + right);
            }

            pub fn start_zero() {
                spawn("vm_std_spawn::zero", ())
            }

            pub fn start_sum() {
                spawn("vm_std_spawn::job", (10, 20))
            }

            pub fn start_closure() {
                spawn(|x, y| {
                    root::add("local/vm_std_spawn/closure", x + y);
                }, (3, 4))
            }

            pub fn start_closure_vars() {
                let x = 5;
                let y = 6;
                spawn(|left, right| {
                    root::add("local/vm_std_spawn/closure_vars", left + right);
                }, (x, y))
            }
            "#
            .to_vec(),
        )?;

        let compiled = vm.get_fn("vm_std_spawn::start_zero", &[])?;
        assert_eq!(compiled.ret_ty(), &Type::Bool);
        let start_zero: extern "C" fn() -> bool = unsafe { std::mem::transmute(compiled.ptr()) };
        assert!(start_zero());

        let compiled = vm.get_fn("vm_std_spawn::start_sum", &[])?;
        assert_eq!(compiled.ret_ty(), &Type::Bool);
        let start_sum: extern "C" fn() -> bool = unsafe { std::mem::transmute(compiled.ptr()) };
        assert!(start_sum());

        let compiled = vm.get_fn("vm_std_spawn::start_closure", &[])?;
        assert_eq!(compiled.ret_ty(), &Type::Bool);
        let start_closure: extern "C" fn() -> bool = unsafe { std::mem::transmute(compiled.ptr()) };
        assert!(start_closure());

        let compiled = vm.get_fn("vm_std_spawn::start_closure_vars", &[])?;
        assert_eq!(compiled.ret_ty(), &Type::Bool);
        let start_closure_vars: extern "C" fn() -> bool = unsafe { std::mem::transmute(compiled.ptr()) };
        assert!(start_closure_vars());

        for _ in 0..50 {
            let zero_done = root::get(zero_path).ok().and_then(|value| value.as_int()) == Some(1);
            let sum_done = root::get(sum_path).ok().and_then(|value| value.as_int()) == Some(30);
            let closure_done = root::get(closure_path).ok().and_then(|value| value.as_int()) == Some(7);
            let closure_vars_done = root::get(closure_vars_path).ok().and_then(|value| value.as_int()) == Some(11);
            if zero_done && sum_done && closure_done && closure_vars_done {
                return Ok(());
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }

        anyhow::bail!("spawned jobs did not write expected results");
    }

    #[test]
    fn native_can_save_and_later_call_closure_callback() -> anyhow::Result<()> {
        static SAVED_CALLBACK: Mutex<Option<ZustCallback>> = Mutex::new(None);

        extern "C" fn save_callback(callback: *const Dynamic) -> bool {
            if callback.is_null() {
                return false;
            }
            let Some(callback) = (unsafe { &*callback }).as_custom::<ZustCallback>().cloned() else {
                return false;
            };
            *SAVED_CALLBACK.lock().unwrap() = Some(callback);
            true
        }

        let path = "local/vm_callback/result";
        let _ = root::remove(path);
        *SAVED_CALLBACK.lock().unwrap() = None;

        let vm = Vm::with_all()?;
        vm.add_native_module_ptr("callback_test", "save", &[Type::Any], Type::Bool, save_callback as *const u8)?;
        vm.import_code(
            "vm_callback",
            br#"
            pub fn register() {
                let n = 41;
                callback_test::save(|| {
                    root::add("local/vm_callback/result", n + 1);
                    true
                })
            }
            "#
            .to_vec(),
        )?;

        let compiled = vm.get_fn("vm_callback::register", &[])?;
        assert_eq!(compiled.ret_ty(), &Type::Bool);
        let register: extern "C" fn() -> bool = unsafe { std::mem::transmute(compiled.ptr()) };
        assert!(register());
        assert!(root::get(path).is_err());

        let callback = SAVED_CALLBACK.lock().unwrap().clone().expect("callback should be saved");
        let result = callback.call0()?;
        assert_eq!(result.as_bool(), Some(true));
        assert_eq!(root::get(path)?.as_int(), Some(42));
        Ok(())
    }

    #[test]
    fn native_can_save_and_later_call_named_function_callback() -> anyhow::Result<()> {
        static SAVED_CALLBACK: Mutex<Option<ZustCallback>> = Mutex::new(None);

        extern "C" fn save_callback(callback: *const Dynamic) -> bool {
            if callback.is_null() {
                return false;
            }
            let Some(callback) = (unsafe { &*callback }).as_custom::<ZustCallback>().cloned() else {
                return false;
            };
            *SAVED_CALLBACK.lock().unwrap() = Some(callback);
            true
        }

        let path = "local/vm_named_callback/result";
        let _ = root::remove(path);
        *SAVED_CALLBACK.lock().unwrap() = None;

        let vm = Vm::with_all()?;
        vm.add_native_module_ptr("callback_test", "save", &[Type::Any], Type::Bool, save_callback as *const u8)?;
        vm.import_code(
            "vm_named_callback",
            br#"
            pub fn on_result() {
                root::add("local/vm_named_callback/result", "done");
                true
            }

            pub fn register() {
                callback_test::save(on_result)
            }
            "#
            .to_vec(),
        )?;

        let register = vm.get_fn("vm_named_callback::register", &[])?;
        let register: extern "C" fn() -> bool = unsafe { std::mem::transmute(register.ptr()) };
        assert!(register());
        assert!(root::get(path).is_err());

        let callback = SAVED_CALLBACK.lock().unwrap().clone().expect("callback should be saved");
        assert_eq!(callback.call1(dynamic::map!("text"=> "done"))?.as_bool(), Some(true));
        assert_eq!(root::get(path)?.as_str(), "done");
        Ok(())
    }

    #[test]
    fn native_callback_can_receive_later_dynamic_args() -> anyhow::Result<()> {
        static SAVED_PATH_CALLBACK: Mutex<Option<ZustCallback>> = Mutex::new(None);
        static SAVED_SUM_CALLBACK: Mutex<Option<ZustCallback>> = Mutex::new(None);

        extern "C" fn save_path_callback(callback: *const Dynamic) -> bool {
            if callback.is_null() {
                return false;
            }
            let Some(callback) = (unsafe { &*callback }).as_custom::<ZustCallback>().cloned() else {
                return false;
            };
            *SAVED_PATH_CALLBACK.lock().unwrap() = Some(callback);
            true
        }

        extern "C" fn save_sum_callback(callback: *const Dynamic) -> bool {
            if callback.is_null() {
                return false;
            }
            let Some(callback) = (unsafe { &*callback }).as_custom::<ZustCallback>().cloned() else {
                return false;
            };
            *SAVED_SUM_CALLBACK.lock().unwrap() = Some(callback);
            true
        }

        let path_result = "local/vm_callback/path";
        let sum_result = "local/vm_callback/sum8";
        let _ = root::remove(path_result);
        let _ = root::remove(sum_result);
        *SAVED_PATH_CALLBACK.lock().unwrap() = None;
        *SAVED_SUM_CALLBACK.lock().unwrap() = None;

        let vm = Vm::with_all()?;
        vm.add_native_module_ptr("callback_test", "save_path", &[Type::Any], Type::Bool, save_path_callback as *const u8)?;
        vm.add_native_module_ptr("callback_test", "save_sum", &[Type::Any], Type::Bool, save_sum_callback as *const u8)?;
        vm.import_code(
            "vm_callback_args",
            br#"
            pub fn register_path() {
                let key = "local/vm_callback/path";
                callback_test::save_path(|path| {
                    root::add(key, path);
                    true
                })
            }

            pub fn register_sum() {
                callback_test::save_sum(|a, b, c, d, e, f, g, h| {
                    root::add("local/vm_callback/sum8", a + b + c + d + e + f + g + h);
                    true
                })
            }
            "#
            .to_vec(),
        )?;

        let register_path = vm.get_fn("vm_callback_args::register_path", &[])?;
        let register_path: extern "C" fn() -> bool = unsafe { std::mem::transmute(register_path.ptr()) };
        assert!(register_path());

        let register_sum = vm.get_fn("vm_callback_args::register_sum", &[])?;
        let register_sum: extern "C" fn() -> bool = unsafe { std::mem::transmute(register_sum.ptr()) };
        assert!(register_sum());

        let path_callback = SAVED_PATH_CALLBACK.lock().unwrap().clone().expect("path callback should be saved");
        assert_eq!(path_callback.call1(Dynamic::from("picked.txt"))?.as_bool(), Some(true));
        assert_eq!(root::get(path_result)?.as_str(), "picked.txt");

        let sum_callback = SAVED_SUM_CALLBACK.lock().unwrap().clone().expect("sum callback should be saved");
        let sum_args = (1i64..=8).map(Dynamic::from).collect();
        assert_eq!(sum_callback.call(sum_args)?.as_bool(), Some(true));
        assert_eq!(root::get(sum_result)?.as_int(), Some(36));
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
    fn root_send_idx_returns_handler_value() -> anyhow::Result<()> {
        fn echo_handler(msg: Dynamic) -> Dynamic {
            dynamic::map!("type"=> "echo", "id"=> msg.get_dynamic("id").unwrap_or(Dynamic::Null))
        }

        let vm = Vm::with_all()?;
        vm.import_code(
            "vm_root_send_idx_return",
            br#"
            pub fn call(req) {
                root::send_idx("local/send_idx_return_handlers", 0, req)
            }
            "#
            .to_vec(),
        )?;

        root::add_list("local/send_idx_return_handlers")?;
        let (mount, name) = root::get_mount("local/send_idx_return_handlers")?;
        mount.push(name, root::Object::Native(echo_handler))?;

        assert_eq!(vm.infer("root::send_idx", &[Type::Any, Type::I64, Type::Any])?, Type::Any);
        let compiled = vm.get_fn("vm_root_send_idx_return::call", &[Type::Any])?;
        assert_eq!(compiled.ret_ty(), &Type::Any);
        let call: extern "C" fn(*const Dynamic) -> *const Dynamic = unsafe { std::mem::transmute(compiled.ptr()) };
        let req = dynamic::map!("id"=> 42i64);
        let result = unsafe { &*call(&req) };

        assert_eq!(result.get_dynamic("type").map(|value| value.as_str().to_string()), Some("echo".to_string()));
        assert_eq!(result.get_dynamic("id").and_then(|value| value.as_int()), Some(42));
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
                let first_note = if first.contains("note") { first.note } else { "fallback" };
                first_note
            }

            pub fn first_ja(steps) {
                let first = if steps.len() > 0 { steps[0] } else { {} };
                if first.contains("ja") { first.ja } else { "すみません" }
            }

            pub fn assign_first_note(steps) {
                let first = {};
                first = if steps.len() > 0 { steps[0] } else { {} };
                if first.contains("note") { first.note } else { "fallback" }
            }
            "#
            .as_bytes()
            .to_vec(),
        )?;

        let compiled = vm.get_fn("vm_if_empty_object_branch::first_note", &[Type::Any])?;
        assert_eq!(compiled.ret_ty(), &Type::Str);
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
    fn string_return_survives_scope_exit() -> anyhow::Result<()> {
        let vm = Vm::with_all()?;
        vm.import_code(
            "vm_string_return_scope",
            r#"
            pub fn source_root() {
                "../assets/character/男主角换装"
            }

            pub fn binary_root() {
                "character_binary/男主角换装"
            }

            pub fn runtime_binary_url() {
                "/" + binary_root()
            }

            pub fn action_groups() {
                let root = source_root();
                let binary_url = runtime_binary_url();
                let binary_root = binary_root();
                [
                    {
                        id: "field_bottom",
                        source_spine: root + "/战斗外/boy_b.spine",
                        skeleton: binary_url + "/战斗外/boy_b/boy_b.skel.bytes",
                        export_skeleton: binary_root + "/战斗外/boy_b/boy_b.skel.bytes"
                    }
                ]
            }
            "#
            .as_bytes()
            .to_vec(),
        )?;

        let compiled = vm.get_fn("vm_string_return_scope::source_root", &[])?;
        assert_eq!(compiled.ret_ty(), &Type::Str);
        let source_root: extern "C" fn() -> *const Dynamic = unsafe { std::mem::transmute(compiled.ptr()) };
        let source_root = unsafe { &*source_root() };
        assert_eq!(source_root.as_str(), "../assets/character/男主角换装");

        let compiled = vm.get_fn("vm_string_return_scope::action_groups", &[])?;
        assert_eq!(compiled.ret_ty(), &Type::Any);
        let action_groups: extern "C" fn() -> *const Dynamic = unsafe { std::mem::transmute(compiled.ptr()) };
        let groups = unsafe { &*action_groups() };
        let first = groups.get_idx(0).expect("first action group");
        assert_eq!(first.get_dynamic("source_spine").map(|value| value.as_str().to_string()), Some("../assets/character/男主角换装/战斗外/boy_b.spine".to_string()));
        assert_eq!(first.get_dynamic("skeleton").map(|value| value.as_str().to_string()), Some("/character_binary/男主角换装/战斗外/boy_b/boy_b.skel.bytes".to_string()));
        Ok(())
    }

    #[test]
    fn dynamic_string_add_uses_any_binary_fast_path() -> anyhow::Result<()> {
        let vm = Vm::with_all()?;
        vm.import_code(
            "vm_dynamic_string_add",
            br#"
            pub fn concat(left, right) {
                left + right
            }
            "#
            .to_vec(),
        )?;

        let compiled = vm.get_fn("vm_dynamic_string_add::concat", &[Type::Any, Type::Any])?;
        assert_eq!(compiled.ret_ty(), &Type::Any);
        let concat: extern "C" fn(*const Dynamic, *const Dynamic) -> *const Dynamic = unsafe { std::mem::transmute(compiled.ptr()) };
        let left = Dynamic::from("hello");
        let right = Dynamic::from(" world");
        let result = unsafe { &*concat(&left, &right) };
        assert_eq!(result.as_str(), "hello world");
        Ok(())
    }

    #[test]
    fn large_dynamic_object_accepts_inline_call_fields() -> anyhow::Result<()> {
        let vm = Vm::with_all()?;
        let model_count = 180;
        let combination_count = 90;
        let models = (0..model_count)
            .map(|idx| {
                format!(
                    r#"{{id: "model_{idx}", name: "模型_{idx}", source: "/美术资源/角色/少年/套装_{idx}/模型_{idx}.model.json", parts: [
                        {{slot: "hair", path: "/模型/头发/颜色_{idx}/默认.png", z: 10}},
                        {{slot: "body", path: "/模型/身体/套装_{idx}/默认.png", z: 1}},
                        {{slot: "face", path: "/模型/表情/表情_{idx}/默认.png", z: 20}}
                    ]}}"#
                )
            })
            .collect::<Vec<_>>()
            .join(",\n");
        let combinations = (0..combination_count).map(|idx| format!(r#"{{hair: "color_{idx}", body: "set_{idx}", face: "face_{idx}"}}"#)).collect::<Vec<_>>().join(",\n");
        let code = format!(
            r#"
            pub fn source_root() {{
                "/美术资源/角色/少年/默认"
            }}

            pub fn runtime_boy_url() {{
                "/cdn/runtime/角色/少年/少年.model.json"
            }}

            pub fn parts() {{
                [
                    {{id: "hair", path: "/模型/头发/黑色/默认.png", z: 10}},
                    {{id: "body", path: "/模型/身体/校服/默认.png", z: 1}},
                    {{id: "face", path: "/模型/表情/微笑/默认.png", z: 20}}
                ]
            }}

            pub fn action_groups() {{
                {{
                    idle: [
                        {{id: "stand", name: "站立", frames: ["待机/0001.png", "待机/0002.png"]}},
                        {{id: "blink", name: "眨眼", frames: ["表情/眨眼/0001.png", "表情/眨眼/0002.png"]}}
                    ],
                    move: [
                        {{id: "walk", name: "行走", frames: ["行走/0001.png", "行走/0002.png"]}},
                        {{id: "run", name: "奔跑", frames: ["奔跑/0001.png", "奔跑/0002.png"]}}
                    ]
                }}
            }}

            pub fn default_model() {{
                {{
                    id: "runtime_boy",
                    name: "运行时少年",
                    skins: [
                        {{id: "school", title: "校服", source: "/套装/校服/model.json"}},
                        {{id: "casual", title: "便服", source: "/套装/便服/model.json"}}
                    ],
                    models: [
                        {models}
                    ]
                }}
            }}

            pub fn first_nine_combinations() {{
                [
                    {combinations}
                ]
            }}

            pub fn config() {{
                {{
                    source_root: source_root(),
                    runtime_boy_url: runtime_boy_url(),
                    parts: parts(),
                    action_groups: action_groups(),
                    default_model: default_model(),
                    first_nine_combinations: first_nine_combinations()
                }}
            }}

            pub fn start() {{
                root::add("local/vm_large_inline_call_object/config", {{
                    source_root: source_root(),
                    runtime_boy_url: runtime_boy_url(),
                    parts: parts(),
                    action_groups: action_groups(),
                    default_model: default_model(),
                    first_nine_combinations: first_nine_combinations()
                }})
            }}
            "#
        );
        vm.import_code("vm_large_inline_call_object", code.into_bytes())?;

        let compiled = vm.get_fn("vm_large_inline_call_object::config", &[])?;
        assert_eq!(compiled.ret_ty(), &Type::Any);
        let config: extern "C" fn() -> *const Dynamic = unsafe { std::mem::transmute(compiled.ptr()) };
        let result = unsafe { &*config() };
        assert_eq!(result.get_dynamic("source_root").map(|value| value.as_str().to_string()), Some("/美术资源/角色/少年/默认".to_string()));
        assert_eq!(result.get_dynamic("first_nine_combinations").map(|value| value.len()), Some(combination_count));

        let compiled = vm.get_fn("vm_large_inline_call_object::start", &[])?;
        assert_eq!(compiled.ret_ty(), &Type::Bool);
        let start: extern "C" fn() -> bool = unsafe { std::mem::transmute(compiled.ptr()) };
        assert!(start());
        let saved = root::get("local/vm_large_inline_call_object/config")?;
        assert_eq!(saved.get_dynamic("first_nine_combinations").map(|value| value.len()), Some(combination_count));
        Ok(())
    }

    #[test]
    fn http_serve_accepts_inline_config_map() -> anyhow::Result<()> {
        let vm = Vm::with_all()?;
        vm.import_code(
            "vm_http_serve_inline_config",
            br#"
            pub fn start() {
                let server = http::serve({host: "127.0.0.1:5192"});
                server
            }
            "#
            .to_vec(),
        )?;

        let compiled = vm.get_fn("vm_http_serve_inline_config::start", &[])?;
        assert_eq!(compiled.ret_ty(), &Type::Any);
        Ok(())
    }

    #[test]
    fn http_serve_accepts_variable_and_quoted_static_key() -> anyhow::Result<()> {
        let vm = Vm::with_all()?;
        vm.import_code(
            "vm_http_serve_quoted_static",
            br#"
            pub fn start(server_addr) {
                let http_server = http::serve({
                    host: server_addr,
                    ws: true,
                    upload: "upload",
                    "static": {
                        path: "/",
                        dir: "public/local"
                    }
                });
                http_server
            }
            "#
            .to_vec(),
        )?;

        let compiled = vm.get_fn("vm_http_serve_quoted_static::start", &[Type::Any])?;
        assert_eq!(compiled.ret_ty(), &Type::Any);
        Ok(())
    }

    #[test]
    fn oss_helpers_accept_explicit_config() -> anyhow::Result<()> {
        let vm = Vm::with_all()?;
        vm.import_code(
            "vm_oss_explicit_config",
            br#"
            pub fn upload(oss, bytes) {
                oss::upload(oss, "llm/input/audio.wav", bytes)
            }

            pub fn http_upload(oss, bytes) {
                http::upload(oss, "uploads/input.bin", bytes)
            }

            pub fn link(oss, uploaded) {
                oss::signed_url(oss, {oss_url: uploaded, expires: 3600})
            }
            "#
            .to_vec(),
        )?;

        assert_eq!(vm.get_fn("vm_oss_explicit_config::upload", &[Type::Any, Type::Any])?.ret_ty(), &Type::Any);
        assert_eq!(vm.get_fn("vm_oss_explicit_config::http_upload", &[Type::Any, Type::Any])?.ret_ty(), &Type::Any);
        assert_eq!(vm.get_fn("vm_oss_explicit_config::link", &[Type::Any, Type::Any])?.ret_ty(), &Type::Any);
        Ok(())
    }

    #[test]
    fn load_script_accepts_http_serve_inline_config() -> anyhow::Result<()> {
        let vm = Vm::with_all()?;
        let (_fn_ptr, ty) = vm.load(
            br#"
            let server_addr = "127.0.0.1:5192";
            let http_server = http::serve({
                host: server_addr,
                ws: true,
                upload: "upload",
                "static": {
                    path: "/",
                    dir: "public/local"
                }
            });
            http_server
            "#
            .to_vec(),
            "arg".into(),
        )?;

        assert_eq!(ty, Type::Any);
        Ok(())
    }

    #[test]
    fn load_script_resolves_import_before_compile() -> anyhow::Result<()> {
        let module_path = std::env::temp_dir().join(format!("zust_vm_load_import_{}.zs", std::process::id()));
        std::fs::write(&module_path, "pub fn init() { return {ok: true}; }")?;
        let module_path = module_path.to_string_lossy().replace('\\', "\\\\").replace('"', "\\\"");

        let vm = Vm::with_all()?;
        let (_fn_ptr, ty) = vm.load(
            format!(
                r#"
                import("create_scene", "{module_path}");
                create_scene::init();
                "#
            )
            .into_bytes(),
            "req".into(),
        )?;

        assert_eq!(ty, Type::Void);
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

    #[derive(Debug, Default)]
    struct PropertyForwardingObject {
        values: RwLock<BTreeMap<String, Dynamic>>,
    }

    impl CustomProperty for PropertyForwardingObject {
        fn get_key(&self, key: &str) -> Option<Dynamic> {
            self.values.read().unwrap().get(key).cloned()
        }

        fn set_key(&self, key: &str, value: Dynamic) -> bool {
            self.values.write().unwrap().insert(key.to_string(), value);
            true
        }
    }

    extern "C" fn property_forwarding_object_new() -> *const Dynamic {
        Box::into_raw(Box::new(Dynamic::custom_with_properties(PropertyForwardingObject::default())))
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
    fn any_field_assignment_forwards_to_custom_properties() -> anyhow::Result<()> {
        let vm = Vm::with_all()?;
        vm.add_empty_type("Dialog")?;
        vm.add_native_method_ptr("Dialog", "new", &[], Type::Any, property_forwarding_object_new as *const u8)?;
        vm.import_code(
            "vm_custom_property_forwarding",
            br#"
            pub fn run() {
                let dialog = Dialog::new();
                dialog.file_mode = 3;
                dialog.file_mode
            }
            "#
            .to_vec(),
        )?;

        let compiled = vm.get_fn("vm_custom_property_forwarding::run", &[])?;
        assert_eq!(compiled.ret_ty(), &Type::Any);
        let run: extern "C" fn() -> *const Dynamic = unsafe { std::mem::transmute(compiled.ptr()) };
        let result = unsafe { &*run() };

        assert_eq!(result.as_int(), Some(3));
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

    // ---- 新增边界条件测试 ----

    #[test]
    fn dynamic_type_checks_on_null_and_primitive_values() -> anyhow::Result<()> {
        let vm = Vm::with_all()?;
        vm.import_code(
            "vm_dynamic_type_checks",
            br#"
            pub fn is_list_on_int() {
                let x = 42i64;
                x.is_list()
            }

            pub fn is_map_on_int() {
                let x = 42i64;
                x.is_map()
            }

            pub fn is_null_on_int() {
                let x = 42i64;
                x.is_null()
            }
            "#
            .to_vec(),
        )?;

        let compiled = vm.get_fn("vm_dynamic_type_checks::is_list_on_int", &[])?;
        assert_eq!(compiled.ret_ty(), &Type::Bool);
        let is_list_on_int: extern "C" fn() -> bool = unsafe { std::mem::transmute(compiled.ptr()) };
        assert!(!is_list_on_int());

        let compiled = vm.get_fn("vm_dynamic_type_checks::is_map_on_int", &[])?;
        assert_eq!(compiled.ret_ty(), &Type::Bool);
        let is_map_on_int: extern "C" fn() -> bool = unsafe { std::mem::transmute(compiled.ptr()) };
        assert!(!is_map_on_int());

        let compiled = vm.get_fn("vm_dynamic_type_checks::is_null_on_int", &[])?;
        assert_eq!(compiled.ret_ty(), &Type::Bool);
        let is_null_on_int: extern "C" fn() -> bool = unsafe { std::mem::transmute(compiled.ptr()) };
        assert!(!is_null_on_int());
        Ok(())
    }

    #[test]
    fn empty_for_loop_range_has_zero_iterations() -> anyhow::Result<()> {
        let vm = Vm::with_all()?;
        vm.import_code(
            "vm_empty_for_range",
            br#"
            pub fn empty_exclusive() {
                let count = 0i32;
                for i in 0..0 {
                    count += i;
                }
                count
            }

            pub fn single_inclusive_iteration() {
                let count = 0i32;
                for i in 5..=5 {
                    count += i;
                }
                count
            }
            "#
            .to_vec(),
        )?;

        let compiled = vm.get_fn("vm_empty_for_range::empty_exclusive", &[])?;
        assert_eq!(compiled.ret_ty(), &Type::I32);
        let empty_exclusive: extern "C" fn() -> i32 = unsafe { std::mem::transmute(compiled.ptr()) };
        assert_eq!(empty_exclusive(), 0);

        let compiled = vm.get_fn("vm_empty_for_range::single_inclusive_iteration", &[])?;
        assert_eq!(compiled.ret_ty(), &Type::I32);
        let single_inclusive: extern "C" fn() -> i32 = unsafe { std::mem::transmute(compiled.ptr()) };
        assert_eq!(single_inclusive(), 5);
        Ok(())
    }

    #[test]
    fn map_contains_key_on_non_existent_and_nested_keys() -> anyhow::Result<()> {
        let vm = Vm::with_all()?;
        vm.import_code(
            "vm_map_contains",
            br#"
            pub fn contains_existing(data) {
                data.contains("name")
            }

            pub fn contains_missing(data) {
                data.contains("nothing")
            }
            "#
            .to_vec(),
        )?;

        let compiled = vm.get_fn("vm_map_contains::contains_existing", &[Type::Any])?;
        assert_eq!(compiled.ret_ty(), &Type::Bool);
        let contains_existing: extern "C" fn(*const Dynamic) -> bool = unsafe { std::mem::transmute(compiled.ptr()) };
        let data = dynamic::map!("name"=> "test");
        assert!(contains_existing(&data));

        let compiled = vm.get_fn("vm_map_contains::contains_missing", &[Type::Any])?;
        assert_eq!(compiled.ret_ty(), &Type::Bool);
        let contains_missing: extern "C" fn(*const Dynamic) -> bool = unsafe { std::mem::transmute(compiled.ptr()) };
        assert!(!contains_missing(&data));
        Ok(())
    }

    #[test]
    fn list_pop_on_empty_list_returns_null() -> anyhow::Result<()> {
        let vm = Vm::with_all()?;
        vm.import_code(
            "vm_pop_empty",
            br#"
            pub fn pop_new_list() {
                let items = [];
                let value = items.pop();
                let still_empty = items.len() == 0;
                {value: value, empty: still_empty}
            }

            pub fn pop_until_empty() {
                let items = [1i64, 2i64];
                items.pop();
                let last = items.pop();
                let drained = items.pop();
                {last: last, drained: drained}
            }
            "#
            .to_vec(),
        )?;

        let compiled = vm.get_fn("vm_pop_empty::pop_new_list", &[])?;
        assert_eq!(compiled.ret_ty(), &Type::Any);
        let pop_new_list: extern "C" fn() -> *const Dynamic = unsafe { std::mem::transmute(compiled.ptr()) };
        let result = unsafe { &*pop_new_list() };
        assert!(result.get_dynamic("value").is_some_and(|v| v.is_null()));
        assert_eq!(result.get_dynamic("empty").and_then(|v| v.as_bool()), Some(true));

        let compiled = vm.get_fn("vm_pop_empty::pop_until_empty", &[])?;
        assert_eq!(compiled.ret_ty(), &Type::Any);
        let pop_until_empty: extern "C" fn() -> *const Dynamic = unsafe { std::mem::transmute(compiled.ptr()) };
        let result = unsafe { &*pop_until_empty() };
        assert_eq!(result.get_dynamic("last").and_then(|v| v.as_int()), Some(1));
        assert!(result.get_dynamic("drained").is_some_and(|v| v.is_null()));
        Ok(())
    }

    #[test]
    fn void_function_with_multiple_code_paths() -> anyhow::Result<()> {
        let vm = Vm::with_all()?;
        vm.import_code(
            "vm_void_multi_path",
            br#"
            pub fn log_if_positive(value: i64) {
                if value > 0 {
                    print(value);
                    return;
                }
                if value < 0 {
                    print(-value);
                    return;
                }
                print(0);
            }
            "#
            .to_vec(),
        )?;

        let compiled = vm.get_fn("vm_void_multi_path::log_if_positive", &[Type::I64])?;
        assert!(compiled.ret_ty().is_void());
        Ok(())
    }

    #[test]
    fn any_method_call_chain_on_returned_dynamic_value() -> anyhow::Result<()> {
        let vm = Vm::with_all()?;
        vm.import_code(
            "vm_any_method_chain",
            br#"
            pub fn get_tags(data) {
                let tags = data.tags;
                if tags.is_list() {
                    return tags.len();
                }
                0
            }
            "#
            .to_vec(),
        )?;

        let compiled = vm.get_fn("vm_any_method_chain::get_tags", &[Type::Any])?;
        assert_eq!(compiled.ret_ty(), &Type::I32);
        let get_tags: extern "C" fn(*const Dynamic) -> i32 = unsafe { std::mem::transmute(compiled.ptr()) };
        let data = dynamic::map!("tags"=> Dynamic::list(vec!["a".into(), "b".into(), "c".into()]));
        assert_eq!(get_tags(&data), 3);

        let empty_data = Dynamic::Null;
        assert_eq!(get_tags(&empty_data), 0);
        Ok(())
    }

    #[test]
    fn infers_any_arg_function_return_before_body_compile() -> anyhow::Result<()> {
        let vm = Vm::with_all()?;
        vm.import_code(
            "vm_infer_any_arg_return",
            br#"
            pub fn caller(candidate) {
                let center = polygon_center(candidate.visualPolygon);
                center[0]
            }

            pub fn polygon_center(point_list) {
                let total_x = 0;
                let total_y = 0;
                let count = 0;
                if point_list.is_list() {
                    for point in point_list {
                        if point.is_list() && point.len() >= 2 {
                            total_x += point[0];
                            total_y += point[1];
                            count += 1;
                        }
                    }
                }
                if count == 0 {
                    return [0, 0];
                }
                [total_x / count, total_y / count]
            }
            "#
            .to_vec(),
        )?;

        let compiled = vm.get_fn("vm_infer_any_arg_return::caller", &[Type::Any])?;
        assert_eq!(compiled.ret_ty(), &Type::Any);
        let caller: extern "C" fn(*const Dynamic) -> *const Dynamic = unsafe { std::mem::transmute(compiled.ptr()) };
        let candidate = dynamic::map!(
            "visualPolygon"=> Dynamic::list(vec![
                Dynamic::list(vec![2i64.into(), 4i64.into()]),
                Dynamic::list(vec![6i64.into(), 8i64.into()]),
            ])
        );
        let result = unsafe { &*caller(&candidate) };
        assert_eq!(result.as_int(), Some(4));
        Ok(())
    }

    #[test]
    fn recursive_factorial_keeps_static_return_type() -> anyhow::Result<()> {
        let vm = Vm::with_all()?;
        vm.import_code(
            "vm_recursive_factorial",
            br#"
            fn factorial(n: i64) {
                if n <= 1 {
                    return 1;
                }
                n * factorial(n - 1)
            }

            pub fn run(n: i64) {
                factorial(n)
            }
            "#
            .to_vec(),
        )?;

        let compiled = vm.get_fn("vm_recursive_factorial::run", &[Type::I64])?;
        assert_eq!(compiled.ret_ty(), &Type::I64);
        let run: extern "C" fn(i64) -> i64 = unsafe { std::mem::transmute(compiled.ptr()) };
        assert_eq!(run(5), 120);
        Ok(())
    }

    #[test]
    fn explicit_const_generic_function_calls_generate_distinct_variants() -> anyhow::Result<()> {
        let vm = Vm::with_all()?;
        vm.import_code(
            "vm_generic_const_variants",
            br#"
            fn value<N>() {
                N
            }

            pub fn two() {
                value::<2>()
            }

            pub fn three() {
                value::<3>()
            }
            "#
            .to_vec(),
        )?;

        let compiled = vm.get_fn("vm_generic_const_variants::two", &[])?;
        assert_eq!(compiled.ret_ty(), &Type::I32);
        let two: extern "C" fn() -> i32 = unsafe { std::mem::transmute(compiled.ptr()) };
        assert_eq!(two(), 2);

        let compiled = vm.get_fn("vm_generic_const_variants::three", &[])?;
        assert_eq!(compiled.ret_ty(), &Type::I32);
        let three: extern "C" fn() -> i32 = unsafe { std::mem::transmute(compiled.ptr()) };
        assert_eq!(three(), 3);
        Ok(())
    }

    #[test]
    fn generic_function_body_resolves_private_generic_helper_after_import() -> anyhow::Result<()> {
        let vm = Vm::with_all()?;
        vm.import_code(
            "vm_generic_private_helper",
            br#"
            fn helper<N>() {
                N
            }

            pub fn bench<N>() {
                helper::<N>()
            }
            "#
            .to_vec(),
        )?;

        let compiled = vm.get_fn_with_params("vm_generic_private_helper::bench", &[], &[Type::ConstInt(7)])?;
        assert_eq!(compiled.ret_ty(), &Type::I32);
        let run: extern "C" fn() -> i32 = unsafe { std::mem::transmute(compiled.ptr()) };
        assert_eq!(run(), 7);
        Ok(())
    }

    #[test]
    fn const_generic_repeat_array_initializes_all_items() -> anyhow::Result<()> {
        let vm = Vm::with_all()?;
        vm.import_code(
            "vm_generic_repeat_array",
            br#"
            fn bench<N>() {
                let is_prime = [true; N];
                is_prime[0] = false;
                is_prime[1] = false;
                let count = 0i64;
                for p in 2i64..N {
                    if is_prime[p] == true {
                        count = count + 1;
                        let step = p;
                        let j = p * p;
                        while j < N {
                            is_prime[j] = false;
                            j = j + step;
                        }
                    }
                }
                count
            }

            pub fn run() {
                bench::<10>()
            }

            pub fn run_1000() {
                bench::<1000>()
            }

            pub fn run_100000() {
                bench::<100000>()
            }
            "#
            .to_vec(),
        )?;

        let compiled = vm.get_fn("vm_generic_repeat_array::run", &[])?;
        assert_eq!(compiled.ret_ty(), &Type::I64);
        let run: extern "C" fn() -> i64 = unsafe { std::mem::transmute(compiled.ptr()) };
        assert_eq!(run(), 4);

        let compiled = vm.get_fn("vm_generic_repeat_array::run_1000", &[])?;
        assert_eq!(compiled.ret_ty(), &Type::I64);
        let run_1000: extern "C" fn() -> i64 = unsafe { std::mem::transmute(compiled.ptr()) };
        assert_eq!(run_1000(), 168);

        let compiled = vm.get_fn("vm_generic_repeat_array::run_100000", &[])?;
        assert_eq!(compiled.ret_ty(), &Type::I64);
        let run_100000: extern "C" fn() -> i64 = unsafe { std::mem::transmute(compiled.ptr()) };
        assert_eq!(run_100000(), 9592);
        Ok(())
    }

    #[test]
    fn repeat_array_initializes_scalar_patterns() -> anyhow::Result<()> {
        let vm = Vm::with_all()?;
        vm.import_code(
            "vm_repeat_scalar_patterns",
            br#"
            pub fn count_true() {
                let items = [true; 100000];
                let count = 0i64;
                for idx in 0i64..100000 {
                    if items[idx] == true {
                        count = count + 1;
                    }
                }
                count
            }

            pub fn i32_pair() {
                let items = [-7i32; 1000];
                items[0i64] + items[999i64]
            }

            pub fn i64_pair() {
                let items = [1234567890123i64; 1000];
                items[0i64] + items[999i64]
            }

            pub fn f64_pair() {
                let items = [1.5f64; 1000];
                items[0i64] + items[999i64]
            }
            "#
            .to_vec(),
        )?;

        let compiled = vm.get_fn("vm_repeat_scalar_patterns::count_true", &[])?;
        assert_eq!(compiled.ret_ty(), &Type::I64);
        let count_true: extern "C" fn() -> i64 = unsafe { std::mem::transmute(compiled.ptr()) };
        assert_eq!(count_true(), 100000);

        let compiled = vm.get_fn("vm_repeat_scalar_patterns::i32_pair", &[])?;
        assert_eq!(compiled.ret_ty(), &Type::I32);
        let i32_pair: extern "C" fn() -> i32 = unsafe { std::mem::transmute(compiled.ptr()) };
        assert_eq!(i32_pair(), -14);

        let compiled = vm.get_fn("vm_repeat_scalar_patterns::i64_pair", &[])?;
        assert_eq!(compiled.ret_ty(), &Type::I64);
        let i64_pair: extern "C" fn() -> i64 = unsafe { std::mem::transmute(compiled.ptr()) };
        assert_eq!(i64_pair(), 2469135780246);

        let compiled = vm.get_fn("vm_repeat_scalar_patterns::f64_pair", &[])?;
        assert_eq!(compiled.ret_ty(), &Type::F64);
        let f64_pair: extern "C" fn() -> f64 = unsafe { std::mem::transmute(compiled.ptr()) };
        assert_eq!(f64_pair(), 3.0);
        Ok(())
    }

    #[test]
    fn bool_array_store_normalizes_condition_values() -> anyhow::Result<()> {
        let vm = Vm::with_all()?;
        vm.import_code(
            "vm_bool_array_store",
            br#"
            pub fn run() {
                let items = [false; 4];
                items[1] = 3i64 > 2i64;
                items[2] = 3i64 < 2i64;
                if items[1] == true && items[2] == false {
                    1i64
                } else {
                    0i64
                }
            }
            "#
            .to_vec(),
        )?;

        let compiled = vm.get_fn("vm_bool_array_store::run", &[])?;
        assert_eq!(compiled.ret_ty(), &Type::I64);
        let run: extern "C" fn() -> i64 = unsafe { std::mem::transmute(compiled.ptr()) };
        assert_eq!(run(), 1);
        Ok(())
    }

    #[test]
    fn bool_array_large_sequential_writes() -> anyhow::Result<()> {
        let vm = Vm::with_all()?;
        vm.import_code(
            "vm_bool_array_large_writes",
            br#"
            pub fn run() {
                let items = [true; 100000];
                for idx in 0i64..100000 {
                    items[idx] = false;
                }
                let count = 0i64;
                for idx in 0i64..100000 {
                    if items[idx] == false {
                        count = count + 1;
                    }
                }
                count
            }
            "#
            .to_vec(),
        )?;

        let compiled = vm.get_fn("vm_bool_array_large_writes::run", &[])?;
        assert_eq!(compiled.ret_ty(), &Type::I64);
        let run: extern "C" fn() -> i64 = unsafe { std::mem::transmute(compiled.ptr()) };
        assert_eq!(run(), 100000);
        Ok(())
    }

    #[test]
    fn bool_array_sieve_style_indices_stay_in_bounds() -> anyhow::Result<()> {
        let vm = Vm::with_all()?;
        vm.import_code(
            "vm_bool_array_sieve_indices",
            br#"
            pub fn run() {
                let items = [true; 100000];
                let writes = 0i64;
                for p in 2i64..100000 {
                    let step = p;
                    let j = p * p;
                    while j < 100000 {
                        items[j] = false;
                        writes = writes + 1;
                        j = j + step;
                    }
                }
                writes
            }
            "#
            .to_vec(),
        )?;

        let compiled = vm.get_fn("vm_bool_array_sieve_indices::run", &[])?;
        assert_eq!(compiled.ret_ty(), &Type::I64);
        let run: extern "C" fn() -> i64 = unsafe { std::mem::transmute(compiled.ptr()) };
        assert!(run() > 0);
        Ok(())
    }

    #[test]
    fn sieve_style_indices_compute_in_bounds_without_array_write() -> anyhow::Result<()> {
        let vm = Vm::with_all()?;
        vm.import_code(
            "vm_sieve_indices_no_write",
            br#"
            pub fn run() {
                let max_j = 0i64;
                for p in 2i64..100000 {
                    let step = p;
                    let j = p * p;
                    while j < 100000 {
                        if j < 0i64 {
                            return -1i64;
                        }
                        if j > max_j {
                            max_j = j;
                        }
                        j = j + step;
                    }
                }
                max_j
            }
            "#
            .to_vec(),
        )?;

        let compiled = vm.get_fn("vm_sieve_indices_no_write::run", &[])?;
        assert_eq!(compiled.ret_ty(), &Type::I64);
        let run: extern "C" fn() -> i64 = unsafe { std::mem::transmute(compiled.ptr()) };
        assert_eq!(run(), 99999);
        Ok(())
    }

    #[test]
    fn dynamic_list_index_sum_uses_static_accumulator_type() -> anyhow::Result<()> {
        let vm = Vm::with_all()?;
        vm.import_code(
            "vm_dynamic_index_sum",
            br#"
            pub fn sum_list(n: i64) {
                let l = [];
                for i in 0..n {
                    l.push(i);
                }
                let sum = 0i64;
                for j in 0..n {
                    sum = sum + l[j];
                }
                sum
            }
            "#
            .to_vec(),
        )?;

        let compiled = vm.get_fn("vm_dynamic_index_sum::sum_list", &[Type::I64])?;
        let sum_list_id = vm.jit.lock().unwrap().compiler.symbols.get_id("vm_dynamic_index_sum::sum_list")?;
        let hints = vm.jit.lock().unwrap().compiler.inferred_local_type_hints(sum_list_id, &[], &[Type::I64]);
        assert!(hints.iter().any(|ty| matches!(ty, Some(Type::List(elem)) if elem.as_ref() == &Type::I64)), "local type hints: {:?}", hints);
        assert_eq!(compiled.ret_ty(), &Type::I64);
        let sum_list: extern "C" fn(i64) -> i64 = unsafe { std::mem::transmute(compiled.ptr()) };
        assert_eq!(sum_list(1000), 499500);
        Ok(())
    }

    #[test]
    fn inferred_empty_list_uses_typed_dynamic_vector() -> anyhow::Result<()> {
        let vm = Vm::with_all()?;
        vm.import_code(
            "vm_inferred_typed_list",
            br#"
            pub fn make() {
                let l = [];
                l.push(1i64);
                l
            }
            "#
            .to_vec(),
        )?;

        let compiled = vm.get_fn("vm_inferred_typed_list::make", &[])?;
        assert_eq!(compiled.ret_ty(), &Type::Any);
        let make: extern "C" fn() -> *const Dynamic = unsafe { std::mem::transmute(compiled.ptr()) };
        let result = unsafe { &*make() };
        assert!(matches!(result, Dynamic::VecI64(values) if values == &vec![1]), "result: {:?}", result);
        Ok(())
    }

    #[test]
    fn inferred_list_shortcuts_cover_scalar_types() -> anyhow::Result<()> {
        let vm = Vm::with_all()?;
        vm.import_code(
            "vm_inferred_list_shortcuts",
            br#"
            pub fn second_bool() {
                let l = [];
                l.push(true);
                l.push(false);
                l[1]
            }

            pub fn first_u8() {
                let l = [];
                l.push(7u8);
                l[0]
            }

            pub fn sum_i32(n: i64) {
                let l = [];
                for i in 0..n {
                    l.push(i as i32);
                }
                let sum = 0i32;
                for j in 0..n {
                    sum = sum + l[j];
                }
                sum
            }

            pub fn sum_f32(n: i64) {
                let l = [];
                for i in 0..n {
                    l.push(i as f32);
                }
                let sum = 0f32;
                for j in 0..n {
                    sum = sum + l[j];
                }
                sum
            }

            pub fn second_str() {
                let l = [];
                l.push("first");
                l.push("second");
                l[1]
            }
            "#
            .to_vec(),
        )?;

        let compiled = vm.get_fn("vm_inferred_list_shortcuts::second_bool", &[])?;
        let second_bool_id = vm.jit.lock().unwrap().compiler.symbols.get_id("vm_inferred_list_shortcuts::second_bool")?;
        let hints = vm.jit.lock().unwrap().compiler.inferred_local_type_hints(second_bool_id, &[], &[]);
        assert!(hints.iter().any(|ty| matches!(ty, Some(Type::List(elem)) if elem.as_ref() == &Type::Bool)), "bool local type hints: {:?}", hints);
        assert_eq!(compiled.ret_ty(), &Type::Bool);
        let second_bool: extern "C" fn() -> bool = unsafe { std::mem::transmute(compiled.ptr()) };
        assert!(!second_bool());

        let compiled = vm.get_fn("vm_inferred_list_shortcuts::first_u8", &[])?;
        let first_u8_id = vm.jit.lock().unwrap().compiler.symbols.get_id("vm_inferred_list_shortcuts::first_u8")?;
        let hints = vm.jit.lock().unwrap().compiler.inferred_local_type_hints(first_u8_id, &[], &[]);
        assert!(hints.iter().any(|ty| matches!(ty, Some(Type::List(elem)) if elem.as_ref() == &Type::U8)), "u8 local type hints: {:?}", hints);
        assert_eq!(compiled.ret_ty(), &Type::U8);
        let first_u8: extern "C" fn() -> u8 = unsafe { std::mem::transmute(compiled.ptr()) };
        assert_eq!(first_u8(), 7);

        let compiled = vm.get_fn("vm_inferred_list_shortcuts::sum_i32", &[Type::I64])?;
        let sum_i32_id = vm.jit.lock().unwrap().compiler.symbols.get_id("vm_inferred_list_shortcuts::sum_i32")?;
        let hints = vm.jit.lock().unwrap().compiler.inferred_local_type_hints(sum_i32_id, &[], &[Type::I64]);
        assert!(hints.iter().any(|ty| matches!(ty, Some(Type::List(elem)) if elem.as_ref() == &Type::I32)), "i32 local type hints: {:?}", hints);
        assert_eq!(compiled.ret_ty(), &Type::I32);
        let sum_i32: extern "C" fn(i64) -> i32 = unsafe { std::mem::transmute(compiled.ptr()) };
        assert_eq!(sum_i32(100), 4950);

        let compiled = vm.get_fn("vm_inferred_list_shortcuts::sum_f32", &[Type::I64])?;
        let sum_f32_id = vm.jit.lock().unwrap().compiler.symbols.get_id("vm_inferred_list_shortcuts::sum_f32")?;
        let hints = vm.jit.lock().unwrap().compiler.inferred_local_type_hints(sum_f32_id, &[], &[Type::I64]);
        assert!(hints.iter().any(|ty| matches!(ty, Some(Type::List(elem)) if elem.as_ref() == &Type::F32)), "f32 local type hints: {:?}", hints);
        assert_eq!(compiled.ret_ty(), &Type::F32);
        let sum_f32: extern "C" fn(i64) -> f32 = unsafe { std::mem::transmute(compiled.ptr()) };
        assert_eq!(sum_f32(10), 45.0);

        let compiled = vm.get_fn("vm_inferred_list_shortcuts::second_str", &[])?;
        let second_str_id = vm.jit.lock().unwrap().compiler.symbols.get_id("vm_inferred_list_shortcuts::second_str")?;
        let hints = vm.jit.lock().unwrap().compiler.inferred_local_type_hints(second_str_id, &[], &[]);
        assert!(hints.iter().any(|ty| matches!(ty, Some(Type::List(elem)) if elem.as_ref() == &Type::Str)), "str local type hints: {:?}", hints);
        assert_eq!(compiled.ret_ty(), &Type::Str);
        let second_str: extern "C" fn() -> *const Dynamic = unsafe { std::mem::transmute(compiled.ptr()) };
        let result = unsafe { &*second_str() };
        assert_eq!(result.as_str(), "second");
        Ok(())
    }

    #[test]
    fn inferred_list_supports_bracket_set_idx() -> anyhow::Result<()> {
        let vm = Vm::with_all()?;
        vm.import_code(
            "vm_inferred_list_set_idx",
            br#"
            pub fn swap_first_two() {
                let items = [];
                items.push(1i64);
                items.push(2i64);
                let j = 0i64;
                let a = items[j];
                let b = items[j + 1];
                items[j] = b;
                items[j + 1] = a;
                items[0] * 10i64 + items[1]
            }

            pub fn replace_string() {
                let items = [];
                items.push("old");
                items[0] = "new";
                items[0]
            }
            "#
            .to_vec(),
        )?;

        let compiled = vm.get_fn("vm_inferred_list_set_idx::swap_first_two", &[])?;
        assert_eq!(compiled.ret_ty(), &Type::I64);
        let swap_first_two: extern "C" fn() -> i64 = unsafe { std::mem::transmute(compiled.ptr()) };
        assert_eq!(swap_first_two(), 21);

        let compiled = vm.get_fn("vm_inferred_list_set_idx::replace_string", &[])?;
        assert_eq!(compiled.ret_ty(), &Type::Str);
        let replace_string: extern "C" fn() -> *const Dynamic = unsafe { std::mem::transmute(compiled.ptr()) };
        let result = unsafe { &*replace_string() };
        assert_eq!(result.as_str(), "new");
        Ok(())
    }

    #[test]
    fn root_get_returns_null_for_missing_key_which_compares_correctly() -> anyhow::Result<()> {
        let vm = Vm::with_all()?;
        vm.import_code(
            "vm_root_get_missing",
            br#"
            pub fn check_missing() {
                let existing = root::get("local/vm_root_get_missing_test");
                if existing.is_map() {
                    return false;
                }
                true
            }
            "#
            .to_vec(),
        )?;

        let compiled = vm.get_fn("vm_root_get_missing::check_missing", &[])?;
        assert_eq!(compiled.ret_ty(), &Type::Bool);
        let check_missing: extern "C" fn() -> bool = unsafe { std::mem::transmute(compiled.ptr()) };
        assert!(check_missing());
        Ok(())
    }

    #[test]
    fn map_get_key_on_null_map_returns_null() -> anyhow::Result<()> {
        let vm = Vm::with_all()?;
        vm.import_code(
            "vm_get_key_null_map",
            br#"
            pub fn get_key_null(data) {
                data.get_key("missing")
            }
            "#
            .to_vec(),
        )?;

        let compiled = vm.get_fn("vm_get_key_null_map::get_key_null", &[Type::Any])?;
        assert_eq!(compiled.ret_ty(), &Type::Any);
        let get_key_null: extern "C" fn(*const Dynamic) -> *const Dynamic = unsafe { std::mem::transmute(compiled.ptr()) };

        let data_map = dynamic::map!("exists"=> 1i64);
        let missing = unsafe { &*get_key_null(&data_map) };
        assert!(missing.is_null());

        let null = Dynamic::Null;
        let result = unsafe { &*get_key_null(&null) };
        assert!(result.is_null());
        Ok(())
    }

    #[test]
    fn keys_on_empty_map_returns_empty_list() -> anyhow::Result<()> {
        let vm = Vm::with_all()?;
        vm.import_code(
            "vm_keys_empty_map",
            br#"
            pub fn empty_map_keys() {
                let data = {};
                data.keys().len()
            }
            "#
            .to_vec(),
        )?;

        let compiled = vm.get_fn("vm_keys_empty_map::empty_map_keys", &[])?;
        assert_eq!(compiled.ret_ty(), &Type::I32);
        let empty_map_keys: extern "C" fn() -> i32 = unsafe { std::mem::transmute(compiled.ptr()) };
        assert_eq!(empty_map_keys(), 0);
        Ok(())
    }

    #[test]
    fn cast_between_all_integer_widths() -> anyhow::Result<()> {
        let vm = Vm::with_all()?;
        vm.import_code(
            "vm_cast_integer_widths",
            br#"
            pub fn i64_to_i32(value: i64) {
                value as i32
            }

            pub fn i32_to_i64(value: i32) {
                value as i64
            }

            pub fn u32_to_i64(value: u32) {
                value as i64
            }
            "#
            .to_vec(),
        )?;

        let compiled = vm.get_fn("vm_cast_integer_widths::i64_to_i32", &[Type::I64])?;
        assert_eq!(compiled.ret_ty(), &Type::I32);
        let i64_to_i32: extern "C" fn(i64) -> i32 = unsafe { std::mem::transmute(compiled.ptr()) };
        assert_eq!(i64_to_i32(42), 42);

        let compiled = vm.get_fn("vm_cast_integer_widths::i32_to_i64", &[Type::I32])?;
        assert_eq!(compiled.ret_ty(), &Type::I64);
        let i32_to_i64: extern "C" fn(i32) -> i64 = unsafe { std::mem::transmute(compiled.ptr()) };
        assert_eq!(i32_to_i64(-1), -1);

        let compiled = vm.get_fn("vm_cast_integer_widths::u32_to_i64", &[Type::U32])?;
        assert_eq!(compiled.ret_ty(), &Type::I64);
        let u32_to_i64: extern "C" fn(u32) -> i64 = unsafe { std::mem::transmute(compiled.ptr()) };
        assert_eq!(u32_to_i64(42), 42);
        Ok(())
    }

    #[test]
    fn boolean_literals_in_complex_expression_trees() -> anyhow::Result<()> {
        let vm = Vm::with_all()?;
        vm.import_code(
            "vm_complex_boolean",
            br#"
            pub fn exclusive_or(a: bool, b: bool) {
                (a && !b) || (!a && b)
            }

            pub fn implies(a: bool, b: bool) {
                !a || b
            }
            "#
            .to_vec(),
        )?;

        let compiled = vm.get_fn("vm_complex_boolean::exclusive_or", &[Type::Bool, Type::Bool])?;
        assert_eq!(compiled.ret_ty(), &Type::Bool);
        let exclusive_or: extern "C" fn(bool, bool) -> bool = unsafe { std::mem::transmute(compiled.ptr()) };
        assert!(exclusive_or(true, false));
        assert!(exclusive_or(false, true));
        assert!(!exclusive_or(true, true));
        assert!(!exclusive_or(false, false));

        let compiled = vm.get_fn("vm_complex_boolean::implies", &[Type::Bool, Type::Bool])?;
        assert_eq!(compiled.ret_ty(), &Type::Bool);
        let implies: extern "C" fn(bool, bool) -> bool = unsafe { std::mem::transmute(compiled.ptr()) };
        assert!(implies(false, true));
        assert!(implies(false, false));
        assert!(implies(true, true));
        assert!(!implies(true, false));
        Ok(())
    }

    #[test]
    fn concrete_struct_method_returning_self_type() -> anyhow::Result<()> {
        let vm = Vm::with_all()?;
        vm.import_code(
            "vm_struct_method_self",
            br#"
            pub struct Vec3 {
                x: f64,
                y: f64,
                z: f64,
            }

            impl Vec3 {
                pub fn add(self: Vec3, other: Vec3) {
                    Vec3{x: self.x + other.x, y: self.y + other.y, z: self.z + other.z}
                }
            }

            pub fn run() {
                let v1 = Vec3{x: 1.0f64, y: 2.0f64, z: 3.0f64};
                let v2 = Vec3{x: 4.0f64, y: 5.0f64, z: 6.0f64};
                let sum = v1.add(v2);
                sum.x + sum.y + sum.z
            }
            "#
            .to_vec(),
        )?;

        let compiled = vm.get_fn("vm_struct_method_self::run", &[])?;
        assert_eq!(compiled.ret_ty(), &Type::F64);
        let run: extern "C" fn() -> f64 = unsafe { std::mem::transmute(compiled.ptr()) };
        assert_eq!(run(), 21.0);
        Ok(())
    }

    #[test]
    fn deep_nested_struct_access_with_multiple_field_levels() -> anyhow::Result<()> {
        let vm = Vm::with_all()?;
        vm.import_code(
            "vm_deep_nested_struct",
            br#"
            pub struct A {
                value: i64,
            }

            pub struct B {
                a: A,
            }

            pub struct C {
                b: B,
            }

            pub fn direct_access() {
                let c = C{b: B{a: A{value: 99}}};
                c.b.a.value
            }

            pub fn via_variable() {
                let c = C{b: B{a: A{value: 77}}};
                let b = c.b;
                let a = b.a;
                a.value
            }
            "#
            .to_vec(),
        )?;

        let compiled = vm.get_fn("vm_deep_nested_struct::direct_access", &[])?;
        assert_eq!(compiled.ret_ty(), &Type::I64);
        let direct_access: extern "C" fn() -> i64 = unsafe { std::mem::transmute(compiled.ptr()) };
        assert_eq!(direct_access(), 99);

        let compiled = vm.get_fn("vm_deep_nested_struct::via_variable", &[])?;
        assert_eq!(compiled.ret_ty(), &Type::I64);
        let via_variable: extern "C" fn() -> i64 = unsafe { std::mem::transmute(compiled.ptr()) };
        assert_eq!(via_variable(), 77);
        Ok(())
    }

    #[test]
    fn array_index_with_dynamic_value_via_method() -> anyhow::Result<()> {
        let vm = Vm::with_all()?;
        vm.import_code(
            "vm_array_idx_dynamic",
            br#"
            pub fn get_by_idx(list, idx) {
                list.get_idx(idx)
            }
            "#
            .to_vec(),
        )?;

        let compiled = vm.get_fn("vm_array_idx_dynamic::get_by_idx", &[Type::Any, Type::I64])?;
        assert_eq!(compiled.ret_ty(), &Type::Any);
        let get_by_idx: extern "C" fn(*const Dynamic, i64) -> *const Dynamic = unsafe { std::mem::transmute(compiled.ptr()) };

        let list = Dynamic::list(vec!["a".into(), "b".into()]);
        let first = unsafe { &*get_by_idx(&list, 0) };
        assert_eq!(first.as_str(), "a");

        let out = unsafe { &*get_by_idx(&list, 10) };
        assert!(out.is_null());
        Ok(())
    }

    #[test]
    fn dynamic_field_access_with_optional_or_fallback() -> anyhow::Result<()> {
        let vm = Vm::with_all()?;
        vm.import_code(
            "vm_dynamic_or_fallback",
            br#"
            pub fn with_fallback(data) {
                if data.contains("name") { data.name } else { "unknown" }
            }

            pub fn with_fallback_missing(data) {
                if data.contains("nickname") { data.nickname } else { "unnamed" }
            }
            "#
            .to_vec(),
        )?;

        let compiled = vm.get_fn("vm_dynamic_or_fallback::with_fallback", &[Type::Any])?;
        assert_eq!(compiled.ret_ty(), &Type::Any);
        let with_fallback: extern "C" fn(*const Dynamic) -> *const Dynamic = unsafe { std::mem::transmute(compiled.ptr()) };
        let data = dynamic::map!("name"=> "Alice");
        let result = unsafe { &*with_fallback(&data) };
        assert_eq!(result.as_str(), "Alice");

        let compiled = vm.get_fn("vm_dynamic_or_fallback::with_fallback_missing", &[Type::Any])?;
        let with_fallback_missing: extern "C" fn(*const Dynamic) -> *const Dynamic = unsafe { std::mem::transmute(compiled.ptr()) };
        let result = unsafe { &*with_fallback_missing(&data) };
        assert_eq!(result.as_str(), "unnamed");
        Ok(())
    }

    #[test]
    fn for_in_loop_iterates_over_list_and_map_directly() -> anyhow::Result<()> {
        let vm = Vm::with_all()?;
        vm.import_code(
            "vm_for_in_collection",
            br#"
            pub fn sum_list(items) {
                let total = 0i64;
                for item in items {
                    total = total + 1;
                }
                total
            }

            pub fn count_map_keys(data) {
                let count = 0i64;
                for key in data.keys() {
                    count = count + 1;
                }
                count
            }

            pub fn for_in_list_works(items) {
                let exists = false;
                for item in items {
                    exists = true;
                }
                exists
            }

            pub fn for_in_map_values_works(data) {
                let exists = false;
                for value in data {
                    exists = true;
                }
                exists
            }
            "#
            .to_vec(),
        )?;

        let compiled = vm.get_fn("vm_for_in_collection::sum_list", &[Type::Any])?;
        assert_eq!(compiled.ret_ty(), &Type::I64);
        let sum_list: extern "C" fn(*const Dynamic) -> i64 = unsafe { std::mem::transmute(compiled.ptr()) };
        let items = Dynamic::list(vec![Dynamic::from(1i64), Dynamic::from(2i64), Dynamic::from(3i64)]);
        assert_eq!(sum_list(&items), 3);

        let data = dynamic::map!("x"=> 1i64, "y"=> 2i64);
        let compiled = vm.get_fn("vm_for_in_collection::count_map_keys", &[Type::Any])?;
        assert_eq!(compiled.ret_ty(), &Type::I64);
        let count_map_keys: extern "C" fn(*const Dynamic) -> i64 = unsafe { std::mem::transmute(compiled.ptr()) };
        assert_eq!(count_map_keys(&data), 2);

        let compiled = vm.get_fn("vm_for_in_collection::for_in_list_works", &[Type::Any])?;
        assert_eq!(compiled.ret_ty(), &Type::Bool);
        let for_in_list_works: extern "C" fn(*const Dynamic) -> bool = unsafe { std::mem::transmute(compiled.ptr()) };
        let empty = Dynamic::list(Vec::new());
        assert!(!for_in_list_works(&empty));
        assert!(for_in_list_works(&items));

        let compiled = vm.get_fn("vm_for_in_collection::for_in_map_values_works", &[Type::Any])?;
        assert_eq!(compiled.ret_ty(), &Type::Bool);
        let for_in_map_values_works: extern "C" fn(*const Dynamic) -> bool = unsafe { std::mem::transmute(compiled.ptr()) };
        let empty_map = dynamic::map!();
        assert!(!for_in_map_values_works(&empty_map));
        assert!(for_in_map_values_works(&data));

        Ok(())
    }

    #[test]
    fn concurrent_100_threads_no_memory_leak() -> anyhow::Result<()> {
        let vm = Vm::with_all()?;
        vm.import_code(
            "vm_stress",
            br#"
            pub fn heavy_alloc(idx: i64) {
                let items = [];
                let i = 0;
                while i < 50 {
                    items.push({
                        id: i + idx,
                        name: "item-" + i,
                        tags: ["tag-a", "tag-b", "tag-c"],
                        meta: {
                            created: 1234567890i64,
                            score: (i * 3.14f64) as i64,
                            extra: "prefix/" + i + "/" + idx
                        }
                    });
                    i = i + 1;
                }
                items
            }

            pub fn string_concat_stress() {
                let i = 0;
                let result = "";
                while i < 200 {
                    result = result + "data-" + i + ",";
                    i = i + 1;
                }
                result
            }
            "#
            .to_vec(),
        )?;

        let (heavy_ptr, _) = vm.get_fn_ptr("vm_stress::heavy_alloc", &[Type::I64])?;
        let (concat_ptr, _) = vm.get_fn_ptr("vm_stress::string_concat_stress", &[])?;

        let threads: usize = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(4).max(100);
        let iters_per_thread = 200;
        let total_calls = threads * iters_per_thread * 2;

        let before = current_rss_kb();
        eprintln!("threads={threads} iters_per_thread={iters_per_thread} total_calls={total_calls} rss_before={before}KB");

        // Round 1: first concurrent execution (arena warm-up)
        run_stress_round(threads, iters_per_thread, heavy_ptr as usize, concat_ptr as usize);
        let r1 = current_rss_kb();
        eprintln!("rss_after_round1={r1}KB");

        // Round 2: should stabilize (no unbounded growth)
        run_stress_round(threads, iters_per_thread, heavy_ptr as usize, concat_ptr as usize);
        let r2 = current_rss_kb();
        eprintln!("rss_after_round2={r2}KB");

        // Round 3: final check
        run_stress_round(threads, iters_per_thread, heavy_ptr as usize, concat_ptr as usize);
        let r3 = current_rss_kb();
        eprintln!("rss_after_round3={r3}KB");

        // Round 4: confirm that any one-time allocator growth has settled.
        run_stress_round(threads, iters_per_thread, heavy_ptr as usize, concat_ptr as usize);
        let r4 = current_rss_kb();
        eprintln!("rss_after_round4={r4}KB");

        // Allocator/arena growth is allowed during warm-up, but it must settle.
        let d12 = r2.saturating_sub(r1);
        let d23 = r3.saturating_sub(r2);
        let d34 = r4.saturating_sub(r3);
        eprintln!("delta_r1→r2={d12}KB delta_r2→r3={d23}KB delta_r3→r4={d34}KB");

        // The last interval must be small to prove the growth is not continuing.
        let max_growth_kb = 20 * 1024;
        assert!(d34 < max_growth_kb, "memory keeps growing after allocator warm-up: round1={r1} round2={r2} round3={r3} round4={r4} delta12={d12}KB delta23={d23}KB delta34={d34}KB (max stable growth={max_growth_kb}KB)");

        Ok(())
    }

    fn run_stress_round(threads: usize, iters: usize, heavy_ptr: usize, concat_ptr: usize) {
        std::thread::scope(|scope| {
            let mut handles = Vec::with_capacity(threads);
            for t in 0..threads {
                let heavy_ptr = heavy_ptr;
                let concat_ptr = concat_ptr;
                handles.push(scope.spawn(move || {
                    let heavy_fn: extern "C" fn(i64) -> *const Dynamic = unsafe { std::mem::transmute(heavy_ptr as *const u8) };
                    let concat_fn: extern "C" fn() -> *const Dynamic = unsafe { std::mem::transmute(concat_ptr as *const u8) };
                    for i in 0..iters {
                        // heavy_alloc: drop returned value to free heap allocation
                        let r_ptr = heavy_fn((t * iters + i) as i64);
                        assert!(!r_ptr.is_null());
                        unsafe {
                            let r = &*r_ptr;
                            assert!(r.len() > 0, "heavy_alloc returned empty list");
                            drop(Box::from_raw(r_ptr as *mut Dynamic));
                        }

                        // concat: same, drop returned value
                        let s_ptr = concat_fn();
                        assert!(!s_ptr.is_null());
                        unsafe {
                            let s = &*s_ptr;
                            assert!(s.len() > 0, "string_concat_stress returned empty");
                            drop(Box::from_raw(s_ptr as *mut Dynamic));
                        }
                    }
                }));
            }
            for h in handles {
                h.join().unwrap();
            }
        });
    }

    fn current_rss_kb() -> u64 {
        // macOS: use ps
        let pid = std::process::id();
        if let Ok(output) = std::process::Command::new("ps").args(["-p", &pid.to_string(), "-o", "rss="]).output() {
            if let Ok(s) = String::from_utf8(output.stdout) {
                if let Some(kb) = s.trim().parse::<u64>().ok() {
                    return kb;
                }
            }
        }
        // Linux fallback: /proc/self/statm
        if let Ok(statm) = std::fs::read_to_string("/proc/self/statm") {
            let parts: Vec<&str> = statm.split_whitespace().collect();
            if let Some(rss_pages) = parts.get(1).and_then(|s| s.parse::<u64>().ok()) {
                return rss_pages * 4; // pages (4KB) → KB
            }
        }
        0
    }
}
