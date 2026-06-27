use std::{collections::BTreeSet, fmt::Debug};

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
    decl_scopes: Vec<BTreeSet<SmolStr>>,
    impl_depth: usize,
    /// 函数体嵌套深度。>0 表示当前 stmt 处于某个 `fn body` 内,需要拒绝
    /// `fn / struct / impl / const / static` 等顶层声明关键字。
    fn_body_depth: usize,
    /// impl 体嵌套深度。>0 表示当前 stmt 处于 `impl { ... }` 内,
    /// 拒绝嵌套 `struct / impl / const / static`(fn 仍允许,即方法)。
    impl_body_depth: usize,
    /// `match` 块顶层临时变量(__m_scrut_N / __m_done_N / __m_out_N)的后缀计数器,
    /// 用于避免嵌套 match 重名。
    pub(crate) match_counter: usize,
    depth: usize, //当前表达式/语句递归深度,防止恶意深嵌套输入打爆调用栈
    fatal: bool,  //递归过深等不可恢复错误;置位后所有解析入口立即失败,避免回溯重试导致死循环
}

/// 解析递归深度上限。超过即返回 [`ParserErr::TooDeep`],把"栈溢出崩溃"降级为
/// 普通解析错误。
///
/// 单层 `expr_with_min_weight` 帧约 7KB,worker 线程默认栈仅 2MB,因此上限取
/// 128(与 rustc 默认 `recursion_limit` 一致):128×7KB≈0.9MB,在最小栈上仍有
/// 余量,而正常代码极少超过几十层嵌套。
pub const MAX_PARSE_DEPTH: usize = 128;

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
const KEYWORDS: &[&str] = &["true", "false", "null", "let", "if", "else", "for", "in", "while", "loop", "pub", "fn", "struct", "impl", "const", "static", "continue", "return", "break", "match"];

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
        let save_decl_scopes = $self.decl_scopes.clone();
        let save_impl_depth = $self.impl_depth;
        match $method {
            Ok(expr) => Ok(expr),
            // fatal(如递归过深)不可恢复:不回退 pos,直接上抛,避免外层换产生式重试导致死循环
            Err(e) if $self.fatal => Err(e),
            Err(e) => {
                $self.pos = save_pos;
                $self.decl_scopes = save_decl_scopes;
                $self.impl_depth = save_impl_depth;
                Err(e)
            }
        }
    }};
}

#[derive(Debug, thiserror::Error)]
pub enum ParserErr {
    #[error("{message}")]
    Spanned { message: String, span: Span },
}

impl ParserErr {
    /// 构造携带 span 的解析错误。所有 ParserErr 错误都应该走这个构造。
    pub fn new(message: impl Into<String>, span: Span) -> Self {
        Self::Spanned { message: message.into(), span }
    }

    /// 便捷构造:span 是 [pos, pos) 的零长 span,用于"在当前位置报错"的场景。
    pub fn at(message: impl Into<String>, pos: usize) -> Self {
        Self::Spanned { message: message.into(), span: Span::new(pos, pos) }
    }

    pub fn span(&self) -> Span {
        match self {
            Self::Spanned { span, .. } => *span,
        }
    }

    pub fn message(&self) -> &str {
        match self {
            Self::Spanned { message, .. } => message,
        }
    }
}

/// 在 ParserErr 基础上附带 parser 当前光标位置。
/// parse_code 顶层 downcast 此类型,做精确的 LSP-style 错误高亮。
#[derive(Debug, thiserror::Error)]
#[error("{err}")]
pub struct SpannedParseError {
    pub err: ParserErr,
    pub pos: usize,
}

impl SpannedParseError {
    pub fn new(err: ParserErr, pos: usize) -> Self {
        Self { err, pos }
    }
}

impl Parser {
    pub fn new(buf: Vec<u8>) -> Self {
        Self { pos: 0, buf, spans: Vec::new(), decl_scopes: vec![BTreeSet::new()], impl_depth: 0, fn_body_depth: 0, impl_body_depth: 0, match_counter: 0, depth: 0, fatal: false }
    }

    /// 进入一层递归:自增深度并校验上限。配合 [`Parser::exit_depth`] 使用。
    ///
    /// 超限时置 [`Parser::fatal`]:这是不可恢复错误。否则 `try_parse!` 的回溯会
    /// 把 [`ParserErr::TooDeep`] 当成"换个产生式再试",pos 回退后外层循环原地重试,
    /// 形成死循环。置位后 [`Parser::check_fatal`] 让每个解析入口立即失败,错误一路
    /// 通过 `?` 上抛终止解析。
    fn enter_depth(&mut self) -> Result<()> {
        self.depth += 1;
        if self.depth > MAX_PARSE_DEPTH {
            self.depth -= 1;
            self.fatal = true;
            return Err(ParserErr::at("表达式嵌套过深", self.current_pos()).into());
        }
        Ok(())
    }

    fn exit_depth(&mut self) {
        self.depth = self.depth.saturating_sub(1);
    }

    /// 解析入口的快速失败检查:一旦进入 fatal 状态,立即返回错误,阻止任何回溯重试。
    fn check_fatal(&self) -> Result<()> {
        if self.fatal { Err(ParserErr::at("表达式嵌套过深", self.current_pos()).into()) } else { Ok(()) }
    }

    pub(crate) fn push_decl_scope(&mut self) {
        self.decl_scopes.push(BTreeSet::new());
    }

    pub(crate) fn pop_decl_scope(&mut self) {
        if self.decl_scopes.len() > 1 {
            self.decl_scopes.pop();
        }
    }

    fn declare_symbol(&mut self, name: &SmolStr) -> Result<()> {
        if name.is_empty() {
            return Ok(());
        }
        if self.decl_scopes.iter().rev().any(|scope| scope.contains(name)) {
            return Err(ParserErr::at(format!("符号 {} 已经声明", name), self.current_pos()).into());
        }
        self.decl_scopes.last_mut().expect("parser always has a declaration scope").insert(name.clone());
        Ok(())
    }

    pub(crate) fn declare_symbol_in_current_scope(&mut self, name: &SmolStr) -> Result<()> {
        if name.is_empty() {
            return Ok(());
        }
        let scope = self.decl_scopes.last_mut().expect("parser always has a declaration scope");
        if scope.contains(name) {
            return Err(ParserErr::at(format!("符号 {} 已经声明", name), self.current_pos()).into());
        }
        scope.insert(name.clone());
        Ok(())
    }

    fn declare_function_name(&mut self, name: &SmolStr) -> Result<()> {
        if self.impl_depth > 0 { self.declare_symbol_in_current_scope(name) } else { self.declare_symbol(name) }
    }

    fn declare_args(&mut self, args: &[(SmolStr, Type)]) -> Result<()> {
        for (name, _) in args {
            self.declare_symbol(name)?;
        }
        Ok(())
    }

    pub(crate) fn declare_pattern_symbols(&mut self, pat: &Pattern) -> Result<()> {
        match &pat.kind {
            PatternKind::Ident { name, .. } => self.declare_symbol_in_current_scope(name),
            PatternKind::Tuple(items) => {
                for item in items {
                    self.declare_pattern_symbols(item)?;
                }
                Ok(())
            }
            PatternKind::List { elems, .. } => {
                for item in elems {
                    self.declare_pattern_symbols(item)?;
                }
                Ok(())
            }
            PatternKind::Struct { fields, .. } => {
                for (name, sub) in fields {
                    if let Some(sub) = sub {
                        self.declare_pattern_symbols(sub)?;
                    } else {
                        self.declare_symbol_in_current_scope(name)?;
                    }
                }
                Ok(())
            }
            PatternKind::Wildcard | PatternKind::Var { .. } | PatternKind::Literal(_) | PatternKind::Member(_, _) | PatternKind::Idx(_, _) => Ok(()),
        }
    }

    fn function_body(&mut self, args: &[(SmolStr, Type)]) -> Result<Stmt> {
        self.push_decl_scope();
        self.fn_body_depth += 1;
        let result = (|| {
            self.declare_args(args)?;
            self.block()
        })();
        self.fn_body_depth -= 1;
        self.pop_decl_scope();
        result
    }

    fn impl_body(&mut self) -> Result<Stmt> {
        self.push_decl_scope();
        self.impl_depth += 1;
        self.impl_body_depth += 1;
        let result = self.block();
        self.impl_body_depth -= 1;
        self.impl_depth -= 1;
        self.pop_decl_scope();
        result
    }

    pub fn is_eof(&self) -> bool {
        self.pos >= self.buf.len()
    }

    pub fn get(&self) -> Result<u8> {
        //查看当前字符
        self.buf.get(self.pos).cloned().ok_or_else(|| ParserErr::at("输入结束", self.pos).into())
    }

    pub fn take(&mut self, ch: u8) -> Result<()> {
        //如果当前字符为 ch 消费该字符 返回 Ok(())
        if self.buf.get(self.pos).map(|b| *b == ch).unwrap_or(false) {
            self.pos += 1;
            Ok(())
        } else {
            Err(SpannedParseError::new(ParserErr::at(format!("期望字符 {} 实际字符 {}", ch as char, self.buf.get(self.pos as usize).cloned().unwrap_or(0) as char), self.pos), self.pos).into())
        }
    }

    pub fn until(&mut self, ch: u8) -> Result<()> {
        //消费直到指定字符 ch 忽略空白和注释
        self.whitespace()?;
        self.take(ch)
    }

    pub fn ahead(&self) -> Result<u8> {
        //朝前看
        self.buf.get(self.pos + 1).cloned().ok_or_else(|| ParserErr::at("输入结束", self.pos).into())
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
        if self.pos > start { Ok((start, self.pos)) } else { Err(ParserErr::at("未发现期望字符", start).into()) }
    }

    pub fn just(&mut self, pattern: &str) -> Result<()> {
        if self.buf.len() - self.pos >= pattern.len() && self.buf[self.pos..self.pos + pattern.len()].eq(pattern.as_bytes()) {
            self.pos += pattern.len();
            Ok(())
        } else {
            Err(ParserErr::at(format!("期望字符串 {}", pattern), self.pos).into())
        }
    }

    pub fn keyword(&mut self, pattern: &str) -> Result<()> {
        self.just(pattern)?;
        if self.pos < self.buf.len() && !NOT_IDENT.contains(&self.buf[self.pos]) {
            self.pos -= pattern.len();
            return Err(ParserErr::at(format!("期望字符串 {}", pattern), self.pos).into());
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
            Err(ParserErr::at("未关闭的注释", self.pos).into())
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
        if self.get()? != b'"' {
            return Err(ParserErr::at("非字符串", self.current_pos()).into());
        }
        self.pos += 1;
        let mut text_buf = Vec::new();
        while self.pos < self.buf.len() {
            if self.buf[self.pos] == b'\\' {
                //转义字符
                self.pos += 1;
                match self.buf[self.pos] {
                    b'n' => {
                        text_buf.push(b'\n');
                        self.pos += 1;
                    }
                    b'r' => {
                        text_buf.push(b'\r');
                        self.pos += 1;
                    }
                    b't' => {
                        text_buf.push(b'\t');
                        self.pos += 1;
                    }
                    ch @ (b'\\' | b'"') => {
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
                        if self.pos + 2 > self.buf.len() {
                            return Err(anyhow!("非法 \\x 转义：需要 2 位十六进制"));
                        }
                        let start = self.pos;
                        self.pos += 2;
                        let hex = &self.buf[start..self.pos];
                        if hex.iter().any(|b| !b.is_ascii_hexdigit()) {
                            return Err(anyhow!("非法 \\x 转义：仅允许十六进制字符"));
                        }
                        let code = u32::from_str_radix(String::from_utf8_lossy(hex).as_ref(), 16)?;
                        if code > 0xFF {
                            return Err(anyhow!("\\x 转义值 0x{:02X} 超出 0xFF", code));
                        }
                        text_buf.push(code as u8);
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
        Err(ParserErr::at("未关闭字符串", self.pos).into())
    }

    pub fn text(&mut self) -> Result<SmolStr> {
        if self.get()? == b'r' && [b'#', b'"'].contains(&self.ahead()?) {
            self.pos += 1;
            let mut end = String::from("\"");
            while self.buf[self.pos] == b'#' {
                end.push('#');
                self.pos += 1;
            }
            if self.get()? != b'"' {
                return Err(ParserErr::at("非法的原始字符串", self.current_pos()).into());
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
            if !ty.is_native() {
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
        // 默认整数类型为 I64:常见的较大十进制数(如 30 亿)不再静默回绕成负数。
        let ty = suffix.unwrap_or(Type::I64);
        // 负号由一元运算符单独解析,这里的字面量恒为非负,因此统一解析成 u128。
        let magnitude = u128::from_str_radix(digits, radix).map_err(|_| anyhow!("整数字面量 {} 超出可表示范围", digits))?;
        let (signed, bits) = match ty {
            Type::I8 => (true, 8u32),
            Type::I16 => (true, 16),
            Type::I32 => (true, 32),
            Type::I64 => (true, 64),
            Type::U8 => (false, 8),
            Type::U16 => (false, 16),
            Type::U32 => (false, 32),
            Type::U64 => (false, 64),
            Type::F16 => return Ok(Dynamic::F16(dynamic::f64_to_f16(magnitude as f64))),
            Type::F32 => return Ok(Dynamic::F32(magnitude as f32)),
            Type::F64 => return Ok(Dynamic::F64(magnitude as f64)),
            ty => return Err(anyhow!("{:?} 不能作为数字后缀", ty)),
        };
        let unsigned_max = (1u128 << bits) - 1;
        // 十进制按数值语义判界(有符号允许到 |MIN|,即 2^(bits-1),以支持 -128i8、i64::MIN);
        // 十六/八/二进制按位模式语义判界,允许写满整型位宽(如 0xFFFFFFFF 仍是合法的位掩码)。
        let max_allowed = if radix == 10 { if signed { unsigned_max / 2 + 1 } else { unsigned_max } } else { unsigned_max };
        if magnitude > max_allowed {
            return Err(anyhow!("整数字面量 {} 超出 {:?} 的范围", digits, ty));
        }
        Ok(match ty {
            Type::I8 => Dynamic::I8(magnitude as i8),
            Type::I16 => Dynamic::I16(magnitude as i16),
            Type::I32 => Dynamic::I32(magnitude as i32),
            Type::I64 => Dynamic::I64(magnitude as i64),
            Type::U8 => Dynamic::U8(magnitude as u8),
            Type::U16 => Dynamic::U16(magnitude as u16),
            Type::U32 => Dynamic::U32(magnitude as u32),
            Type::U64 => Dynamic::U64(magnitude as u64),
            _ => unreachable!(),
        })
    }

    fn float_literal(&mut self, digits: &str, suffix: Option<Type>) -> Result<Dynamic> {
        let value: f64 = digits.parse()?;
        if let Some(ref ty) = suffix {
            // 整数类后缀:校验是否在目标范围内。NaN / Inf 一律拒绝;
            // 不允许小数部分。F16/F32/F64 不做范围 / 整数性校验。
            let is_int_suffix = matches!(ty, Type::I8 | Type::I16 | Type::I32 | Type::I64 | Type::U8 | Type::U16 | Type::U32 | Type::U64);
            if is_int_suffix {
                let (min, max): (f64, f64) = match ty {
                    Type::I8 => (i8::MIN as f64, i8::MAX as f64),
                    Type::I16 => (i16::MIN as f64, i16::MAX as f64),
                    Type::I32 => (i32::MIN as f64, i32::MAX as f64),
                    Type::I64 => (i64::MIN as f64, i64::MAX as f64),
                    Type::U8 => (0.0, u8::MAX as f64),
                    Type::U16 => (0.0, u16::MAX as f64),
                    Type::U32 => (0.0, u32::MAX as f64),
                    Type::U64 => (0.0, u64::MAX as f64),
                    _ => unreachable!(),
                };
                if !value.is_finite() || value < min || value > max || value.fract() != 0.0 {
                    return Err(anyhow!("浮点字面量 {:?} 超出 {:?} 范围", value, ty));
                }
            } else if !value.is_finite() {
                return Err(anyhow!("非法浮点字面量: {:?}", value));
            }
        }
        Ok(match suffix.unwrap_or(Type::F32) {
            Type::I8 => Dynamic::I8(value as i8),
            Type::I16 => Dynamic::I16(value as i16),
            Type::I32 => Dynamic::I32(value as i32),
            Type::I64 => Dynamic::I64(value as i64),
            Type::U8 => Dynamic::U8(value as u8),
            Type::U16 => Dynamic::U16(value as u16),
            Type::U32 => Dynamic::U32(value as u32),
            Type::U64 => Dynamic::U64(value as u64),
            Type::F16 => Dynamic::F16(dynamic::f64_to_f16(value)),
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
        Err(ParserErr::at("非数字", start).into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse_all(code: &str) -> Result<Vec<Stmt>> {
        let mut parser = Parser::new(code.as_bytes().to_vec());
        let mut stmts = Vec::new();
        loop {
            match parser.stmt(false) {
                Ok(stmt) => stmts.push(stmt),
                Err(err) => {
                    if parser.is_eof() {
                        return Ok(stmts);
                    }
                    return Err(err);
                }
            }
        }
    }

    // 调试构建里单帧约 16KB,病态深嵌套即便有深度守卫也会在守卫触发"之前"打爆
    // 测试线程默认 2MB 栈;因此用大栈线程跑,验证守卫确实返回 TooDeep(而非崩溃)。
    // 生产是 release 构建,单帧仅数 KB,128 层上限在 8MB 主栈上余量充足。
    fn run_with_big_stack(f: impl FnOnce() + Send + 'static) {
        std::thread::Builder::new().stack_size(64 * 1024 * 1024).spawn(f).unwrap().join().unwrap();
    }

    #[test]
    fn deeply_nested_parens_error_instead_of_stack_overflow() {
        run_with_big_stack(|| {
            let depth = MAX_PARSE_DEPTH + 50;
            let code = format!("{}1{}", "(".repeat(depth), ")".repeat(depth));
            let mut parser = Parser::new(code.into_bytes());
            let err = parser.get_expr().unwrap_err();
            assert!(err.to_string().contains("嵌套过深"), "got: {err}");
        });
    }

    #[test]
    fn deeply_nested_blocks_error_instead_of_stack_overflow() {
        run_with_big_stack(|| {
            let depth = MAX_PARSE_DEPTH + 50;
            let code = format!("fn f() {}{}{}", "{".repeat(depth), "1", "}".repeat(depth));
            let err = parse_all(&code).unwrap_err();
            assert!(err.to_string().contains("嵌套过深"), "got: {err}");
        });
    }

    #[test]
    fn normal_nesting_within_limit_parses() {
        // 远低于上限的正常嵌套不受影响
        let code = format!("{}1{}", "(".repeat(32), ")".repeat(32));
        let mut parser = Parser::new(code.into_bytes());
        parser.get_expr().unwrap();
    }

    fn parse_literal(code: &str) -> Result<Dynamic> {
        let mut parser = Parser::new(code.as_bytes().to_vec());
        match parser.get_expr()?.kind {
            crate::ExprKind::Value(value) => Ok(value),
            other => Err(anyhow!("不是字面量: {:?}", other)),
        }
    }

    #[test]
    fn unsuffixed_integer_defaults_to_i64() {
        assert_eq!(parse_literal("5").unwrap(), Dynamic::I64(5));
        // 30 亿:旧的 I32 默认会静默回绕成负数,I64 默认保留正确数值
        assert_eq!(parse_literal("3000000000").unwrap(), Dynamic::I64(3000000000));
    }

    #[test]
    fn out_of_range_integer_literals_error() {
        // 超出 u64,连 i128 解析也容纳不下 → 报错而非回绕
        assert!(parse_literal("99999999999999999999999999999999999999999").is_err());
        // 窄后缀越界
        assert!(parse_literal("255i8").unwrap_err().to_string().contains("超出"));
        assert!(parse_literal("70000i16").unwrap_err().to_string().contains("超出"));
        assert!(parse_literal("256u8").unwrap_err().to_string().contains("超出"));
    }

    #[test]
    fn signed_min_magnitude_literals_allowed() {
        // -128i8 由一元负号 + 字面量 128 组成,字面量 128 必须可被接受
        assert_eq!(parse_literal("128i8").unwrap(), Dynamic::I8(-128));
        assert_eq!(parse_literal("9223372036854775808").unwrap(), Dynamic::I64(i64::MIN));
    }

    #[test]
    fn hex_literals_keep_bit_pattern() {
        // 十六进制按位模式语义:0xFFFFFFFF 是合法掩码,默认 I64 容纳为正值
        assert_eq!(parse_literal("0xFFFFFFFF").unwrap(), Dynamic::I64(0xFFFFFFFF));
        // 写满目标位宽的掩码允许通过(0xFF -> i8 的 -1)
        assert_eq!(parse_literal("0xFFi8").unwrap(), Dynamic::I8(-1));
        assert_eq!(parse_literal("0xFFFFFFFFu32").unwrap(), Dynamic::U32(u32::MAX));
    }

    // 把表达式 AST 渲染成 S 表达式,用来锁定优先级/结合性(expr.rs 手写树旋转逻辑)。
    fn shape(code: &str) -> String {
        let mut parser = Parser::new(code.as_bytes().to_vec());
        let expr = parser.get_expr().expect("parse");
        fmt_shape(&expr)
    }

    fn binop_sym(op: &crate::BinaryOp) -> &'static str {
        use crate::BinaryOp::*;
        match op {
            Add => "+",
            Sub => "-",
            Mul => "*",
            Div => "/",
            Mod => "%",
            Shl => "<<",
            Shr => ">>",
            BitAnd => "&",
            BitOr => "|",
            BitXor => "^",
            Assign => "=",
            AddAssign => "+=",
            Eq => "==",
            Ne => "!=",
            Lt => "<",
            Gt => ">",
            Le => "<=",
            Ge => ">=",
            And => "&&",
            Or => "||",
            Idx => "idx",
            other => {
                let _ = other;
                "?"
            }
        }
    }

    fn fmt_shape(expr: &crate::Expr) -> String {
        use crate::ExprKind::*;
        match &expr.kind {
            Value(v) => format!("{:?}", v).replace("I64(", "").replace("I32(", "").trim_end_matches(')').to_string(),
            Ident(name) => name.to_string(),
            Unary { op, value } => {
                let s = if matches!(op, crate::UnaryOp::Neg) { "-" } else { "!" };
                format!("({} {})", s, fmt_shape(value))
            }
            Binary { left, op, right } => format!("({} {} {})", binop_sym(op), fmt_shape(left), fmt_shape(right)),
            Range { start, stop, inclusive } => format!("({} {} {})", if *inclusive { "..=" } else { ".." }, fmt_shape(start), fmt_shape(stop)),
            Typed { value, ty } => format!("(as {} {:?})", fmt_shape(value), ty),
            other => format!("{:?}", other),
        }
    }

    #[test]
    fn precedence_and_associativity_golden() {
        // 乘法高于加法
        assert_eq!(shape("1 + 2 * 3"), "(+ 1 (* 2 3))");
        assert_eq!(shape("1 * 2 + 3"), "(+ (* 1 2) 3)");
        // 同级左结合
        assert_eq!(shape("1 - 2 - 3"), "(- (- 1 2) 3)");
        assert_eq!(shape("8 / 4 / 2"), "(/ (/ 8 4) 2)");
        // 移位低于加法
        assert_eq!(shape("2 + 3 << 4"), "(<< (+ 2 3) 4)");
        // 位运算优先级:& 高于 ^ 高于 |
        assert_eq!(shape("1 | 2 ^ 3 & 4"), "(| 1 (^ 2 (& 3 4)))");
        // 比较低于算术
        assert_eq!(shape("1 + 2 == 3"), "(== (+ 1 2) 3)");
        // 逻辑:&& 高于 ||
        assert_eq!(shape("a && b || c"), "(|| (&& a b) c)");
        // 一元高于乘法
        assert_eq!(shape("-a * b"), "(* (- a) b)");
        assert_eq!(shape("!a == b"), "(== (! a) b)");
    }

    #[test]
    fn assignment_range_and_as_precedence_golden() {
        // 赋值最低优先级,右结合
        assert_eq!(shape("a = b + c"), "(= a (+ b c))");
        assert_eq!(shape("a = b = c"), "(= a (= b c))");
        assert_eq!(shape("a = b = c = d"), "(= a (= b (= c d)))");
        // 复合赋值
        assert_eq!(shape("a += b * c"), "(+= a (* b c))");
        // range 边界是完整算术表达式(上界按完整子表达式解析)
        assert_eq!(shape("1 + 1 .. n * 2"), "(.. (+ 1 1) (* n 2))");
        assert_eq!(shape("0 ..= n - 1"), "(..= 0 (- n 1))");
        // as 紧绑定到操作数,优先级高于二元算术(Rust 语义)
        assert_eq!(shape("a + b as i64"), "(+ a (as b I64))");
        assert_eq!(shape("a as i64 + b"), "(+ (as a I64) b)");
        assert_eq!(shape("(a + b) as i64"), "(as (+ a b) I64)");
    }

    // 轻量 fuzz:用确定性 PRNG 生成大量随机/半结构化输入喂给解析器,断言它永远
    // 不 panic、不崩溃(返回 Ok 或 Err 都可),也不卡死(B2 的深度守卫保证有界)。
    // 在大栈线程上跑,避免深嵌套合法解析在调试构建里耗尽测试线程的 2MB 栈。
    #[test]
    fn parser_never_panics_on_random_input() {
        run_with_big_stack(|| {
            const FRAGMENTS: &[&str] = &[
                "fn", "let", "if", "else", "for", "in", "while", "return", "struct", "impl", "pub", "(", ")", "{", "}", "[", "]", "<", ">", "+", "-", "*", "/", "%", "=", "==", "&&", "||", "..", "..=", "as", "i32",
                "u64", "f64", ".", ",", ";", ":", "::", "x", "0", "1", "255i8", "0xFF", "\"s\"", "true", "null", "|a|", "->",
            ];
            // xorshift64* 确定性 PRNG
            let mut state: u64 = 0x9E3779B97F4A7C15;
            let mut next = || {
                state ^= state >> 12;
                state ^= state << 25;
                state ^= state >> 27;
                state = state.wrapping_mul(0x2545F4914F6CDD1D);
                state
            };

            for _ in 0..4000 {
                let mut code = String::new();
                let tokens = (next() % 40) as usize;
                for _ in 0..tokens {
                    code.push_str(FRAGMENTS[(next() as usize) % FRAGMENTS.len()]);
                    if next() % 2 == 0 {
                        code.push(' ');
                    }
                }
                // 解析全程不应 panic;parse_all 返回 Ok/Err 均可接受。
                let result = std::panic::catch_unwind(|| {
                    let mut parser = Parser::new(code.clone().into_bytes());
                    let mut count = 0;
                    loop {
                        match parser.stmt(false) {
                            Ok(_) => {
                                count += 1;
                                if parser.is_eof() || count > 1000 {
                                    break;
                                }
                            }
                            Err(_) => break,
                        }
                    }
                });
                assert!(result.is_ok(), "parser panicked on input: {:?}", code);
            }
        });
    }

    #[test]
    fn allows_local_name_to_shadow_prior_function() {
        parse_all(
            r#"
            fn chunk_id(x, y) {
                x + y
            }

            fn open() {
                let chunk_id = 1;
                chunk_id
            }
            "#,
        )
        .unwrap();
    }

    #[test]
    fn rejects_duplicate_function_args() {
        let err = parse_all("fn open(value, value) { value }").unwrap_err();
        assert!(err.to_string().contains("符号 value 已经声明"));
    }

    #[test]
    fn rejects_duplicate_local_let_names() {
        let err = parse_all(
            r#"
            fn open() {
                let value = 1;
                let value = 2;
                value
            }
            "#,
        )
        .unwrap_err();
        assert!(err.to_string().contains("符号 value 已经声明"));
    }

    #[test]
    fn allows_same_method_name_in_different_impl_blocks() {
        parse_all(
            r#"
            struct A {}
            struct B {}

            impl A {
                fn zero() { 0 }
            }

            impl B {
                fn zero() { 0 }
            }
            "#,
        )
        .unwrap();
    }

    #[test]
    fn rejects_nested_fn_inside_function_body() {
        let err = parse_all("fn outer() { fn inner() { 1 } }").unwrap_err();
        assert!(err.to_string().contains("函数体内不能定义"), "got: {err}");
    }

    #[test]
    fn rejects_nested_struct_inside_function_body() {
        let err = parse_all("fn outer() { struct S { x: i32 } S{x: 1} }").unwrap_err();
        assert!(err.to_string().contains("函数体内不能定义"), "got: {err}");
    }

    #[test]
    fn rejects_nested_const_inside_function_body() {
        let err = parse_all("fn outer() { const K = 1 } K").unwrap_err();
        assert!(err.to_string().contains("函数体内不能定义"), "got: {err}");
    }

    #[test]
    fn hex_escape_at_end_of_string_preserves_byte() {
        let mut p = Parser::new(br#""abc\x41""#.to_vec());
        let s = p.string().unwrap();
        assert_eq!(s.as_str(), "abcA");
    }

    #[test]
    fn hex_escape_truncated_reports_clear_error() {
        let mut p = Parser::new(br#""abc\x""#.to_vec());
        let err = p.string().unwrap_err();
        assert!(err.to_string().contains("\\x"), "got: {err}");
    }

    #[test]
    fn hex_escape_non_hex_char_reports_clear_error() {
        let mut p = Parser::new(br#""abc\xZZ""#.to_vec());
        let err = p.string().unwrap_err();
        assert!(err.to_string().contains("\\x"), "got: {err}");
    }

    #[test]
    fn else_with_invalid_body_reports_error() {
        // 让 block() 在 else 后失败:解析到 '}' 紧跟一个无闭的 '{' 触发 "not code block"
        let err = parse_all("fn f() { if true { 1 } else }").unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("not code block") || msg.contains("未结束的"), "got: {msg}");
    }

    #[test]
    fn float_literal_with_int_suffix_out_of_range_errors() {
        let mut p = Parser::new(b"1e30u8".to_vec());
        let err = p.number().unwrap_err();
        assert!(err.to_string().contains("超出"), "got: {err}");
    }

    #[test]
    fn float_literal_with_int_suffix_fractional_errors() {
        let mut p = Parser::new(b"1.5i32".to_vec());
        let err = p.number().unwrap_err();
        assert!(err.to_string().contains("超出"), "got: {err}");
    }

    #[test]
    fn float_literal_with_float_suffix_accepts_fractional() {
        let mut p = Parser::new(b"1e-3f32".to_vec());
        assert!(matches!(p.number().unwrap(), Dynamic::F32(v) if (v - 1e-3).abs() < 1e-8));
    }

    #[test]
    fn allows_closure_inside_function_body() {
        parse_all("fn outer() { let f = |x: i32| { x + 1 }; f(1) }").unwrap();
    }

    #[test]
    fn rejects_const_inside_impl_body() {
        let err = parse_all("struct S {}\nimpl S { const K = 1 }").unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("impl 体内不能定义") && msg.contains("const"), "got: {msg}");
    }

    #[test]
    fn allows_fn_inside_impl_body() {
        parse_all("struct S {}\nimpl S { pub fn m(self: S) { 1 } }").unwrap();
    }

    #[test]
    fn parser_err_carries_span() {
        // 用 fn 重复声明触发 DuplicateSymbol,ParserErr span 应当指向重复位置。
        let src = "fn f() {}\nfn f() {}\n";
        let err = parse_all(src).unwrap_err();
        eprintln!("err display: {err}");
        let downcast = err.downcast_ref::<ParserErr>().expect("ParserErr");
        eprintln!("message: {}", downcast.message());
        eprintln!("span: {:?}", downcast.span());
        assert!(downcast.message().contains("f"));
        // span 应当在文件范围内
        assert!(downcast.span().start < src.len());
    }

    #[test]
    fn block_as_let_value_is_expression() {
        parse_all("pub fn f() { let x = { let y = 1; y + 1 }; x }").unwrap();
    }

    #[test]
    fn dict_still_takes_priority_over_block() {
        // dict 仍是 dict,不能误判为 block
        parse_all("pub fn f() { let d = { key: 1 }; d }").unwrap();
    }

    #[test]
    fn list_pattern_with_rest_parses() {
        parse_all("pub fn f(items) { let [first, ..rest] = items; first }").unwrap();
    }

    #[test]
    fn list_pattern_with_only_rest_parses() {
        parse_all("pub fn f(items) { let [..all] = items; all }").unwrap();
    }

    #[test]
    fn take_error_carries_precise_pos() {
        // take 失败时,SpannedParseError.pos 应该指向缺失字符的位置,
        // 而不是 parse_code 默认的 parser.current_pos。
        use crate::SpannedParseError;
        let mut p = Parser::new(b"ab".to_vec());
        let pos_before = p.current_pos();
        let err = p.take(b'c').unwrap_err();
        let spanned = err.downcast_ref::<SpannedParseError>().expect("take should wrap in SpannedParseError");
        // take 在 pos_before 处失败,期望 pos == pos_before(0)
        assert_eq!(spanned.pos, pos_before);
    }

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

    #[test]
    fn parses_empty_tuple_expression() {
        let mut parser = Parser::new(b"()".to_vec());
        let expr = parser.get_expr().unwrap();
        let ExprKind::Tuple(items) = expr.kind else {
            panic!("expected empty tuple, got {expr:?}");
        };
        assert!(items.is_empty());
    }

    #[test]
    fn parses_explicit_generic_function_call() {
        let mut parser = Parser::new(b"value::<4>()".to_vec());
        let expr = parser.get_expr().unwrap();
        let ExprKind::Call { obj, params } = expr.kind else {
            panic!("expected function call, got {expr:?}");
        };
        assert!(params.is_empty());
        let ExprKind::Generic { obj, params } = obj.kind else {
            panic!("expected generic callee, got {obj:?}");
        };
        assert!(matches!(obj.kind, ExprKind::Ident(name) if name.as_str() == "value"));
        assert!(matches!(params.as_slice(), [Type::ConstInt(4)]));
    }

    #[test]
    fn parses_import_top_level_declaration() {
        // 顶层 import 声明:`import "module";` 和 `import "module", "path";`。
        let stmts = parse_all(r#"import "foo";"#).expect("parse import decl");
        assert_eq!(stmts.len(), 1);
        let StmtKind::Import { module, path, is_pub } = &stmts[0].kind else {
            panic!("expected StmtKind::Import, got {:?}", stmts[0].kind);
        };
        assert_eq!(module.as_str(), "foo");
        assert_eq!(path.as_str(), "foo.zs", "省略路径时默认 <module>.zs");
        assert!(!*is_pub);

        let stmts = parse_all(r#"import "foo", "bar.zs";"#).expect("parse import decl with path");
        let StmtKind::Import { module, path, .. } = &stmts[0].kind else {
            panic!("expected StmtKind::Import, got {:?}", stmts[0].kind);
        };
        assert_eq!(module.as_str(), "foo");
        assert_eq!(path.as_str(), "bar.zs");

        let stmts = parse_all(r#"pub import "foo";"#).expect("parse pub import");
        let StmtKind::Import { module, is_pub, .. } = &stmts[0].kind else {
            panic!("expected StmtKind::Import, got {:?}", stmts[0].kind);
        };
        assert_eq!(module.as_str(), "foo");
        assert!(*is_pub);
    }

    #[test]
    fn import_call_form_is_still_recognized_as_expression() {
        // 兼容旧 `import("name", "path");` 函数调用形式 —— 仍要能解析
        // 成 `Expr(Call(import, ...))`,不应当成 import 顶层声明。
        // 因为 `import` 后面紧跟 `(`(不是空白+字符串),peek 走 fall-through。
        let stmts = parse_all(r#"import("foo", "foo.zs");"#).expect("parse import call");
        assert_eq!(stmts.len(), 1);
        let StmtKind::Expr(expr, _) = &stmts[0].kind else {
            panic!("expected StmtKind::Expr, got {:?}", stmts[0].kind);
        };
        let ExprKind::Call { obj, params } = &expr.kind else {
            panic!("expected ExprKind::Call, got {expr:?}");
        };
        let ExprKind::Ident(name) = &obj.kind else {
            panic!("expected ident callee, got {:?}", obj.kind);
        };
        assert_eq!(name.as_str(), "import");
        assert_eq!(params.len(), 2);
    }

    #[test]
    fn parses_bigfloat_cmp_context_segment() {
        let code = r#"
            struct BigFloat<N> { data: [u32; N], exp: i32, sign: bool }

            impl BigFloat<N> {
                fn abs_cmp(self: BigFloat<N>, rhs: BigFloat<N>) {
                    let self_high = self.exp + ((N - 1) as i32);
                    let rhs_high = rhs.exp + ((N - 1) as i32);
                    let high = if self_high >= rhs_high { self_high } else { rhs_high };
                    let low = if self.exp <= rhs.exp { self.exp } else { rhs.exp };
                    let result = 0i32;
                    let power = high;

                    while power >= low && result == 0i32 {
                        let a_idx = power - self.exp;
                        let b_idx = power - rhs.exp;
                        let a_limb = 0u32;
                        let b_limb = 0u32;

                        if a_idx >= 0i32 && a_idx < (N as i32) {
                            a_limb = self.data[a_idx as u32];
                        }
                        if b_idx >= 0i32 && b_idx < (N as i32) {
                            b_limb = rhs.data[b_idx as u32];
                        }

                        if a_limb > b_limb {
                            result = 1i32;
                        } else if a_limb < b_limb {
                            result = -1i32;
                        }

                        power -= 1i32;
                    }

                    result
                }

                pub fn cmp(self: BigFloat<N>, rhs: BigFloat<N>) {
                    if self.is_zero() && rhs.is_zero() {
                        0i32
                    } else if self.sign != rhs.sign {
                        if self.sign { -1i32 } else { 1i32 }
                    } else {
                        let cmp = self.abs_cmp(rhs);
                        if self.sign { -cmp } else { cmp }
                    }
                }
            }
            "#;
        parse_all(code).unwrap();
    }

    #[test]
    fn parses_bigfloat_file() {
        let code = include_str!("../../zusts/bigfloat.zs");
        parse_all(code).unwrap();
    }
}
