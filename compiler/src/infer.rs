use super::{Compiler, Symbol};
use anyhow::Result;
use dynamic::{Dynamic, Type};
use parser::{BinaryOp, Expr, ExprKind, PatternKind, Span, Stmt, StmtKind};

impl Compiler {
    fn merge_return_type(left: Option<Type>, right: Type) -> Type {
        match left {
            Some(left) if left == right => left,
            Some(left) => left + right,
            None => right,
        }
    }

    fn infer_return_type(&mut self, stmt: &Stmt) -> Result<Option<Type>> {
        match &stmt.kind {
            StmtKind::Return(Some(expr)) => Ok(Some(self.infer_expr(expr)?)),
            StmtKind::Return(None) => Ok(Some(Type::Void)),
            StmtKind::Block(stmts) => {
                let mut ret = None;
                for stmt in stmts {
                    if let Some(ty) = self.infer_return_type(stmt)? {
                        ret = Some(Self::merge_return_type(ret, ty));
                    }
                }
                Ok(ret)
            }
            StmtKind::If { cond, then_body, else_body } => {
                let cond_ty = self.infer_expr(cond)?;
                if cond_ty != Type::Bool {
                    return Err(Self::semantic_error(cond.span, "条件表达式必须是布尔类型"));
                }
                let mut ret = self.infer_return_type(then_body)?;
                if let Some(body) = else_body {
                    if let Some(ty) = self.infer_return_type(body)? {
                        ret = Some(Self::merge_return_type(ret, ty));
                    }
                }
                Ok(ret)
            }
            StmtKind::While { cond, body } => {
                let cond_ty = self.infer_expr(cond)?;
                if cond_ty != Type::Bool {
                    return Err(Self::semantic_error(cond.span, "条件表达式必须是布尔类型"));
                }
                self.infer_return_type(body)
            }
            StmtKind::Loop(body) => self.infer_return_type(body),
            StmtKind::For { pat, range, body } => {
                if let PatternKind::Var { idx, .. } = &pat.kind {
                    let ty = self.infer_expr(range)?;
                    self.set_ty(*idx, ty);
                } else if let PatternKind::Tuple(pats) = &pat.kind {
                    let ty = self.infer_expr(range)?;
                    assert!(ty.is_any());
                    for pat in pats {
                        if let Some(idx) = pat.var() {
                            self.set_ty(idx, Type::Any);
                        }
                    }
                }
                self.infer_return_type(body)
            }
            StmtKind::Let { .. } => {
                self.infer_stmt(stmt)?;
                Ok(None)
            }
            StmtKind::Expr(expr, close) => {
                let ty = self.infer_expr(expr)?;
                Ok(if *close { None } else { Some(ty) })
            }
            _ => {
                self.infer_stmt(stmt)?;
                Ok(None)
            }
        }
    }

    pub fn infer_expr(&mut self, expr: &Expr) -> Result<Type> {
        match &expr.kind {
            ExprKind::Value(Dynamic::Null) => Ok(Type::Any),
            ExprKind::Value(v) => Ok(v.get_type()),
            ExprKind::Var(idx) => {
                let idx = self.top() + (*idx as usize);
                if idx < self.tys.len() { self.symbols.get_type(&self.tys[idx]) } else { Ok(Type::Any) }
            }
            ExprKind::Id(id, _) => match self.symbols.get_symbol(*id)?.1 {
                Symbol::Const { ty, .. } => Ok(ty.clone()),
                Symbol::Static { ty, .. } => Ok(ty.clone()),
                Symbol::Struct(ty, _) => Ok(ty.clone()),
                Symbol::Fn { .. } => Ok(Type::Symbol { id: *id, params: Vec::new() }),
                Symbol::Native(ty) => Ok(ty.clone()),
                s => Err(Self::semantic_error(expr.span, format!("符号 {:?} 不是变量、常量、静态变量、结构体", s))),
            },
            ExprKind::AssocId { id, params } => Ok(Type::Symbol { id: *id, params: params.clone() }),
            ExprKind::Unary { value, .. } => self.infer_expr(value.as_ref()),
            ExprKind::Binary { left, op, right } => {
                let assign_idx = if op.is_assign() { if let ExprKind::Var(idx) = &left.kind { Some(*idx) } else { None } } else { None };
                let ty = if op.is_logic() {
                    let left_ty = self.infer_expr(left)?;
                    if matches!(op, BinaryOp::And | BinaryOp::Or) && left_ty.is_any() { Type::Any } else { Type::Bool }
                } else if op == &BinaryOp::Idx {
                    let left_ty = self.infer_expr(left)?;
                    if let Type::Array(elem_ty, _) = left_ty {
                        (*elem_ty).clone()
                    } else if let Type::Vec(elem_ty, _) = left_ty {
                        (*elem_ty).clone()
                    } else {
                        let left_ty = self.symbols.get_type(&left_ty)?;
                        let right_ty = if right.is_value() || right.is_const() {
                            let right_value = if let ExprKind::Const(c) = &right.kind { self.consts[*c].clone() } else { right.clone().value()? };
                            if right_value.is_str() {
                                if left_ty.is_any() {
                                    return Ok(Type::Any);
                                }
                                if let Ok(field) = self.symbols.get_field(&left_ty, right_value.as_str()) {
                                    return if let Type::Fn { ret, .. } = field.1 { Ok(ret.as_ref().clone()) } else { Ok(field.1.clone()) };
                                }
                            } else if let Type::Struct { fields, .. } = &left_ty
                                && let Some(idx) = right_value.as_int()
                            {
                                return fields.get(idx as usize).map(|(_, ty)| ty.clone()).ok_or_else(|| Self::semantic_error(right.span, format!("结构字段索引越界 {}", idx)));
                            }
                            right_value.get_type()
                        } else {
                            self.infer_expr(right)?
                        };
                        if right_ty.is_int() || right_ty.is_uint() {
                            if left_ty.is_any() {
                                return Ok(Type::Any);
                            }
                            let (_, s) = self.symbols.get_field(&left_ty, "get_idx")?;
                            let fn_ty = self.symbols.get_type(&s)?;
                            return if let Type::Fn { ret, .. } = &fn_ty { Ok(ret.as_ref().clone()) } else { Ok(fn_ty) };
                        }
                        if left_ty.is_any() {
                            return Ok(Type::Any);
                        }
                        Type::Any
                    }
                } else {
                    let right_ty = self.infer_expr(right)?;
                    if op == &BinaryOp::Assign { right_ty } else { self.infer_expr(left)? + right_ty }
                };
                assign_idx.map(|idx| self.set_ty(idx, ty.clone()));
                Ok(ty)
            }
            ExprKind::Call { obj, params } => {
                if let ExprKind::AssocId { id, params: generic_args } = &obj.kind {
                    let mut args = Vec::new();
                    for p in params {
                        args.push(self.infer_expr(p)?);
                    }
                    self.infer_fn_with_params(*id, &args, generic_args)
                } else if let ExprKind::TypedMethod { obj: target, ty, name } = &obj.kind {
                    let base_name = match ty {
                        Type::Ident { name, .. } => name.clone(),
                        Type::Symbol { id, .. } => self.symbols.get_symbol(*id)?.0.clone(),
                        _ => return Ok(Type::Any),
                    };
                    let id = self.symbols.get_id(&format!("{}::{}", base_name, name))?;
                    let mut args = vec![self.infer_expr(target)?];
                    for p in params {
                        args.push(self.infer_expr(p)?);
                    }
                    self.infer_fn(id, &args)
                } else if let ExprKind::Id(id, obj_expr) = &obj.kind {
                    let mut args: Vec<Type> = if let Some(obj) = obj_expr { vec![self.infer_expr(obj)?] } else { Vec::new() };
                    for p in params {
                        args.push(self.infer_expr(p)?);
                    }
                    self.infer_fn(*id, &args)
                } else if obj.is_idx() {
                    let (target, _, method) = obj.clone().binary().unwrap();
                    let ty = self.infer_expr(&target)?;
                    if let Some(method) = self.get_value(&method) {
                        let method = method.as_str();
                        let fn_ty = match self.get_field(&ty, method) {
                            Ok((_, fn_ty)) => fn_ty,
                            Err(_) => {
                                let id = self.symbols.get_id(method)?;
                                if self.symbols.get_symbol(id)?.1.is_fn() {
                                    Type::Symbol { id, params: Vec::new() }
                                } else {
                                    return Err(Self::semantic_error(obj.span, format!("符号 {method} 不是函数")));
                                }
                            }
                        };
                        if let Type::Symbol { id, .. } = fn_ty {
                            let mut args = vec![ty];
                            for p in params {
                                args.push(self.infer_expr(p)?);
                            }
                            self.infer_fn(id, &args)
                        } else {
                            Ok(fn_ty)
                        }
                    } else {
                        Ok(Type::Any)
                    }
                } else if let ExprKind::Var(idx) = &obj.kind {
                    let idx = self.top() + (*idx as usize);
                    if idx < self.tys.len()
                        && let Type::Symbol { id, .. } = self.tys[idx]
                    {
                        let mut args = Vec::new();
                        for p in params {
                            args.push(self.infer_expr(p)?);
                        }
                        self.infer_fn(id, &args)
                    } else {
                        Ok(Type::Any)
                    }
                } else if obj.is_value() {
                    Ok(Type::Void)
                } else {
                    Ok(Type::Any)
                }
            }
            ExprKind::Typed { ty, .. } => Ok(ty.clone()),
            ExprKind::Stmt(stmt) => self.infer_stmt(stmt),
            ExprKind::Range { start, stop, .. } => {
                let start_ty = self.infer_expr(start)?;
                let stop_ty = self.infer_expr(stop)?;
                Ok(if start_ty.is_any() {
                    stop_ty
                } else if stop_ty.is_any() {
                    start_ty
                } else {
                    stop_ty
                })
            }
            _ => Ok(Type::Any),
        }
    }

    fn get_fn_tys(&mut self, tys: &[Type], arg_tys: &[Type]) -> Result<Vec<Type>> {
        let mut fn_tys = Vec::new();
        for (i, ty) in tys.iter().enumerate() {
            if !ty.is_any() {
                fn_tys.push(ty.clone());
            } else if let Some(arg_ty) = arg_tys.get(i) {
                fn_tys.push(self.symbols.get_type(arg_ty)?);
            } else {
                fn_tys.push(Type::Any);
            }
        }
        Ok(fn_tys)
    }

    pub fn infer_fn(&mut self, id: u32, arg_tys: &[Type]) -> Result<Type> {
        self.infer_fn_with_params(id, arg_tys, &[])
    }

    pub fn infer_fn_with_params(&mut self, id: u32, arg_tys: &[Type], generic_args: &[Type]) -> Result<Type> {
        let (name, s) = self.symbols.get_symbol(id).map(|(n, s)| (n.clone(), s.clone()))?;
        if let Symbol::Fn { ty, args, generic_params, cap, body, .. } = s {
            if let Type::Fn { tys, ret: _ } = ty {
                let inferred_generic_args = if generic_args.is_empty() { crate::infer_generic_args_from_types(&generic_params, &tys, arg_tys) } else { generic_args.to_vec() };
                let generic_args = if generic_params.is_empty() { &[] } else { inferred_generic_args.as_slice() };
                let tys = if generic_params.is_empty() { tys } else { tys.iter().map(|ty| crate::substitute_type(ty, &generic_params, generic_args)).collect() };
                let body = if generic_params.is_empty() { body.as_ref().clone() } else { crate::substitute_stmt(body.as_ref(), &generic_params, generic_args) };
                let fn_tys = self.get_fn_tys(&tys, arg_tys)?;
                let body = if generic_params.is_empty() {
                    body
                } else {
                    let mut compile_tys = tys.clone();
                    let mut compile_cap = cap.clone();
                    let saved_state = self.take_local_state();
                    let compiled = self.compile_fn(&args, &mut compile_tys, body, &mut compile_cap);
                    self.restore_local_state(saved_state);
                    Stmt::new(StmtKind::Block(compiled?), Span::default())
                };
                if let Some(fns) = self.fns.get_mut(&id) {
                    for f in fns.iter() {
                        if f.0 == generic_args && f.1 == fn_tys {
                            return Ok(f.2.clone());
                        }
                    }
                    fns.push((generic_args.to_vec(), fn_tys.clone(), Type::Any));
                } else {
                    self.fns.insert(id, vec![(generic_args.to_vec(), fn_tys.clone(), Type::Any)]);
                }
                let top = self.tys.len();
                self.tys.append(&mut fn_tys.clone());
                for c in cap.vars.iter() {
                    self.tys.push(self.tys[self.top() + *c].clone());
                }
                self.frames.push(top);
                let ret_ty = self.infer_return_type(&body).map(|ty| ty.unwrap_or(Type::Void));
                if let Some(top) = self.frames.pop() {
                    self.tys.truncate(top);
                }
                let ret_ty = match ret_ty {
                    Ok(ret_ty) => ret_ty,
                    Err(err) => {
                        log::error!("infer_fn {} failed: {:?}", name, err);
                        let should_remove = self
                            .fns
                            .get_mut(&id)
                            .map(|fns| {
                                fns.retain(|item| item.0 != generic_args || item.1 != fn_tys || item.2 != Type::Any);
                                fns.is_empty()
                            })
                            .unwrap_or(false);
                        if should_remove {
                            self.fns.remove(&id);
                        }
                        return Err(err);
                    }
                };
                self.fns.get_mut(&id).map(|f| {
                    f.iter_mut().find(|item| item.0 == generic_args && item.1 == fn_tys).map(|item| item.2 = ret_ty.clone());
                });
                Ok(ret_ty)
            } else {
                Ok(Type::Any)
            }
        } else if let Symbol::Native(f) = s {
            if let Type::Fn { ret, .. } = f { Ok((*ret).clone()) } else { Ok(Type::Any) }
        } else if matches!(s, Symbol::Null) {
            Ok(Type::Any)
        } else {
            Err(Self::semantic_error(Span::default(), format!("符号 {:?} 不是函数", name)))
        }
    }

    pub fn infer_stmt(&mut self, stmt: &Stmt) -> Result<Type> {
        match &stmt.kind {
            StmtKind::Expr(expr, close) => {
                if !close {
                    self.infer_expr(expr)
                } else {
                    self.infer_expr(expr)?;
                    Ok(Type::Void)
                }
            }
            StmtKind::Return(expr) => {
                if let Some(e) = expr {
                    self.infer_expr(e)
                } else {
                    Ok(Type::Void)
                }
            }
            StmtKind::Block(stmts) => {
                for (idx, stmt) in stmts.iter().enumerate() {
                    let ty = self.infer_stmt(stmt)?;
                    if stmt.is_return() || idx == stmts.len() - 1 {
                        return Ok(ty);
                    }
                }
                Ok(Type::Void)
            }
            StmtKind::If { then_body, else_body, .. } => {
                let then_ty = self.infer_stmt(then_body)?;
                if let Some(e) = else_body {
                    let else_ty = self.infer_stmt(e)?;
                    if then_ty != else_ty {
                        log::info!("then 和 else 有不同类型 {:?} {:?}", then_ty, else_ty);
                        return Ok(if then_ty.is_any() { else_ty } else { then_ty });
                    }
                }
                if else_body.is_none() {
                    return Ok(Type::Void);
                }
                Ok(then_ty)
            }
            StmtKind::While { cond, body } => {
                let cond_ty = self.infer_expr(cond)?;
                if cond_ty != Type::Bool {
                    return Err(Self::semantic_error(cond.span, "条件表达式必须是布尔类型"));
                }
                self.infer_stmt(body)
            }
            StmtKind::For { pat, range, body } => {
                if let PatternKind::Var { idx, .. } = &pat.kind {
                    let ty = self.infer_expr(range)?;
                    self.set_ty(*idx, ty);
                } else if let PatternKind::Tuple(pats) = &pat.kind {
                    let ty = self.infer_expr(range)?;
                    assert!(ty.is_any());
                    for pat in pats {
                        if let Some(idx) = pat.var() {
                            self.set_ty(idx, Type::Any);
                        }
                    }
                }
                self.infer_stmt(body)
            }
            StmtKind::Let { pat, value } => {
                let expr_ty = if let StmtKind::Expr(expr, _) = &value.kind { self.infer_expr(expr)? } else { self.infer_stmt(value)? };
                if let PatternKind::Ident { ty, .. } = &pat.kind {
                    let annotated_ty = self.symbols.get_type(ty)?;
                    if annotated_ty.is_any() {
                        self.add_ty(expr_ty);
                    } else {
                        self.add_ty(annotated_ty);
                    }
                } else if let PatternKind::Var { idx, .. } = &pat.kind {
                    self.set_ty(*idx, expr_ty);
                } else if matches!(pat.kind, PatternKind::Wildcard) {
                    self.add_ty(expr_ty);
                }
                Ok(Type::Void)
            }
            _ => Ok(Type::Void),
        }
    }
}
