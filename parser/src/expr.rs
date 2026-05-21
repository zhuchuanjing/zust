use crate::Stmt;
use anyhow::{Result, anyhow};
use dynamic::Dynamic;
use smol_str::SmolStr;

use super::{Parser, ParserErr, Span, Type, try_parse};
use num_enum::{FromPrimitive, IntoPrimitive};

#[repr(i32)]
#[derive(Debug, Clone, PartialEq, IntoPrimitive, FromPrimitive)]
pub enum UnaryOp {
    Neg = 0,
    Not,
    #[num_enum(default)]
    Unknow = 255,
}

#[repr(i32)]
#[derive(Debug, Clone, PartialEq, IntoPrimitive, FromPrimitive)]
pub enum BinaryOp {
    Add = 10,
    Sub,
    Mul,
    Div,
    Mod,
    Shr,
    Shl,
    BitAnd,
    BitOr,
    BitXor,
    AddAssign,
    SubAssign,
    MulAssign,
    DivAssign,
    ModAssign,
    ShrAssign,
    ShlAssign,
    BitAndAssign,
    BitOrAssign,
    BitXorAssign,
    Assign,
    Eq,
    Ne,
    Lt,
    Gt,
    Le,
    Ge,
    And,
    Or,
    Idx,
    RangeOpen,
    RangeClose,
    #[num_enum(default)]
    Unknow = 255,
}

impl BinaryOp {
    pub fn is_logic(&self) -> bool {
        matches!(self, Self::And | Self::Or | Self::Eq | Self::Ne | Self::Lt | Self::Gt | Self::Le | Self::Ge)
    }

    pub fn is_add(&self) -> bool {
        matches!(self, Self::Add | Self::AddAssign)
    }

    pub fn is_assign(&self) -> bool {
        matches!(
            self,
            Self::AddAssign | Self::Assign | Self::DivAssign | Self::ModAssign | Self::MulAssign | Self::SubAssign | Self::BitAndAssign | Self::BitOrAssign | Self::BitXorAssign | Self::ShlAssign | Self::ShrAssign
        )
    }

    pub fn weight(&self) -> usize {
        match self {
            Self::Idx => 30,
            Self::Mul | Self::Div | Self::Mod => 20,
            Self::Add | Self::Sub => 19,
            Self::Shl | Self::Shr => 18,
            Self::BitAnd => 17,
            Self::BitXor => 16,
            Self::BitOr => 15,
            Self::Eq | Self::Ne | Self::Lt | Self::Gt | Self::Le | Self::Ge => 10,
            Self::And | Self::Or => 9,
            Self::RangeOpen | Self::RangeClose => 5,
            _ => usize::MIN,
        }
    }
}

#[derive(Debug, Clone)]
pub struct Expr {
    pub kind: ExprKind,
    pub span: Span,
}

#[derive(Debug, Clone, Default)]
pub enum ExprKind {
    #[default]
    Null,
    Value(Dynamic),
    Const(usize),
    Typed {
        value: Box<Expr>,
        ty: Type,
    },
    Unary {
        op: UnaryOp,
        value: Box<Expr>,
    },
    Binary {
        left: Box<Expr>,
        op: BinaryOp,
        right: Box<Expr>,
    },
    Ident(SmolStr),
    Var(u32),
    Capture(u32),
    Id(u32, Option<Box<Expr>>),
    Assoc {
        ty: Type,
        name: SmolStr,
    },
    AssocId {
        id: u32,
        params: Vec<Type>,
    },
    Tuple(Vec<Expr>),
    List(Vec<Expr>),
    Repeat {
        value: Box<Expr>,
        len: Type,
    },
    Dict(Vec<(SmolStr, Expr)>),
    Range {
        start: Box<Expr>,
        stop: Box<Expr>,
        inclusive: bool,
    },
    Call {
        obj: Box<Expr>,
        params: Vec<Expr>,
    },
    Stmt(Box<Stmt>),
    Closure {
        args: Vec<(SmolStr, Type)>,
        body: Box<Stmt>,
    },
}

#[derive(Debug, thiserror::Error)]
pub enum ExprErr {
    #[error("{0} 不是标识符")]
    NotIdent(SmolStr),
    #[error("{0} 非原生类型")]
    NotNative(SmolStr),
    #[error("期望表达式")]
    ExpectExpr,
}

impl Default for Expr {
    fn default() -> Self {
        Self::new(ExprKind::Null, Span::default())
    }
}

impl From<Dynamic> for Expr {
    fn from(value: Dynamic) -> Self {
        Self::new(ExprKind::Value(value), Span::default())
    }
}

impl Expr {
    pub fn new(kind: ExprKind, span: Span) -> Self {
        Self { kind, span }
    }

    pub fn with_span(mut self, span: Span) -> Self {
        self.span = span;
        self
    }

    pub fn is_range(&self) -> bool {
        matches!(self.kind, ExprKind::Range { .. })
    }

    pub fn is_typed(&self) -> bool {
        matches!(self.kind, ExprKind::Typed { .. })
    }

    pub fn get_type(&self) -> Type {
        match &self.kind {
            ExprKind::Typed { ty, .. } => ty.clone(),
            ExprKind::Value(v) => v.get_type(),
            ExprKind::Unary { value, .. } => value.get_type(),
            ExprKind::Tuple(list) => Type::Tuple(list.iter().map(|l| l.get_type()).collect()),
            ExprKind::Repeat { value, len } => {
                if let Type::ConstInt(len) = len {
                    Type::Array(std::rc::Rc::new(value.get_type()), *len as u32)
                } else {
                    Type::ArrayParam(std::rc::Rc::new(value.get_type()), std::rc::Rc::new(len.clone()))
                }
            }
            ExprKind::Range { start, .. } => start.get_type(),
            ExprKind::Stmt(stmt) => stmt.get_type().unwrap_or(Type::Any),
            _ => Type::Any,
        }
    }

    pub fn id(&self) -> Option<u32> {
        if let ExprKind::Id(id, _) = &self.kind { Some(*id) } else { None }
    }

    pub fn var(&self) -> Option<u32> {
        if let ExprKind::Var(idx) = &self.kind { Some(*idx) } else { None }
    }

    pub fn ident(&self) -> Result<&str> {
        if let ExprKind::Ident(ident) = &self.kind { Ok(ident.as_str()) } else { Err(ExprErr::NotIdent(SmolStr::from(format!("{:?}", self))).into()) }
    }

    pub fn binary_op(&self) -> Option<BinaryOp> {
        if let ExprKind::Binary { op, .. } = &self.kind { Some(op.clone()) } else { None }
    }

    pub fn is_idx(&self) -> bool {
        matches!(&self.kind, ExprKind::Binary { op, .. } if *op == BinaryOp::Idx)
    }

    pub fn is_value(&self) -> bool {
        match &self.kind {
            ExprKind::Value(_) => true,
            ExprKind::Typed { value, ty } => ty.is_native() && value.is_value(),
            _ => false,
        }
    }

    pub fn is_const(&self) -> bool {
        matches!(self.kind, ExprKind::Const(_))
    }

    pub fn value(self) -> Result<Dynamic> {
        match self.kind {
            ExprKind::Value(v) => Ok(v),
            ExprKind::Typed { value, ty } => {
                if ty.is_native() {
                    Ok(ty.force(value.value()?)?)
                } else {
                    Err(anyhow!("不是 Value"))
                }
            }
            _ => Err(anyhow!("不是 Value")),
        }
    }

    pub fn binary(self) -> Option<(Expr, BinaryOp, Expr)> {
        match self.kind {
            ExprKind::Binary { left, op, right } => Some((*left, op, *right)),
            _ => None,
        }
    }

    pub fn compact(&self) -> Option<Dynamic> {
        match &self.kind {
            ExprKind::Value(v) => Some(v.clone()),
            ExprKind::Unary { op, value } => {
                if value.is_value() {
                    let v = value.clone().value().unwrap();
                    match op {
                        UnaryOp::Neg => Some(-v),
                        UnaryOp::Not => Some(!v),
                        _ => None,
                    }
                } else {
                    None
                }
            }
            ExprKind::Binary { left, op, right } => {
                if left.is_value() && right.is_value() {
                    let left = left.clone().value().unwrap();
                    let right = right.clone().value().unwrap();
                    let r = match op {
                        BinaryOp::Add | BinaryOp::AddAssign => left + right,
                        BinaryOp::Sub | BinaryOp::SubAssign => left - right,
                        BinaryOp::Mul | BinaryOp::MulAssign => left * right,
                        BinaryOp::Div | BinaryOp::DivAssign => left / right,
                        BinaryOp::Mod | BinaryOp::ModAssign => left % right,
                        BinaryOp::And => Dynamic::Bool(left.is_true() && right.is_true()),
                        BinaryOp::Or => Dynamic::Bool(left.is_true() || right.is_true()),
                        BinaryOp::Eq => Dynamic::Bool(left == right),
                        BinaryOp::Ne => Dynamic::Bool(left != right),
                        BinaryOp::Le => Dynamic::Bool(left <= right),
                        BinaryOp::Lt => Dynamic::Bool(left < right),
                        BinaryOp::Ge => Dynamic::Bool(left >= right),
                        BinaryOp::Gt => Dynamic::Bool(left > right),
                        BinaryOp::Shl | BinaryOp::ShlAssign => left << right,
                        BinaryOp::Shr | BinaryOp::ShrAssign => left >> right,
                        BinaryOp::Assign => right,
                        BinaryOp::BitAnd | BinaryOp::BitAndAssign => left & right,
                        BinaryOp::BitXor | BinaryOp::BitXorAssign => left ^ right,
                        BinaryOp::BitOr | BinaryOp::BitOrAssign => left | right,
                        BinaryOp::Idx => {
                            if let Some(idx) = right.as_int() {
                                return left.get_idx(idx as usize);
                            } else if let Ok(key) = SmolStr::try_from(right) {
                                return left.get_dynamic(&key);
                            } else {
                                return None;
                            }
                        }
                        _ => Dynamic::Null,
                    };
                    Some(r)
                } else {
                    None
                }
            }
            _ => None,
        }
    }
}

impl Parser {
    fn is_dict_item_boundary(ch: u8) -> bool {
        matches!(ch, b',' | b'}')
    }

    fn is_shorthand_field_name(name: &str) -> bool {
        name.as_bytes().first().is_some_and(|ch| ch.is_ascii_alphabetic() || *ch == b'_')
    }

    pub(crate) fn looks_like_dict(&mut self) -> bool {
        let save_pos = self.pos;
        let result = (|| -> Result<bool> {
            self.whitespace()?;
            if self.take(b'{').is_err() {
                return Ok(false);
            }
            self.whitespace()?;
            if self.take(b'}').is_ok() {
                return Ok(true);
            }
            if self.ident().is_err() && self.string().is_err() {
                return Ok(false);
            }
            self.whitespace()?;
            Ok(matches!(self.get(), Ok(b':' | b',' | b'}')))
        })()
        .unwrap_or(false);
        self.pos = save_pos;
        result
    }

    pub(crate) fn looks_like_empty_dict(&mut self) -> bool {
        let save_pos = self.pos;
        let result = (|| -> Result<bool> {
            self.whitespace()?;
            self.take(b'{')?;
            self.whitespace()?;
            Ok(self.take(b'}').is_ok())
        })()
        .unwrap_or(false);
        self.pos = save_pos;
        result
    }

    fn postfix_expr(&mut self, start: usize, mut expr: Expr) -> Result<Expr> {
        while !self.is_eof() && [b'.', b'[', b'('].contains(&self.get()?) {
            if self.ahead()? == b'.' && self.get()? == b'.' {
                break;
            }
            if self.take(b'.').is_ok() {
                let key_start = self.current_pos();
                let key = self.ident()?;
                let right = Expr::new(ExprKind::Value(Dynamic::String(key)), self.span_from(key_start));
                let span = expr.span.merge(right.span);
                expr = Expr::new(ExprKind::Binary { left: Box::new(expr), op: BinaryOp::Idx, right: Box::new(right) }, span);
            } else if self.take(b'[').is_ok() {
                let key = self.expr(None, None)?.0;
                self.until(b']')?;
                let span = Span::new(start, self.current_pos());
                expr = Expr::new(ExprKind::Binary { left: Box::new(expr), op: BinaryOp::Idx, right: Box::new(key) }, span);
            } else if self.take(b'(').is_ok() {
                let params = crate::parse_list!(self, Vec::new(), b')', b',', self.expr(None, None)?.0);
                expr = Expr::new(ExprKind::Call { obj: Box::new(expr), params }, Span::new(start, self.current_pos()));
            }
        }
        Ok(expr.with_span(Span::new(start, self.current_pos())))
    }

    fn binary_op(&mut self) -> Option<BinaryOp> {
        if self.just("<<=").is_ok() {
            Some(BinaryOp::ShlAssign)
        } else if self.just(">>=").is_ok() {
            Some(BinaryOp::ShrAssign)
        } else if self.just("<<").is_ok() {
            Some(BinaryOp::Shl)
        } else if self.just(">>").is_ok() {
            Some(BinaryOp::Shr)
        } else if self.just(">=").is_ok() {
            Some(BinaryOp::Ge)
        } else if self.just("==").is_ok() {
            Some(BinaryOp::Eq)
        } else if self.just("!=").is_ok() {
            Some(BinaryOp::Ne)
        } else if self.just("<=").is_ok() {
            Some(BinaryOp::Le)
        } else if self.just("&&").is_ok() {
            Some(BinaryOp::And)
        } else if self.just("||").is_ok() {
            Some(BinaryOp::Or)
        } else if self.just("+=").is_ok() {
            Some(BinaryOp::AddAssign)
        } else if self.just("-=").is_ok() {
            Some(BinaryOp::SubAssign)
        } else if self.just("*=").is_ok() {
            Some(BinaryOp::MulAssign)
        } else if self.just("/=").is_ok() {
            Some(BinaryOp::DivAssign)
        } else if self.just("%=").is_ok() {
            Some(BinaryOp::ModAssign)
        } else if self.just("&=").is_ok() {
            Some(BinaryOp::BitAndAssign)
        } else if self.just("|=").is_ok() {
            Some(BinaryOp::BitOrAssign)
        } else if self.just("^=").is_ok() {
            Some(BinaryOp::BitXorAssign)
        } else if self.just("..=").is_ok() {
            Some(BinaryOp::RangeClose)
        } else if self.just("..").is_ok() {
            Some(BinaryOp::RangeOpen)
        } else {
            match self.get() {
                Ok(b'+') => {
                    self.pos += 1;
                    Some(BinaryOp::Add)
                }
                Ok(b'-') => {
                    self.pos += 1;
                    Some(BinaryOp::Sub)
                }
                Ok(b'*') => {
                    self.pos += 1;
                    Some(BinaryOp::Mul)
                }
                Ok(b'/') => {
                    self.pos += 1;
                    Some(BinaryOp::Div)
                }
                Ok(b'%') => {
                    self.pos += 1;
                    Some(BinaryOp::Mod)
                }
                Ok(b'<') => {
                    self.pos += 1;
                    Some(BinaryOp::Lt)
                }
                Ok(b'>') => {
                    self.pos += 1;
                    Some(BinaryOp::Gt)
                }
                Ok(b'=') => {
                    self.pos += 1;
                    Some(BinaryOp::Assign)
                }
                Ok(b'&') => {
                    self.pos += 1;
                    Some(BinaryOp::BitAnd)
                }
                Ok(b'|') => {
                    self.pos += 1;
                    Some(BinaryOp::BitOr)
                }
                Ok(b'^') => {
                    self.pos += 1;
                    Some(BinaryOp::BitXor)
                }
                _ => None,
            }
        }
    }

    pub fn kv(&mut self) -> Result<(SmolStr, Expr)> {
        let start = self.current_pos();
        if let Ok(key) = self.ident() {
            self.whitespace()?;
            if self.take(b':').is_ok() {
                let value = self.expr(None, None)?.0;
                Ok((SmolStr::from(key), value))
            } else if Self::is_shorthand_field_name(&key) && self.get().map(Self::is_dict_item_boundary).unwrap_or(false) {
                let span = Span::new(start, start + key.len());
                Ok((key.clone(), Expr::new(ExprKind::Ident(key), span)))
            } else {
                Err(anyhow!("expect ':' after field name"))
            }
        } else if let Ok(key) = self.string() {
            self.until(b':')?;
            let value = self.expr(None, None)?.0;
            Ok((SmolStr::from(key), value))
        } else {
            Err(anyhow!("expect string as key"))
        }
    }

    pub fn base_expr(&mut self, allow_struct_literal: bool) -> Result<Expr> {
        let start = self.current_pos();
        if let Ok(s) = self.text() {
            Ok(Expr::new(ExprKind::Value(Dynamic::String(s)), self.span_from(start)))
        } else if let Ok(n) = self.number() {
            if let Ok(ty) = self.get_type() {
                if ty.is_native() {
                    Ok(Expr::new(ExprKind::Typed { value: Box::new(Expr::new(ExprKind::Value(n), self.span_from(start))), ty }, self.span_from(start)))
                } else {
                    Err(ExprErr::NotNative(SmolStr::from(format!("{:?}", ty))).into())
                }
            } else {
                Ok(Expr::new(ExprKind::Value(n), self.span_from(start)))
            }
        } else if self.keyword("true").is_ok() {
            Ok(Expr::new(ExprKind::Value(Dynamic::Bool(true)), self.span_from(start)))
        } else if self.keyword("false").is_ok() {
            Ok(Expr::new(ExprKind::Value(Dynamic::Bool(false)), self.span_from(start)))
        } else if self.keyword("null").is_ok() {
            Ok(Expr::new(ExprKind::Value(Dynamic::Null), self.span_from(start)))
        } else if let Ok(ident) = self.ident() {
            self.whitespace()?;
            let save_pos = self.pos;
            if self.take(b'<').is_ok() {
                let typed_literal = (|| -> Result<Expr> {
                    let type_params = crate::parse_list!(self, Vec::new(), b'>', b',', self.get_type_param()?);
                    self.whitespace()?;
                    if self.just("::").is_ok() {
                        let name = self.ident()?;
                        let expr = Expr::new(ExprKind::Assoc { ty: Type::Ident { name: ident.clone(), params: type_params }, name }, self.span_from(start));
                        return self.postfix_expr(start, expr);
                    }
                    if allow_struct_literal
                        && self.looks_like_dict()
                        && let Ok(b'{') = self.get()
                        && let Ok(dict) = try_parse!(self, self.dict())
                    {
                        return Ok(Expr::new(ExprKind::Typed { value: Box::new(dict), ty: Type::Ident { name: ident.clone(), params: type_params } }, self.span_from(start)));
                    }
                    Err(ExprErr::ExpectExpr.into())
                })();
                if let Ok(expr) = typed_literal {
                    return Ok(expr);
                }
                self.pos = save_pos;
            }
            if allow_struct_literal
                && self.looks_like_dict()
                && let Ok(b'{') = self.get()
                && let Ok(dict) = try_parse!(self, self.dict())
            {
                return Ok(Expr::new(ExprKind::Typed { value: Box::new(dict), ty: Type::Ident { name: ident, params: Vec::new() } }, self.span_from(start)));
            }
            self.postfix_expr(start, Expr::new(ExprKind::Ident(ident), self.span_from(start)))
        } else {
            Err(ExprErr::ExpectExpr.into())
        }
    }

    pub(crate) fn dict(&mut self) -> Result<Expr> {
        let start = self.current_pos();
        self.pos += 1;
        Ok(Expr::new(ExprKind::Dict(crate::parse_list!(self, Vec::new(), b'}', b',', self.kv()?)), self.span_from(start)))
    }

    fn static_dynamic_literal_expr(&mut self) -> Result<Expr> {
        let start = self.current_pos();
        let value = self.static_dynamic_value()?;
        Ok(Expr::new(ExprKind::Value(value), self.span_from(start)))
    }

    fn static_dynamic_value(&mut self) -> Result<Dynamic> {
        self.whitespace()?;
        if self.get()? == b'[' {
            return self.static_dynamic_list();
        }
        if self.get()? == b'{' {
            return self.static_dynamic_map();
        }
        if self.take(b'-').is_ok() {
            return Ok(-self.number()?);
        }
        if let Ok(text) = self.text() {
            return Ok(Dynamic::String(text));
        }
        if let Ok(number) = self.number() {
            return Ok(number);
        }
        if self.keyword("true").is_ok() {
            return Ok(Dynamic::Bool(true));
        }
        if self.keyword("false").is_ok() {
            return Ok(Dynamic::Bool(false));
        }
        if self.keyword("null").is_ok() {
            return Ok(Dynamic::Null);
        }
        Err(ExprErr::ExpectExpr.into())
    }

    fn static_dynamic_list(&mut self) -> Result<Dynamic> {
        self.take(b'[')?;
        let mut values = Vec::new();
        loop {
            self.whitespace()?;
            if self.take(b']').is_ok() {
                break;
            }
            values.push(self.static_dynamic_value()?);
            self.whitespace()?;
            if self.take(b',').is_ok() {
                continue;
            }
            self.until(b']')?;
            break;
        }
        Ok(Dynamic::list(values))
    }

    fn static_dynamic_map(&mut self) -> Result<Dynamic> {
        self.take(b'{')?;
        let mut values = std::collections::BTreeMap::new();
        loop {
            self.whitespace()?;
            if self.take(b'}').is_ok() {
                break;
            }
            let key = if let Ok(key) = self.ident() { key } else { self.string()? };
            self.until(b':')?;
            let value = self.static_dynamic_value()?;
            values.insert(key, value);
            self.whitespace()?;
            if self.take(b',').is_ok() {
                continue;
            }
            self.until(b'}')?;
            break;
        }
        Ok(Dynamic::map(values))
    }

    pub fn get_expr(&mut self) -> Result<Expr> {
        self.expr(None, None).map(|(e, _)| e)
    }

    pub fn get_expr_without_struct_literal(&mut self) -> Result<Expr> {
        self.expr_with_min_weight(None, None, 0, false).map(|(e, _)| e)
    }

    pub fn expr(&mut self, left: Option<(Expr, bool)>, left_op: Option<BinaryOp>) -> Result<(Expr, bool)> {
        self.expr_with_min_weight(left, left_op, 0, true)
    }

    fn expr_with_min_weight(&mut self, left: Option<(Expr, bool)>, left_op: Option<BinaryOp>, min_weight: usize, allow_struct_literal: bool) -> Result<(Expr, bool)> {
        self.whitespace()?;
        if self.is_eof() {
            return left.ok_or(ParserErr::EndofInput.into());
        }
        let start = self.current_pos();
        let ch = self.get()?;
        let mut expr = if ch == b'(' {
            let start = self.current_pos();
            self.pos += 1;
            let (e, _closed) = self.expr_with_min_weight(None, None, 0, true)?;
            self.whitespace()?;
            if self.get()? == b',' {
                self.pos += 1;
                let list = crate::parse_list!(self, vec![e], b')', b',', self.expr_with_min_weight(None, None, 0, true)?.0);
                let expr = Expr::new(ExprKind::Tuple(list), Span::new(start, self.current_pos()));
                Ok((self.postfix_expr(start, expr)?, true))
            } else {
                self.until(b')')?;
                let expr = e.with_span(Span::new(start, self.current_pos()));
                Ok((self.postfix_expr(start, expr)?, true))
            }
        } else if ch == b'!' && self.ahead().map(|a| a != b'=').unwrap_or(true) {
            let start = self.current_pos();
            self.pos += 1;
            let value = self.expr_with_min_weight(None, None, BinaryOp::Mul.weight() + 1, allow_struct_literal)?.0;
            Ok((Expr::new(ExprKind::Unary { op: UnaryOp::Not, value: Box::new(value) }, Span::new(start, self.current_pos())), false))
        } else if ch == b'-' && self.ahead().map(|a| a != b'=').unwrap_or(true) && (left.is_none() || (left.is_some() && left_op.is_some())) {
            let start = self.current_pos();
            self.pos += 1;
            let value = self.expr_with_min_weight(None, None, BinaryOp::Mul.weight() + 1, allow_struct_literal)?.0;
            Ok((Expr::new(ExprKind::Unary { op: UnaryOp::Neg, value: Box::new(value) }, Span::new(start, self.current_pos())), false))
        } else if ch == b'[' {
            if let Ok(expr) = try_parse!(self, self.static_dynamic_literal_expr()) {
                return Ok((expr, false));
            }
            let start = self.current_pos();
            self.pos += 1;
            self.whitespace()?;
            if self.take(b']').is_ok() {
                Ok((Expr::new(ExprKind::List(Vec::new()), Span::new(start, self.current_pos())), false))
            } else {
                let first = self.expr_with_min_weight(None, None, 0, true)?.0;
                self.whitespace()?;
                if self.take(b';').is_ok() {
                    let len = self.get_type_param()?;
                    self.until(b']')?;
                    Ok((Expr::new(ExprKind::Repeat { value: Box::new(first), len }, Span::new(start, self.current_pos())), false))
                } else {
                    let mut items = vec![first];
                    while self.take(b',').is_ok() {
                        self.whitespace()?;
                        if self.take(b']').is_ok() {
                            return Ok((Expr::new(ExprKind::List(items), Span::new(start, self.current_pos())), false));
                        }
                        items.push(self.expr_with_min_weight(None, None, 0, true)?.0);
                        self.whitespace()?;
                    }
                    self.until(b']')?;
                    Ok((Expr::new(ExprKind::List(items), Span::new(start, self.current_pos())), false))
                }
            }
        } else if ch == b'{'
            && (left.is_none() || left_op.is_some())
            && let Ok(expr) = try_parse!(self, self.static_dynamic_literal_expr())
        {
            Ok((expr, false))
        } else if ch == b'{'
            && (left.is_none() || left_op.is_some())
            && let Ok(dict) = try_parse!(self, self.dict())
        {
            Ok((dict, false))
        } else if (left.is_none() || left_op.is_some()) && self.keyword("if").is_ok() {
            let stmt = self.if_block()?;
            Ok((Expr::new(ExprKind::Stmt(Box::new(stmt)), Span::new(start, self.current_pos())), true))
        } else if ch == b'|' && left.is_none() {
            let start = self.current_pos();
            self.pos += 1;
            let args = crate::parse_list!(self, Vec::new(), b'|', b',', self.ident_typed()?);
            let body = Box::new(self.block()?);
            Ok((Expr::new(ExprKind::Closure { args, body }, Span::new(start, self.current_pos())), true))
        } else if let Some(this_op) = self.binary_op() {
            let (left, close) = left.ok_or(anyhow!("{:?} need left value", this_op))?;
            if this_op == BinaryOp::RangeOpen {
                self.whitespace()?;
                if self.get()? == b']' {
                    let span = Span::new(left.span.start, self.current_pos());
                    let stop = Expr::new(ExprKind::Value(Dynamic::Null), Span::empty(self.current_pos()));
                    return Ok((Expr::new(ExprKind::Range { start: Box::new(left), stop: Box::new(stop), inclusive: false }, span), false));
                }
            }
            if this_op.weight() < min_weight {
                self.pos = start;
                return Ok((left, close));
            }
            if left_op.is_some() {
                return Err(anyhow!("unexpected binary op {:?}", this_op));
            }
            return if !close && left.binary_op().map(|op| op.weight() < this_op.weight()).unwrap_or(false) {
                let (binary_left, op, right) = left.binary().unwrap();
                let this_weight = this_op.weight();
                let new_right = self.expr_with_min_weight(Some((right, false)), Some(this_op), this_weight, allow_struct_literal)?.0;
                let span = binary_left.span.merge(new_right.span);
                let expr = Expr::new(ExprKind::Binary { left: Box::new(binary_left), op, right: Box::new(new_right) }, span);
                self.expr_with_min_weight(Some((expr, false)), None, min_weight, allow_struct_literal)
            } else {
                self.expr_with_min_weight(Some((left, false)), Some(this_op), min_weight, allow_struct_literal)
            };
        } else if self.keyword("as").is_ok() && left.is_some() {
            let value = left.unwrap().0;
            let start = value.span.start;
            let ty = self.get_type()?;
            return Ok((Expr::new(ExprKind::Typed { value: Box::new(value), ty }, Span::new(start, self.current_pos())), false));
        } else {
            self.base_expr(allow_struct_literal).map(|e| (e, false))
        };

        if left.is_some() {
            if let Some(op) = left_op {
                let left = left.unwrap().0;
                let right = expr?.0;
                let span = left.span.merge(right.span);
                expr = Ok((
                    match op {
                        BinaryOp::RangeOpen => Expr::new(ExprKind::Range { start: Box::new(left), stop: Box::new(right), inclusive: false }, span),
                        BinaryOp::RangeClose => Expr::new(ExprKind::Range { start: Box::new(left), stop: Box::new(right), inclusive: true }, span),
                        _ => Expr::new(ExprKind::Binary { left: Box::new(left), op, right: Box::new(right) }, span),
                    },
                    false,
                ));
            } else if expr.is_ok() {
                return Err(anyhow!("unexpected {:?}", expr));
            } else {
                return Ok((left.unwrap().0, false));
            }
        }
        let result = self.expr_with_min_weight(Some(expr?), None, min_weight, allow_struct_literal)?;
        Ok(result)
    }
}
