//使用 cranelift 作为后端 直接 jit 解释脚本
mod binary;
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

use std::cell::RefCell;
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

pub fn init_jit(mut jit: JITRunTime) -> Result<JITRunTime> {
    jit.add_all()?;
    Ok(jit)
}

use std::sync::Arc;
unsafe impl Send for JITRunTime {}
unsafe impl Sync for JITRunTime {}

thread_local! {
    static CURRENT_VM: RefCell<Option<Weak<Mutex<JITRunTime>>>> = const { RefCell::new(None) };
}

fn set_current_vm(vm: &Vm) {
    CURRENT_VM.with(|current| {
        *current.borrow_mut() = Some(Arc::downgrade(&vm.jit));
    });
}

fn with_current_vm<T>(f: impl FnOnce(&Vm) -> Result<T>) -> Result<T> {
    CURRENT_VM.with(|current| {
        let jit = current.borrow().as_ref().and_then(Weak::upgrade).ok_or_else(|| anyhow!("当前线程没有 VM"))?;
        let vm = Vm { jit };
        f(&vm)
    })
}

pub(crate) fn import_current(name: &str, path: &str) -> Result<()> {
    with_current_vm(|vm| vm.import(name, path))
}

pub(crate) fn get_current_fn_ptr(name: &str, arg_tys: &[Type]) -> Result<(*const u8, Type)> {
    with_current_vm(|vm| vm.get_fn_ptr(name, arg_tys))
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

    pub fn add_native_method_ptr(&mut self, def: &str, method: &str, arg_tys: &[Type], ret_ty: Type, fn_ptr: *const u8) -> Result<u32> {
        self.add_empty_type(def)?;
        let full_name = format!("{}::{}", def, method);
        let id = self.add_native_ptr(&full_name, &full_name, arg_tys, ret_ty, fn_ptr)?;
        add_method_field(self, def, method, id)?;
        Ok(id)
    }

    pub fn add_std(&mut self) -> Result<()> {
        self.add_module("std");
        for (name, arg_tys, ret_ty, fn_ptr) in STD {
            self.add_native_ptr(name, name, arg_tys, ret_ty, fn_ptr)?;
        }
        Ok(())
    }

    pub fn add_any(&mut self) -> Result<()> {
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
        add_native_module_fns(self, "root", &root_module::ROOT_NATIVE)
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
        set_current_vm(&self.owner);
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
        Self { jit: Arc::new(Mutex::new(JITRunTime::new(|_| {}))) }
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
        set_current_vm(self);
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
        Ok(())
    }
}
