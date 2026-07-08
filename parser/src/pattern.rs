use anyhow::{Result, anyhow};
use dynamic::{Dynamic, Type};
use smol_str::SmolStr;

use super::{BinaryOp, Parser, Span, expr::Expr, expr::ExprKind};

#[derive(Debug, Clone)]
pub struct Pattern {
    pub kind: PatternKind,
    pub span: Span,
}

#[derive(Debug, Clone)]
pub enum PatternKind {
    Wildcard,
    Var { idx: u32, ty: Type },
    Ident { name: SmolStr, ty: Type },
    Literal(Dynamic),
    Tuple(Vec<Pattern>),
    List { elems: Vec<Pattern>, has_rest: bool },
    Struct { name: SmolStr, fields: Vec<(SmolStr, Option<Pattern>)> },
    Member(Box<Pattern>, SmolStr),
    Idx(Box<Pattern>, Expr),
}

impl Pattern {
    pub fn new(kind: PatternKind, span: Span) -> Self {
        Self { kind, span }
    }

    pub fn var(&self) -> Option<u32> {
        if let PatternKind::Var { idx, .. } = &self.kind { Some(*idx) } else { None }
    }

    pub fn expr(&self) -> Result<Expr> {
        if let PatternKind::Var { idx, .. } = &self.kind { Ok(Expr::new(super::expr::ExprKind::Var(*idx), self.span)) } else { Err(anyhow!("不是变量")) }
    }

    /// 生成"scrut 是否匹配 pat"的布尔表达式。
    /// `scrut` 必须是个无副作用、可重复求值的表达式(实际使用时一般是
    /// 临时变量 `__m_scrut`)。
    pub fn match_test(&self, scrut: &Expr) -> Expr {
        let span = self.span;
        match &self.kind {
            PatternKind::Wildcard | PatternKind::Var { .. } | PatternKind::Ident { .. } => Expr::new(ExprKind::Value(Dynamic::Bool(true)), span),
            PatternKind::Literal(v) => bin(scrut.clone(), BinaryOp::Eq, ExprKind::Value(v.clone()).at(span), span),
            PatternKind::Tuple(items) => {
                let mut tests = Vec::new();
                tests.push(bin(method_call(scrut.clone(), "len", Vec::new(), span), BinaryOp::Eq, ExprKind::Value(Dynamic::I32(items.len() as i32)).at(span), span));
                for (i, p) in items.iter().enumerate() {
                    let elem = idx_expr(scrut.clone(), i as i32, span);
                    tests.push(p.match_test(&elem));
                }
                and_chain(tests, span)
            }
            PatternKind::List { elems, has_rest } => {
                let prefix_len = if *has_rest { elems.len() - 1 } else { elems.len() };
                let mut tests = Vec::new();
                tests.push(method_call(scrut.clone(), "is_list", Vec::new(), span));
                let len_expr = method_call(scrut.clone(), "len", Vec::new(), span);
                let len_lit = ExprKind::Value(Dynamic::I32(prefix_len as i32)).at(span);
                let len_op = if *has_rest { BinaryOp::Ge } else { BinaryOp::Eq };
                tests.push(bin(len_expr, len_op, len_lit, span));
                for (i, p) in elems.iter().take(prefix_len).enumerate() {
                    let elem = idx_expr(scrut.clone(), i as i32, span);
                    tests.push(p.match_test(&elem));
                }
                and_chain(tests, span)
            }
            PatternKind::Struct { fields, .. } => {
                let mut tests = Vec::new();
                for (name, sub) in fields {
                    let field = field_expr(scrut.clone(), name.clone(), span);
                    if let Some(sub) = sub {
                        tests.push(sub.match_test(&field));
                    }
                    // 简写 `field` 永远绑定且永远匹配,跳过 test。
                }
                if tests.is_empty() { ExprKind::Value(Dynamic::Bool(true)).at(span) } else { and_chain(tests, span) }
            }
            PatternKind::Member(_, _) | PatternKind::Idx(_, _) => ExprKind::Value(Dynamic::Bool(true)).at(span),
        }
    }
}

fn bin(left: Expr, op: BinaryOp, right: Expr, span: Span) -> Expr {
    Expr::new(ExprKind::Binary { left: Box::new(left), op, right: Box::new(right) }, span)
}

fn idx_expr(scrut: Expr, idx: i32, span: Span) -> Expr {
    bin(scrut, BinaryOp::Idx, ExprKind::Value(Dynamic::I32(idx)).at(span), span)
}

fn field_expr(scrut: Expr, name: SmolStr, span: Span) -> Expr {
    bin(scrut, BinaryOp::Idx, ExprKind::Value(Dynamic::String(name)).at(span), span)
}

fn method_call(obj: Expr, name: &str, args: Vec<Expr>, span: Span) -> Expr {
    let member = field_expr(obj, SmolStr::from(name), span);
    Expr::new(ExprKind::Call { obj: Box::new(member), params: args }, span)
}

fn and_chain(mut parts: Vec<Expr>, span: Span) -> Expr {
    if parts.is_empty() {
        return ExprKind::Value(Dynamic::Bool(true)).at(span);
    }
    let first = parts.remove(0);
    parts.into_iter().fold(first, |acc, e| bin(acc, BinaryOp::And, e, span))
}

trait ExprKindExt {
    fn at(self, span: Span) -> Expr;
}

impl ExprKindExt for ExprKind {
    fn at(self, span: Span) -> Expr {
        Expr::new(self, span)
    }
}

impl Parser {
    pub fn pattern(&mut self) -> Result<Pattern> {
        self.whitespace()?;
        let start = self.current_pos();
        // 数字字面量(支持负号)。
        if self.get()?.is_ascii_digit() || (self.get()? == b'-' && self.ahead().map(|c| c.is_ascii_digit()).unwrap_or(false)) {
            let negative = if self.get()? == b'-' {
                self.pos += 1;
                true
            } else {
                false
            };
            let mut value = self.number()?;
            if negative {
                // 支持所有有符号整数与浮点类型的负数字面量模式。
                // 无符号类型(U8 等)取负无意义,落到 other 报错。
                value = match value {
                    Dynamic::I8(n) => Dynamic::I8(n.wrapping_neg()),
                    Dynamic::I16(n) => Dynamic::I16(n.wrapping_neg()),
                    Dynamic::I32(n) => Dynamic::I32(-n),
                    Dynamic::I64(n) => Dynamic::I64(-n),
                    Dynamic::F16(n) => Dynamic::F16(dynamic::f64_to_f16(-dynamic::f16_to_f64(n))),
                    Dynamic::F32(n) => Dynamic::F32(-n),
                    Dynamic::F64(n) => Dynamic::F64(-n),
                    other => return Err(anyhow!("不支持负数模式: {:?}", other)),
                };
            }
            return Ok(Pattern::new(PatternKind::Literal(value), self.span_from(start)));
        }
        if self.keyword("_").is_ok() {
            Ok(Pattern::new(PatternKind::Wildcard, self.span_from(start)))
        } else if self.keyword("true").is_ok() {
            Ok(Pattern::new(PatternKind::Literal(Dynamic::Bool(true)), self.span_from(start)))
        } else if self.keyword("false").is_ok() {
            Ok(Pattern::new(PatternKind::Literal(Dynamic::Bool(false)), self.span_from(start)))
        } else if self.keyword("null").is_ok() {
            Ok(Pattern::new(PatternKind::Literal(Dynamic::Null), self.span_from(start)))
        } else if self.just("(").is_ok() {
            self.whitespace()?;
            if self.take(b')').is_ok() {
                return Ok(Pattern::new(PatternKind::Tuple(Vec::new()), self.span_from(start)));
            }
            let first = self.pattern()?;
            self.whitespace()?;
            if self.take(b',').is_ok() {
                let mut items = vec![first];
                loop {
                    self.whitespace()?;
                    if self.take(b')').is_ok() {
                        break;
                    }
                    items.push(self.pattern()?);
                    self.whitespace()?;
                    if self.take(b',').is_err() {
                        self.take(b')')?;
                        break;
                    }
                }
                Ok(Pattern::new(PatternKind::Tuple(items), self.span_from(start)))
            } else {
                self.take(b')')?;
                Ok(first)
            }
        } else if self.just("[").is_ok() {
            // 手写 list 解析,处理 `..rest`。
            let mut elems = Vec::new();
            let mut has_rest = false;
            self.whitespace()?;
            if self.take(b']').is_ok() {
                return Ok(Pattern::new(PatternKind::List { elems, has_rest }, self.span_from(start)));
            }
            // 第一个元素可能是 `..name` 形式(整个 list 都是 rest)。
            if self.take(b'.').is_ok() && self.take(b'.').is_ok() {
                self.whitespace()?;
                let (name, ty) = self.ident_typed()?;
                elems.push(Pattern::new(PatternKind::Ident { name, ty }, self.span_from(start)));
                has_rest = true;
                self.whitespace()?;
                self.take(b']')?;
                return Ok(Pattern::new(PatternKind::List { elems, has_rest }, self.span_from(start)));
            }
            elems.push(self.pattern()?);
            self.whitespace()?;
            if self.take(b'.').is_ok() && self.take(b'.').is_ok() {
                self.whitespace()?;
                let (name, ty) = self.ident_typed()?;
                elems.push(Pattern::new(PatternKind::Ident { name, ty }, self.span_from(start)));
                has_rest = true;
                self.whitespace()?;
                let _ = self.take(b','); // 容忍尾随逗号
                self.whitespace()?;
                self.take(b']')?;
                return Ok(Pattern::new(PatternKind::List { elems, has_rest }, self.span_from(start)));
            } else if self.take(b',').is_ok() {
                loop {
                    self.whitespace()?;
                    if self.take(b']').is_ok() {
                        break;
                    }
                    if self.take(b'.').is_ok() && self.take(b'.').is_ok() {
                        self.whitespace()?;
                        let (name, ty) = self.ident_typed()?;
                        elems.push(Pattern::new(PatternKind::Ident { name, ty }, self.span_from(start)));
                        has_rest = true;
                        self.whitespace()?;
                        self.take(b']')?;
                        return Ok(Pattern::new(PatternKind::List { elems, has_rest }, self.span_from(start)));
                    }
                    elems.push(self.pattern()?);
                    self.whitespace()?;
                    if !self.take(b',').is_ok() {
                        break;
                    }
                }
            }
            self.whitespace()?;
            self.take(b']')?;
            Ok(Pattern::new(PatternKind::List { elems, has_rest }, self.span_from(start)))
        } else if let Ok(text) = self.string() {
            Ok(Pattern::new(PatternKind::Literal(Dynamic::String(text)), self.span_from(start)))
        } else if let Ok((name, ty)) = self.ident_typed() {
            // 如果紧跟 `{`,且看起来像 struct 字面量,则解析为 Struct 模式。
            self.whitespace()?;
            if matches!(self.get(), Ok(b'{')) {
                self.pos += 1;
                let mut fields: Vec<(SmolStr, Option<Pattern>)> = Vec::new();
                self.whitespace()?;
                if self.take(b'}').is_err() {
                    loop {
                        self.whitespace()?;
                        let field_name = self.ident()?;
                        self.whitespace()?;
                        let sub = if self.take(b':').is_ok() { Some(self.pattern()?) } else { None };
                        fields.push((field_name, sub));
                        self.whitespace()?;
                        if !self.take(b',').is_ok() {
                            break;
                        }
                    }
                    self.whitespace()?;
                    self.take(b'}')?;
                }
                return Ok(Pattern::new(PatternKind::Struct { name, fields }, self.span_from(start)));
            }
            Ok(Pattern::new(PatternKind::Ident { name, ty }, self.span_from(start)))
        } else {
            Err(anyhow!("无效的模式 {:?}", String::from_utf8_lossy(&self.buf[self.pos..])))
        }
    }
}
