use anyhow::{Result, anyhow, bail};
use compiler::{Compiler, Symbol};
use dynamic::Type;
use parser::{Expr, ExprKind};
use std::{
    collections::{BTreeMap, BTreeSet},
    rc::Rc,
};

use crate::{
    api::{BuiltinFn, ExternalFn, ExternalFnKind},
    context::{MetalCompiler, Value},
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

impl MetalCompiler {
    pub(crate) fn call_external(&mut self, id: u32, args: Vec<Value>) -> Result<Value> {
        let kind = self.externs.get(&id).cloned().ok_or_else(|| anyhow!("Metal external function {id} is not registered"))?;
        match kind {
            ExternalFnKind::Builtin(BuiltinFn::GroupId) => Ok(Value { code: "zust_group_id".to_string(), ty: Type::Vec(Rc::new(Type::U32), 3) }),
            ExternalFnKind::Builtin(BuiltinFn::LocalId) => Ok(Value { code: "zust_local_id".to_string(), ty: Type::Vec(Rc::new(Type::U32), 3) }),
            ExternalFnKind::Builtin(BuiltinFn::Barrier) => {
                if !args.is_empty() {
                    bail!("barrier expects no arguments");
                }
                self.line("threadgroup_barrier(mem_flags::mem_threadgroup);");
                Ok(Value { code: String::new(), ty: Type::Void })
            }
            ExternalFnKind::Builtin(BuiltinFn::AtomicAdd) => bail!("atomic_add must be called with a workgroup static as its first argument"),
            ExternalFnKind::MathUnary(name) => {
                let [value]: [Value; 1] = args.try_into().map_err(|_| anyhow!("Metal unary external expects one argument"))?;
                Ok(Value { code: format!("{name}({})", value.code), ty: value.ty })
            }
            ExternalFnKind::MathBinary(name) => {
                let [left, right]: [Value; 2] = args.try_into().map_err(|_| anyhow!("Metal binary external expects two arguments"))?;
                self.call_math2(name, left, right)
            }
            ExternalFnKind::MathFloatBinary(name) => {
                let [left, right]: [Value; 2] = args.try_into().map_err(|_| anyhow!("Metal float binary external expects two arguments"))?;
                self.call_float_math2(name, left, right)
            }
            ExternalFnKind::MathTernary(name) => {
                let [first, second, third]: [Value; 3] = args.try_into().map_err(|_| anyhow!("Metal ternary external expects three arguments"))?;
                self.call_math3(name, first, second, third)
            }
        }
    }

    pub(crate) fn call_atomic_add(&mut self, params: &[Expr]) -> Result<Value> {
        if params.len() != 2 {
            bail!("atomic_add expects a workgroup static and a value");
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
            bail!("atomic_add first argument must be a workgroup static");
        };
        let ty = self.resolve_type(self.workgroup_static_tys.get(id).ok_or_else(|| anyhow!("Metal workgroup static {id} not found"))?);
        if !matches!(ty, Type::U32 | Type::I32) {
            bail!("atomic_add currently supports only u32/i32 workgroup statics, got {:?}", ty);
        }
        let value = if let Some(value) = value { self.gen_expr(value)? } else { Value { code: self.one_literal(&ty), ty: ty.clone() } };
        let value = self.convert_code(value, ty.clone())?;
        Ok(Value { code: format!("atomic_fetch_add_explicit(&zust_static_{id}, {}, memory_order_relaxed)", value.code), ty })
    }
}
