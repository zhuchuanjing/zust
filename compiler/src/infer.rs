use super::{Compiler, Symbol};
use anyhow::Result;
use dynamic::{Dynamic, Type};
use parser::{BinaryOp, Expr, ExprKind, Pattern, PatternKind, Span, Stmt, StmtKind, UnaryOp};

#[derive(Clone)]
struct ReturnInfo {
    ty: Type,
    shape: Option<Type>,
}

impl Compiler {
    fn add_pattern_bindings_for_infer(&mut self, pat: &Pattern, expr_ty: Type) -> Result<()> {
        match &pat.kind {
            PatternKind::Ident { name, ty } => {
                let annotated_ty = self.symbols.get_type(ty)?;
                self.add_name(name.clone());
                self.add_ty(if annotated_ty.is_any() { expr_ty } else { annotated_ty });
            }
            PatternKind::Var { idx, .. } => self.set_ty(*idx, expr_ty),
            PatternKind::Tuple(pats) => {
                if let Type::Tuple(tys) = expr_ty {
                    for (pat, ty) in pats.iter().zip(tys) {
                        self.add_pattern_bindings_for_infer(pat, ty)?;
                    }
                } else {
                    for pat in pats {
                        self.add_pattern_bindings_for_infer(pat, Type::Any)?;
                    }
                }
            }
            PatternKind::List { elems, .. } => {
                for pat in elems {
                    self.add_pattern_bindings_for_infer(pat, Type::Any)?;
                }
            }
            PatternKind::Wildcard => {
                self.add_name("".into());
                self.add_ty(expr_ty);
            }
            PatternKind::Literal(_) | PatternKind::Member(_, _) | PatternKind::Idx(_, _) => {}
        }
        Ok(())
    }

    fn for_pattern_ty(&mut self, range: &Expr) -> Result<Type> {
        if matches!(range.kind, ExprKind::Range { .. }) {
            return self.infer_expr(range);
        }
        Ok(match self.infer_expr(range)? {
            Type::Array(elem_ty, _) | Type::Vec(elem_ty, _) => elem_ty.as_ref().clone(),
            _ => Type::Any,
        })
    }

    fn merge_return_type(span: Span, left: Option<Type>, right: Type) -> Result<Type> {
        match left {
            Some(left) if left == right => Ok(left),
            Some(left) if left.is_void() || right.is_void() => Err(Self::semantic_error(span, format!("返回类型不一致: {:?} 和 {:?}", left, right))),
            Some(left) => Ok(left + right),
            None => Ok(right),
        }
    }

    fn return_shape(&self, expr: &Expr, ty: &Type) -> Option<Type> {
        if !ty.is_any() {
            return if ty.is_struct() { Some(ty.clone()) } else { None };
        }
        match &expr.kind {
            ExprKind::List(_) | ExprKind::Tuple(_) => Some(Type::List),
            ExprKind::Dict(_) => Some(Type::Map),
            ExprKind::Value(value) => Self::dynamic_return_shape(value.get_type()),
            ExprKind::Const(idx) => self.consts.get(*idx).and_then(|value| Self::dynamic_return_shape(value.get_type())),
            ExprKind::Typed { ty, .. } => Some(ty.clone()),
            _ => None,
        }
    }

    fn dynamic_return_shape(ty: Type) -> Option<Type> {
        match ty {
            Type::Map => Some(Type::Map),
            Type::List | Type::Array(_, _) => Some(Type::List),
            _ => None,
        }
    }

    fn infer_return_expr(&mut self, expr: &Expr) -> Result<ReturnInfo> {
        let ty = self.infer_expr(expr)?;
        let shape = self.return_shape(expr, &ty);
        Ok(ReturnInfo { ty, shape })
    }

    fn merge_return_info(span: Span, left: Option<ReturnInfo>, right: ReturnInfo) -> Result<ReturnInfo> {
        let Some(left) = left else {
            return Ok(right);
        };
        if let (Some(left_shape), Some(right_shape)) = (&left.shape, &right.shape)
            && left_shape != right_shape
        {
            return Err(Self::semantic_error(span, format!("返回类型不一致: {:?} 和 {:?}", left_shape, right_shape)));
        }
        if let Some(left_shape) = &left.shape
            && left_shape.is_struct()
            && right.ty.is_any()
            && right.shape.is_none()
        {
            return Err(Self::semantic_error(span, format!("返回类型不一致: {:?} 和 {:?}", left_shape, Type::Any)));
        }
        if let Some(right_shape) = &right.shape
            && right_shape.is_struct()
            && left.ty.is_any()
            && left.shape.is_none()
        {
            return Err(Self::semantic_error(span, format!("返回类型不一致: {:?} 和 {:?}", Type::Any, right_shape)));
        }
        let ty = Self::merge_return_type(span, Some(left.ty), right.ty)?;
        Ok(ReturnInfo { ty, shape: left.shape.or(right.shape) })
    }

    fn infer_return_type(&mut self, stmt: &Stmt) -> Result<Option<Type>> {
        self.infer_returns(stmt, true).map(|(info, _)| info.map(|info| info.ty))
    }

    pub(crate) fn check_return_type(&mut self, stmt: &Stmt) -> Result<()> {
        self.infer_returns(stmt, true).map(|_| ())
    }

    fn infer_returns(&mut self, stmt: &Stmt, tail: bool) -> Result<(Option<ReturnInfo>, bool)> {
        match &stmt.kind {
            StmtKind::Return(Some(expr)) => Ok((Some(self.infer_return_expr(expr)?), true)),
            StmtKind::Return(None) => Ok((Some(ReturnInfo { ty: Type::Void, shape: Some(Type::Void) }), true)),
            StmtKind::Block(stmts) => {
                let mut ret = None;
                for (idx, stmt) in stmts.iter().enumerate() {
                    let (info, always_returns) = self.infer_returns(stmt, tail && idx == stmts.len().saturating_sub(1))?;
                    if let Some(info) = info {
                        ret = Some(Self::merge_return_info(stmt.span, ret, info)?);
                    }
                    if always_returns {
                        return Ok((ret, true));
                    }
                }
                Ok((ret, false))
            }
            StmtKind::If { cond, then_body, else_body } => {
                let cond_ty = self.infer_expr(cond)?;
                if cond_ty != Type::Bool {
                    return Err(Self::semantic_error(cond.span, format!("条件表达式必须是布尔类型，实际是 {:?}", cond_ty)));
                }
                let (mut ret, then_returns) = self.infer_returns(then_body, tail)?;
                let else_returns = if let Some(body) = else_body {
                    let (else_ty, else_returns) = self.infer_returns(body, tail)?;
                    if let Some(info) = else_ty {
                        ret = Some(Self::merge_return_info(body.span, ret, info)?);
                    }
                    else_returns
                } else {
                    false
                };
                Ok((ret, then_returns && else_returns))
            }
            StmtKind::While { cond, body } => {
                let cond_ty = self.infer_expr(cond)?;
                if cond_ty != Type::Bool {
                    return Err(Self::semantic_error(cond.span, format!("条件表达式必须是布尔类型，实际是 {:?}", cond_ty)));
                }
                self.infer_returns(body, false).map(|(ty, _)| (ty, false))
            }
            StmtKind::Loop(body) => self.infer_returns(body, false),
            StmtKind::For { pat, range, body } => {
                let ty = self.for_pattern_ty(range)?;
                self.add_pattern_bindings_for_infer(pat, ty)?;
                self.infer_returns(body, false).map(|(ty, _)| (ty, false))
            }
            StmtKind::Let { .. } => {
                self.infer_stmt(stmt)?;
                Ok((None, false))
            }
            StmtKind::Expr(expr, close) => {
                let info = self.infer_return_expr(expr)?;
                Ok(if *close || !tail { (None, false) } else { (Some(info), true) })
            }
            _ => {
                self.infer_stmt(stmt)?;
                Ok((None, false))
            }
        }
    }

    pub fn infer_expr(&mut self, expr: &Expr) -> Result<Type> {
        match &expr.kind {
            ExprKind::Value(Dynamic::Null) => Ok(Type::Any),
            ExprKind::Value(v) if v.is_list() || v.is_map() => Ok(Type::Any),
            ExprKind::Value(v) => Ok(v.get_type()),
            ExprKind::Const(_) => Ok(Type::Any),
            ExprKind::Var(idx) => {
                let idx = self.top() + (*idx as usize);
                if idx < self.tys.len() { self.symbols.get_type(&self.tys[idx]) } else { Ok(Type::Any) }
            }
            ExprKind::Ident(ident) => {
                for idx in (self.top()..self.names.len()).rev() {
                    if self.names[idx].eq(ident) && idx < self.tys.len() {
                        return self.symbols.get_type(&self.tys[idx]);
                    }
                }
                let id = self.symbols.get_id(ident).map_err(|_| Self::semantic_error(expr.span, format!("未找到标识符 {}", ident)))?;
                match self.symbols.get_symbol(id)?.1 {
                    Symbol::Const { ty, .. } => Ok(ty.clone()),
                    Symbol::Static { ty, .. } => Ok(ty.clone()),
                    Symbol::Struct(ty, _) => Ok(ty.clone()),
                    Symbol::Fn { .. } => Ok(Type::Symbol { id, params: Vec::new() }),
                    Symbol::Native(ty) => Ok(ty.clone()),
                    s => Err(Self::semantic_error(expr.span, format!("符号 {:?} 不是变量、常量、静态变量、结构体", s))),
                }
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
            ExprKind::Unary { op, value } => match op {
                UnaryOp::Not => {
                    let ty = self.infer_expr(value.as_ref())?;
                    if ty.is_int() || ty.is_uint() { Ok(ty) } else { Ok(Type::Bool) }
                }
                UnaryOp::Neg => self.infer_expr(value.as_ref()),
                UnaryOp::Unknow => Ok(Type::Any),
            },
            ExprKind::Binary { left, op, right } => {
                let assign_idx = if op.is_assign() { if let ExprKind::Var(idx) = &left.kind { Some(*idx) } else { None } } else { None };
                let ty = if op.is_logic() { Type::Bool } else if op == &BinaryOp::Idx {
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
            ExprKind::Typed { ty, .. } => self.symbols.get_type(ty),
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
                            return self.symbols.get_type(&f.2);
                        }
                    }
                    fns.push((generic_args.to_vec(), fn_tys.clone(), Type::Any));
                } else {
                    self.fns.insert(id, vec![(generic_args.to_vec(), fn_tys.clone(), Type::Any)]);
                }
                let top = self.tys.len();
                self.frames.push(top);
                for (arg, ty) in args.iter().zip(fn_tys.iter()) {
                    self.add_name(arg.clone());
                    self.add_ty(ty.clone());
                }
                for c in cap.vars.iter() {
                    if let Some((name, ty)) = cap.names.get(*c) {
                        self.add_name(name.clone());
                        self.add_ty(ty.clone());
                    } else {
                        self.add_name("".into());
                        self.add_ty(Type::Any);
                    }
                }
                let ret_ty = self.infer_return_type(&body).map(|ty| ty.unwrap_or(Type::Void));
                if let Some(top) = self.frames.pop() {
                    self.tys.truncate(top);
                    self.names.truncate(top);
                }
                let ret_ty = match ret_ty {
                    Ok(ret_ty) => self.symbols.get_type(&ret_ty).unwrap_or(ret_ty),
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
                if generic_args.is_empty()
                    && let Some((_, Symbol::Fn { ty: Type::Fn { ret, .. }, .. })) = self.symbols.get_symbol_mut(id)
                    && ret.is_any()
                {
                    *ret = std::rc::Rc::new(ret_ty.clone());
                }
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
                    return Err(Self::semantic_error(cond.span, format!("条件表达式必须是布尔类型，实际是 {:?}", cond_ty)));
                }
                self.infer_stmt(body)
            }
            StmtKind::For { pat, range, body } => {
                let ty = self.for_pattern_ty(range)?;
                self.add_pattern_bindings_for_infer(pat, ty)?;
                self.infer_stmt(body)
            }
            StmtKind::Let { pat, value } => {
                let expr_ty = if let StmtKind::Expr(expr, _) = &value.kind { self.infer_expr(expr)? } else { self.infer_stmt(value)? };
                self.add_pattern_bindings_for_infer(pat, expr_ty)?;
                Ok(Type::Void)
            }
            _ => Ok(Type::Void),
        }
    }
}
