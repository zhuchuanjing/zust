use super::{Compiler, FnInferRet, ListElemState, Symbol};
use anyhow::Result;
use dynamic::{Dynamic, Type};
use parser::{BinaryOp, Expr, ExprKind, Pattern, PatternKind, Span, Stmt, StmtKind, UnaryOp};
use smol_str::SmolStr;

#[derive(Clone)]
struct ReturnInfo {
    ty: Type,
    shape: Option<Type>,
}

/// 类型推断递归链的硬上限。同一实例化由 `self.type_ctx.fns` 记忆化挡住,但互递归的泛型
/// 函数每次可能产生新的 (generic_args, fn_tys) 实例化,记忆化命不中,会无限递归
/// 直至栈溢出。超过此深度即把推断结果回退成 [`Type::Any`],把"挂起/崩溃"降级为
/// 一个保守但安全的类型。正常代码的推断链远不及此。
const MAX_INFER_DEPTH: usize = 64;

impl Compiler {
    /// 类型检查:算术/位/移位运算符两侧涉及 Str 或 Bool 时报错。
    /// 设计依据:动态语义下,字符串与数值拼接合理 (`1 + "x"` = "1x"),
    /// 但减/乘/除/位运算遇到字符串或布尔没有合理动态行为,运行时只会 panic。
    /// `Add/AddAssign` 显式跳过 — 与 `Type::add` 一致允许字符串拼接。
    /// 比较运算 (Eq/Ne/Lt/...) 也跳过 — dynamic 端已有任意类型比较语义。
    fn check_arith_types(op: &BinaryOp, left_ty: &Type, right_ty: &Type, span: Span) -> Result<()> {
        if !op.is_arith() || op.is_add() {
            return Ok(());
        }
        let bad_left = matches!(left_ty, Type::Str | Type::Bool);
        let bad_right = matches!(right_ty, Type::Str | Type::Bool);
        if bad_left || bad_right {
            let op_name = op.symbol();
            return Err(Self::semantic_error(span, format!("运算符 {op_name} 不支持 Str/Bool 类型:左侧 {left_ty:?},右侧 {right_ty:?}")));
        }
        Ok(())
    }

    fn current_infer_key(&self) -> Option<(u32, Vec<Type>, Vec<Type>)> {
        self.type_ctx.infer_stack.last().cloned()
    }

    fn pending_return_seed(&self, id: u32, generic_args: &[Type], fn_tys: &[Type]) -> Option<Type> {
        self.type_ctx.fns.get(&id).and_then(|fns| {
            fns.iter().find_map(|item| {
                if item.0 == generic_args
                    && item.1 == fn_tys
                    && let FnInferRet::Pending(seed) = &item.2
                {
                    seed.clone()
                } else {
                    None
                }
            })
        })
    }

    fn update_pending_return_seed(&mut self, ty: &Type) {
        if ty.is_any() {
            return;
        }
        let Some((id, generic_args, fn_tys)) = self.current_infer_key() else {
            return;
        };
        let Some(fns) = self.type_ctx.fns.get_mut(&id) else {
            return;
        };
        if let Some(item) = fns.iter_mut().find(|item| item.0 == generic_args && item.1 == fn_tys)
            && let FnInferRet::Pending(seed) = &mut item.2
        {
            let next = seed.take().map(|prev| prev + ty.clone()).unwrap_or_else(|| ty.clone());
            *seed = Some(next);
        }
    }

    /// 扫描函数体，查找第一个非递归路径上的返回值类型（仅处理字面量）。
    fn try_find_base_return_ty(&self, body: &Stmt) -> Option<Type> {
        match &body.kind {
            StmtKind::Block(stmts) => stmts.iter().find_map(|s| self.try_find_base_return_ty(s)),
            StmtKind::If { then_body, else_body, .. } => self.try_find_base_return_ty(then_body).or_else(|| else_body.as_ref().and_then(|b| self.try_find_base_return_ty(b))),
            StmtKind::Return(Some(expr)) => Self::try_literal_type(expr),
            StmtKind::Expr(expr, false) => Self::try_literal_type(expr),
            _ => None,
        }
    }

    /// 带作用域的 base case 返回类型查找
    fn try_find_base_return_ty_with_scope(&mut self, body: &Stmt, fn_id: u32, fn_name: &str, args: &[SmolStr], fn_tys: &[Type]) -> Option<Type> {
        let saved_state = self.take_local_state();
        self.sym_tab.frames.push(0);
        for (arg, ty) in args.iter().zip(fn_tys.iter()) {
            self.add_name(arg.clone());
            self.add_ty(ty.clone());
        }
        let result = self.try_find_base_return_ty_with_scope_inner(body, fn_id, fn_name);
        self.restore_local_state(saved_state);
        result
    }

    fn try_find_base_return_ty_with_scope_inner(&mut self, body: &Stmt, fn_id: u32, fn_name: &str) -> Option<Type> {
        match &body.kind {
            StmtKind::Block(stmts) => stmts.iter().find_map(|s| self.try_find_base_return_ty_with_scope_inner(s, fn_id, fn_name)),
            StmtKind::If { then_body, else_body, .. } => {
                self.try_find_base_return_ty_with_scope_inner(then_body, fn_id, fn_name).or_else(|| else_body.as_ref().and_then(|b| self.try_find_base_return_ty_with_scope_inner(b, fn_id, fn_name)))
            }
            StmtKind::Return(Some(expr)) => {
                if Self::expr_calls_fn(expr, fn_id, fn_name) {
                    None
                } else {
                    self.infer_return_expr(expr).ok().map(|info| info.ty)
                }
            }
            StmtKind::Expr(expr, false) => {
                if Self::expr_calls_fn(expr, fn_id, fn_name) {
                    None
                } else {
                    self.infer_return_expr(expr).ok().map(|info| info.ty)
                }
            }
            _ => None,
        }
    }

    fn expr_calls_fn(expr: &Expr, fn_id: u32, fn_name: &str) -> bool {
        match &expr.kind {
            ExprKind::Call { obj, params } => {
                if let ExprKind::Id(id, _) = &obj.kind {
                    return *id == fn_id;
                }
                if let ExprKind::Ident(name) = &obj.kind {
                    if name.as_str() == fn_name || fn_name.ends_with(&format!("::{}", name)) {
                        return true;
                    }
                }
                params.iter().any(|p| Self::expr_calls_fn(p, fn_id, fn_name))
            }
            ExprKind::Binary { left, op: _, right } => Self::expr_calls_fn(left, fn_id, fn_name) || Self::expr_calls_fn(right, fn_id, fn_name),
            ExprKind::Unary { op: _, value } => Self::expr_calls_fn(value, fn_id, fn_name),
            ExprKind::Typed { value, ty: _ } => Self::expr_calls_fn(value, fn_id, fn_name),
            _ => false,
        }
    }

    fn try_literal_type(expr: &Expr) -> Option<Type> {
        match &expr.kind {
            ExprKind::Value(v) => Some(v.get_type()),
            ExprKind::Unary { op: UnaryOp::Neg, value } => Self::try_literal_type(value),
            _ => None,
        }
    }

    fn add_pattern_bindings_for_infer(&mut self, pat: &Pattern, expr_ty: Type) -> Result<()> {
        match &pat.kind {
            PatternKind::Ident { name, ty } => {
                let annotated_ty = self.sym_tab.symbols.get_type(ty)?;
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
            PatternKind::Literal(_) | PatternKind::Member(_, _) | PatternKind::Idx(_, _) | PatternKind::Struct { .. } => {}
        }
        Ok(())
    }

    fn for_pattern_ty(&mut self, range: &Expr) -> Result<Type> {
        if matches!(range.kind, ExprKind::Range { .. }) {
            return self.infer_range_expr(range);
        }
        Ok(match self.infer_expr(range)? {
            Type::Array(elem_ty, _) | Type::Vec(elem_ty, _) | Type::List(elem_ty) => elem_ty.as_ref().clone(),
            _ => Type::Any,
        })
    }

    fn infer_range_expr(&mut self, range: &Expr) -> Result<Type> {
        let ExprKind::Range { start, stop, .. } = &range.kind else {
            return self.infer_expr(range);
        };
        let start_ty = self.infer_expr(start)?;
        let stop_ty = self.infer_expr(stop)?;
        Ok(Self::merge_range_bound_types(start_ty, stop_ty))
    }

    fn merge_range_bound_types(start_ty: Type, stop_ty: Type) -> Type {
        if start_ty.is_any() {
            stop_ty
        } else if stop_ty.is_any() {
            start_ty
        // 无后缀整数字面量(默认 I32/I64)在 range 里向另一端的具体无符号类型靠拢,
        // 这样 `0..n`(n: u32)仍是 u32 range,而不是被默认 I64 拖宽成 i64(会拆穿 GPU 后端)。
        } else if matches!(start_ty, Type::I32 | Type::I64) && stop_ty.is_uint() {
            stop_ty
        } else if matches!(stop_ty, Type::I32 | Type::I64) && start_ty.is_uint() {
            start_ty
        } else {
            start_ty + stop_ty
        }
    }

    fn merge_return_type(span: Span, left: Option<Type>, right: Type) -> Result<Type> {
        match left {
            Some(left) if left == right => Ok(left),
            Some(left) if left.is_void() || right.is_void() => Err(Self::semantic_error(span, format!("返回类型不一致: {:?} 和 {:?}", left, right))),
            Some(left) if left.is_any() || right.is_any() => Ok(Type::Any),
            Some(left) => Ok(left + right),
            None => Ok(right),
        }
    }

    fn return_shape(&self, expr: &Expr, ty: &Type) -> Option<Type> {
        if !ty.is_any() {
            return match ty {
                Type::Struct { .. } => Some(ty.clone()),
                Type::Map => Some(Type::Map),
                Type::List(elem) | Type::Array(elem, _) => Some(Type::List(elem.clone())),
                _ => None,
            };
        }
        match &expr.kind {
            ExprKind::List(_) | ExprKind::Tuple(_) => Some(Type::list_any()),
            ExprKind::Dict(_) => Some(Type::Map),
            ExprKind::Value(value) => Self::dynamic_return_shape(value.get_type()),
            ExprKind::Const(idx) => self.sym_tab.consts.get_index(*idx).and_then(|(_, value)| Self::dynamic_return_shape(value.get_type())),
            ExprKind::Typed { ty, .. } => Some(ty.clone()),
            _ => None,
        }
    }

    fn dynamic_return_shape(ty: Type) -> Option<Type> {
        match ty {
            Type::Map => Some(Type::Map),
            Type::List(elem) => Some(Type::List(elem)),
            Type::Array(elem, _) => Some(Type::List(elem)),
            _ => None,
        }
    }

    fn local_var_idx_for_expr(&self, expr: &Expr) -> Option<u32> {
        match &expr.kind {
            ExprKind::Var(idx) => Some(*idx),
            ExprKind::Ident(name) => (self.top()..self.sym_tab.names.len()).rev().find(|idx| self.sym_tab.names[*idx].eq(name)).map(|idx| (idx - self.top()) as u32),
            _ => None,
        }
    }

    fn infer_list_method(&mut self, target: &Expr, elem_ty: &Type, method: &str, params: &[Expr]) -> Result<Option<Type>> {
        match method {
            "get_idx" | "pop" => Ok(Some(match self.local_var_idx_for_expr(target).and_then(|idx| self.list_elem_state(idx)) {
                Some(ListElemState::Known(ty)) => ty,
                Some(ListElemState::Unknown | ListElemState::Mixed) => Type::Any,
                None => elem_ty.clone(),
            })),
            "push" => {
                let pushed_ty = params
                    .first()
                    .map(|param| {
                        if let Some(value) = self.get_value(param)
                            && (value.is_str() || value.is_native())
                        {
                            Ok(value.get_type())
                        } else {
                            self.infer_expr(param)
                        }
                    })
                    .transpose()?
                    .unwrap_or(Type::Any);
                if let Some(idx) = self.local_var_idx_for_expr(target) {
                    let state = self.list_elem_state(idx).unwrap_or_else(|| if elem_ty.is_any() { ListElemState::Unknown } else { ListElemState::Known(elem_ty.clone()) });
                    let next_state = match state {
                        ListElemState::Unknown if pushed_ty.is_any() => ListElemState::Mixed,
                        ListElemState::Unknown => ListElemState::Known(pushed_ty),
                        ListElemState::Known(_) if pushed_ty.is_any() => ListElemState::Mixed,
                        ListElemState::Known(prev) => {
                            let merged = if prev == pushed_ty {
                                prev
                            } else if (prev.is_int() || prev.is_uint() || prev.is_float()) && (pushed_ty.is_int() || pushed_ty.is_uint() || pushed_ty.is_float()) {
                                prev + pushed_ty
                            } else {
                                Type::Any
                            };
                            if merged.is_any() { ListElemState::Mixed } else { ListElemState::Known(merged) }
                        }
                        ListElemState::Mixed => ListElemState::Mixed,
                    };
                    let next_elem = if let ListElemState::Known(ty) = &next_state { ty.clone() } else { Type::Any };
                    self.set_ty(idx, Type::List(std::rc::Rc::new(next_elem)));
                    self.set_list_elem_state(idx, Some(next_state));
                }
                Ok(Some(Type::Void))
            }
            "len" => Ok(Some(Type::I32)),
            "contains" | "is_list" | "is_null" => Ok(Some(Type::Bool)),
            _ => Ok(None),
        }
    }

    fn infer_return_expr(&mut self, expr: &Expr) -> Result<ReturnInfo> {
        let ty = self.infer_expr(expr)?;
        let shape = self.return_shape(expr, &ty);
        let ty = if matches!(shape, Some(Type::Map | Type::List(_))) { Type::Any } else { ty };
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
                        self.update_pending_return_seed(&info.ty);
                        ret = Some(Self::merge_return_info(stmt.span, ret, info)?);
                        if let Some(ret) = &ret {
                            self.update_pending_return_seed(&ret.ty);
                        }
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
                if let Some(ret) = &ret {
                    self.update_pending_return_seed(&ret.ty);
                }
                let else_returns = if let Some(body) = else_body {
                    let (else_ty, else_returns) = self.infer_returns(body, tail)?;
                    if let Some(info) = else_ty {
                        self.update_pending_return_seed(&info.ty);
                        ret = Some(Self::merge_return_info(body.span, ret, info)?);
                        if let Some(ret) = &ret {
                            self.update_pending_return_seed(&ret.ty);
                        }
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
            ExprKind::Value(v) if v.is_list() => Ok(v.get_type()),
            ExprKind::Value(v) if v.is_map() => Ok(Type::Any),
            ExprKind::Value(v) => Ok(v.get_type()),
            ExprKind::Const(idx) => Ok(match self.sym_tab.consts.get_index(*idx) {
                Some((_, value)) if value.is_str() => Type::Str,
                Some((_, value)) if value.is_list() && value.len() == 0 => Type::list_any(),
                _ => Type::Any,
            }),
            ExprKind::Var(idx) => {
                let idx = self.top() + (*idx as usize);
                if idx < self.sym_tab.tys.len() { self.sym_tab.symbols.get_type(&self.sym_tab.tys[idx]) } else { Ok(Type::Any) }
            }
            ExprKind::Ident(ident) => {
                for idx in (self.top()..self.sym_tab.names.len()).rev() {
                    if self.sym_tab.names[idx].eq(ident) && idx < self.sym_tab.tys.len() {
                        return self.sym_tab.symbols.get_type(&self.sym_tab.tys[idx]);
                    }
                }
                let id = self.sym_tab.symbols.get_id(ident).map_err(|_| Self::semantic_error(expr.span, format!("未找到标识符 {}", ident)))?;
                match self.sym_tab.symbols.get_symbol(id)?.1 {
                    Symbol::Const { ty, .. } => Ok(ty.clone()),
                    Symbol::Static { ty, .. } => Ok(ty.clone()),
                    Symbol::Struct(ty, _) => Ok(ty.clone()),
                    Symbol::Fn { .. } => Ok(Type::Symbol { id, params: Vec::new() }),
                    Symbol::Native(ty) => Ok(ty.clone()),
                    s => Err(Self::semantic_error(expr.span, format!("符号 {:?} 不是变量、常量、静态变量、结构体", s))),
                }
            }
            ExprKind::Id(id, _) => match self.sym_tab.symbols.get_symbol(*id)?.1 {
                Symbol::Const { ty, .. } => Ok(ty.clone()),
                Symbol::Static { ty, .. } => Ok(ty.clone()),
                Symbol::Struct(ty, _) => Ok(ty.clone()),
                Symbol::Fn { .. } => Ok(Type::Symbol { id: *id, params: Vec::new() }),
                Symbol::Native(ty) => Ok(ty.clone()),
                s => Err(Self::semantic_error(expr.span, format!("符号 {:?} 不是变量、常量、静态变量、结构体", s))),
            },
            ExprKind::Generic { obj, params } => {
                let params = params.iter().map(|param| self.sym_tab.symbols.get_type(param).unwrap_or_else(|_| param.clone())).collect();
                match self.infer_expr(obj)? {
                    Type::Symbol { id, .. } => Ok(Type::Symbol { id, params }),
                    _ => Ok(Type::Any),
                }
            }
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
                if op == &BinaryOp::Assign
                    && let ExprKind::Tuple(left_items) | ExprKind::List(left_items) = &left.kind
                {
                    if let ExprKind::Tuple(right_items) | ExprKind::List(right_items) = &right.kind {
                        if left_items.len() != right_items.len() {
                            return Err(Self::semantic_error(expr.span, format!("多重赋值数量不匹配: 左侧 {} 个，右侧 {} 个", left_items.len(), right_items.len())));
                        }
                        for item in right_items {
                            let _ = self.infer_expr(item)?;
                        }
                    } else {
                        let _ = self.infer_expr(right)?;
                    }
                    return Ok(Type::Void);
                }
                let assign_idx = if op.is_assign() { if let ExprKind::Var(idx) = &left.kind { Some(*idx) } else { None } } else { None };
                let ty = if op.is_logic() {
                    Type::Bool
                } else if op == &BinaryOp::Idx {
                    let left_ty = self.infer_expr(left)?;
                    if matches!(right.kind, ExprKind::Range { .. }) {
                        // 切片 `arr[a..b]` 仍产生同元素类型的 list,
                        // 长度未定所以固定返回 `Type::List(elem)`。
                        let elem_ty = match &left_ty {
                            Type::Array(e, _) | Type::Vec(e, _) | Type::List(e) => (**e).clone(),
                            _ => Type::Any,
                        };
                        return Ok(Type::List(std::rc::Rc::new(elem_ty)));
                    }
                    if let Type::Array(elem_ty, _) = left_ty {
                        (*elem_ty).clone()
                    } else if let Type::Vec(elem_ty, _) = left_ty {
                        (*elem_ty).clone()
                    } else if let Type::List(elem_ty) = left_ty {
                        (*elem_ty).clone()
                    } else {
                        let left_ty = self.sym_tab.symbols.get_type(&left_ty)?;
                        let right_ty = if right.is_value() || right.is_const() {
                            let right_value = if let ExprKind::Const(c) = &right.kind {
                                match self.sym_tab.consts.get_index(*c) {
                                    Some((_, v)) => v.clone(),
                                    None => right.clone().value()?,
                                }
                            } else {
                                right.clone().value()?
                            };
                            if right_value.is_str() {
                                if left_ty.is_any() {
                                    return Ok(Type::Any);
                                }
                                if let Ok(field) = self.sym_tab.symbols.get_field(&left_ty, right_value.as_str()) {
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
                            let (_, s) = self.sym_tab.symbols.get_field(&left_ty, "get_idx")?;
                            let fn_ty = self.sym_tab.symbols.get_type(&s)?;
                            return if let Type::Fn { ret, .. } = &fn_ty { Ok(ret.as_ref().clone()) } else { Ok(fn_ty) };
                        }
                        if left_ty.is_any() {
                            return Ok(Type::Any);
                        }
                        Type::Any
                    }
                } else {
                    let left_ty = self.infer_expr(left)?;
                    let right_ty = self.infer_expr(right)?;
                    Self::check_arith_types(op, &left_ty, &right_ty, expr.span)?;
                    if op == &BinaryOp::Assign {
                        if !left_ty.is_any() && right_ty.is_any() { left_ty } else { right_ty }
                    } else if op.is_assign() && !left_ty.is_any() && right_ty.is_any() {
                        left_ty
                    } else {
                        left_ty + right_ty
                    }
                };
                assign_idx.map(|idx| self.set_ty(idx, ty.clone()));
                Ok(ty)
            }
            ExprKind::Call { obj, params } => {
                if let ExprKind::Assoc { ty, name } = &obj.kind {
                    let base_name = match ty {
                        Type::Ident { name, .. } => name.clone(),
                        Type::Symbol { id, .. } => self.sym_tab.symbols.get_symbol(*id)?.0.clone(),
                        _ => return Ok(Type::Any),
                    };
                    let id = self.sym_tab.symbols.get_id(&format!("{}::{}", base_name, name))?;
                    let generic_args = match ty {
                        Type::Ident { params, .. } | Type::Symbol { params, .. } => params.iter().map(|param| self.sym_tab.symbols.get_type(param).unwrap_or_else(|_| param.clone())).collect::<Vec<_>>(),
                        _ => Vec::new(),
                    };
                    let mut args = Vec::new();
                    for p in params {
                        args.push(self.infer_expr(p)?);
                    }
                    self.infer_fn_with_params(id, &args, &generic_args)
                } else if let ExprKind::AssocId { id, params: generic_args } = &obj.kind {
                    let mut args = Vec::new();
                    for p in params {
                        args.push(self.infer_expr(p)?);
                    }
                    self.infer_fn_with_params(*id, &args, generic_args)
                } else if let ExprKind::Generic { obj, params: generic_args } = &obj.kind {
                    let Type::Symbol { id, .. } = self.infer_expr(obj)? else {
                        return Ok(Type::Any);
                    };
                    let generic_args = generic_args.iter().map(|param| self.sym_tab.symbols.get_type(param).unwrap_or_else(|_| param.clone())).collect::<Vec<_>>();
                    let mut args = Vec::new();
                    for p in params {
                        args.push(self.infer_expr(p)?);
                    }
                    self.infer_fn_with_params(id, &args, &generic_args)
                } else if let ExprKind::TypedMethod { obj: target, ty, name } = &obj.kind {
                    let base_name = match ty {
                        Type::Ident { name, .. } => name.clone(),
                        Type::Symbol { id, .. } => self.sym_tab.symbols.get_symbol(*id)?.0.clone(),
                        _ => return Ok(Type::Any),
                    };
                    let id = self.sym_tab.symbols.get_id(&format!("{}::{}", base_name, name))?;
                    let mut args = vec![self.infer_expr(target)?];
                    for p in params {
                        args.push(self.infer_expr(p)?);
                    }
                    self.infer_fn(id, &args)
                } else if let ExprKind::Id(id, obj_expr) = &obj.kind {
                    let method = self.sym_tab.symbols.get_symbol(*id).ok().and_then(|(name, _)| name.rsplit_once("::").map(|(_, method)| method.to_string()));
                    if let Some(target) = obj_expr
                        && let Some(method) = method
                    {
                        let target_ty = self.infer_expr(target)?;
                        if let Type::List(elem_ty) | Type::Array(elem_ty, _) = &target_ty
                            && let Some(ret_ty) = self.infer_list_method(target, elem_ty, method.as_str(), params)?
                        {
                            return Ok(ret_ty);
                        }
                    }
                    let mut args: Vec<Type> = if let Some(obj) = obj_expr { vec![self.infer_expr(obj)?] } else { Vec::new() };
                    for p in params {
                        args.push(self.infer_expr(p)?);
                    }
                    self.infer_fn(*id, &args)
                } else if let ExprKind::Ident(name) = &obj.kind {
                    for idx in (self.top()..self.sym_tab.names.len()).rev() {
                        if self.sym_tab.names[idx].eq(name) && idx < self.sym_tab.tys.len() {
                            return if let Type::Symbol { id, .. } = &self.sym_tab.tys[idx] {
                                let id = *id;
                                let mut args = Vec::new();
                                for p in params {
                                    args.push(self.infer_expr(p)?);
                                }
                                self.infer_fn(id, &args)
                            } else {
                                Ok(Type::Any)
                            };
                        }
                    }
                    let Ok(id) = self.sym_tab.symbols.get_id(name) else {
                        return Ok(Type::Any);
                    };
                    if !self.sym_tab.symbols.get_symbol(id)?.1.is_fn() {
                        return Err(Self::semantic_error(obj.span, format!("符号 {} 不是函数", name)));
                    }
                    let mut args = Vec::new();
                    for p in params {
                        args.push(self.infer_expr(p)?);
                    }
                    self.infer_fn(id, &args)
                } else if obj.is_idx() {
                    let (target, _, method) = obj.clone().binary().unwrap();
                    let ty = self.infer_expr(&target)?;
                    if let Some(method) = self.get_value(&method) {
                        let method = method.as_str();
                        if let Type::List(elem_ty) | Type::Array(elem_ty, _) = &ty
                            && let Some(ret_ty) = self.infer_list_method(&target, elem_ty, method, params)?
                        {
                            return Ok(ret_ty);
                        }
                        let fn_ty = match self.get_field(&ty, method) {
                            Ok((_, fn_ty)) => fn_ty,
                            Err(_) => {
                                let id = self.sym_tab.symbols.get_id(method)?;
                                if self.sym_tab.symbols.get_symbol(id)?.1.is_fn() {
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
                    if idx < self.sym_tab.tys.len()
                        && let Type::Symbol { id, .. } = self.sym_tab.tys[idx]
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
            ExprKind::Typed { ty, .. } => self.sym_tab.symbols.get_type(ty),
            ExprKind::Stmt(stmt) => self.infer_stmt(stmt),
            ExprKind::Repeat { value, len } => {
                let value_ty = self.infer_expr(value)?;
                let len = self.sym_tab.symbols.get_type(len).unwrap_or_else(|_| len.clone());
                if let Type::ConstInt(len) = len {
                    let len = u32::try_from(len).map_err(|_| Self::semantic_error(expr.span, "重复数组长度必须是非负 u32"))?;
                    Ok(Type::Array(std::rc::Rc::new(value_ty), len))
                } else {
                    Ok(Type::ArrayParam(std::rc::Rc::new(value_ty), std::rc::Rc::new(len)))
                }
            }
            ExprKind::List(items) => {
                if items.is_empty() {
                    return Ok(Type::list_any());
                }
                let mut elem_ty = Type::Any;
                for item in items {
                    let item_ty = self.infer_expr(item)?;
                    elem_ty = if elem_ty.is_any() { item_ty } else { elem_ty + item_ty };
                }
                Ok(Type::Array(std::rc::Rc::new(elem_ty), items.len() as u32))
            }
            ExprKind::Range { start, stop, .. } => {
                let start_ty = self.infer_expr(start)?;
                let stop_ty = self.infer_expr(stop)?;
                Ok(Self::merge_range_bound_types(start_ty, stop_ty))
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
                fn_tys.push(self.sym_tab.symbols.get_type(arg_ty)?);
            } else {
                fn_tys.push(Type::Any);
            }
        }
        Ok(fn_tys)
    }

    fn is_optimizable_local_ty(ty: &Type) -> bool {
        ty.is_bool() || ty.is_native()
    }

    fn is_optimizable_list_elem_ty(ty: &Type) -> bool {
        matches!(ty, Type::Bool | Type::U8 | Type::I8 | Type::U16 | Type::I16 | Type::U32 | Type::I32 | Type::F32 | Type::U64 | Type::I64 | Type::F64 | Type::Str)
    }

    fn local_type_hint_at(&self, pos: usize) -> Option<Type> {
        let ty = self.sym_tab.tys.get(pos)?;
        match ty {
            Type::List(_) => self.type_ctx.list_elem_states.get(pos).cloned().flatten().and_then(|state| {
                if let ListElemState::Known(elem_ty) = state
                    && Self::is_optimizable_list_elem_ty(&elem_ty)
                {
                    Some(Type::List(std::rc::Rc::new(elem_ty)))
                } else {
                    None
                }
            }),
            ty if Self::is_optimizable_local_ty(ty) => Some(ty.clone()),
            _ => None,
        }
    }

    fn collect_local_type_hints(&self) -> Vec<Option<Type>> {
        (self.top()..self.sym_tab.tys.len()).map(|pos| self.local_type_hint_at(pos)).collect()
    }

    fn set_local_type_hints(&mut self, id: u32, generic_args: &[Type], fn_tys: &[Type], hints: Vec<Option<Type>>) {
        let items = self.type_ctx.local_type_hints.entry(id).or_default();
        if let Some(item) = items.iter_mut().find(|item| item.0 == generic_args && item.1 == fn_tys) {
            item.2 = hints;
        } else {
            items.push((generic_args.to_vec(), fn_tys.to_vec(), hints));
        }
    }

    pub fn inferred_local_type_hints(&self, id: u32, generic_args: &[Type], fn_tys: &[Type]) -> Vec<Option<Type>> {
        self.type_ctx.local_type_hints.get(&id).and_then(|items| items.iter().find(|item| item.0 == generic_args && item.1 == fn_tys)).map(|item| item.2.clone()).unwrap_or_default()
    }

    pub fn infer_fn(&mut self, id: u32, arg_tys: &[Type]) -> Result<Type> {
        self.infer_fn_with_params(id, arg_tys, &[])
    }

    pub fn infer_fn_with_params(&mut self, id: u32, arg_tys: &[Type], generic_args: &[Type]) -> Result<Type> {
        // 病态(互)递归泛型推断会不断产生新实例化、绕过记忆化;到达深度上限即回退 Any,
        // 避免推断阶段栈溢出崩溃。
        if self.type_ctx.infer_stack.len() > MAX_INFER_DEPTH {
            return Ok(Type::Any);
        }
        let (name, s) = self.sym_tab.symbols.get_symbol(id).map(|(n, s)| (n.clone(), s.clone()))?;
        if let Symbol::Fn { ty, args, generic_params, cap, body, .. } = s {
            if let Type::Fn { tys, ret: _ } = ty {
                let resolved_generic_args = crate::resolve_generic_args_from_types(&generic_params, &tys, arg_tys, generic_args)?;
                let generic_args = resolved_generic_args.as_slice();
                let tys = if generic_params.is_empty() { tys } else { tys.iter().map(|ty| crate::substitute_type(ty, &generic_params, generic_args)).collect() };
                let body = if generic_params.is_empty() { body.as_ref().clone() } else { crate::substitute_stmt(body.as_ref(), &generic_params, generic_args) };
                let fn_tys = self.get_fn_tys(&tys, arg_tys)?;
                let body = if generic_params.is_empty() {
                    body
                } else {
                    let mut compile_tys = tys.clone();
                    let mut compile_cap = cap.clone();
                    let saved_state = self.take_local_state();
                    if let Some((module, _)) = name.split_once("::") {
                        self.sym_tab.symbols.push_module_scope(module.into());
                    }
                    let compiled = self.compile_fn(&args, &mut compile_tys, body, &mut compile_cap);
                    if name.contains("::") {
                        self.sym_tab.symbols.pop_module_scope();
                    }
                    self.restore_local_state(saved_state);
                    Stmt::new(StmtKind::Block(compiled?), Span::default())
                };
                if let Some(fns) = self.type_ctx.fns.get_mut(&id) {
                    for f in fns.iter() {
                        if f.0 == generic_args && f.1 == fn_tys {
                            return match &f.2 {
                                FnInferRet::Done(ret_ty) => self.sym_tab.symbols.get_type(ret_ty),
                                FnInferRet::Pending(seed) => seed.as_ref().map(|ty| self.sym_tab.symbols.get_type(ty)).unwrap_or_else(|| {
                                    // 递归自调用且种子为空：尝试从函数体 base case 查找返回类型
                                    if self.type_ctx.infer_stack.iter().any(|(sid, sargs, _)| *sid == id && sargs == generic_args) {
                                        if let Some(base_ty) = self.try_find_base_return_ty(&body) {
                                            return self.sym_tab.symbols.get_type(&base_ty);
                                        }
                                    }
                                    Ok(Type::Any)
                                }),
                            };
                        }
                    }
                    fns.push((generic_args.to_vec(), fn_tys.clone(), FnInferRet::Pending(None)));
                } else {
                    self.type_ctx.fns.insert(id, vec![(generic_args.to_vec(), fn_tys.clone(), FnInferRet::Pending(None))]);
                }
                // 递归函数：预扫描 base case 返回类型作为种子
                if self.pending_return_seed(id, generic_args, &fn_tys).is_none() {
                    if let Some(base_ty) = self.try_find_base_return_ty_with_scope(&body, id, &name, &args, &fn_tys) {
                        if let Some(fns) = self.type_ctx.fns.get_mut(&id) {
                            if let Some(item) = fns.iter_mut().find(|item| item.0 == generic_args && item.1 == fn_tys)
                                && let FnInferRet::Pending(seed) = &mut item.2
                                && seed.is_none()
                            {
                                *seed = Some(base_ty);
                            }
                        }
                    }
                }
                let mut ret_ty = None;
                let mut local_type_hints = Vec::new();
                for _ in 0..4 {
                    let before_seed = self.pending_return_seed(id, generic_args, &fn_tys);
                    let saved_state = self.take_local_state();
                    self.sym_tab.frames.push(0);
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
                    self.type_ctx.infer_stack.push((id, generic_args.to_vec(), fn_tys.clone()));
                    let pass_ret_ty = self.infer_return_type(&body).map(|ty| ty.unwrap_or(Type::Void));
                    self.type_ctx.infer_stack.pop();
                    let pass_local_type_hints = self.collect_local_type_hints();
                    self.restore_local_state(saved_state);
                    let pass_ret_ty = match pass_ret_ty {
                        Ok(pass_ret_ty) => self.sym_tab.symbols.get_type(&pass_ret_ty).unwrap_or(pass_ret_ty),
                        Err(err) => {
                            log::error!("infer_fn {} failed: {:?}", name, err);
                            let should_remove = self
                                .type_ctx
                                .fns
                                .get_mut(&id)
                                .map(|fns| {
                                    fns.retain(|item| item.0 != generic_args || item.1 != fn_tys || !matches!(item.2, FnInferRet::Pending(_)));
                                    fns.is_empty()
                                })
                                .unwrap_or(false);
                            if should_remove {
                                self.type_ctx.fns.remove(&id);
                            }
                            return Err(err);
                        }
                    };
                    if !pass_ret_ty.is_any() {
                        self.update_pending_return_seed(&pass_ret_ty);
                        ret_ty = Some(pass_ret_ty.clone());
                    } else if ret_ty.is_none() {
                        ret_ty = Some(pass_ret_ty);
                    }
                    local_type_hints = pass_local_type_hints;
                    let after_seed = self.pending_return_seed(id, generic_args, &fn_tys);
                    if before_seed == after_seed {
                        break;
                    }
                }
                let ret_ty = ret_ty.unwrap_or(Type::Any);
                self.type_ctx.fns.get_mut(&id).map(|f| {
                    f.iter_mut().find(|item| item.0 == generic_args && item.1 == fn_tys).map(|item| item.2 = FnInferRet::Done(ret_ty.clone()));
                });
                self.set_local_type_hints(id, generic_args, &fn_tys, local_type_hints);
                if generic_args.is_empty()
                    && let Some((_, Symbol::Fn { ty: Type::Fn { ret, .. }, .. })) = self.sym_tab.symbols.get_symbol_mut(id)
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
                        log::debug!("then 和 else 有不同类型 {:?} {:?}", then_ty, else_ty);
                        return Self::merge_return_type(stmt.span, Some(then_ty), else_ty);
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
