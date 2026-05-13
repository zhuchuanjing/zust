use anyhow::{Result, anyhow};
use dynamic::Dynamic;
use smol_str::SmolStr;

use super::{Parser, Span, Type, expr::Expr};

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
}

impl Parser {
    pub fn pattern(&mut self) -> Result<Pattern> {
        self.whitespace()?;
        let start = self.current_pos();
        if self.just("_").is_ok() {
            Ok(Pattern::new(PatternKind::Wildcard, self.span_from(start)))
        } else if self.just("(").is_ok() {
            Ok(Pattern::new(PatternKind::Tuple(crate::parse_list!(self, Vec::new(), b')', b',', self.pattern()?)), self.span_from(start)))
        } else if self.just("[").is_ok() {
            Ok(Pattern::new(PatternKind::List { elems: crate::parse_list!(self, Vec::new(), b']', b',', self.pattern()?), has_rest: false }, self.span_from(start)))
        } else if let Ok(text) = self.string() {
            Ok(Pattern::new(PatternKind::Literal(Dynamic::String(text)), self.span_from(start)))
        } else if let Ok((name, ty)) = self.ident_typed() {
            Ok(Pattern::new(PatternKind::Ident { name, ty }, self.span_from(start)))
        } else {
            Err(anyhow!("无效的模式 {:?}", String::from_utf8_lossy(&self.buf[self.pos..])))
        }
    }
}
