use std::fmt::Debug;

use anyhow::{Result, anyhow};
use dynamic::{ConstIntOp, Dynamic, Type};
use smol_str::SmolStr;

mod expr;
pub use expr::{BinaryOp, Expr, ExprKind, UnaryOp};

mod pattern;
pub use pattern::{Pattern, PatternKind};

mod stmt;
pub use stmt::{Stmt, StmtKind};

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Span {
    pub start: usize,
    pub end: usize,
}

impl Span {
    pub const fn new(start: usize, end: usize) -> Self {
        Self { start, end }
    }

    pub const fn empty(pos: usize) -> Self {
        Self { start: pos, end: pos }
    }

    pub fn merge(self, other: Self) -> Self {
        Self { start: self.start.min(other.start), end: self.end.max(other.end) }
    }
}

#[derive(Debug)]
pub struct Parser {
    pos: usize,   //当前解析的位置
    buf: Vec<u8>, //待解析的字符串
    spans: Vec<usize>,
}

const NOT_IDENT: &[u8] = &[b' ', b'\t', b'\n', b'\r', b'/', b'*', b'+', b'-', b'=', b'(', b')', b'{', b'}', b'[', b']', b';', b':', b',', b'.', b'<', b'>', b'!', b'#', b'$', b'%', b'^', b'&', b'|', b'\\', b'"', b'\''];
const WHITE_SPACE: &[u8] = &[b' ', b'\t', b'\n', b'\r'];
const TYPES: &[(&str, Type)] = &[
    ("bool", Type::Bool),
    ("string", Type::Str),
    ("i8", Type::I8),
    ("i16", Type::I16),
    ("i32", Type::I32),
    ("i64", Type::I64),
    ("u8", Type::U8),
    ("u16", Type::U16),
    ("u32", Type::U32),
    ("u64", Type::U64),
    ("f16", Type::F16),
    ("f32", Type::F32),
    ("f64", Type::F64),
];
const KEYWORDS: &[&str] = &["true", "false", "null", "let", "if", "else", "for", "in", "while", "pub", "fn", "struct", "impl", "const", "static", "continue", "return", "break"];

#[macro_export]
macro_rules! parse_list {
    ($self: ident, $start: expr, $end: expr, $sep: expr, $item_expr: expr) => {{
        let mut items = $start;
        loop {
            $self.whitespace()?;
            if $self.get()? == $end {
                $self.pos += 1;
                break;
            }
            let item = $item_expr;
            items.push(item);
            $self.whitespace()?;
            if $self.get()? == $sep {
                $self.pos += 1;
            }
        }
        items
    }};
}

#[macro_export]
macro_rules! try_parse {
    ($self: ident, $method: expr) => {{
        let save_pos = $self.pos; //保存当前 pos
        match $method {
            Ok(expr) => Ok(expr),
            Err(e) => {
                $self.pos = save_pos;
                Err(e)
            }
        }
    }};
}

#[derive(Debug, thiserror::Error)]
pub enum ParserErr {
    #[error("期望字符 {0} 实际字符 {1}")]
    ExpectChar(char, char),
    #[error("未发现期望字符")]
    NoCharCollect,
    #[error("期望字符串 {0}")]
    ExpectedString(SmolStr),
    #[error("输入结束")]
    EndofInput,
    #[error("未关闭的注释")]
    UncloseComment,
    #[error("非法的原始字符串")]
    IllegalRawString,
    #[error("未关闭字符串")]
    UnclosedString,
    #[error("非字符串")]
    NotString,
    #[error("非数字")]
    NotNumber,
}

impl Parser {
    pub fn new(buf: Vec<u8>) -> Self {
        Self { pos: 0, buf, spans: Vec::new() }
    }

    pub fn is_eof(&self) -> bool {
        self.pos >= self.buf.len()
    }

    pub fn get(&self) -> Result<u8> {
        //查看当前字符
        self.buf.get(self.pos).cloned().ok_or(ParserErr::EndofInput.into())
    }

    pub fn take(&mut self, ch: u8) -> Result<()> {
        //如果当前字符为 ch 消费该字符 返回 Ok(())
        if self.buf.get(self.pos).map(|b| *b == ch).unwrap_or(false) {
            self.pos += 1;
            Ok(())
        } else {
            Err(ParserErr::ExpectChar(ch as char, self.buf.get(self.pos as usize).cloned().unwrap_or(0) as char).into())
        }
    }

    pub fn until(&mut self, ch: u8) -> Result<()> {
        //消费直到指定字符 ch 忽略空白和注释
        self.whitespace()?;
        self.take(ch)
    }

    pub fn ahead(&self) -> Result<u8> {
        //朝前看
        self.buf.get(self.pos + 1).cloned().ok_or(ParserErr::EndofInput.into())
    }

    pub fn get_str(&self, start: usize, stop: usize) -> SmolStr {
        SmolStr::from(String::from_utf8_lossy(&self.buf[start..stop]))
    }

    pub fn error_stmt(&self) -> SmolStr {
        SmolStr::from(String::from_utf8_lossy(&self.buf[self.spans.last().cloned().unwrap_or(0)..self.pos]))
    }

    pub fn current_pos(&self) -> usize {
        self.pos
    }

    pub fn span_from(&self, start: usize) -> Span {
        Span::new(start, self.pos)
    }

    pub fn collect<F: Fn(u8) -> bool>(&mut self, f: F) -> Result<(usize, usize)> {
        let start = self.pos;
        while self.pos < self.buf.len() && f(self.buf[self.pos]) {
            self.pos += 1;
        }
        if self.pos > start { Ok((start, self.pos)) } else { Err(ParserErr::NoCharCollect.into()) }
    }

    pub fn just(&mut self, pattern: &str) -> Result<()> {
        if self.buf.len() - self.pos >= pattern.len() && self.buf[self.pos..self.pos + pattern.len()].eq(pattern.as_bytes()) {
            self.pos += pattern.len();
            Ok(())
        } else {
            Err(ParserErr::ExpectedString(SmolStr::new(pattern)).into())
        }
    }

    pub fn keyword(&mut self, pattern: &str) -> Result<()> {
        self.just(pattern)?;
        if self.pos < self.buf.len() && !NOT_IDENT.contains(&self.buf[self.pos]) {
            self.pos -= pattern.len();
            return Err(ParserErr::ExpectedString(SmolStr::new(pattern)).into());
        }
        Ok(())
    }

    pub fn get_type(&mut self) -> Result<Type> {
        self.whitespace()?;
        if self.get()? == b'[' {
            self.pos += 1;
            let ty = self.get_type()?;
            self.until(b';')?;
            self.whitespace()?;
            let len = self.get_type_param()?;
            self.until(b']')?;
            if let Type::ConstInt(number) = len {
                let number = u32::try_from(number).map_err(|_| anyhow!("数组长度超出 u32 范围"))?;
                Ok(Type::Array(std::rc::Rc::new(ty), number))
            } else {
                Ok(Type::ArrayParam(std::rc::Rc::new(ty), std::rc::Rc::new(len)))
            }
        } else {
            for ty in TYPES {
                if self.just(ty.0).is_ok() {
                    return Ok(ty.1.clone());
                }
            }
            let name = self.ident()?;
            if self.take(b'<').is_ok() {
                let params = crate::parse_list!(self, Vec::new(), b'>', b',', self.get_type_param()?);
                Ok(Type::Ident { name, params })
            } else {
                Ok(Type::Ident { name, params: Vec::new() })
            }
        }
    }

    pub fn get_type_param(&mut self) -> Result<Type> {
        self.const_type_param_add()
    }

    fn const_type_param_add(&mut self) -> Result<Type> {
        let mut left = self.const_type_param_mul()?;
        loop {
            self.whitespace()?;
            let op = if self.take(b'+').is_ok() {
                Some(ConstIntOp::Add)
            } else if self.take(b'-').is_ok() {
                Some(ConstIntOp::Sub)
            } else {
                None
            };
            let Some(op) = op else { break };
            let right = self.const_type_param_mul()?;
            left = Self::fold_const_type_binary(op, left, right)?;
        }
        Ok(left)
    }

    fn const_type_param_mul(&mut self) -> Result<Type> {
        let mut left = self.const_type_param_primary()?;
        loop {
            self.whitespace()?;
            let op = if self.take(b'*').is_ok() {
                Some(ConstIntOp::Mul)
            } else if self.take(b'/').is_ok() {
                Some(ConstIntOp::Div)
            } else if self.take(b'%').is_ok() {
                Some(ConstIntOp::Mod)
            } else {
                None
            };
            let Some(op) = op else { break };
            let right = self.const_type_param_primary()?;
            left = Self::fold_const_type_binary(op, left, right)?;
        }
        Ok(left)
    }

    fn const_type_param_primary(&mut self) -> Result<Type> {
        self.whitespace()?;
        if self.take(b'(').is_ok() {
            let ty = self.get_type_param()?;
            self.until(b')')?;
            return Ok(ty);
        }
        if self.get()?.is_ascii_digit() {
            let value = self.number()?;
            if let Some(value) = value.as_uint() {
                let value = i64::try_from(value).map_err(|_| anyhow!("模板数字参数超出 i64 范围"))?;
                Ok(Type::ConstInt(value))
            } else if let Some(value) = value.as_int() {
                Ok(Type::ConstInt(value))
            } else {
                Err(anyhow!("模板数字参数必须是整数"))
            }
        } else {
            self.get_type()
        }
    }

    fn fold_const_type_binary(op: ConstIntOp, left: Type, right: Type) -> Result<Type> {
        if let (Type::ConstInt(left), Type::ConstInt(right)) = (&left, &right) {
            let value = match op {
                ConstIntOp::Add => left + right,
                ConstIntOp::Sub => left - right,
                ConstIntOp::Mul => left * right,
                ConstIntOp::Div => {
                    if *right == 0 {
                        return Err(anyhow!("模板整数除以 0"));
                    }
                    left / right
                }
                ConstIntOp::Mod => {
                    if *right == 0 {
                        return Err(anyhow!("模板整数取模 0"));
                    }
                    left % right
                }
            };
            Ok(Type::ConstInt(value))
        } else {
            Ok(Type::ConstBinary { op, left: std::rc::Rc::new(left), right: std::rc::Rc::new(right) })
        }
    }

    pub fn comment(&mut self) -> Result<()> {
        if self.get()? == b'/' && self.ahead()? == b'/' {
            self.pos += 2;
            while self.pos < self.buf.len() && self.buf[self.pos] != b'\n' {
                self.pos += 1;
            }
            Ok(())
        } else if self.get()? == b'/' && self.ahead()? == b'*' {
            self.pos += 2;
            while self.pos + 1 < self.buf.len() {
                if self.buf[self.pos] == b'*' && self.buf[self.pos + 1] == b'/' {
                    self.pos += 2;
                    return Ok(());
                }
                self.pos += 1;
            }
            Err(ParserErr::UncloseComment.into())
        } else {
            Ok(())
        }
    }

    pub fn whitespace(&mut self) -> Result<()> {
        while self.pos < self.buf.len() {
            self.comment()?;
            if self.pos >= self.buf.len() || !WHITE_SPACE.contains(&self.buf[self.pos]) {
                break;
            }
            self.pos += 1;
        }
        Ok(())
    }

    pub fn ident(&mut self) -> Result<SmolStr> {
        let (start, mut stop) = self.collect(|ch| !NOT_IDENT.contains(&ch))?;
        loop {
            let save_pos = self.pos;
            if self.just("::").is_err() {
                break;
            }
            match self.collect(|ch| !NOT_IDENT.contains(&ch)) {
                Ok((_, next_stop)) => {
                    stop = next_stop;
                }
                Err(_) => {
                    self.pos = save_pos;
                    break;
                }
            }
        }
        if KEYWORDS.iter().position(|k| k.as_bytes() == &self.buf[start..stop]).is_some() {
            return Err(anyhow!("发现关键字{}", String::from_utf8_lossy(&self.buf[start..stop])));
        }
        Ok(self.get_str(start, stop))
    }

    pub fn string(&mut self) -> Result<SmolStr> {
        if self.buf[self.pos] == b'"' {
            self.pos += 1;
            let mut text_buf = Vec::new();
            while self.pos < self.buf.len() {
                if self.buf[self.pos] == b'\\' {
                    //转义字符
                    self.pos += 1;
                    match self.buf[self.pos] {
                        ch @ (b'n' | b'r' | b't' | b'\\' | b'"') => {
                            text_buf.push(ch);
                            self.pos += 1;
                        }
                        b'u' => {
                            self.pos += 1;
                            let unicode = if self.take(b'{').is_ok() {
                                let code = self.hex()?;
                                self.pos += 1;
                                code
                            } else {
                                self.hex()?
                            };
                            let ch = char::from_u32(unicode as u32).ok_or(anyhow!("非法 unicode {}", unicode))?;
                            let mut utf8_buf = [0u8; 4];
                            let s = ch.encode_utf8(&mut utf8_buf);
                            text_buf.extend_from_slice(s.as_bytes());
                        }
                        b'x' => {
                            self.pos += 1;
                            if self.pos + 2 < self.buf.len() {
                                let start = self.pos;
                                self.pos += 2;
                                let hex = &self.buf[start..self.pos];
                                let code = u32::from_str_radix(String::from_utf8_lossy(hex).as_ref(), 16)?;
                                text_buf.push(code as u8);
                            }
                        }
                        other => {
                            return Err(anyhow!("invalid escape character: {}", other as char));
                        }
                    }
                } else {
                    if self.buf[self.pos] == b'"' {
                        self.pos += 1;
                        return Ok(String::from_utf8(text_buf)?.into());
                    }
                    text_buf.push(self.buf[self.pos]);
                    self.pos += 1;
                }
            }
            Err(ParserErr::UnclosedString.into())
        } else {
            Err(ParserErr::NotString.into())
        }
    }

    pub fn text(&mut self) -> Result<SmolStr> {
        if self.get()? == b'r' && [b'#', b'"'].contains(&self.ahead()?) {
            self.pos += 1;
            let mut end = String::new();
            while self.buf[self.pos] == b'#' {
                end.push('#');
                self.pos += 1;
            }
            if self.get()? != b'"' {
                return Err(ParserErr::IllegalRawString.into());
            }
            self.pos += 1;
            let start_pos = self.pos;
            while self.pos < self.buf.len() {
                if self.just(&end).is_ok() {
                    break;
                }
                self.pos += 1;
            }
            Ok(self.get_str(start_pos, self.pos - end.len()))
        } else {
            self.string()
        }
    }

    fn hex(&mut self) -> Result<i32> {
        //注意 hex 会消耗当前字符 设置新的 self.pos
        let (start, stop) = self.collect(|ch| (ch >= b'0' && ch <= b'9') || (ch >= b'a' && ch <= b'f') || (ch >= b'A' && ch <= b'F'))?;
        Ok(i32::from_str_radix(&String::from_utf8_lossy(&self.buf[start..stop]), 16)?)
    }

    fn numeric_suffix(&mut self) -> Option<Type> {
        let save = self.pos;
        for (name, ty) in TYPES {
            if !ty.is_native() || *ty == Type::F16 {
                continue;
            }
            if self.buf.len() >= self.pos + name.len() && self.buf[self.pos..self.pos + name.len()].eq(name.as_bytes()) {
                self.pos += name.len();
                return Some(ty.clone());
            }
        }
        self.pos = save;
        None
    }

    fn int_literal(&mut self, digits: &str, radix: u32, suffix: Option<Type>) -> Result<Dynamic> {
        Ok(match suffix.unwrap_or(Type::I32) {
            Type::I8 => Dynamic::I8(i128::from_str_radix(digits, radix)? as i8),
            Type::I16 => Dynamic::I16(i128::from_str_radix(digits, radix)? as i16),
            Type::I32 => Dynamic::I32(i128::from_str_radix(digits, radix)? as i32),
            Type::I64 => Dynamic::I64(i128::from_str_radix(digits, radix)? as i64),
            Type::U8 => Dynamic::U8(u128::from_str_radix(digits, radix)? as u8),
            Type::U16 => Dynamic::U16(u128::from_str_radix(digits, radix)? as u16),
            Type::U32 => Dynamic::U32(u128::from_str_radix(digits, radix)? as u32),
            Type::U64 => Dynamic::U64(u128::from_str_radix(digits, radix)? as u64),
            Type::F32 => Dynamic::F32(u128::from_str_radix(digits, radix)? as f32),
            Type::F64 => Dynamic::F64(u128::from_str_radix(digits, radix)? as f64),
            ty => return Err(anyhow!("{:?} 不能作为数字后缀", ty)),
        })
    }

    fn float_literal(&mut self, digits: &str, suffix: Option<Type>) -> Result<Dynamic> {
        let value: f64 = digits.parse()?;
        Ok(match suffix.unwrap_or(Type::F32) {
            Type::I8 => Dynamic::I8(value as i8),
            Type::I16 => Dynamic::I16(value as i16),
            Type::I32 => Dynamic::I32(value as i32),
            Type::I64 => Dynamic::I64(value as i64),
            Type::U8 => Dynamic::U8(value as u8),
            Type::U16 => Dynamic::U16(value as u16),
            Type::U32 => Dynamic::U32(value as u32),
            Type::U64 => Dynamic::U64(value as u64),
            Type::F32 => Dynamic::F32(value as f32),
            Type::F64 => Dynamic::F64(value),
            ty => return Err(anyhow!("{:?} 不能作为浮点数字后缀", ty)),
        })
    }

    pub fn number(&mut self) -> Result<Dynamic> {
        if self.get()? == b'0' {
            if [b'b', b'B'].contains(&self.ahead()?) {
                self.pos += 2;
                let (start, stop) = self.collect(|ch| ch == b'0' || ch == b'1')?;
                let s = String::from_utf8_lossy(&self.buf[start..stop]).to_string();
                let suffix = self.numeric_suffix();
                return self.int_literal(&s, 2, suffix);
            } else if [b'o', b'O'].contains(&self.ahead()?) {
                self.pos += 2;
                let (start, stop) = self.collect(|ch| ch >= b'0' && ch <= b'7')?;
                let s = String::from_utf8_lossy(&self.buf[start..stop]).to_string();
                let suffix = self.numeric_suffix();
                return self.int_literal(&s, 8, suffix);
            } else if [b'x', b'X'].contains(&self.ahead()?) {
                self.pos += 2;
                let (start, stop) = self.collect(|ch| (ch >= b'0' && ch <= b'9') || (ch >= b'a' && ch <= b'f') || (ch >= b'A' && ch <= b'F'))?;
                let s = String::from_utf8_lossy(&self.buf[start..stop]).to_string();
                let suffix = self.numeric_suffix();
                return self.int_literal(&s, 16, suffix);
            }
        }
        let start = self.pos;
        while self.pos < self.buf.len() && self.buf[self.pos] <= b'9' && self.buf[self.pos] >= b'0' {
            self.pos += 1;
        }
        let mut is_float = false;
        if self.pos < self.buf.len() && self.buf[self.pos] == b'.' && self.ahead().map(|ch| ch <= b'9' && ch >= b'0').unwrap_or(false) {
            is_float = true;
            self.pos += 1;
            while self.pos < self.buf.len() && self.buf[self.pos] <= b'9' && self.buf[self.pos] >= b'0' {
                self.pos += 1;
            }
        }
        if self.pos < self.buf.len() && (self.buf[self.pos] == b'e' || self.buf[self.pos] == b'E') {
            let mut exp_pos = self.pos + 1;
            if exp_pos < self.buf.len() && (self.buf[exp_pos] == b'+' || self.buf[exp_pos] == b'-') {
                exp_pos += 1;
            }
            if exp_pos < self.buf.len() && self.buf[exp_pos] <= b'9' && self.buf[exp_pos] >= b'0' {
                is_float = true;
                self.pos = exp_pos + 1;
                while self.pos < self.buf.len() && self.buf[self.pos] <= b'9' && self.buf[self.pos] >= b'0' {
                    self.pos += 1;
                }
            }
        }
        if self.pos > start {
            let text = String::from_utf8_lossy(&self.buf[start..self.pos]).to_string();
            let suffix = self.numeric_suffix();
            if is_float {
                return self.float_literal(&text, suffix);
            }
            return self.int_literal(&text, 10, suffix);
        }
        Err(ParserErr::NotNumber.into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_scientific_float_suffixes() {
        let mut parser = Parser::new(b"1.7976931348623157e308f64".to_vec());
        assert_eq!(parser.number().unwrap(), Dynamic::F64(1.7976931348623157e308));

        let mut parser = Parser::new(b"1e-3f32".to_vec());
        assert_eq!(parser.number().unwrap(), Dynamic::F32(1e-3f32));
    }

    #[test]
    fn parses_immediate_closure_call() {
        let mut parser = Parser::new(b"|| { 1i32 }()".to_vec());
        let expr = parser.get_expr().unwrap();
        let ExprKind::Call { obj, params } = expr.kind else {
            panic!("expected closure call, got {expr:?}");
        };
        assert!(params.is_empty());
        let ExprKind::Closure { args, .. } = obj.kind else {
            panic!("expected closure callee, got {obj:?}");
        };
        assert!(args.is_empty());
    }
}
