use crate::try_parse;
use dynamic::Type;

use super::{Expr, Parser, Pattern, Span, expr::ExprKind, pattern::PatternKind};
use anyhow::{Result, anyhow};
use smol_str::SmolStr;

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
                PatternKind::Tuple(list) | PatternKind::List { elems: list, has_rest: _ } => {
                    let mut stmts = Vec::new();
                    for (idx, p) in list.into_iter().enumerate() {
                        match p.expr() {
                            Ok(p) => stmts.push(Self::get_idx_assign(p, idx, expr.clone())),
                            Err(e) => {
                                println!("{:?}", e);
                            }
                        }
                    }
                    Self::new(StmtKind::Block(stmts), self.span)
                }
                p => panic!("{:?}", p),
            };
            let _ = std::mem::replace(self, stmt);
        } else {
            match &mut self.kind {
                StmtKind::Block(stmts) => {
                    stmts.last_mut().map(|stmt| stmt.bind_pattern(pat));
                }
                StmtKind::If { then_body, else_body, .. } => {
                    let _ = then_body.bind_pattern(pat.clone());
                    else_body.as_mut().map(|e| e.bind_pattern(pat));
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
            _ => {
                panic!("无效语句 {:?}", self)
            }
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
        self.whitespace()?;
        let start = self.current_pos();
        if self.get()? == b'{' {
            self.pos += 1;
            Ok(Stmt::new(StmtKind::Block(crate::parse_list!(self, Vec::new(), b'}', 0, self.stmt(false)?)), self.span_from(start)))
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
            if self.keyword("if").is_ok() { self.if_block().map(Box::new).ok() } else { self.block().map(Box::new).ok() }
        } else {
            None
        };
        Ok(Stmt::new(StmtKind::If { cond, then_body, else_body }, Span::new(start, self.current_pos())))
    }

    pub fn stmt(&mut self, is_pub: bool) -> Result<Stmt> {
        self.whitespace()?;
        self.spans.push(self.pos);
        let start = self.current_pos();
        let stmt = if self.keyword("let").is_ok() {
            let pat = self.pattern()?;
            self.until(b'=')?;
            self.whitespace()?;
            let stmt = if self.get()? == b'{' {
                if self.looks_like_dict() {
                    let expr = self.get_expr()?;
                    self.whitespace()?;
                    if self.get()? == b';' {
                        self.pos += 1;
                    }
                    Stmt::new(StmtKind::Expr(expr, true), Span::new(start, self.current_pos()))
                } else {
                    return Err(anyhow!("代码块不能直接作为表达式，请使用 || {{ ... }} 包装为匿名函数"));
                }
            } else {
                let stmt = self.stmt(false)?;
                if stmt.expr().is_none() {
                    self.until(b';')?;
                }
                stmt
            };
            Stmt::new(StmtKind::Let { pat, value: Box::new(stmt) }, Span::new(start, self.current_pos()))
        } else if self.keyword("break").is_ok() {
            self.until(b';')?;
            Stmt::new(StmtKind::Break, Span::new(start, self.current_pos()))
        } else if self.keyword("continue").is_ok() {
            self.until(b';')?;
            Stmt::new(StmtKind::Continue, Span::new(start, self.current_pos()))
        } else if self.keyword("return").is_ok() {
            self.whitespace()?;
            let expr = self.get_expr().ok();
            self.until(b';')?;
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
            let body = Box::new(self.block()?);
            Stmt::new(StmtKind::For { pat, range, body }, Span::new(start, self.current_pos()))
        } else if self.keyword("fn").is_ok() {
            self.whitespace()?;
            let (name, generic_params) = self.ident_generic()?;
            self.until(b'(')?;
            let args = crate::parse_list!(self, Vec::new(), b')', b',', self.ident_typed()?);
            let body = Box::new(self.block()?);
            Stmt::new(StmtKind::Fn { name, generic_params, args, body, is_pub }, Span::new(start, self.current_pos()))
        } else if self.keyword("struct").is_ok() {
            let (name, params) = self.ident_generic()?;
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
            self.until(b'=')?;
            let value = self.get_expr()?;
            self.until(b';')?;
            Stmt::new(StmtKind::Const { name, ty, value, is_pub }, Span::new(start, self.current_pos()))
        } else if self.keyword("static").is_ok() {
            self.whitespace()?;
            let (name, ty) = self.ident_typed()?;
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
            Stmt::new(StmtKind::Impl { target, body: Box::new(self.block()?) }, Span::new(start, self.current_pos()))
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
