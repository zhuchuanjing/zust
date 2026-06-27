use crate::try_parse;
use dynamic::{Dynamic, Type};

use super::{Expr, Parser, Pattern, Span, expr::ExprKind, pattern::PatternKind};
use anyhow::{Result, anyhow};
use smol_str::SmolStr;

/// 把 pattern 引入的所有变量绑定生成出来,追加到 `out`。
/// `access` 是该 pattern 整体对应的访问表达式(如 `__m_scrut` 或 `__m_scrut.field`)。
fn collect_bindings(pat: &Pattern, access: &Expr, out: &mut Vec<Stmt>, span: Span) {
    use crate::expr::BinaryOp;
    let mk_let_ident = |name: SmolStr, value: Expr, span: Span| {
        let p = Pattern::new(PatternKind::Ident { name, ty: Type::Any }, span);
        let value_stmt = Stmt::new(StmtKind::Expr(value, true), span);
        Stmt::new(StmtKind::Let { pat: p, value: Box::new(value_stmt) }, span)
    };
    let mk_bin = |left: Expr, op: BinaryOp, right: Expr, span: Span| Expr::new(ExprKind::Binary { left: Box::new(left), op, right: Box::new(right) }, span);
    let mk_idx_int = |obj: Expr, i: i32, span: Span| mk_bin(obj, BinaryOp::Idx, Expr::new(ExprKind::Value(Dynamic::I32(i)), span), span);
    let mk_field = |obj: Expr, name: SmolStr, span: Span| mk_bin(obj, BinaryOp::Idx, Expr::new(ExprKind::Value(Dynamic::String(name)), span), span);

    match &pat.kind {
        PatternKind::Ident { name, .. } => {
            out.push(mk_let_ident(name.clone(), access.clone(), span));
        }
        PatternKind::Tuple(items) => {
            for (i, p) in items.iter().enumerate() {
                let elem = mk_idx_int(access.clone(), i as i32, span);
                collect_bindings(p, &elem, out, span);
            }
        }
        PatternKind::List { elems, has_rest } => {
            let prefix = if *has_rest { elems.len() - 1 } else { elems.len() };
            for (i, p) in elems.iter().take(prefix).enumerate() {
                let elem = mk_idx_int(access.clone(), i as i32, span);
                collect_bindings(p, &elem, out, span);
            }
            if *has_rest {
                if let PatternKind::Ident { name, .. } = &elems.last().unwrap().kind {
                    // 切片: access[prefix..];用 `ExprKind::Range` 而不是 `Binary{RangeOpen}`,
                    // 与 parser 产物一致,这样下游 type infer / 优化都走同一条路径。
                    let from = Expr::new(ExprKind::Value((prefix as u32).into()), span);
                    let range = Expr::new(ExprKind::Range { start: Box::new(from), stop: Box::new(Expr::new(ExprKind::Value(Dynamic::Null), span)), inclusive: false }, span);
                    let slice = Expr::new(ExprKind::Binary { left: Box::new(access.clone()), op: BinaryOp::Idx, right: Box::new(range) }, span);
                    out.push(mk_let_ident(name.clone(), slice, span));
                }
            }
        }
        PatternKind::Struct { fields, .. } => {
            for (fname, sub) in fields {
                let field_access = mk_field(access.clone(), fname.clone(), span);
                match sub {
                    None => {
                        // 简写 `field` —— 绑定同名变量
                        out.push(mk_let_ident(fname.clone(), field_access, span));
                    }
                    Some(sub_pat) => {
                        collect_bindings(sub_pat, &field_access, out, span);
                    }
                }
            }
        }
        // 其它(Wildcard / Literal / Var / Member / Idx)在 match 上下文中不产生绑定
        _ => {}
    }
}

#[derive(Debug, Clone)]
pub struct Stmt {
    pub kind: StmtKind,
    pub span: Span,
}

#[derive(Debug, Clone)]
pub enum StmtKind {
    Let { pat: Pattern, value: Box<Stmt> },
    Expr(Expr, bool),
    Block(Vec<Stmt>),
    Break,
    Continue,
    Return(Option<Expr>),
    While { cond: Expr, body: Box<Stmt> },
    Loop(Box<Stmt>),
    For { pat: Pattern, range: Expr, body: Box<Stmt> },
    Fn { name: SmolStr, generic_params: Vec<Type>, args: Vec<(SmolStr, Type)>, body: Box<Stmt>, is_pub: bool },
    Struct { name: SmolStr, def: Type, is_pub: bool },
    Impl { target: Type, body: Box<Stmt> },
    If { cond: Expr, then_body: Box<Stmt>, else_body: Option<Box<Stmt>> },
    Static { name: SmolStr, ty: Type, value: Option<Expr>, is_pub: bool },
    Import { module: SmolStr, path: SmolStr, is_pub: bool },
    Const { name: SmolStr, ty: Type, value: Expr, is_pub: bool },
}

impl Stmt {
    pub fn new(kind: StmtKind, span: Span) -> Self {
        Self { kind, span }
    }

    pub fn expr(&self) -> Option<Expr> {
        if let StmtKind::Expr(expr, _) = &self.kind { Some(expr.clone()) } else { None }
    }

    pub fn is_return(&self) -> bool {
        matches!(self.kind, StmtKind::Return(_))
    }

    pub fn last_return(&mut self) -> bool {
        match &mut self.kind {
            StmtKind::Block(stmts) => stmts.last_mut().map(|stmt| stmt.last_return()).unwrap_or(false),
            StmtKind::If { then_body, else_body, .. } => {
                let then_returns = then_body.last_return();
                let else_returns = else_body.as_mut().map(|body| body.last_return()).unwrap_or(false);
                then_returns && else_returns
            }
            StmtKind::Expr(e, close) => {
                if !*close {
                    let span = e.span;
                    *self = Self::new(StmtKind::Return(Some(std::mem::take(e))), span);
                    true
                } else {
                    false
                }
            }
            StmtKind::Return(_) => true,
            _ => false,
        }
    }

    pub fn get_type(&self) -> Option<Type> {
        match &self.kind {
            StmtKind::Expr(expr, _) => Some(expr.get_type()),
            StmtKind::Block(stmts) => stmts.last().and_then(|stmt| stmt.get_type()),
            StmtKind::If { then_body, .. } => then_body.get_type(),
            _ => None,
        }
    }

    fn get_assign(idx: u32, expr: Expr) -> Self {
        let span = expr.span;
        Self::new(StmtKind::Expr(Expr::new(ExprKind::Binary { left: Box::new(Expr::new(ExprKind::Var(idx), span)), op: crate::BinaryOp::Assign, right: Box::new(expr) }, span), true), span)
    }

    fn get_idx_assign(pat: Expr, idx: usize, expr: Expr) -> Self {
        let span = pat.span.merge(expr.span);
        let right = Expr::new(ExprKind::Binary { left: Box::new(expr), op: crate::BinaryOp::Idx, right: Box::new(Expr::new(ExprKind::Value((idx as u32).into()), span)) }, span);
        Self::new(StmtKind::Expr(Expr::new(ExprKind::Binary { left: Box::new(pat), op: crate::BinaryOp::Assign, right: Box::new(right) }, span), true), span)
    }

    /// 与 `get_idx_assign` 类似,但把右侧索引读取包成 `Typed { value, ty }`。
    /// 解构生成的 `arr[i]` 默认返回 `Any`(指向 Dynamic 的指针),
    /// 不加类型标注的话元素槽会被声明成 Any,后续读取拿到的是裸指针而不是值。
    /// 这里按模式里推导出的元素类型 `ty` 包一层 `Typed`,让 JIT 走 Any→ty 的转换。
    fn get_idx_assign_typed(pat: Expr, idx: usize, expr: Expr, ty: Type) -> Self {
        let span = pat.span.merge(expr.span);
        let idx_expr = Expr::new(ExprKind::Binary { left: Box::new(expr), op: crate::BinaryOp::Idx, right: Box::new(Expr::new(ExprKind::Value((idx as u32).into()), span)) }, span);
        let right = Expr::new(ExprKind::Typed { value: Box::new(idx_expr), ty }, span);
        Self::new(StmtKind::Expr(Expr::new(ExprKind::Binary { left: Box::new(pat), op: crate::BinaryOp::Assign, right: Box::new(right) }, span), true), span)
    }

    fn get_assign_expr(pat: Expr, expr: Expr) -> Self {
        let span = pat.span.merge(expr.span);
        Self::new(StmtKind::Expr(Expr::new(ExprKind::Binary { left: Box::new(pat), op: crate::BinaryOp::Assign, right: Box::new(expr) }, span), true), span)
    }

    fn var_expr_with_ty(pat: &Pattern) -> Option<(Expr, Type)> {
        if let PatternKind::Var { idx, ty } = &pat.kind
            && !ty.is_any()
        {
            Some((Expr::new(ExprKind::Var(*idx), pat.span), ty.clone()))
        } else {
            None
        }
    }

    pub fn bind_pattern(&mut self, pat: Pattern) -> Result<()> {
        if let Some(expr) = self.expr() {
            let stmt = match pat.kind {
                PatternKind::Var { idx, ty } => {
                    if expr.get_type() != ty {
                        Self::get_assign(idx, Expr::new(ExprKind::Typed { value: Box::new(expr), ty }, pat.span))
                    } else {
                        Self::get_assign(idx, expr)
                    }
                }
                PatternKind::Tuple(list) => {
                    let mut stmts = Vec::new();
                    for (idx, p) in list.into_iter().enumerate() {
                        match Self::var_expr_with_ty(&p) {
                            Some((p, ty)) => stmts.push(Self::get_idx_assign_typed(p, idx, expr.clone(), ty)),
                            None => match p.expr() {
                                Ok(p) => stmts.push(Self::get_idx_assign(p, idx, expr.clone())),
                                Err(e) => return Err(e),
                            },
                        }
                    }
                    Self::new(StmtKind::Block(stmts), self.span)
                }
                PatternKind::List { elems, has_rest } => {
                    let mut stmts = Vec::new();
                    let prefix_count = if has_rest { elems.len() - 1 } else { elems.len() };
                    if has_rest {
                        for (idx, p) in elems.iter().take(prefix_count).enumerate() {
                            match Self::var_expr_with_ty(p) {
                                Some((p, ty)) => stmts.push(Self::get_idx_assign_typed(p, idx, expr.clone(), ty)),
                                None => match p.expr() {
                                    Ok(p) => stmts.push(Self::get_idx_assign(p, idx, expr.clone())),
                                    Err(e) => return Err(e),
                                },
                            }
                        }
                    } else {
                        for (idx, p) in elems.iter().enumerate() {
                            match Self::var_expr_with_ty(p) {
                                Some((p, ty)) => stmts.push(Self::get_idx_assign_typed(p, idx, expr.clone(), ty)),
                                None => match p.expr() {
                                    Ok(p) => stmts.push(Self::get_idx_assign(p, idx, expr.clone())),
                                    Err(e) => return Err(e),
                                },
                            }
                        }
                    }
                    if has_rest {
                        // 最后一个元素是 `..rest`,把它绑定为 expr[prefix_count..] 的切片。
                        let rest_pat = elems.last().unwrap();
                        let rest_expr = match &rest_pat.kind {
                            PatternKind::Ident { name, .. } => Expr::new(ExprKind::Ident(name.clone()), rest_pat.span),
                            PatternKind::Var { idx, .. } => Expr::new(ExprKind::Var(*idx), rest_pat.span),
                            _ => return Err(anyhow!("..rest 后的模式必须是标识符")),
                        };
                        let from = Expr::new(ExprKind::Value((prefix_count as u32).into()), rest_pat.span);
                        // 用 `ExprKind::Range` 与 parser 产物对齐,确保下游 type infer 走切片分支。
                        let range = Expr::new(ExprKind::Range { start: Box::new(from), stop: Box::new(Expr::new(ExprKind::Value(Dynamic::Null), rest_pat.span)), inclusive: false }, rest_pat.span);
                        let slice_idx = Expr::new(ExprKind::Binary { left: Box::new(expr.clone()), op: crate::BinaryOp::Idx, right: Box::new(range) }, rest_pat.span);
                        stmts.push(Self::get_assign_expr(rest_expr, slice_idx));
                    }
                    Self::new(StmtKind::Block(stmts), self.span)
                }
                p => return Err(anyhow!("不支持的模式绑定: {:?}", p)),
            };
            let _ = std::mem::replace(self, stmt);
        } else {
            match &mut self.kind {
                StmtKind::Block(stmts) => {
                    if let Some(stmt) = stmts.last_mut() {
                        stmt.bind_pattern(pat)?;
                    }
                }
                StmtKind::If { then_body, else_body, .. } => {
                    then_body.bind_pattern(pat.clone())?;
                    if let Some(e) = else_body {
                        e.bind_pattern(pat)?;
                    }
                }
                _ => {}
            }
        }
        Ok(())
    }
}

use std::fmt;
impl fmt::Display for Stmt {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.kind {
            StmtKind::Let { pat, value } => writeln!(f, "let {:?} = {}", pat, value)?,
            StmtKind::Block(stmts) => stmts.iter().for_each(|s| {
                let _ = write!(f, "{}", s);
            }),
            StmtKind::Expr(expr, close) => writeln!(f, "{:?}[{}]", expr, close)?,
            StmtKind::Break => writeln!(f, "break")?,
            StmtKind::Continue => writeln!(f, "continue")?,
            StmtKind::Return(r) => writeln!(f, "return {:?}", r)?,
            StmtKind::While { cond, body } => write!(f, "while {:?}\n{}", cond, body)?,
            StmtKind::Loop(body) => write!(f, "loop\n{}", body)?,
            StmtKind::For { pat, range, body } => writeln!(f, "for {:?} in {:?} \n{}", pat, range, body)?,
            StmtKind::If { cond, then_body, else_body } => {
                write!(f, "if {:?}\nthen-> {}\n", cond, then_body)?;
                if let Some(e) = else_body {
                    writeln!(f, "{}", e)?;
                }
            }
            StmtKind::Fn { name, generic_params, args, body, is_pub } => {
                let generic_suffix = if generic_params.is_empty() { String::new() } else { format!("<{:?}>", generic_params) };
                if *is_pub {
                    write!(f, "pub fn {:?}{} {:?}\n", name, generic_suffix, args)?
                } else {
                    write!(f, "fn {:?}{} {:?}\n", name, generic_suffix, args)?
                }
                write!(f, "{}", body)?;
            }
            StmtKind::Import { module, path, is_pub } => {
                if *is_pub {
                    writeln!(f, "pub import {:?}, {:?};", module, path)?
                } else {
                    writeln!(f, "import {:?}, {:?};", module, path)?
                }
            }
            _ => write!(f, "(todo display: {:?})", self.kind)?,
        }
        fmt::Result::Ok(())
    }
}

impl Parser {
    pub fn ident_typed(&mut self) -> Result<(SmolStr, Type)> {
        let name = self.ident()?;
        self.whitespace()?;
        if self.take(b':').is_ok() { Ok((name, self.get_type()?)) } else { Ok((name, Type::Any)) }
    }

    pub fn ident_generic(&mut self) -> Result<(SmolStr, Vec<Type>)> {
        self.whitespace()?;
        let name = self.ident()?;
        self.whitespace()?;
        let params = if self.get()? == b'<' {
            self.pos += 1;
            crate::parse_list!(self, Vec::new(), b'>', b',', self.get_type_param()?)
        } else {
            Vec::new()
        };
        Ok((name, params))
    }

    pub fn block(&mut self) -> Result<Stmt> {
        self.check_fatal()?;
        self.whitespace()?;
        let start = self.current_pos();
        if self.get()? == b'{' {
            self.pos += 1;
            self.enter_depth()?;
            self.push_decl_scope();
            let result = (|| -> Result<Stmt> {
                let body = crate::parse_list!(self, Vec::new(), b'}', 0, self.stmt(false)?);
                Ok(Stmt::new(StmtKind::Block(body), self.span_from(start)))
            })();
            self.pop_decl_scope();
            self.exit_depth();
            result
        } else {
            Err(anyhow!("not code block"))
        }
    }

    pub fn if_block(&mut self) -> Result<Stmt> {
        let start = self.spans.last().copied().unwrap_or_else(|| self.current_pos());
        let cond = self.get_expr_without_struct_literal()?;
        let then_body = Box::new(self.block()?);
        self.whitespace()?;
        let else_body = if self.keyword("else").is_ok() {
            self.whitespace()?;
            let body = if self.keyword("if").is_ok() { self.if_block()? } else { self.block()? };
            Some(Box::new(body))
        } else {
            None
        };
        Ok(Stmt::new(StmtKind::If { cond, then_body, else_body }, Span::new(start, self.current_pos())))
    }

    /// 解析 `match scrut { pat ("|" pat)* ("if" guard)? => body ","? ... }`,
    /// desugar 成 `Block + Let + If 链 + 临时变量`,返回顶层 `Block`(再由
    /// 调用方用 `ExprKind::Stmt` 包成表达式)。
    pub fn match_block(&mut self, start: usize) -> Result<Stmt> {
        use crate::expr::{BinaryOp, ExprKind, UnaryOp};
        use crate::pattern::{Pattern, PatternKind};

        // ---- 一、解析阶段 ----
        let scrut = self.get_expr_without_struct_literal()?;
        self.whitespace()?;
        self.take(b'{').map_err(|_| anyhow!("match 缺少 `{{`"))?;

        // 每个 arm 是 (pats: Vec<Pattern>, guard: Option<Expr>, body: Expr)
        let mut arms: Vec<(Vec<Pattern>, Option<Expr>, Expr)> = Vec::new();
        loop {
            self.whitespace()?;
            if self.take(b'}').is_ok() {
                break;
            }
            self.push_decl_scope();
            let arm_res: Result<(Vec<Pattern>, Option<Expr>, Expr)> = (|| {
                // 一组 pattern(用 `|` 分隔,or-pattern)
                let mut pats = vec![self.pattern()?];
                self.whitespace()?;
                while matches!(self.get(), Ok(b'|')) && !matches!(self.ahead(), Ok(b'|')) {
                    self.pos += 1;
                    self.whitespace()?;
                    pats.push(self.pattern()?);
                    self.whitespace()?;
                }
                // 声明 pattern 引入的变量(对每个 alt 都声明,允许重名 → 由
                // declare_symbol_in_current_scope 容错;这里我们只用第一个 alt 的 binding)
                for pat in &pats[..1] {
                    self.declare_pattern_symbols(pat)?;
                }
                // 校验 or-pattern:第二及之后必须是 Literal(MVP 限制)
                if pats.len() > 1 {
                    for p in &pats[1..] {
                        if !matches!(p.kind, PatternKind::Literal(_) | PatternKind::Wildcard) {
                            return Err(anyhow!("or-pattern 中除第一个外只支持字面量 / 通配模式 (MVP 限制)"));
                        }
                    }
                }
                // 可选 guard
                self.whitespace()?;
                let guard = if self.keyword("if").is_ok() {
                    self.whitespace()?;
                    Some(self.get_expr_no_assign()?)
                } else {
                    None
                };
                // `=>`
                self.whitespace()?;
                self.just("=>").map_err(|_| anyhow!("match arm 缺少 `=>`"))?;
                self.whitespace()?;
                // 体
                let body_start = self.current_pos();
                let body = if self.get()? == b'{' {
                    let block_stmt = self.block()?;
                    Expr::new(ExprKind::Stmt(Box::new(block_stmt)), Span::new(body_start, self.current_pos()))
                } else {
                    // body 也用 no_assign 防止 `=> body, ...` 中 `,` 后或下一 arm
                    // 的 `=>` 被错误吞入(同时仍允许 +-*/ 等正常二元)。
                    self.get_expr_no_assign()?
                };
                // 容忍尾随逗号
                self.whitespace()?;
                let _ = self.take(b',');
                Ok((pats, guard, body))
            })();
            self.pop_decl_scope();
            arms.push(arm_res?);
        }
        let span = Span::new(start, self.current_pos());

        // ---- 二、声明 match 顶层临时变量 ----
        // 为避免嵌套 match 重名,加 counter 后缀(简单递增,parser 本地状态)
        let suffix = self.match_counter;
        self.match_counter += 1;
        let scrut_name: SmolStr = format!("__m_scrut_{}", suffix).into();
        let done_name: SmolStr = format!("__m_done_{}", suffix).into();
        let out_name: SmolStr = format!("__m_out_{}", suffix).into();
        self.declare_symbol_in_current_scope(&scrut_name)?;
        self.declare_symbol_in_current_scope(&done_name)?;
        self.declare_symbol_in_current_scope(&out_name)?;

        // ---- 三、构建顶层 Block ----
        let mk_ident = |name: &SmolStr, span: Span| Expr::new(ExprKind::Ident(name.clone()), span);
        let mk_value = |v: Dynamic, span: Span| Expr::new(ExprKind::Value(v), span);
        let mk_binary = |left: Expr, op: BinaryOp, right: Expr, span: Span| Expr::new(ExprKind::Binary { left: Box::new(left), op, right: Box::new(right) }, span);
        let mk_let = |name: SmolStr, value: Expr, span: Span| {
            let pat = Pattern::new(PatternKind::Ident { name, ty: Type::Any }, span);
            let value_stmt = Stmt::new(StmtKind::Expr(value, true), span);
            Stmt::new(StmtKind::Let { pat, value: Box::new(value_stmt) }, span)
        };
        let mk_assign_stmt = |name: &SmolStr, value: Expr, span: Span| {
            let assign = mk_binary(mk_ident(name, span), BinaryOp::Assign, value, span);
            Stmt::new(StmtKind::Expr(assign, true), span)
        };
        let mk_any = |value: Expr, span: Span| Expr::new(ExprKind::Typed { value: Box::new(value), ty: Type::Any }, span);

        let mut stmts = Vec::new();
        // let __m_scrut = SCRUT;
        stmts.push(mk_let(scrut_name.clone(), scrut, span));
        // let __m_done = false;
        stmts.push(mk_let(done_name.clone(), mk_value(Dynamic::Bool(false), span), span));
        // let __m_out = null;
        stmts.push(mk_let(out_name.clone(), mk_value(Dynamic::Null, span), span));

        // 每个 arm 一个 if(顺序构造)
        for (pats, guard, body) in arms {
            let arm_span = body.span;
            let scrut_ident = mk_ident(&scrut_name, arm_span);
            // test = pat[0].test || pat[1].test || ...
            let mut test_chain = pats[0].match_test(&scrut_ident);
            for p in &pats[1..] {
                let t = p.match_test(&scrut_ident);
                test_chain = mk_binary(test_chain, BinaryOp::Or, t, arm_span);
            }
            // !__m_done && (test_chain)
            let not_done = Expr::new(ExprKind::Unary { op: UnaryOp::Not, value: Box::new(mk_ident(&done_name, arm_span)) }, arm_span);
            let outer_cond = mk_binary(not_done, BinaryOp::And, test_chain, arm_span);

            // arm body block:bind + guard + assign + done
            let mut body_stmts = Vec::new();
            collect_bindings(&pats[0], &mk_ident(&scrut_name, arm_span), &mut body_stmts, arm_span);
            // 内层 body:__m_out = BODY; __m_done = true;
            let inner_stmts = vec![mk_assign_stmt(&out_name, mk_any(body, arm_span), arm_span), mk_assign_stmt(&done_name, mk_value(Dynamic::Bool(true), arm_span), arm_span)];
            let inner_block = Stmt::new(StmtKind::Block(inner_stmts), arm_span);
            // 如果有 guard,再裹一层 if;否则直接展开
            if let Some(g) = guard {
                body_stmts.push(Stmt::new(StmtKind::If { cond: g, then_body: Box::new(inner_block), else_body: None }, arm_span));
            } else {
                body_stmts.push(inner_block);
            }
            let then_body = Stmt::new(StmtKind::Block(body_stmts), arm_span);
            stmts.push(Stmt::new(StmtKind::If { cond: outer_cond, then_body: Box::new(then_body), else_body: None }, arm_span));
        }
        // 末尾求值:__m_out
        stmts.push(Stmt::new(StmtKind::Expr(mk_any(mk_ident(&out_name, span), span), false), span));

        Ok(Stmt::new(StmtKind::Block(stmts), span))
    }

    /// 在 stmt 顶层 peek 一下,判断接下来的 token 是不是 `import` 顶层
    /// 声明(`import "name";` 或 `import "name", "path";`)。区分点:
    /// 声明形式是 `import` 后跟空白+字符串字面量(`"`)或 ident;
    /// 函数调用形式是 `import(...)`,`import` 后紧跟 `(` —— 这种仍走
    /// 普通表达式路径(`import` ident + 调用)。`import` 关键字不进
    /// KEYWORDS 列表就是为了让函数调用形式继续 work。
    fn peek_import_top_level(&self) -> bool {
        let rest = &self.buf[self.pos..];
        let Some(after_kw) = rest.strip_prefix(b"import") else {
            return false;
        };
        // import 后必须是空白
        let Some(&first) = after_kw.first() else {
            return false;
        };
        if !first.is_ascii_whitespace() {
            return false;
        }
        // 跳过空白, peek 第一个非空白字符
        let mut after_ws = after_kw.iter().skip_while(|b| b.is_ascii_whitespace());
        matches!(after_ws.next(), Some(b'"'))
    }

    pub fn stmt(&mut self, is_pub: bool) -> Result<Stmt> {
        self.check_fatal()?;
        self.whitespace()?;
        self.spans.push(self.pos);
        let start = self.current_pos();
        // 函数体内不允许 fn / struct / impl / const / static 顶层声明。
        // 编译器对这些位置直接 panic，这里前置到 parser，让错误落到用户可见的地方。
        if self.fn_body_depth > 0 {
            for kw in &["fn", "struct", "impl", "const", "static"] {
                if self.keyword(kw).is_ok() {
                    return Err(anyhow!("函数体内不能定义 {}；请移到顶层或改用闭包", kw));
                }
            }
        }
        // impl body 允许 fn(方法)和 pub fn,但拒绝嵌套 struct / impl / const / static。
        if self.impl_body_depth > 0 {
            for kw in &["struct", "impl", "const", "static"] {
                if self.keyword(kw).is_ok() {
                    return Err(anyhow!("impl 体内不能定义 {}；请移到顶层", kw));
                }
            }
        }
        let stmt = if self.peek_import_top_level() {
            // 顶层 import 声明:`import "module";` 或 `import "module", "path";`。
            // 必须用 `peek` 而不是 `keyword`,因为 `import` 在 HEAD 下不
            // 在 KEYWORDS 列表里 —— `import(...)` 函数调用形式仍要把
            // `import` 当 ident 通过表达式解析路径走通。
            // 区分要点:声明形式 `import` 后面是空白+字符串/ident;
            // 函数调用形式 `import` 后面是 `(`。只在 stmt 顶层做这个判断。
            if self.fn_body_depth > 0 || self.impl_body_depth > 0 {
                return Err(anyhow!("import 只能作为顶层声明；请写在模块顶层"));
            }
            self.just("import").map_err(|_| anyhow!("expected import"))?;
            self.whitespace()?;
            let module = match self.get_expr()?.kind {
                ExprKind::Value(Dynamic::String(value)) => value,
                ExprKind::Ident(value) => value,
                other => return Err(anyhow!("import 模块名必须是字符串或标识符，实际是 {:?}", other)),
            };
            self.whitespace()?;
            let path = if self.take(b',').is_ok() {
                self.whitespace()?;
                match self.get_expr()?.kind {
                    ExprKind::Value(Dynamic::String(value)) => value,
                    ExprKind::Ident(value) => value,
                    other => return Err(anyhow!("import 路径必须是字符串或标识符，实际是 {:?}", other)),
                }
            } else {
                format!("{module}.zs").into()
            };
            self.until(b';')?;
            Stmt::new(StmtKind::Import { module, path, is_pub }, Span::new(start, self.current_pos()))
        } else if self.keyword("let").is_ok() {
            let pat = self.pattern()?;
            self.declare_pattern_symbols(&pat)?;
            self.until(b'=')?;
            self.whitespace()?;
            let value = if self.get()? == b'{' {
                if self.looks_like_dict() {
                    self.get_expr()?
                } else {
                    // 块作为表达式:{ stmts; expr } 的值是最后一条语句的值。
                    let span = self.current_pos();
                    let block_stmt = self.block()?;
                    Expr::new(ExprKind::Stmt(Box::new(block_stmt)), Span::new(span, self.current_pos()))
                }
            } else {
                self.get_expr()?
            };
            self.whitespace()?;
            let close = self.take(b';').is_ok();
            let stmt = Stmt::new(StmtKind::Expr(value, close), Span::new(start, self.current_pos()));
            Stmt::new(StmtKind::Let { pat, value: Box::new(stmt) }, Span::new(start, self.current_pos()))
        } else if self.keyword("break").is_ok() {
            self.until(b';')?;
            Stmt::new(StmtKind::Break, Span::new(start, self.current_pos()))
        } else if self.keyword("continue").is_ok() {
            self.until(b';')?;
            Stmt::new(StmtKind::Continue, Span::new(start, self.current_pos()))
        } else if self.keyword("return").is_ok() {
            self.whitespace()?;
            let expr = if matches!(self.get(), Ok(b';' | b'}')) { None } else { Some(self.get_expr()?) };
            self.whitespace()?;
            if self.take(b';').is_err() && !matches!(self.get(), Ok(b'}')) {
                self.until(b';')?;
            }
            Stmt::new(StmtKind::Return(expr), Span::new(start, self.current_pos()))
        } else if self.keyword("if").is_ok() {
            self.if_block()?
        } else if self.keyword("loop").is_ok() {
            Stmt::new(StmtKind::Loop(Box::new(self.block()?)), Span::new(start, self.current_pos()))
        } else if self.keyword("while").is_ok() {
            self.whitespace()?;
            let cond = self.get_expr()?;
            let body = Box::new(self.block()?);
            Stmt::new(StmtKind::While { cond, body }, Span::new(start, self.current_pos()))
        } else if self.keyword("for").is_ok() {
            self.whitespace()?;
            let pat = self.pattern()?;
            self.whitespace()?;
            self.keyword("in")?;
            self.whitespace()?;
            let range = self.get_expr()?;
            self.push_decl_scope();
            let result: Result<Stmt> = (|| {
                self.declare_pattern_symbols(&pat)?;
                let body = Box::new(self.block()?);
                Ok(Stmt::new(StmtKind::For { pat, range, body }, Span::new(start, self.current_pos())))
            })();
            self.pop_decl_scope();
            result?
        } else if self.keyword("fn").is_ok() {
            self.whitespace()?;
            let (name, generic_params) = self.ident_generic()?;
            self.declare_function_name(&name)?;
            self.until(b'(')?;
            let args = crate::parse_list!(self, Vec::new(), b')', b',', self.ident_typed()?);
            let body = Box::new(self.function_body(&args)?);
            Stmt::new(StmtKind::Fn { name, generic_params, args, body, is_pub }, Span::new(start, self.current_pos()))
        } else if self.keyword("struct").is_ok() {
            let (name, params) = self.ident_generic()?;
            self.declare_symbol(&name)?;
            if self.until(b'{').is_ok() {
                let fields = crate::parse_list!(self, Vec::new(), b'}', b',', self.ident_typed()?);
                if let Some(f) = fields.iter().find(|f| f.1.is_any()) {
                    return Err(anyhow!("字段 {} 的类型未知", f.0));
                }
                Stmt::new(StmtKind::Struct { name, def: Type::Struct { params, fields }, is_pub }, Span::new(start, self.current_pos()))
            } else {
                self.until(b';')?;
                Stmt::new(StmtKind::Struct { name, def: Type::Struct { params, fields: Vec::new() }, is_pub }, Span::new(start, self.current_pos()))
            }
        } else if self.keyword("const").is_ok() {
            self.whitespace()?;
            let (name, ty) = self.ident_typed()?;
            self.declare_symbol(&name)?;
            self.until(b'=')?;
            let value = self.get_expr()?;
            self.until(b';')?;
            Stmt::new(StmtKind::Const { name, ty, value, is_pub }, Span::new(start, self.current_pos()))
        } else if self.keyword("static").is_ok() {
            self.whitespace()?;
            let (name, ty) = self.ident_typed()?;
            self.declare_symbol(&name)?;
            self.whitespace()?;
            if self.take(b'=').is_ok() {
                let expr = self.get_expr()?;
                self.until(b';')?;
                Stmt::new(StmtKind::Static { name, ty, value: Some(expr), is_pub }, Span::new(start, self.current_pos()))
            } else {
                self.until(b';')?;
                Stmt::new(StmtKind::Static { name, ty, value: None, is_pub }, Span::new(start, self.current_pos()))
            }
        } else if self.keyword("impl").is_ok() {
            self.whitespace()?;
            let target = self.get_type()?;
            Stmt::new(StmtKind::Impl { target, body: Box::new(self.impl_body()?) }, Span::new(start, self.current_pos()))
        } else if self.keyword("pub").is_ok() {
            self.stmt(true)?
        } else {
            let expr = if self.get()? == b'{' {
                if self.looks_like_empty_dict() {
                    self.dict()?
                } else if let Ok(block) = try_parse!(self, self.block()) {
                    let _ = self.spans.pop();
                    return Ok(block);
                } else if let Ok(dict) = try_parse!(self, self.dict()) {
                    dict
                } else {
                    let block = self.block()?;
                    let _ = self.spans.pop();
                    return Ok(block);
                }
            } else {
                self.get_expr()?
            };
            self.whitespace()?;
            if self.is_eof() {
                Stmt::new(StmtKind::Expr(expr, false), Span::new(start, self.current_pos()))
            } else if self.get()? == b';' {
                self.pos += 1;
                Stmt::new(StmtKind::Expr(expr, true), Span::new(start, self.current_pos()))
            } else if self.get()? == b'}' {
                Stmt::new(StmtKind::Expr(expr, false), Span::new(start, self.current_pos()))
            } else {
                return Err(anyhow!("未结束的表达式"));
            }
        };
        let _ = self.spans.pop();
        Ok(stmt)
    }
}
