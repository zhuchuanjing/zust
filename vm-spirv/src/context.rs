use anyhow::Result;
use compiler::Compiler;
use dynamic::{Dynamic, Type};
use parser::Stmt;
use rspirv::{binary::Assemble, dr::Builder};
use smol_str::SmolStr;
use spirv::{AddressingModel, Capability, ExecutionModel, MemoryModel, StorageClass};
use std::{collections::BTreeMap, sync::Arc};

use crate::api::{BuiltinFn, ExternalFnKind, SpirvModule};

#[derive(Debug, Clone)]
pub(crate) struct Value {
    pub(crate) id: u32,
    pub(crate) ty: Type,
}

#[derive(Debug, Clone)]
pub(crate) struct Phi {
    pub(crate) idx: usize,
    pub(crate) ty: Type,
    pub(crate) result_id: u32,
    pub(crate) incoming: Vec<(u32, u32)>,
}

#[derive(Debug, Clone)]
pub(crate) struct UserFn {
    pub(crate) arg_names: Vec<SmolStr>,
    pub(crate) arg_tys: Vec<Type>,
    pub(crate) generic_params: Vec<Type>,
    pub(crate) body: Arc<Stmt>,
}

#[derive(Debug, Clone)]
pub(crate) struct UserFnInstance {
    pub(crate) source_id: u32,
    pub(crate) generic_args: Vec<Type>,
    pub(crate) arg_names: Vec<SmolStr>,
    pub(crate) arg_tys: Vec<Type>,
    pub(crate) ret_ty: Type,
    pub(crate) body: Stmt,
    pub(crate) fn_id: u32,
    pub(crate) fn_ty: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum SpirvTy {
    Value(Type),
    LayoutValue(Type),
    Pointer(Type, StorageClass),
    Buffer(Type),
}

pub(crate) struct SpirvCompiler {
    pub(crate) builder: Builder,
    pub(crate) types: Vec<(SpirvTy, u32)>,
    pub(crate) consts: Vec<(Dynamic, u32)>,
    pub(crate) vars: Vec<Option<Value>>,
    pub(crate) local_ptrs: Vec<Option<Value>>,
    pub(crate) names: Vec<Option<SmolStr>>,
    pub(crate) buffers: Vec<u32>,
    pub(crate) interfaces: Vec<u32>,
    pub(crate) current_block: Option<u32>,
    pub(crate) loop_stack: Vec<(u32, u32)>,
    pub(crate) externs: BTreeMap<u32, ExternalFnKind>,
    pub(crate) user_fns: BTreeMap<u32, UserFn>,
    pub(crate) type_defs: BTreeMap<u32, Type>,
    pub(crate) workgroup_static_tys: BTreeMap<u32, Type>,
    pub(crate) workgroup_statics: BTreeMap<u32, Value>,
    pub(crate) compiler: Compiler,
    pub(crate) inline_stack: Vec<u32>,
    pub(crate) fn_instances: Vec<UserFnInstance>,
    pub(crate) compiled_fn_instances: usize,
    pub(crate) builtin_vars: BTreeMap<BuiltinFn, (u32, Type, u32)>,
    pub(crate) workgroup_size: [u32; 3],
    pub(crate) statement_temp_ptrs: Vec<Value>,
    pub(crate) function_var_pool: Vec<(Type, Vec<Value>)>,
}

impl SpirvCompiler {
    pub(crate) fn new(
        externs: BTreeMap<u32, ExternalFnKind>,
        user_fns: BTreeMap<u32, UserFn>,
        type_defs: BTreeMap<u32, Type>,
        workgroup_static_tys: BTreeMap<u32, Type>,
        compiler: Compiler,
        workgroup_size: [u32; 3],
    ) -> Self {
        let mut builder = Builder::new();
        builder.set_version(1, 5);
        builder.capability(Capability::Shader);
        builder.memory_model(AddressingModel::Logical, MemoryModel::GLSL450);
        Self {
            builder,
            types: Vec::new(),
            consts: Vec::new(),
            vars: Vec::new(),
            local_ptrs: Vec::new(),
            names: Vec::new(),
            buffers: Vec::new(),
            interfaces: Vec::new(),
            current_block: None,
            loop_stack: Vec::new(),
            externs,
            user_fns,
            type_defs,
            workgroup_static_tys,
            workgroup_statics: BTreeMap::new(),
            compiler,
            inline_stack: Vec::new(),
            fn_instances: Vec::new(),
            compiled_fn_instances: 0,
            builtin_vars: BTreeMap::new(),
            workgroup_size,
            statement_temp_ptrs: Vec::new(),
            function_var_pool: Vec::new(),
        }
    }

    pub(crate) fn compile_kernel(mut self, arg_tys: &[Type], ret_ty: Type, body: &Stmt) -> Result<SpirvModule> {
        let void_ty = self.get_type(SpirvTy::Value(Type::Void));
        let fn_ty = self.builder.type_function(void_ty, Vec::<u32>::new());

        self.vars.clear();
        self.local_ptrs.clear();
        self.names.clear();
        self.buffers.clear();
        self.function_var_pool.clear();
        let arg_buffers = arg_tys.iter().enumerate().map(|(idx, ty)| self.add_storage_buffer(ty.clone(), idx as u32)).collect::<Result<Vec<_>>>()?;
        let ret_buffer = if ret_ty.is_void() { None } else { Some(self.add_storage_buffer(ret_ty.clone(), arg_tys.len() as u32)?) };
        self.add_workgroup_statics()?;

        self.get_builtin_input(BuiltinFn::GroupId);
        self.get_builtin_input(BuiltinFn::LocalId);

        let fn_id = self.builder.id();
        self.builder.begin_function(void_ty, Some(fn_id), spirv::FunctionControl::NONE, fn_ty)?;
        let entry_block = self.builder.begin_block(None)?;
        self.current_block = Some(entry_block);

        for (idx, ty) in arg_tys.iter().enumerate() {
            let ptr = arg_buffers[idx];
            let val = if Self::is_runtime_array(ty) { Value { id: ptr, ty: ty.clone() } } else { self.load_buffer_value(ptr, ty.clone())? };
            self.vars.push(Some(val));
            self.local_ptrs.push(None);
            self.names.push(None);
        }

        let ret = self.gen_stmt(body)?;
        if let (Some(buf), Some(value)) = (ret_buffer, ret) {
            let value = self.convert(value, ret_ty.clone())?;
            self.store_buffer_value(buf, value)?;
        }
        if self.current_block.is_some() {
            self.builder.ret()?;
        }
        self.builder.end_function()?;

        while self.compiled_fn_instances < self.fn_instances.len() {
            let idx = self.compiled_fn_instances;
            self.compiled_fn_instances += 1;
            self.compile_user_fn_instance(idx)?;
        }

        let mut interfaces = self.buffers.clone();
        interfaces.extend(self.interfaces.iter().copied());
        self.builder.entry_point(ExecutionModel::GLCompute, fn_id, "main", interfaces);
        self.builder.execution_mode(fn_id, spirv::ExecutionMode::LocalSize, self.workgroup_size);
        let module = self.builder.module();
        let words = module.assemble();
        Ok(SpirvModule { words, module })
    }

    pub(crate) fn compile_user_fn_instance(&mut self, idx: usize) -> Result<()> {
        let instance = self.fn_instances[idx].clone();
        let ret_id = self.get_type(SpirvTy::Value(instance.ret_ty.clone()));

        self.vars.clear();
        self.local_ptrs.clear();
        self.names.clear();
        self.loop_stack.clear();
        self.current_block = None;
        self.function_var_pool.clear();

        self.builder.begin_function(ret_id, Some(instance.fn_id), spirv::FunctionControl::NONE, instance.fn_ty)?;

        let mut params = Vec::with_capacity(instance.arg_tys.len());
        for ty in &instance.arg_tys {
            let ty_id = self.get_type(SpirvTy::Value(ty.clone()));
            let id = self.builder.function_parameter(ty_id)?;
            params.push(Value { id, ty: ty.clone() });
        }

        self.current_block = Some(self.builder.begin_block(None)?);
        for (arg_idx, param) in params.into_iter().enumerate() {
            self.set_var(arg_idx, param);
            self.names[arg_idx] = instance.arg_names.get(arg_idx).cloned();
        }
        let ret = self.gen_stmt(&instance.body)?;
        if self.current_block.is_some() {
            if instance.ret_ty.is_void() {
                self.builder.ret()?;
            } else {
                let value = ret.ok_or_else(|| anyhow::anyhow!("SPIR-V function {} did not produce a return value", instance.source_id))?;
                let value = self.convert(value, instance.ret_ty.clone())?;
                self.builder.ret_value(value.id)?;
            }
        }
        self.builder.end_function()?;

        self.vars.clear();
        self.local_ptrs.clear();
        self.names.clear();
        self.loop_stack.clear();
        self.current_block = None;
        self.function_var_pool.clear();
        Ok(())
    }
}
