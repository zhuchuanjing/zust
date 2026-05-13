use anyhow::{Result, anyhow, bail};
use compiler::{Compiler, Symbol};
use dynamic::Type;
use parser::{Expr, ExprKind};
use spirv::Scope;
use std::collections::{BTreeMap, BTreeSet};

use crate::{
    api::{BuiltinFn, ExternalFn, ExternalFnKind},
    context::{SpirvCompiler, SpirvTy, Value},
};

pub(crate) fn register_externs(compiler: &mut Compiler, externs: impl IntoIterator<Item = ExternalFn>) -> Result<BTreeMap<u32, ExternalFnKind>> {
    let mut registered = BTreeMap::new();
    let mut modules = BTreeSet::new();
    for ext in externs {
        let native = Symbol::native(ext.arg_tys, ext.ret_ty);
        let kind = ext.kind.clone();
        let add_atomic_add_alias = ext.full_name.as_str() == "spirv::atomic_add" && matches!(kind, ExternalFnKind::Builtin(BuiltinFn::AtomicAdd));
        let id = if let Some((module, name)) = ext.full_name.split_once("::") {
            if modules.insert(module.to_string()) {
                compiler.symbols.add_module(module.into());
            }
            compiler.symbols.add_to_module(module, name.into(), native.clone())?
        } else {
            if modules.insert("__extern".to_string()) {
                compiler.symbols.add_module("__extern".into());
            }
            compiler.add_symbol(&ext.full_name, native.clone())
        };
        registered.insert(id, kind.clone());
        if add_atomic_add_alias {
            if modules.insert("__extern".to_string()) {
                compiler.symbols.add_module("__extern".into());
            }
            let alias_id = compiler.symbols.add_to_module("__extern", "atomic_add".into(), native)?;
            registered.insert(alias_id, kind);
        }
    }
    Ok(registered)
}

impl SpirvCompiler {
    pub(crate) fn call_atomic_add(&mut self, params: &[Expr]) -> Result<Value> {
        if params.len() != 2 {
            bail!("spirv::atomic_add expects a workgroup static and a value");
        }
        self.call_atomic_add_target(&params[0], Some(&params[1]))
    }

    pub(crate) fn call_atomic_add_receiver(&mut self, target: &Expr, params: &[Expr]) -> Result<Value> {
        if params.len() > 1 {
            bail!("workgroup_static.atomic_add expects zero or one value");
        }
        self.call_atomic_add_target(target, params.first())
    }

    pub(crate) fn call_atomic_add_target(&mut self, target: &Expr, value: Option<&Expr>) -> Result<Value> {
        let ExprKind::Id(id, None) = &target.kind else {
            bail!("spirv::atomic_add first argument must be a workgroup static");
        };
        let ptr = self.workgroup_static_ptr(*id)?;
        if !matches!(ptr.ty, Type::U32 | Type::I32) {
            bail!("spirv::atomic_add currently supports only u32/i32 workgroup statics, got {:?}", ptr.ty);
        }
        let value = if let Some(value) = value { self.gen_expr(value)? } else { Value { id: self.const_u32(1), ty: Type::U32 } };
        let value = self.convert(value, ptr.ty.clone())?;
        let ty_id = self.get_type(SpirvTy::Value(ptr.ty.clone()));
        let u32_ty = self.get_type(SpirvTy::Value(Type::U32));
        let scope = self.builder.constant_bit32(u32_ty, Scope::Workgroup as u32);
        let semantics = self.builder.constant_bit32(u32_ty, (spirv::MemorySemantics::ACQUIRE_RELEASE | spirv::MemorySemantics::WORKGROUP_MEMORY).bits());
        let id = self.builder.atomic_i_add(ty_id, None, ptr.id, scope, semantics, value.id)?;
        Ok(Value { id, ty: ptr.ty })
    }

    pub(crate) fn call_external(&mut self, id: u32, args: Vec<Value>) -> Result<Value> {
        let kind = self.externs.get(&id).cloned().ok_or_else(|| anyhow!("SPIR-V external function {id} is not registered"))?;
        match kind {
            ExternalFnKind::Builtin(builtin) => self.call_spirv_builtin(builtin, args),
            ExternalFnKind::GlslUnary { float_op, signed_int_op } => {
                let [value]: [Value; 1] = args.try_into().map_err(|_| anyhow!("GLSL unary external expects one argument"))?;
                let op = if value.ty.is_float() {
                    float_op
                } else if value.ty.is_int() {
                    signed_int_op.ok_or_else(|| anyhow!("GLSL unary external does not support signed integer {:?}", value.ty))?
                } else {
                    bail!("GLSL unary external does not support {:?}", value.ty);
                };
                self.glsl1_raw(value, op)
            }
            ExternalFnKind::GlslBinary { float_op, signed_int_op, unsigned_int_op } => {
                let [left, right]: [Value; 2] = args.try_into().map_err(|_| anyhow!("GLSL binary external expects two arguments"))?;
                self.glsl2(left, right, float_op, signed_int_op, unsigned_int_op)
            }
            ExternalFnKind::GlslFloatBinary { op } => {
                let [left, right]: [Value; 2] = args.try_into().map_err(|_| anyhow!("GLSL float binary external expects two arguments"))?;
                self.glsl_float2(left, right, op)
            }
            ExternalFnKind::GlslFloatTernary { op } => {
                let [first, second, third]: [Value; 3] = args.try_into().map_err(|_| anyhow!("GLSL float ternary external expects three arguments"))?;
                self.glsl_float3(first, second, third, op)
            }
        }
    }

    pub(crate) fn call_spirv_builtin(&mut self, builtin: BuiltinFn, args: Vec<Value>) -> Result<Value> {
        match builtin {
            BuiltinFn::GroupId | BuiltinFn::LocalId => {
                if !args.is_empty() {
                    bail!("{builtin:?} expects no arguments");
                }
                let (ptr, ty, ty_id) = self.get_builtin_input(builtin);
                let id = self.builder.load(ty_id, None, ptr, None, None)?;
                Ok(Value { id, ty })
            }
            BuiltinFn::Barrier => {
                if !args.is_empty() {
                    bail!("barrier expects no arguments");
                }
                let u32_ty = self.get_type(SpirvTy::Value(Type::U32));
                let scope = self.builder.constant_bit32(u32_ty, Scope::Workgroup as u32);
                let semantics = self.builder.constant_bit32(u32_ty, (spirv::MemorySemantics::ACQUIRE_RELEASE | spirv::MemorySemantics::WORKGROUP_MEMORY).bits());
                self.builder.control_barrier(scope, scope, semantics)?;
                Ok(Value { id: self.const_u32(0), ty: Type::Void })
            }
            BuiltinFn::AtomicAdd => bail!("spirv::atomic_add must be called with a workgroup static as its first argument"),
        }
    }
}
