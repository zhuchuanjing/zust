use anyhow::{Result, anyhow, bail};
use compiler::{Capture, substitute_stmt, substitute_type};
use dynamic::Type;
use parser::{BinaryOp, Expr, ExprKind, Span, Stmt, StmtKind, UnaryOp};
use std::rc::Rc;

use crate::{
    api::{BuiltinFn, ExternalFnKind},
    context::{MetalCompiler, UserFn, Value},
    util::{assignment_base_op, sanitize_ident},
};

impl MetalCompiler {
    pub(crate) fn gen_expr(&mut self, expr: &Expr) -> Result<Value> {
        if let Some(value) = expr.compact() {
            return self.const_dynamic(value);
        }
        match &expr.kind {
            ExprKind::Value(value) => self.const_dynamic(value.clone()),
            ExprKind::Const(idx) => self.const_dynamic(self.compiler.consts.get_index(*idx).map(|(_, v)| v.clone()).ok_or_else(|| anyhow!("compiler constant {idx} missing"))?),
            ExprKind::Typed { value, ty } => {
                let ty = self.resolve_type(ty);
                if ty.is_native()
                    && let Some(value) = value.compact()
                {
                    return self.const_dynamic(ty.force(value)?);
                }
                if let Type::Struct { fields, .. } = &ty
                    && let ExprKind::List(items) = &value.kind
                {
                    let values = items.iter().zip(fields.iter()).map(|(item, (_, field_ty))| self.gen_expr(item).and_then(|v| self.convert_code(v, field_ty.clone()))).collect::<Result<Vec<_>>>()?;
                    let code = self.struct_literal_code(&ty, values)?;
                    return Ok(Value { code, ty });
                }
                if let Type::Array(elem, len) = &ty
                    && let ExprKind::List(items) = &value.kind
                {
                    if items.len() != *len as usize {
                        bail!("Metal array literal length {} does not match {len}", items.len());
                    }
                    let values = items.iter().map(|item| self.gen_expr(item).and_then(|v| self.convert_code(v, elem.as_ref().clone()))).collect::<Result<Vec<_>>>()?;
                    return Ok(Value { code: format!("{}{{{}}}", self.msl_type(&ty), values.into_iter().map(|v| v.code).collect::<Vec<_>>().join(", ")), ty });
                }
                let value = self.gen_expr(value)?;
                self.convert_code(value, ty)
            }
            ExprKind::Ident(name) => self.get_named_var(name),
            ExprKind::Var(idx) => self.get_var(*idx as usize),
            ExprKind::Id(id, None) if self.workgroup_static_tys.contains_key(id) => {
                let ty = self.resolve_type(self.workgroup_static_tys.get(id).unwrap());
                let code = if self.atomic_type(&ty).is_some() { format!("atomic_load_explicit(&zust_static_{id}, memory_order_relaxed)") } else { format!("zust_static_{id}") };
                Ok(Value { code, ty })
            }
            ExprKind::Unary { op, value } => {
                let value = self.gen_expr(value)?;
                match op {
                    UnaryOp::Neg => Ok(Value { code: format!("(-{})", value.code), ty: value.ty }),
                    UnaryOp::Not if value.ty.is_int() || value.ty.is_uint() => Ok(Value { code: format!("(~{})", value.code), ty: value.ty }),
                    UnaryOp::Not => Ok(Value { code: format!("(!{})", self.bool_expr(value)?.code), ty: Type::Bool }),
                    _ => bail!("unsupported Metal unary op {op:?}"),
                }
            }
            ExprKind::Binary { left, op, right } => {
                if *op == BinaryOp::Assign {
                    let value = self.gen_expr(right)?;
                    self.assign(left, value.clone())?;
                    Ok(value)
                } else if op.is_assign() {
                    let target = self.lvalue(left)?;
                    let right = self.gen_expr(right)?;
                    let bin_op = assignment_base_op(op).ok_or_else(|| anyhow!("unsupported assignment op {op:?}"))?;
                    let value = self.binary(Value { code: target.code.clone(), ty: target.ty.clone() }, bin_op, right)?;
                    self.line(format!("{} = {};", target.code, value.code));
                    Ok(value)
                } else if *op == BinaryOp::Idx {
                    let idx = self.gen_expr(right)?;
                    let target = self.gen_expr(left)?;
                    self.index(target, idx)
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
                Ok(Value { code: format!("{}({})", self.msl_type(&ty), values.into_iter().map(|v| v.code).collect::<Vec<_>>().join(", ")), ty })
            }
            ExprKind::Repeat { value, len } => {
                let value = self.gen_expr(value)?;
                let Type::ConstInt(len) = len else {
                    bail!("Metal repeat length must be a compile-time integer: {len:?}");
                };
                let ty = Type::Array(Rc::new(value.ty.clone()), *len as u32);
                Ok(Value { code: format!("{}{{{}}}", self.msl_type(&ty), std::iter::repeat(value.code).take(*len as usize).collect::<Vec<_>>().join(", ")), ty })
            }
            other => bail!("unsupported Metal expression: {other:?}"),
        }
    }

    fn struct_literal_code(&self, ty: &Type, values: Vec<Value>) -> Result<String> {
        let Type::Struct { fields, .. } = self.resolve_type(ty) else {
            bail!("Metal struct literal expected struct type, got {ty:?}");
        };
        if values.len() != fields.len() {
            bail!("Metal struct literal field count {} does not match {}", values.len(), fields.len());
        }
        let (size, offsets) = Type::struct_layout(&fields);
        let mut cursor = 0usize;
        let mut parts = Vec::with_capacity(values.len());
        for ((_, field_ty), offset, value) in fields.iter().zip(offsets).zip(values).map(|((field, offset), value)| (field, offset, value)) {
            let offset = offset as usize;
            if offset > cursor {
                parts.push(format!("array<uchar, {}>{{}}", offset - cursor));
            }
            parts.push(value.code);
            cursor = offset + field_ty.storage_width() as usize;
        }
        let size = size as usize;
        if size > cursor {
            parts.push(format!("array<uchar, {}>{{}}", size - cursor));
        }
        Ok(format!("{}{{{}}}", self.msl_type(ty), parts.join(", ")))
    }

    pub(crate) fn call_function(&mut self, obj: &Expr, params: &[Expr]) -> Result<Value> {
        if let ExprKind::Id(id, receiver) = &obj.kind {
            if matches!(self.externs.get(id), Some(ExternalFnKind::Builtin(BuiltinFn::AtomicAdd))) {
                return if let Some(receiver) = receiver { self.call_atomic_add_receiver(receiver, params) } else { self.call_atomic_add(params) };
            }
            let mut args = Vec::with_capacity(params.len() + receiver.is_some() as usize);
            if let Some(receiver) = receiver {
                args.push(self.gen_expr(receiver)?);
            }
            args.extend(params.iter().map(|p| self.gen_expr(p)).collect::<Result<Vec<_>>>()?);
            if self.user_fns.contains_key(id) {
                return self.inline_user_fn(*id, &[], args);
            }
            return self.call_external(*id, args);
        }
        let args = params.iter().map(|p| self.gen_expr(p)).collect::<Result<Vec<_>>>()?;
        if let ExprKind::AssocId { id, params: generic_args } = &obj.kind {
            if self.user_fns.contains_key(id) {
                return self.inline_user_fn(*id, generic_args, args);
            }
            bail!("Metal associated function {id} is not available");
        }
        let ExprKind::Ident(name) = &obj.kind else {
            bail!("only registered Metal calls and simple builtins are supported, got {obj:?}");
        };
        match (name.as_str(), args.as_slice()) {
            ("min", [a, b]) => self.call_math2("min", a.clone(), b.clone()),
            ("max", [a, b]) => self.call_math2("max", a.clone(), b.clone()),
            ("abs", [a]) => Ok(Value { code: format!("abs({})", a.code), ty: a.ty.clone() }),
            ("log", [a]) => Ok(Value { code: format!("log({})", a.code), ty: a.ty.clone() }),
            _ => bail!("unsupported Metal builtin call {name}"),
        }
    }

    pub(crate) fn inline_user_fn(&mut self, id: u32, generic_args: &[Type], args: Vec<Value>) -> Result<Value> {
        let user_fn = self.user_fns.get(&id).cloned().ok_or_else(|| anyhow!("Metal user function {id} not found"))?;
        if self.inline_stack.contains(&id) {
            bail!("recursive Metal user function calls are not supported yet: {id}");
        }
        let inferred_generic_args;
        let generic_args = if generic_args.is_empty() && !user_fn.generic_params.is_empty() {
            inferred_generic_args = self.infer_user_fn_generic_args(&user_fn, &args)?;
            inferred_generic_args.as_slice()
        } else {
            generic_args
        };
        let (arg_tys, body) = if user_fn.generic_params.is_empty() {
            (user_fn.arg_tys.clone(), user_fn.body.as_ref().clone())
        } else {
            if user_fn.generic_params.len() != generic_args.len() {
                bail!("Metal generic function {id} expects {} generic args, got {}", user_fn.generic_params.len(), generic_args.len());
            }
            (user_fn.arg_tys.iter().map(|ty| substitute_type(ty, &user_fn.generic_params, generic_args)).collect(), substitute_stmt(user_fn.body.as_ref(), &user_fn.generic_params, generic_args))
        };
        let mut compile_tys = arg_tys;
        let saved_state = self.compiler.take_local_state();
        let compiled_body = self.compiler.compile_fn(&user_fn.arg_names, &mut compile_tys, body, &mut Capture::default());
        self.compiler.restore_local_state(saved_state);
        let body = Stmt::new(StmtKind::Block(compiled_body?), Span::default());
        let ret_ty = self.compiler.infer_fn_with_params(id, &args.iter().map(|arg| arg.ty.clone()).collect::<Vec<_>>(), generic_args).unwrap_or(Type::Void);

        let result_var = (!ret_ty.is_void()).then(|| self.fresh("ret"));
        if let Some(result_var) = &result_var {
            self.line(format!("{} {result_var};", self.msl_type(&self.resolve_type(&ret_ty))));
        }
        self.line("{");
        self.indent += 1;

        let saved_vars = std::mem::take(&mut self.vars);
        let saved_names = std::mem::take(&mut self.names);
        let saved_return = self.inline_return.clone();
        self.inline_return = result_var.clone();
        self.inline_stack.push(id);
        for ((arg, ty), name) in args.into_iter().zip(compile_tys.iter()).zip(user_fn.arg_names.iter()) {
            let ty = if ty.is_any() { self.resolve_type(&arg.ty) } else { self.resolve_type(ty) };
            let idx = self.vars.len();
            let local_name = self.fresh(&format!("arg_{}", sanitize_ident(name)));
            if Self::is_runtime_array(&ty) {
                self.set_var(idx, arg.code, ty);
            } else {
                let arg = self.convert_code(arg, ty.clone())?;
                self.line(format!("{} {local_name} = {};", self.msl_type(&ty), arg.code));
                self.set_var(idx, local_name, ty);
            }
            self.names[idx] = Some(name.clone());
        }
        self.gen_stmt(&body)?;
        self.inline_stack.pop();
        self.inline_return = saved_return;
        self.vars = saved_vars;
        self.names = saved_names;

        self.indent -= 1;
        self.line("}");
        Ok(if let Some(result_var) = result_var { Value { code: result_var, ty: ret_ty } } else { Value { code: String::new(), ty: Type::Void } })
    }

    pub(crate) fn infer_user_fn_generic_args(&self, user_fn: &UserFn, args: &[Value]) -> Result<Vec<Type>> {
        let mut inferred = vec![None; user_fn.generic_params.len()];
        for (formal, actual) in user_fn.arg_tys.iter().zip(args.iter().map(|arg| &arg.ty)) {
            Self::infer_generic_type(&user_fn.generic_params, formal, actual, &mut inferred);
        }
        if user_fn.generic_params.len() == 1
            && inferred[0].is_none()
            && let Some(arg) = args.first()
            && let Type::Vec(elem, _) | Type::Array(elem, _) = &arg.ty
        {
            inferred[0] = Some(elem.as_ref().clone());
        }
        inferred.into_iter().enumerate().map(|(idx, ty)| ty.ok_or_else(|| anyhow!("could not infer Metal generic arg {:?}", user_fn.generic_params[idx]))).collect()
    }

    pub(crate) fn infer_generic_type(params: &[Type], formal: &Type, actual: &Type, inferred: &mut [Option<Type>]) {
        if let Some(pos) = params.iter().position(|param| param == formal) {
            inferred[pos] = Some(actual.clone());
            return;
        }
        match (formal, actual) {
            (Type::Array(formal_elem, formal_len), Type::Array(actual_elem, actual_len)) | (Type::Vec(formal_elem, formal_len), Type::Vec(actual_elem, actual_len)) => {
                Self::infer_generic_type(params, formal_elem, actual_elem, inferred);
                Self::infer_generic_type(params, &Type::ConstInt(*formal_len as i64), &Type::ConstInt(*actual_len as i64), inferred);
            }
            (Type::ArrayParam(formal_elem, formal_len), Type::Array(actual_elem, actual_len)) => {
                Self::infer_generic_type(params, formal_elem, actual_elem, inferred);
                Self::infer_generic_type(params, formal_len, &Type::ConstInt(*actual_len as i64), inferred);
            }
            (Type::Ident { params: nested, .. }, Type::Struct { params: actual_params, .. })
            | (Type::Ident { params: nested, .. }, Type::Ident { params: actual_params, .. })
            | (Type::Ident { params: nested, .. }, Type::Symbol { params: actual_params, .. })
            | (Type::Symbol { params: nested, .. }, Type::Struct { params: actual_params, .. })
            | (Type::Symbol { params: nested, .. }, Type::Ident { params: actual_params, .. })
            | (Type::Symbol { params: nested, .. }, Type::Symbol { params: actual_params, .. }) => {
                for (formal, actual) in nested.iter().zip(actual_params.iter()) {
                    Self::infer_generic_type(params, formal, actual, inferred);
                }
            }
            _ => {}
        }
    }
}
