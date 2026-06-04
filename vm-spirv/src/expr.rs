use anyhow::{Result, anyhow, bail};
use compiler::{Capture, substitute_stmt, substitute_type};
use dynamic::{Dynamic, Type};
use parser::{BinaryOp, Expr, ExprKind, Span, Stmt, StmtKind};
use std::rc::Rc;

use crate::{
    api::{BuiltinFn, ExternalFnKind},
    context::{SpirvCompiler, SpirvTy, UserFn, UserFnInstance, Value},
};

impl SpirvCompiler {
    pub(crate) fn gen_expr(&mut self, expr: &Expr) -> Result<Value> {
        if let Some(value) = expr.compact() {
            let value = if matches!(expr.kind, ExprKind::Unary { .. } | ExprKind::Binary { .. }) { Self::narrow_small_compact_int(value) } else { value };
            return self.const_dynamic(value);
        }
        match &expr.kind {
            ExprKind::Value(value) => self.const_dynamic(value.clone()),
            ExprKind::Const(idx) => bail!("compiler constant index {idx} is not available in this SPIR-V context"),
            ExprKind::Typed { value, ty } => {
                let ty = self.resolve_type(ty);
                if ty.is_native()
                    && let Some(value) = value.compact()
                {
                    return self.const_dynamic(ty.force(value)?);
                }
                if let Type::Struct { fields, .. } = &ty
                    && let ExprKind::Dict(items) = &value.kind
                {
                    let mut values = Vec::with_capacity(fields.len());
                    for (field_name, field_ty) in fields {
                        let (_, expr) = items.iter().find(|(name, _)| name == field_name).ok_or_else(|| anyhow!("missing SPIR-V struct field {field_name}"))?;
                        let value = self.gen_expr(expr)?;
                        values.push(self.convert(value, field_ty.clone())?);
                    }
                    let ty_id = self.get_type(SpirvTy::Value(ty.clone()));
                    let id = self.builder.composite_construct(ty_id, None, values.iter().map(|v| v.id))?;
                    return Ok(Value { id, ty });
                }
                if let Type::Struct { fields, .. } = &ty
                    && let ExprKind::List(items) = &value.kind
                {
                    let mut values = Vec::with_capacity(fields.len());
                    for (item, (_, field_ty)) in items.iter().zip(fields.iter()) {
                        let value = self.gen_expr(item)?;
                        values.push(self.convert(value, field_ty.clone())?);
                    }
                    let ty_id = self.get_type(SpirvTy::Value(ty.clone()));
                    let id = self.builder.composite_construct(ty_id, None, values.iter().map(|v| v.id))?;
                    return Ok(Value { id, ty });
                }
                if let Type::Array(elem_ty, len) = &ty
                    && let ExprKind::List(items) = &value.kind
                {
                    if items.len() != *len as usize {
                        bail!("SPIR-V array literal length {} does not match {len}", items.len());
                    }
                    let mut values = Vec::with_capacity(items.len());
                    for item in items {
                        let value = self.gen_expr(item)?;
                        values.push(self.convert(value, elem_ty.as_ref().clone())?);
                    }
                    let ty_id = self.get_type(SpirvTy::Value(ty.clone()));
                    let id = self.builder.composite_construct(ty_id, None, values.iter().map(|v| v.id))?;
                    return Ok(Value { id, ty });
                }
                let value = self.gen_expr(value)?;
                self.convert(value, ty)
            }
            ExprKind::Ident(name) => self.get_named_var(name),
            ExprKind::Id(id, None) => self.load_workgroup_static(*id),
            ExprKind::Var(idx) => self.get_var(*idx as usize).ok_or_else(|| anyhow!("SPIR-V variable {idx} not found")),
            ExprKind::Unary { op, value } => {
                let value = self.gen_expr(value)?;
                self.unary(op, value)
            }
            ExprKind::Binary { left, op, right } => {
                if op == &BinaryOp::Assign {
                    let value = self.gen_expr(right)?;
                    self.assign(left, value.clone())?;
                    Ok(value)
                } else if op.is_assign() {
                    let left_value = self.gen_expr(left)?;
                    let right_value = self.gen_expr(right)?;
                    let value = self.binary(left_value, op, right_value)?;
                    self.assign(left, value.clone())?;
                    Ok(value)
                } else if op == &BinaryOp::Idx {
                    let idx = self.gen_expr(right)?;
                    if let Some(ptr) = self.expr_existing_ptr(left)? {
                        let ptr = self.index_local_ptr(ptr, idx)?;
                        let ty_id = self.get_type(SpirvTy::Value(ptr.ty.clone()));
                        let id = self.builder.load(ty_id, None, ptr.id, None, None)?;
                        Ok(Value { id, ty: ptr.ty })
                    } else {
                        let obj = self.gen_expr(left)?;
                        if matches!(obj.ty, Type::Array(_, _)) && self.get_const_u32(&idx).is_none() {
                            let ptr = self.materialize_temp_ptr(obj)?;
                            let ptr = self.index_local_ptr(ptr, idx)?;
                            let ty_id = self.get_type(SpirvTy::Value(ptr.ty.clone()));
                            let id = self.builder.load(ty_id, None, ptr.id, None, None)?;
                            Ok(Value { id, ty: ptr.ty })
                        } else {
                            self.index(obj, idx)
                        }
                    }
                } else {
                    let left = self.gen_expr(left)?;
                    let right = self.gen_expr(right)?;
                    self.binary(left, op, right)
                }
            }
            ExprKind::Call { obj, params } => self.call_function(obj, params),
            ExprKind::Tuple(items) | ExprKind::List(items) if items.len() <= 4 && !items.is_empty() => {
                let values = items.iter().map(|item| self.gen_expr(item)).collect::<Result<Vec<_>>>()?;
                let elem_ty = values[0].ty.clone();
                let ty = Type::Vec(Rc::new(elem_ty), values.len() as u32);
                let ty_id = self.get_type(SpirvTy::Value(ty.clone()));
                let id = self.builder.composite_construct(ty_id, None, values.iter().map(|v| v.id))?;
                Ok(Value { id, ty })
            }
            ExprKind::Repeat { value, len } => {
                let value = self.gen_expr(value)?;
                let Type::ConstInt(len) = len else {
                    bail!("SPIR-V repeat length must be a compile-time integer: {len:?}");
                };
                if *len < 0 {
                    bail!("SPIR-V repeat length cannot be negative: {len}");
                }
                let ty = Type::Array(Rc::new(value.ty.clone()), *len as u32);
                let ty_id = self.get_type(SpirvTy::Value(ty.clone()));
                let ids = (0..*len).map(|_| value.id);
                let id = self.builder.composite_construct(ty_id, None, ids)?;
                Ok(Value { id, ty })
            }
            other => bail!("unsupported SPIR-V expression: {other:?}"),
        }
    }

    pub(crate) fn narrow_small_compact_int(value: Dynamic) -> Dynamic {
        match value {
            Dynamic::I64(value) if i32::try_from(value).is_ok() => Dynamic::I32(value as i32),
            Dynamic::U64(value) if u32::try_from(value).is_ok() => Dynamic::U32(value as u32),
            other => other,
        }
    }

    pub(crate) fn assign(&mut self, target: &Expr, value: Value) -> Result<()> {
        match &target.kind {
            ExprKind::Var(idx) => {
                self.set_var(*idx as usize, value);
                Ok(())
            }
            ExprKind::Id(id, None) if self.workgroup_statics.contains_key(id) => {
                let ptr = self.workgroup_static_ptr(*id)?;
                let value = self.convert(value, ptr.ty.clone())?;
                self.builder.store(ptr.id, value.id, None, None)?;
                Ok(())
            }
            ExprKind::Binary { left, op: BinaryOp::Idx, right } => {
                let idx = self.gen_expr(right)?;
                let ptr = if let Some(ptr) = self.expr_ptr(left)? {
                    self.index_local_ptr(ptr, idx)?
                } else {
                    let obj = self.gen_expr(left)?;
                    self.index_ptr(obj, idx)?
                };
                let value = self.convert(value, ptr.ty.clone())?;
                self.builder.store(ptr.id, value.id, None, None)?;
                Ok(())
            }
            other => bail!("unsupported SPIR-V assignment target: {other:?}"),
        }
    }

    pub(crate) fn is_runtime_array(ty: &Type) -> bool {
        matches!(ty, Type::Vec(_, 0))
    }

    pub(crate) fn call_function(&mut self, obj: &Expr, params: &[Expr]) -> Result<Value> {
        if let ExprKind::Id(id, receiver) = &obj.kind {
            if matches!(self.externs.get(id), Some(ExternalFnKind::Builtin(BuiltinFn::AtomicAdd))) {
                return if let Some(receiver) = receiver { self.call_atomic_add_receiver(receiver, params) } else { self.call_atomic_add(params) };
            }
            let mut args = Vec::with_capacity(params.len() + receiver.is_some() as usize);
            if let Some(receiver) = receiver {
                let receiver = self.gen_expr(receiver)?;
                args.push(receiver);
            }
            args.extend(params.iter().map(|p| self.gen_expr(p)).collect::<Result<Vec<_>>>()?);
            if self.user_fns.contains_key(id) {
                return self.call_user_fn(*id, &[], args);
            }
            return self.call_external(*id, args);
        }
        let args = params.iter().map(|p| self.gen_expr(p)).collect::<Result<Vec<_>>>()?;
        if let ExprKind::AssocId { id, params: generic_args } = &obj.kind {
            if self.user_fns.contains_key(id) {
                return self.call_user_fn(*id, generic_args, args);
            }
            bail!("SPIR-V associated function {id} is not available");
        }
        let ExprKind::Ident(name) = &obj.kind else {
            bail!("only registered external SPIR-V calls and simple builtins are supported, got {obj:?}");
        };
        match (name.as_str(), args.as_slice()) {
            ("min", [a, b]) => self.glsl2(a.clone(), b.clone(), spirv::GlslStd450Op::FMin, spirv::GlslStd450Op::SMin, spirv::GlslStd450Op::UMin),
            ("max", [a, b]) => self.glsl2(a.clone(), b.clone(), spirv::GlslStd450Op::FMax, spirv::GlslStd450Op::SMax, spirv::GlslStd450Op::UMax),
            ("abs", [a]) => self.glsl1(a.clone(), spirv::GlslStd450Op::FAbs, spirv::GlslStd450Op::SAbs),
            ("log", [a]) => self.glsl1_raw(a.clone(), spirv::GlslStd450Op::Log),
            _ => bail!("unsupported SPIR-V builtin call {name}"),
        }
    }

    pub(crate) fn call_user_fn(&mut self, id: u32, generic_args: &[Type], args: Vec<Value>) -> Result<Value> {
        if args.iter().any(|arg| Self::is_runtime_array(&arg.ty)) {
            return self.inline_user_fn(id, generic_args, args);
        }

        let (generic_args, arg_tys, ret_ty, body) = self.specialize_user_fn(id, generic_args, &args)?;
        if let Some(instance) = self.fn_instances.iter().find(|instance| instance.source_id == id && instance.generic_args == generic_args && instance.arg_tys == arg_tys).cloned() {
            let args = args.into_iter().zip(instance.arg_tys.clone()).map(|(arg, ty)| self.convert(arg, ty)).collect::<Result<Vec<_>>>()?;
            let ret_id = self.get_type(SpirvTy::Value(instance.ret_ty.clone()));
            let id = self.builder.function_call(ret_id, None, instance.fn_id, args.iter().map(|arg| arg.id))?;
            return Ok(Value { id, ty: instance.ret_ty.clone() });
        }

        let ret_id = self.get_type(SpirvTy::Value(ret_ty.clone()));
        let param_ids = arg_tys.iter().map(|ty| self.get_type(SpirvTy::Value(ty.clone()))).collect::<Vec<_>>();
        let fn_ty = self.builder.type_function(ret_id, param_ids);
        let fn_id = self.builder.id();
        let user_fn = self.user_fns.get(&id).cloned().ok_or_else(|| anyhow!("SPIR-V user function {id} not found"))?;
        self.fn_instances.push(UserFnInstance { source_id: id, generic_args, arg_names: user_fn.arg_names, arg_tys: arg_tys.clone(), ret_ty: ret_ty.clone(), body, fn_id, fn_ty });

        let args = args.into_iter().zip(arg_tys).map(|(arg, ty)| self.convert(arg, ty)).collect::<Result<Vec<_>>>()?;
        let id = self.builder.function_call(ret_id, None, fn_id, args.iter().map(|arg| arg.id))?;
        Ok(Value { id, ty: ret_ty })
    }

    pub(crate) fn inline_user_fn(&mut self, id: u32, generic_args: &[Type], args: Vec<Value>) -> Result<Value> {
        let user_fn = self.user_fns.get(&id).cloned().ok_or_else(|| anyhow!("SPIR-V user function {id} not found"))?;
        if self.inline_stack.contains(&id) {
            bail!("recursive SPIR-V user function calls are not supported yet: {id}");
        }
        let (_, arg_tys, _, body) = self.specialize_user_fn(id, generic_args, &args)?;
        if args.len() != arg_tys.len() {
            bail!("SPIR-V user function {id} expects {} args, got {}", arg_tys.len(), args.len());
        }

        let saved_vars = std::mem::take(&mut self.vars);
        let saved_local_ptrs = std::mem::take(&mut self.local_ptrs);
        let saved_names = std::mem::take(&mut self.names);
        self.inline_stack.push(id);
        for ((arg, ty), name) in args.into_iter().zip(arg_tys.iter()).zip(user_fn.arg_names.iter()) {
            let ty = self.resolve_type(ty);
            let value = if ty.is_any() { arg } else { self.convert(arg, ty)? };
            let idx = self.vars.len();
            self.set_var(idx, value);
            self.names[idx] = Some(name.clone());
        }

        let result = self.gen_stmt(&body);
        self.inline_stack.pop();
        self.vars = saved_vars;
        self.local_ptrs = saved_local_ptrs;
        self.names = saved_names;

        match result? {
            Some(value) => Ok(value),
            None => Ok(Value { id: self.const_u32(0), ty: Type::Void }),
        }
    }

    pub(crate) fn specialize_user_fn(&mut self, id: u32, generic_args: &[Type], args: &[Value]) -> Result<(Vec<Type>, Vec<Type>, Type, Stmt)> {
        let user_fn = self.user_fns.get(&id).cloned().ok_or_else(|| anyhow!("SPIR-V user function {id} not found"))?;
        let inferred_generic_args;
        let generic_args = if generic_args.is_empty() && !user_fn.generic_params.is_empty() {
            inferred_generic_args = self.infer_user_fn_generic_args(&user_fn, args)?;
            inferred_generic_args.as_slice()
        } else {
            generic_args
        };
        let (mut arg_tys, body) = if user_fn.generic_params.is_empty() {
            (user_fn.arg_tys.clone(), user_fn.body.as_ref().clone())
        } else {
            if user_fn.generic_params.len() != generic_args.len() {
                bail!("SPIR-V generic function {id} expects {} generic args, got {}", user_fn.generic_params.len(), generic_args.len());
            }
            (user_fn.arg_tys.iter().map(|ty| substitute_type(ty, &user_fn.generic_params, generic_args)).collect(), substitute_stmt(user_fn.body.as_ref(), &user_fn.generic_params, generic_args))
        };
        for (ty, arg) in arg_tys.iter_mut().zip(args) {
            if ty.is_any() {
                *ty = arg.ty.clone();
            }
        }
        let actual_arg_tys = args.iter().map(|arg| arg.ty.clone()).collect::<Vec<_>>();
        let ret_ty = self.compiler.infer_fn_with_params(id, &actual_arg_tys, generic_args)?;
        let ret_ty = self.resolve_type(&self.compiler.symbols.get_type(&ret_ty)?);
        let mut compile_tys = arg_tys;
        let saved_state = self.compiler.take_local_state();
        let compiled_body = self.compiler.compile_fn(&user_fn.arg_names, &mut compile_tys, body, &mut Capture::default());
        self.compiler.restore_local_state(saved_state);
        let body = Stmt::new(StmtKind::Block(compiled_body?), Span::default());
        let ret_ty = if ret_ty.is_any() {
            let saved_state = self.compiler.take_local_state();
            let inferred = self.compiler.infer_stmt(&body);
            self.compiler.restore_local_state(saved_state);
            self.resolve_type(&self.compiler.symbols.get_type(&inferred?)?)
        } else {
            ret_ty
        };
        Ok((generic_args.to_vec(), compile_tys.into_iter().map(|ty| self.resolve_type(&ty)).collect(), ret_ty, body))
    }

    pub(crate) fn clear_statement_temps(&mut self) {
        let temps = std::mem::take(&mut self.statement_temp_ptrs);
        for ptr in temps {
            if let Some((_, values)) = self.function_var_pool.iter_mut().find(|(ty, _)| ty == &ptr.ty) {
                values.push(ptr);
            } else {
                self.function_var_pool.push((ptr.ty.clone(), vec![ptr]));
            }
        }
    }

    pub(crate) fn infer_user_fn_generic_args(&self, user_fn: &UserFn, args: &[Value]) -> Result<Vec<Type>> {
        let mut inferred = vec![None; user_fn.generic_params.len()];
        for (formal, actual) in user_fn.arg_tys.iter().zip(args.iter().map(|arg| &arg.ty)) {
            Self::infer_generic_type(&user_fn.generic_params, formal, actual, &mut inferred);
        }
        if user_fn.generic_params.len() == 1
            && inferred[0].is_none()
            && let Some(arg) = args.first()
        {
            if let Type::Vec(elem, _) | Type::Array(elem, _) = &arg.ty {
                inferred[0] = Some(elem.as_ref().clone());
            }
        }
        inferred.into_iter().enumerate().map(|(idx, ty)| ty.ok_or_else(|| anyhow!("could not infer SPIR-V generic arg {:?}", user_fn.generic_params[idx]))).collect()
    }

    pub(crate) fn infer_generic_type(params: &[Type], formal: &Type, actual: &Type, inferred: &mut [Option<Type>]) {
        if let Some(pos) = params.iter().position(|param| param == formal) {
            inferred[pos] = Some(actual.clone());
            return;
        }
        match (formal, actual) {
            (Type::Ident { params: nested, .. }, Type::Struct { params: actual_params, .. })
            | (Type::Ident { params: nested, .. }, Type::Ident { params: actual_params, .. })
            | (Type::Ident { params: nested, .. }, Type::Symbol { id: _, params: actual_params }) => {
                for (formal, actual) in nested.iter().zip(actual_params.iter()) {
                    Self::infer_generic_type(params, formal, actual, inferred);
                }
            }
            (Type::Symbol { params: nested, .. }, Type::Struct { params: actual_params, .. })
            | (Type::Symbol { params: nested, .. }, Type::Ident { params: actual_params, .. })
            | (Type::Symbol { params: nested, .. }, Type::Symbol { params: actual_params, .. }) => {
                for (formal, actual) in nested.iter().zip(actual_params.iter()) {
                    Self::infer_generic_type(params, formal, actual, inferred);
                }
            }
            (Type::Array(formal_elem, formal_len), Type::Array(actual_elem, actual_len)) | (Type::Vec(formal_elem, formal_len), Type::Vec(actual_elem, actual_len)) => {
                Self::infer_generic_type(params, formal_elem, actual_elem, inferred);
                Self::infer_generic_type(params, &Type::ConstInt(*formal_len as i64), &Type::ConstInt(*actual_len as i64), inferred);
            }
            (Type::ArrayParam(formal_elem, formal_len), Type::Array(actual_elem, actual_len)) => {
                Self::infer_generic_type(params, formal_elem, actual_elem, inferred);
                Self::infer_generic_type(params, formal_len, &Type::ConstInt(*actual_len as i64), inferred);
            }
            _ => {}
        }
    }
}
