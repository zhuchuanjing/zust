use crate::{Dynamic, ZOnce};

use anyhow::{Result, anyhow};
use indexmap::IndexMap;
use parking_lot::RwLock;
use smol_str::SmolStr;
use std::sync::Arc;

/// 序列化方向:把一个 `Dynamic` 写成 YAML 文本。
///
/// 输出风格刻意保持"LLM 友好":
/// - 默认 block style,序列用 `- item`,映射用 `key: value`。
/// - 字符串尽量裸出;只有会改变语义或被误读时才加双引号。
/// - 多行字符串用 `|` literal block scalar,保留换行,避免 `\n` 转义噪声。
/// - 不输出 anchor / alias / 显式 `!!str` 标签,这些东西 LLM 经常搞错。
pub trait ToYaml {
    fn to_yaml(&self, buf: &mut String);
}

/// 反序列化方向:从 YAML 文本里解析出一个 `Dynamic`。
///
/// 只覆盖 LLM 常用 YAML 子集:标量(整数/浮点/字符串/bool/null)、block 风格
/// mapping / sequence、有限的 flow 风格(嵌套用得不多)、注释。anchors / 复杂 tag
/// / 折叠 scalar(`>`)/ 集合类型提示(`!!set` 等)不在支持范围内,遇到直接报错。
pub trait FromYaml: Sized {
    fn from_yaml(buf: &[u8]) -> Result<(Self, usize)>;
}

// ---- 序列化 -----------------------------------------------------------

/// 判断字符串是否"必须"加引号,避免和 YAML 标量字面量冲突。
///
/// 规则:
/// - 空串;
/// - 首字符是 reserved indicator (`!`, `&`, `*`, `?`, `|`, `>`, `'`, `"`, `%`,
///   `@`, `\``, `#`, `,`, `[`, `]`, `{`, `}`, `:`, `-`);
/// - 包含 `: ` / ` #`(在普通位置会触发 mapping / 注释);
/// - 看起来像数字、bool、null、~ 等保留字;
/// - 包含控制字符;
/// - 是单独的 `---` / `...` 这种文档分隔符。
///
/// LLM 经常输出类似 `"true"` 或 `"123"` 这样的"看起来是数字但其实是字符串"的
/// 值,因此需要在"字符串内容本身合法但和标量字面量撞"时也强制加引号。
fn yaml_string_needs_quoting(s: &str) -> bool {
    if s.is_empty() {
        return true;
    }
    // 文档分隔符不能裸出
    if s == "---" || s == "..." {
        return true;
    }
    let bytes = s.as_bytes();
    // 首字符限定
    const RESERVED_FIRST: &[u8] = b"!\"&'*|>%@`,[]{}#?:";
    if RESERVED_FIRST.contains(&bytes[0]) {
        return true;
    }
    // 数字 / null / bool 字面量全比较一遍,大小写敏感 (YAML 1.2)。
    if matches!(
        s,
        "true"
            | "false"
            | "null"
            | "Null"
            | "NULL"
            | "~"
            | "yes"
            | "no"
            | "on"
            | "off"
            | "True"
            | "False"
            | "TRUE"
            | "FALSE"
            | "YES"
            | "NO"
            | "ON"
            | "OFF"
    ) {
        return true;
    }
    // 像数字的字符串也需要引号,避免下游解析误读。
    if looks_like_number(s) {
        return true;
    }
    // 走一遍内容,处理 ": "、首尾空格、tab、控制字符、"#" 前有空格
    for (idx, b) in bytes.iter().enumerate() {
        match *b {
            b'\n' | b'\t' => return true,
            0..=0x1f => return true,
            b':' if idx + 1 < bytes.len() && (bytes[idx + 1] == b' ' || bytes[idx + 1] == b'\t') => return true,
            b'#' if idx > 0 && (bytes[idx - 1] == b' ' || bytes[idx - 1] == b'\t') => return true,
            _ => {}
        }
    }
    // 首字符是 `-` 或 `?` 时为了避免被解析成 sequence / complex key,也加引号。
    // (已在 RESERVED_FIRST 包含)
    // 末尾空格 / tab 需要引号,否则会被 trim 掉。
    if matches!(bytes.last(), Some(b' ' | b'\t')) {
        return true;
    }
    false
}

fn looks_like_number(s: &str) -> bool {
    // 包含 `.` / `e` / `E` 当作浮点尝试;否则当整数。允许前导 `+` / `-`。
    if s.eq_ignore_ascii_case("inf") || s.eq_ignore_ascii_case("nan") || s.eq_ignore_ascii_case(".inf") || s.eq_ignore_ascii_case(".nan") {
        return true;
    }
    if s.contains('.') || s.contains('e') || s.contains('E') {
        return s.parse::<f64>().is_ok()
    } else {
        return s.parse::<i64>().is_ok()
    }
}

/// 把字符串内容里必须转义的字符按 YAML 双引号 scalar 的规则写出。
fn yaml_quote_string(s: &str, buf: &mut String) {
    buf.push('"');
    for ch in s.chars() {
        match ch {
            '\\' => buf.push_str("\\\\"),
            '"' => buf.push_str("\\\""),
            '\n' => buf.push_str("\\n"),
            '\r' => buf.push_str("\\r"),
            '\t' => buf.push_str("\\t"),
            '\0' => buf.push_str("\\0"),
            '\x08' => buf.push_str("\\b"),
            '\x0c' => buf.push_str("\\f"),
            c if (c as u32) < 0x20 => {
                use std::fmt::Write;
                let _ = write!(buf, "\\u{:04x}", c as u32);
            }
            c => buf.push(c),
        }
    }
    buf.push('"');
}

/// 多行字符串用 `|` literal block scalar 输出。
/// `|` 之后的 chomping indicator 用 `+` 表示保留末尾所有换行;LLM 解析 YAML 时
/// 这个最不会出错,代价是多一两行。块缩进以首个非空行的缩进为基准。
fn yaml_block_string(s: &str, indent: usize, buf: &mut String) {
    let pad = " ".repeat(indent);
    buf.push_str("|+\n");
    let mut count = 0usize;
    for ch in s.chars() {
        buf.push_str(&pad);
        if ch == '\n' {
            buf.push('\n');
            count = 0;
        } else {
            buf.push(ch);
            count += 1;
        }
    }
    if count > 0 {
        buf.push('\n');
    }
}

/// 把一个字符串写出。优先裸出;必要时双引号;含换行时用 `|` block。
fn yaml_write_string(s: &str, indent: usize, buf: &mut String) {
    if s.contains('\n') {
        yaml_block_string(s, indent, buf);
        return;
    }
    if yaml_string_needs_quoting(s) {
        yaml_quote_string(s, buf);
    } else {
        buf.push_str(s);
    }
}

fn yaml_write_key(key: &str, buf: &mut String) {
    // key 不强制缩进风格;统一处理成"裸出优先,不行就加引号"。
    if yaml_string_needs_quoting(key) {
        yaml_quote_string(key, buf);
    } else {
        buf.push_str(key);
    }
}

impl ToYaml for &str {
    fn to_yaml(&self, buf: &mut String) {
        yaml_write_string(self, 0, buf);
    }
}

impl ToYaml for i64 {
    fn to_yaml(&self, buf: &mut String) {
        buf.push_str(&self.to_string());
    }
}

impl ToYaml for Dynamic {
    fn to_yaml(&self, buf: &mut String) {
        self.to_yaml_indent(0, buf);
    }
}

impl Dynamic {
    /// 把当前值以 YAML 写出,根节点的缩进是 `indent`(通常传 0)。
    /// 顶层若不是 mapping / sequence,需要补一行 `---` 开头,避免直接写裸字符串
    /// 看起来像单 key 的文档。这是 LLM 阅读时一个常见的边界 bug。
    fn to_yaml_indent(&self, indent: usize, buf: &mut String) {
        match self {
            Self::Iter { .. } => {}
            Self::Null => buf.push_str("null"),
            Self::Bool(b) => buf.push_str(if *b { "true" } else { "false" }),
            Self::F16(bits) => buf.push_str(&crate::f16_to_f64(*bits).to_string()),
            Self::F32(f) => buf.push_str(&f.to_string()),
            Self::F64(f) => buf.push_str(&f.to_string()),
            Self::I8(i) => buf.push_str(&i.to_string()),
            Self::I16(i) => buf.push_str(&i.to_string()),
            Self::I32(i) => buf.push_str(&i.to_string()),
            Self::I64(i) => buf.push_str(&i.to_string()),
            Self::U8(i) => buf.push_str(&i.to_string()),
            Self::U16(i) => buf.push_str(&i.to_string()),
            Self::U32(i) => buf.push_str(&i.to_string()),
            Self::U64(i) => buf.push_str(&i.to_string()),
            Self::String(s) => yaml_write_string(s.as_str(), indent, buf),
            Self::StringBuf(s) => yaml_write_string(s.as_str(), indent, buf),
            Self::Bytes(vec) => yaml_write_seq(vec.iter().map(|b| Dynamic::U8(*b)), indent, buf),
            Self::VecI8(vec) => yaml_write_seq(vec.iter().map(|v| Dynamic::I8(*v)), indent, buf),
            Self::VecU16(vec) => yaml_write_seq(vec.iter().map(|v| Dynamic::U16(*v)), indent, buf),
            Self::VecI16(vec) => yaml_write_seq(vec.iter().map(|v| Dynamic::I16(*v)), indent, buf),
            Self::VecU32(vec) => yaml_write_seq(vec.iter().map(|v| Dynamic::U32(*v)), indent, buf),
            Self::VecI32(vec) => yaml_write_seq(vec.iter().map(|v| Dynamic::I32(*v)), indent, buf),
            Self::VecF32(vec) => yaml_write_seq(vec.iter().map(|v| Dynamic::F32(*v)), indent, buf),
            Self::VecU64(vec) => yaml_write_seq(vec.iter().map(|v| Dynamic::U64(*v)), indent, buf),
            Self::VecI64(vec) => yaml_write_seq(vec.iter().map(|v| Dynamic::I64(*v)), indent, buf),
            Self::VecF64(vec) => yaml_write_seq(vec.iter().map(|v| Dynamic::F64(*v)), indent, buf),
            Self::List(items) => {
                let items = items.read().clone();
                yaml_write_seq(items.into_iter(), indent, buf);
            }
            Self::Map(map) => {
                let map = map.read().clone();
                yaml_write_map(map.into_iter(), indent, buf);
            }
            Self::StructView { .. } | Self::StructOwned { .. } => {
                let keys = self.keys();
                if keys.is_empty() {
                    buf.push_str("{}\n");
                    return;
                }
                for key in keys.iter() {
                    buf.push_str(&" ".repeat(indent));
                    yaml_write_key(key.as_str(), buf);
                    buf.push(':');
                    let value = self.get_dynamic(key).unwrap_or(Dynamic::Null);
                    if is_yaml_block(&value) {
                        buf.push('\n');
                        value.to_yaml_indent(indent + 2, buf);
                    } else {
                        buf.push(' ');
                        value.to_yaml_indent(0, buf);
                        buf.push('\n');
                    }
                }
            }
            Self::Custom(value) => {
                buf.push_str("{@custom: ");
                yaml_write_string(value.custom_type_name(), indent, buf);
                buf.push_str("}\n");
            }
        }
    }
}

/// 序列和映射的 child 是否需要展开到下一行(block style),还是能塞进当前行
/// (flow scalar)。标量、null、bool、纯数字字符串这些可以直接 inline;mapping /
/// sequence 必须换行展开。
fn is_yaml_block(value: &Dynamic) -> bool {
    // 短小的纯标量序列(2~4 个 int/float/bool)用 inline flow `[a, b]`,
    // 走 is_yaml_compact_flow;其余 List / Map / Vec 仍走 block。
    if is_yaml_compact_flow(value) {
        return false;
    }
    matches!(value, Dynamic::Map(_) | Dynamic::List(_) | Dynamic::Bytes(_) | Dynamic::StructView { .. } | Dynamic::StructOwned { .. })
        || matches!(value, Dynamic::VecI8(_) | Dynamic::VecU16(_) | Dynamic::VecI16(_) | Dynamic::VecU32(_) | Dynamic::VecI32(_) | Dynamic::VecF32(_) | Dynamic::VecU64(_) | Dynamic::VecI64(_) | Dynamic::VecF64(_))
}

/// 是否能用 inline flow `[a, b]` 表示的短标量序列。
/// 用于 `line: [6, 8]` 这种位置对的紧凑写法,省掉 block 风格的两行缩进。
fn is_yaml_compact_flow(value: &Dynamic) -> bool {
    let items = match value {
        Dynamic::List(items) => items.read().clone(),
        _ => return false,
    };
    items.len() >= 2
        && items.len() <= 4
        && items.iter().all(is_yaml_flow_scalar)
}

fn yaml_write_seq<I: Iterator<Item = Dynamic>>(items: I, indent: usize, buf: &mut String) {
    let pad = " ".repeat(indent);

    // 收集所有 item,以决定走 block 还是 flow。
    // 短小的纯标量序列(2~4 个 int / float / bool / 短字符串)用 flow `[a, b]`
    // 节省垂直空间;长序列或含 map/嵌套结构的仍走 block `- ` 形式。
    let items_vec: Vec<Dynamic> = items.collect();
    let use_flow = items_vec.len() >= 2
        && items_vec.len() <= 4
        && items_vec.iter().all(is_yaml_flow_scalar);

    if use_flow {
        buf.push('[');
        for (i, item) in items_vec.iter().enumerate() {
            if i > 0 {
                buf.push_str(", ");
            }
            item.to_yaml_indent(0, buf);
        }
        buf.push(']');
        return;
    }

    let mut first = true;
    for item in items_vec {
        // 第一个 item 用 pad (无 newline);后续 item 用 "\n + pad" 分隔。
        buf.push_str(if first { &pad } else { "\n" });
        if !first {
            buf.push_str(&pad);
        }
        first = false;
        buf.push_str("- ");
        // 序列里的 Map 项用紧凑形式:首条 `key: value` 跟 `- ` 同行,
        // 后续 key 缩进对齐。这是 LLM 最容易看懂的列表项写法。
        if let Some(pairs) = map_first_pair_inline(&item) {
            yaml_write_key(pairs.0.as_str(), buf);
            buf.push(':');
            if is_yaml_block(&pairs.1) {
                buf.push('\n');
                pairs.1.to_yaml_indent(indent + 4, buf);
            } else {
                buf.push(' ');
                pairs.1.to_yaml_indent(0, buf);
            }
            if let Some(rest) = map_remaining_pairs(&item) {
                // 第一条 sibling key 必须换到新行;yaml_write_map 自己
                // 负责 pad 和后续换行,不要再手动 push pad(否则会跟 map
                // 内部的首条 pad 叠加成双倍缩进)。
                buf.push('\n');
                yaml_write_map(rest, indent + 2, buf);
            }
        } else if is_yaml_block(&item) {
            let one_more = indent + 2;
            buf.push('\n');
            item.to_yaml_indent(one_more, buf);
        } else {
            item.to_yaml_indent(0, buf);
        }
    }
    if first {
        // 空序列:用 flow 形式 `[]`,但必须先 push pad 对齐到当前 block 的缩进,
        // 否则会落到第 0 列,被 block parser 当成顶层 sequence 截断(整个 round-trip
        // 都崩)。`indent` 反映的是调用方在 yaml_write_map 里为 block value 准备的
        // `indent + 2` 缩进,这里把 `[]` 摆到该缩进上即可。
        buf.push_str(&pad);
        buf.push_str("[]");
    }
}

/// 是否可以在 flow `[a, b]` 里内联的标量(int / float / bool / 短字符串 / null)。
fn is_yaml_flow_scalar(value: &Dynamic) -> bool {
    matches!(
        value,
        Dynamic::Null
            | Dynamic::Bool(_)
            | Dynamic::U64(_)
            | Dynamic::I64(_)
            | Dynamic::F32(_)
            | Dynamic::F64(_)
    )
}

fn yaml_write_map<I: Iterator<Item = (SmolStr, Dynamic)>>(entries: I, indent: usize, buf: &mut String) {
    let pad = " ".repeat(indent);
    let mut first = true;
    for (key, value) in entries {
        if first {
            // 嵌套 map(indent>0)的第一条 entry 需要缩进;
            // 根 map(indent==0)由调用方负责前导换行,不加 pad。
            if indent > 0 {
                buf.push_str(&pad);
            }
        } else {
            buf.push('\n');
            buf.push_str(&pad);
        }
        first = false;
        yaml_write_key(key.as_str(), buf);
        buf.push(':');
        if is_yaml_block(&value) {
            buf.push('\n');
            value.to_yaml_indent(indent + 2, buf);
        } else {
            buf.push(' ');
            value.to_yaml_indent(0, buf);
        }
    }
    if indent == 0 {
        buf.push('\n');
    }
}

/// 如果 `value` 是一个至少有 1 个 entry 的 map,返回第一个 (key, value) 对;
/// 否则返回 None。给序列里的 map 紧凑写法用。
fn map_first_pair_inline(value: &Dynamic) -> Option<(SmolStr, Dynamic)> {
    if let Dynamic::Map(m) = value {
        let m = m.read();
        let first = m.iter().next()?;
        Some((first.0.clone(), first.1.clone()))
    } else {
        None
    }
}

/// 返回 map 剩余 (除第一对外) 的 entry 迭代器。
fn map_remaining_pairs(value: &Dynamic) -> Option<impl Iterator<Item = (SmolStr, Dynamic)>> {
    if let Dynamic::Map(m) = value {
        let m = m.read().clone();
        let mut iter = m.into_iter();
        iter.next();
        Some(iter)
    } else {
        None
    }
}

// ---- 反序列化 ---------------------------------------------------------

/// 跳过空白和注释。YAML 注释从 `#` 开始,到行尾结束。
/// 跟 json 不同,空白是空格 / tab / 换行 + 注释。
fn skip_white(buf: &[u8], mut pos: usize) -> Result<usize> {
    while pos < buf.len() {
        match buf[pos] {
            b' ' | b'\t' | b'\r' | b'\n' => pos += 1,
            b'#' => {
                while pos < buf.len() && buf[pos] != b'\n' {
                    pos += 1;
                }
            }
            _ => break,
        }
    }
    Ok(pos)
}

/// 返回当前行(从行首到 pos)的缩进空格数。空 buffer 返回 0。
fn line_indent(buf: &[u8], pos: usize) -> usize {
    let mut p = pos;
    while p > 0 && buf[p - 1] != b'\n' {
        p -= 1;
    }
    let mut count = 0usize;
    // 数到非空字符为止;`while p < pos` 是错的,pos 本身可能就是首字符,
    // 那时一次都不进入循环。
    while p < buf.len() && buf[p] == b' ' {
        count += 1;
        p += 1;
    }
    count
}

/// 看 pos 处是不是 `\n` 或文件末尾。
fn at_line_end(buf: &[u8], pos: usize) -> bool {
    pos >= buf.len() || buf[pos] == b'\n'
}

/// 判断一行是否完全为空 (只有空白 / 注释 / 换行)。
fn line_is_blank(buf: &[u8], mut pos: usize) -> bool {
    while pos < buf.len() && buf[pos] != b'\n' {
        match buf[pos] {
            b' ' | b'\t' | b'\r' => pos += 1,
            b'#' => return true,
            _ => return false,
        }
    }
    true
}

/// 看接下来的 token 是否以 `---` (文档开始) 开头。
/// 这里只把 `---` 当一个"可选的前缀"吃掉,不影响实际语义。
fn skip_doc_marker(buf: &[u8], mut pos: usize) -> usize {
    pos = skip_white(buf, pos).unwrap_or(pos);
    if buf.get(pos..pos + 3) == Some(b"---") && (pos + 3 == buf.len() || matches!(buf[pos + 3], b' ' | b'\t' | b'\n' | b'\r')) {
        pos += 3;
    }
    skip_white(buf, pos).unwrap_or(pos)
}

/// 解析一个 YAML scalar 字面量(到行尾或遇到 `#` 注释为止)。
/// 返回 (去掉 trailing comment 后的内容, 消耗的字节数)。
///
/// 这里不处理 multi-line scalar (`|` / `>`);要支持的话需要在 mapping / sequence
/// 解析中专门检测。当前实现里,parser 一旦进入 scalar 就只读本行,后面的多行
/// 内容会被当成"意外的额外内容"报错 —— 这是有意的取舍,LLM 输出通常不会写
/// 折叠 scalar。
fn read_scalar(buf: &[u8], pos: usize) -> Result<(&str, usize)> {
    let start = pos;
    let mut end = pos;
    let mut in_flow_bracket = 0i32; // [ ] { } 计数,处理 inline 集合里的 scalar
    let mut in_double_quote = false;
    let mut in_single_quote = false;
    while end < buf.len() {
        let b = buf[end];
        if in_double_quote {
            if b == b'\\' && end + 1 < buf.len() {
                end += 2;
                continue;
            }
            if b == b'"' {
                in_double_quote = false;
                end += 1;
                continue;
            }
            end += 1;
            continue;
        }
        if in_single_quote {
            if b == b'\'' {
                if end + 1 < buf.len() && buf[end + 1] == b'\'' {
                    // '' 是单引号里的转义,跳两个字符。
                    end += 2;
                    continue;
                }
                in_single_quote = false;
                end += 1;
                continue;
            }
            end += 1;
            continue;
        }
        match b {
            b'"' => {
                in_double_quote = true;
                end += 1;
            }
            b'\'' => {
                in_single_quote = true;
                end += 1;
            }
            b'[' | b'{' => {
                in_flow_bracket += 1;
                end += 1;
            }
            b']' | b'}' => {
                // `]` / `}` 在 plain scalar 上下文里就是终止符,
                // 不管是不是在 flow 集合里 —— 调用方会在外层恢复对它们的处理。
                break;
            }
            b',' => {
                // `,` 是 flow 集合分隔符,plain scalar 遇到直接终止。
                // 用户如果真的需要带 `,` 的裸字符串,自己加引号。
                break;
            }
            b':' if in_flow_bracket == 0 && end + 1 < buf.len() && (buf[end + 1] == b' ' || buf[end + 1] == b'\t' || buf[end + 1] == b'\n') => {
                // mapping 的 key:value 分隔,跳出。
                break;
            }
            b'#' if in_flow_bracket == 0 => break,
            b'\n' if in_flow_bracket == 0 => break,
            _ => end += 1,
        }
    }
    // 裁掉 trailing 空白。
    while end > start && matches!(buf[end - 1], b' ' | b'\t' | b'\r') {
        end -= 1;
    }
    if end == start {
        return Err(anyhow!("yaml scalar 为空 @{}", start));
    }
    std::str::from_utf8(&buf[start..end]).map(|s| (s, end - start)).map_err(|e| anyhow!("yaml scalar 含非法 UTF-8: {e}"))
}

/// 把 scalar token 解析成 Dynamic:支持整数 / 浮点 / bool / null / 普通字符串。
fn scalar_to_dynamic(raw: &str) -> Dynamic {
    // null / ~
    if matches!(raw, "" | "null" | "Null" | "NULL" | "~") {
        return Dynamic::Null;
    }
    // bool (YAML 1.2 只有 true/false,大写也接受。LLM 经常写 yes/no,这里
    // 也兼容一下,但这是非标准的,使用时要谨慎)。
    match raw {
        "true" | "True" | "TRUE" => return Dynamic::Bool(true),
        "false" | "False" | "FALSE" => return Dynamic::Bool(false),
        _ => {}
    }
    // 整数优先,失败再试浮点。
    if let Ok(v) = raw.parse::<i64>() {
        return Dynamic::I64(v);
    }
    if let Ok(v) = raw.parse::<f64>() {
        if v.is_finite() {
            return Dynamic::F64(v);
        }
    }
    // 字符串:处理双引号 / 单引号包裹 + 转义。
    if raw.starts_with('"') && raw.ends_with('"') && raw.len() >= 2 {
        return Dynamic::String(unescape_double_quoted(&raw[1..raw.len() - 1]).into());
    }
    if raw.starts_with('\'') && raw.ends_with('\'') && raw.len() >= 2 {
        return Dynamic::String(unescape_single_quoted(&raw[1..raw.len() - 1]).into());
    }
    Dynamic::String(raw.into())
}

fn unescape_double_quoted(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars();
    while let Some(c) = chars.next() {
        if c == '\\' {
            match chars.next() {
                Some('n') => out.push('\n'),
                Some('t') => out.push('\t'),
                Some('r') => out.push('\r'),
                Some('\\') => out.push('\\'),
                Some('"') => out.push('"'),
                Some('0') => out.push('\0'),
                Some('/') => out.push('/'),
                Some('u') => {
                    let hex: String = chars.by_ref().take(4).collect();
                    if let Ok(code) = u32::from_str_radix(&hex, 16) {
                        if let Some(ch) = char::from_u32(code) {
                            out.push(ch);
                        }
                    }
                }
                Some(other) => {
                    out.push('\\');
                    out.push(other);
                }
                None => out.push('\\'),
            }
        } else {
            out.push(c);
        }
    }
    out
}

fn unescape_single_quoted(s: &str) -> String {
    // 单引号 scalar 里,所有内容都是字面量,只有 '' 代表单个 '。
    s.replace("''", "'")
}

/// `from_yaml` 入口。跟 `from_json` 一样返回 (value, consumed)。
impl FromYaml for Dynamic {
    fn from_yaml(buf: &[u8]) -> Result<(Self, usize)> {
        let pos = skip_doc_marker(buf, 0);
        parse_node(buf, pos, 0, false)
    }
}

/// 解析一个 YAML node。`min_indent` 是当前 block 的最小缩进,`in_block` 表示
/// 我们正在某个 block 集合(mapping / sequence)的内部 —— 这种情况下一旦遇到
/// 缩进小于 `min_indent` 的行,就应该结束。
fn parse_node(buf: &[u8], pos: usize, min_indent: usize, in_block: bool) -> Result<(Dynamic, usize)> {
    let pos = skip_white(buf, pos)?;
    if pos >= buf.len() {
        return Err(anyhow!("yaml 文档提前结束"));
    }
    // 先看是否是流集合 `{ ... }` / `[ ... ]`。
    if buf[pos] == b'{' {
        return parse_flow_map(buf, pos);
    }
    if buf[pos] == b'[' {
        return parse_flow_seq(buf, pos);
    }
    // 否则是 block 风格。看当前行缩进以及接下来的 token 形态。
    let indent = line_indent(buf, pos);
    if indent < min_indent {
        return Err(anyhow!("yaml 缩进不足: {} < {}", indent, min_indent));
    }
    if buf[pos] == b'-' && (pos + 1 == buf.len() || matches!(buf[pos + 1], b' ' | b'\t' | b'\n')) {
        return parse_block_seq(buf, pos, indent);
    }
    // mapping 第一个 key:value 可能跟 caller 同一行(在 indent 等于 caller
    // 缩进的情况下),所以不强制从行首开始。
    if let Some((key, colon)) = try_read_key(buf, pos) {
        // 把 pos 推到 `:` 之后;parse_block_map 不再重新扫描 key。
        let value_pos = skip_white(buf, colon + 1)?;
        return parse_block_map(buf, value_pos, indent, key);
    }
    // 走到这里说明是孤立的 scalar —— 只有当 caller 不要求 block 形态时才能
    // 接受,否则应当报错(避免 mapping 里 `- foo` 被误读成 `foo`)。
    if in_block {
        return Err(anyhow!("yaml 期望 block 结构但遇到孤立标量 @{}", pos));
    }
    let (raw, consumed) = read_scalar(buf, pos)?;
    Ok((scalar_to_dynamic(raw), pos + consumed))
}

/// 在 pos 处尝试读取一个 `key:` 形式的 mapping key。
/// 成功时返回 (key, ':' 位置)。不修改 pos。
fn try_read_key(buf: &[u8], pos: usize) -> Option<(SmolStr, usize)> {
    let start = pos;
    // 单 / 双引号包裹的 key 直接到对应引号收尾。
    if buf[pos] == b'"' || buf[pos] == b'\'' {
        let quote = buf[pos];
        let mut p = pos + 1;
        while p < buf.len() && buf[p] != quote {
            if buf[p] == b'\\' && quote == b'"' && p + 1 < buf.len() {
                p += 2;
                continue;
            }
            p += 1;
        }
        if p >= buf.len() {
            return None;
        }
        let key = &buf[start + 1..p];
        let after = p + 1;
        let after = skip_white(buf, after).ok()?;
        if after >= buf.len() || buf[after] != b':' {
            return None;
        }
        let key_str = std::str::from_utf8(key).ok()?;
        return Some((SmolStr::from(if quote == b'"' { unescape_double_quoted(key_str) } else { unescape_single_quoted(key_str) }), after));
    }
    // 普通 key:扫到 `:` 或"明显不能作为 key 字符"为止。空白 / `,` / `}` / `]` /
// `#` / `\n` 都属于 separator,提前终止。这样在 `{a: 1, b: 2}` 这种 inline 上下文里
// 才不会把后面的 `, b: 2` 吞进 key。
    let mut p = pos;
    while p < buf.len() {
        match buf[p] {
            b':' if p + 1 < buf.len() && (matches!(buf[p + 1], b' ' | b'\t' | b'\n') || p + 1 == buf.len()) => break,
            b' ' | b'\t' | b'\r' | b'\n' | b'#' | b',' | b'}' | b']' => return None,
            _ => p += 1,
        }
    }
    if p >= buf.len() || buf[p] != b':' {
        return None;
    }
    let key_bytes = &buf[start..p];
    let key = std::str::from_utf8(key_bytes).ok()?.trim();
    if key.is_empty() {
        return None;
    }
    Some((SmolStr::from(key), p))
}

/// 在 block 风格中解析一个 mapping。`parent_indent` 是 caller 期望的最浅缩进,
/// `pos` 指向 `:` 之后的第一个字符(可能就是 value 起点,可能是换行,
/// 也可能是注释/行尾)。`first_key` 是 caller 已经读到的 key。
/// 返回值会一直读到缩进小于 `parent_indent` 行为止。
fn parse_block_map(buf: &[u8], mut pos: usize, parent_indent: usize, first_key: SmolStr) -> Result<(Dynamic, usize)> {
    let mut map: IndexMap<SmolStr, Dynamic> = IndexMap::new();
    let mut current_key = first_key;
    let mut expect_value = true;
    loop {
        pos = skip_white(buf, pos)?;
        // 跳过空行 / 纯注释行。
        while pos < buf.len() && line_is_blank(buf, pos) {
            let mut p = pos;
            while p < buf.len() && buf[p] != b'\n' {
                p += 1;
            }
            if p < buf.len() {
                p += 1;
            }
            pos = p;
        }
        if pos >= buf.len() {
            break;
        }
        let indent = line_indent(buf, pos);
        if indent < parent_indent {
            break;
        }
        if expect_value {
            // 因为 pos 已经在 `:` 之后,这里不需要再 try_read_key。
            // 但 compact block sequence 的边界:上一层调用方可能在 pos 处
            // 直接撞上更深的兄弟 key (例如 `- name: alice` 后面的 `    age: 30`),
            // try_read_key 会匹配,这种情况应当让 current_key 没值并切换。
            if indent == parent_indent {
                if let Some((next_key, colon)) = try_read_key(buf, pos) {
                    map.insert(current_key.clone(), Dynamic::Null);
                    current_key = next_key;
                    pos = colon + 1;
                    expect_value = true;
                    continue;
                }
            }
            // 解析 value。先看 value 是跟 `:` 同行 (inline),还是已经跳到下一行。
            // pos 在 `:` 之后;如果当前字符是 `\n`,那 value 一定在下一行;
            // 如果不是 `\n`,要看是不是到了行尾 (即空格 / tab / 行尾)。
            let after_value = if pos < buf.len() && buf[pos] == b'\n' {
                // 已经在 `:` 紧跟的换行处,跳到下一行起点。
                pos + 1
            } else {
                skip_white(buf, pos)?
            };
            let on_next_line = pos < buf.len() && buf[pos] == b'\n';
            if on_next_line || at_line_end(buf, after_value) {
                // 值在下一行,缩进必须更深一档。
                let p = after_value;
                let next_indent = if p < buf.len() { line_indent(buf, p) } else { 0 };
                if next_indent <= parent_indent {
                    map.insert(current_key.clone(), Dynamic::Null);
                    expect_value = false;
                    pos = p;
                    continue;
                }
                if let Some((next_key, colon)) = try_read_key(buf, p) {
                    // 续行 mapping:把 next_key 接到 current_key 的 mapping 里。
                    let value_pos = skip_white(buf, colon + 1)?;
                    let (value, consumed) = parse_block_map(buf, value_pos, next_indent - 1, next_key)?;
                    map.insert(current_key.clone(), value);
                    pos = consumed;
                } else {
                    // 真正的 block value (sequence 或 deeper mapping)。
                    let (value, consumed) = parse_node(buf, p, next_indent, true)?;
                    map.insert(current_key.clone(), value);
                    pos = consumed;
                }
            } else {
                // 同行 inline value。
                let (value, consumed) = parse_node(buf, after_value, parent_indent, false)?;
                map.insert(current_key.clone(), value);
                pos = consumed;
            }
            expect_value = false;
            continue;
        }
        // 期待下一个 key。
        if let Some((next_key, colon)) = try_read_key(buf, pos) {
            current_key = next_key;
            pos = colon + 1;
            expect_value = true;
        } else {
            break;
        }
    }
    Ok((Dynamic::Map(Arc::new(RwLock::new(map))), pos))
}

/// 解析 block 风格的 sequence。`- ` 开头,每项一个。
fn parse_block_seq(buf: &[u8], mut pos: usize, parent_indent: usize) -> Result<(Dynamic, usize)> {
    let mut items: Vec<Dynamic> = Vec::new();
    loop {
        pos = skip_white(buf, pos)?;
        while pos < buf.len() && line_is_blank(buf, pos) {
            let mut p = pos;
            while p < buf.len() && buf[p] != b'\n' {
                p += 1;
            }
            if p < buf.len() {
                p += 1;
            }
            pos = p;
        }
        if pos >= buf.len() {
            break;
        }
        let indent = line_indent(buf, pos);
        if indent < parent_indent {
            break;
        }
        if buf[pos] != b'-' || (pos + 1 < buf.len() && !matches!(buf[pos + 1], b' ' | b'\t' | b'\n')) {
            break;
        }
        // 跳过 `- ` 前缀。
        let after_dash = pos + 1;
        let after_dash = skip_white(buf, after_dash)?;
        // inline scalar?
        if !at_line_end(buf, after_dash) {
            // `- foo` 或 `- key: value` 形式。
            // 先看是不是 inline mapping(同行带 `key: value`)。
            if let Some((key, colon)) = try_read_key(buf, after_dash) {
                let after_colon = skip_white(buf, colon + 1)?;
                if at_line_end(buf, after_colon) {
                    // 值在下一行,缩进至少要比 `- ` 之后更深一档。
                    let mut p = after_colon;
                    if p < buf.len() && buf[p] == b'\n' {
                        p += 1;
                    }
                    let next_indent = if p < buf.len() { line_indent(buf, p) } else { 0 };
                    let (value, consumed) = parse_node(buf, p, next_indent.max(parent_indent + 2), true)?;
                    let mut map = IndexMap::new();
                    map.insert(key, value);
                    items.push(Dynamic::Map(Arc::new(RwLock::new(map))));
                    pos = consumed;
                } else {
                    // `- key: value` 同行内联值。处理完首条 entry 后,
                    // 还要看后面是不是还有以更深缩进延续的 `key: value` 行
                    // (YAML compact block sequence)。
                    let (value, consumed) = parse_node(buf, after_colon, parent_indent, false)?;
                    let mut map = IndexMap::new();
                    map.insert(key.clone(), value);
                    // 把已读位置推到行尾之后,准备看下一行。
                    let mut p = consumed;
                    if p < buf.len() && buf[p] != b'\n' {
                        // parse_node 没吞掉换行,手动跳过空白。
                        p = skip_white(buf, p)?;
                    }
                    // 跳过空行 / 注释行,看下一条 entry 是不是更深缩进的 key:。
                    while p < buf.len() && line_is_blank(buf, p) {
                        let mut q = p;
                        while q < buf.len() && buf[q] != b'\n' {
                            q += 1;
                        }
                        if q < buf.len() {
                            q += 1;
                        }
                        p = q;
                    }
                    if p < buf.len() {
                        let next_indent = line_indent(buf, p);
                        let dash_indent = line_indent(buf, pos);
                        // 跳过行首缩进,看看是不是 `key:` 续行。
                        let key_start = p + next_indent;
                        if next_indent > dash_indent && key_start < buf.len()
                            && let Some((next_key, colon)) = try_read_key(buf, key_start)
                        {
                            // 用 parse_block_map 把后续 key:value 行都收进来。
                            // parse_block_map 现在期望 pos 在 `:` 之后。
                            let value_pos = skip_white(buf, colon + 1)?;
                            let (rest, consumed2) = parse_block_map(buf, value_pos, next_indent - 1, next_key)?;
                            if let Dynamic::Map(extra) = rest {
                                for (k, v) in extra.read().iter() {
                                    map.insert(k.clone(), v.clone());
                                }
                            }
                            p = consumed2;
                        }
                    }
                    items.push(Dynamic::Map(Arc::new(RwLock::new(map))));
                    pos = p;
                }
            } else {
                let (value, consumed) = parse_node(buf, after_dash, parent_indent, false)?;
                items.push(value);
                pos = consumed;
            }
            continue;
        }
        // 值在下一行,缩进必须比 `- ` 之后更深。
        let mut p = after_dash;
        if p < buf.len() && buf[p] == b'\n' {
            p += 1;
        }
        if p >= buf.len() {
            items.push(Dynamic::Null);
            break;
        }
        let next_indent = line_indent(buf, p);
        if next_indent <= parent_indent {
            items.push(Dynamic::Null);
            break;
        }
        let (value, consumed) = parse_node(buf, p, next_indent, true)?;
        items.push(value);
        pos = consumed;
    }
    Ok((Dynamic::List(Arc::new(RwLock::new(items))), pos))
}

/// 解析 `{ key: value, ... }` 这种 inline mapping。
fn parse_flow_map(buf: &[u8], pos: usize) -> Result<(Dynamic, usize)> {
    let mut p = pos + 1;
    let mut map: IndexMap<SmolStr, Dynamic> = IndexMap::new();
    p = skip_white(buf, p)?;
    if p < buf.len() && buf[p] == b'}' {
        return Ok((Dynamic::Map(Arc::new(RwLock::new(map))), p + 1));
    }
    loop {
        p = skip_white(buf, p)?;
        let (key, after) = match try_read_key(buf, p) {
            Some(pair) => pair,
            None => return Err(anyhow!("yaml flow mapping 缺少 key @{}", p)),
        };
        p = skip_white(buf, after + 1)?;
        let (value, consumed) = parse_node(buf, p, 0, false)?;
        map.insert(key, value);
        p = consumed;
        p = skip_white(buf, p)?;
        if p < buf.len() && buf[p] == b',' {
            p += 1;
            continue;
        }
        if p < buf.len() && buf[p] == b'}' {
            return Ok((Dynamic::Map(Arc::new(RwLock::new(map))), p + 1));
        }
        return Err(anyhow!("yaml flow mapping 缺少 ',' 或 '}}' @{}", p));
    }
}

/// 解析 `[ a, b, c ]` 这种 inline sequence。
fn parse_flow_seq(buf: &[u8], pos: usize) -> Result<(Dynamic, usize)> {
    let mut p = pos + 1;
    let mut items: Vec<Dynamic> = Vec::new();
    p = skip_white(buf, p)?;
    if p < buf.len() && buf[p] == b']' {
        return Ok((Dynamic::List(Arc::new(RwLock::new(items))), p + 1));
    }
    loop {
        p = skip_white(buf, p)?;
        let (value, consumed) = parse_node(buf, p, 0, false)?;
        items.push(value);
        p = consumed;
        p = skip_white(buf, p)?;
        if p < buf.len() && buf[p] == b',' {
            p += 1;
            continue;
        }
        if p < buf.len() && buf[p] == b']' {
            return Ok((Dynamic::List(Arc::new(RwLock::new(items))), p + 1));
        }
        return Err(anyhow!("yaml flow sequence 缺少 ',' 或 ']' @{}", p));
    }
}

// ---- Dynamic 上的便捷方法 ------------------------------------------

impl Dynamic {
    /// 顶层便捷入口:把 `Dynamic` 序列化成 YAML 字符串。
    pub fn to_yaml_string(&self) -> String {
        let mut buf = String::new();
        self.to_yaml(&mut buf);
        buf
    }

    /// 顶层便捷入口:从 YAML 文本解析 `Dynamic`。
    pub fn from_yaml_buf(buf: &[u8]) -> Result<Self> {
        let (value, _) = <Self as FromYaml>::from_yaml(buf)?;
        Ok(value)
    }
}

impl Dynamic {
    /// 给 `to_yaml` / 测试用的小工具:在已有 map 里塞一个 entry 并返回 map。
    fn map_with_entry(key: SmolStr, value: Dynamic) -> Self {
        let mut map = IndexMap::new();
        map.insert(key, value);
        Dynamic::Map(Arc::new(RwLock::new(map)))
    }
}

// ---- 测试 --------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn round_trip(input: &str) -> String {
        let (value, _) = <Dynamic as FromYaml>::from_yaml(input.as_bytes()).expect("parse yaml");
        let mut buf = String::new();
        value.to_yaml(&mut buf);
        buf
    }

    #[test]
    fn simple_scalar_round_trip() {
        assert_eq!(round_trip("42"), "42");
        assert_eq!(round_trip("-7"), "-7");
        assert_eq!(round_trip("3.14"), "3.14");
        assert_eq!(round_trip("true"), "true");
        assert_eq!(round_trip("false"), "false");
        assert_eq!(round_trip("null"), "null");
        assert_eq!(round_trip("~"), "null");
    }

    #[test]
    fn string_quoting_rules() {
        // 像数字的字符串必须加引号。
        let value = Dynamic::String("123".into());
        let mut buf = String::new();
        value.to_yaml(&mut buf);
        assert_eq!(buf, "\"123\"");

        // 像 bool 的字符串也要加引号。
        let value = Dynamic::String("yes".into());
        let mut buf = String::new();
        value.to_yaml(&mut buf);
        assert_eq!(buf, "\"yes\"");

        // 普通字符串裸出。
        let value = Dynamic::String("hello world".into());
        let mut buf = String::new();
        value.to_yaml(&mut buf);
        assert_eq!(buf, "hello world");

        // 含 `:` 的字符串必须引号。
        let value = Dynamic::String("a: b".into());
        let mut buf = String::new();
        value.to_yaml(&mut buf);
        assert_eq!(buf, "\"a: b\"");

        // 含 `#` 的字符串要引号。
        let value = Dynamic::String("color #ff00ff".into());
        let mut buf = String::new();
        value.to_yaml(&mut buf);
        assert_eq!(buf, "\"color #ff00ff\"");
    }

    #[test]
    fn multiline_string_uses_block_scalar() {
        let value = Dynamic::String("line1\nline2\nline3".into());
        let mut buf = String::new();
        value.to_yaml(&mut buf);
        assert!(buf.starts_with("|+\n"), "should use block scalar: {buf}");
        assert!(buf.contains("line1\n"));
        assert!(buf.contains("line2\n"));
        assert!(buf.contains("line3\n"));
    }

    #[test]
    fn mapping_block_style() {
        let mut map = IndexMap::new();
        map.insert(SmolStr::from("name"), Dynamic::String("zust".into()));
        map.insert(SmolStr::from("version"), Dynamic::I64(1));
        map.insert(SmolStr::from("active"), Dynamic::Bool(true));
        let value = Dynamic::Map(Arc::new(RwLock::new(map)));
        let mut buf = String::new();
        value.to_yaml(&mut buf);
        assert!(buf.contains("name: zust"), "{buf}");
        assert!(buf.contains("version: 1"), "{buf}");
        assert!(buf.contains("active: true"), "{buf}");
    }

    #[test]
    fn nested_mapping_and_list() {
        let mut inner = IndexMap::new();
        inner.insert(SmolStr::from("a"), Dynamic::I64(1));
        inner.insert(SmolStr::from("b"), Dynamic::I64(2));
        let mut map = IndexMap::new();
        map.insert(SmolStr::from("items"), Dynamic::list(vec![Dynamic::Map(Arc::new(RwLock::new(inner)))]));
        let value = Dynamic::Map(Arc::new(RwLock::new(map)));
        let yaml = value.to_yaml_string();
        assert!(yaml.contains("items:\n  - a: 1"), "{yaml}");
    }

    #[test]
    fn block_sequence_round_trip() {
        let input = "- 1\n- 2\n- 3\n";
        let (value, consumed) = <Dynamic as FromYaml>::from_yaml(input.as_bytes()).expect("parse");
        assert!(matches!(value, Dynamic::List(_)));
        assert_eq!(consumed, input.len());
    }

    #[test]
    fn block_mapping_round_trip() {
        let input = "name: zust\nversion: 1\n";
        let (value, _) = <Dynamic as FromYaml>::from_yaml(input.as_bytes()).expect("parse");
        let map = value.get_dynamic("name").unwrap();
        assert_eq!(map.as_str(), "zust");
        assert_eq!(value.get_dynamic("version").and_then(|v| v.as_int()), Some(1));
    }

    #[test]
    fn nested_block_structures() {
        let input = "users:\n  - name: alice\n    age: 30\n  - name: bob\n    age: 25\n";
        let (value, _) = <Dynamic as FromYaml>::from_yaml(input.as_bytes()).expect("parse");
        let users = value.get_dynamic("users").unwrap();
        assert!(users.is_list());
        let first = users.get_idx(0).unwrap();
        assert_eq!(first.get_dynamic("name").unwrap().as_str(), "alice");
        assert_eq!(first.get_dynamic("age").unwrap().as_int(), Some(30));
    }

    #[test]
    fn inline_mapping_and_sequence() {
        let input = "{a: 1, b: [2, 3]}";
        let (value, _) = <Dynamic as FromYaml>::from_yaml(input.as_bytes()).expect("parse");
        assert_eq!(value.get_dynamic("a").unwrap().as_int(), Some(1));
        let b = value.get_dynamic("b").unwrap();
        assert!(b.is_list());
    }

    #[test]
    fn comments_and_blank_lines_are_ignored() {
        let input = "# header comment\nname: zust  # trailing comment\n\nversion: 1\n";
        let (value, _) = <Dynamic as FromYaml>::from_yaml(input.as_bytes()).expect("parse");
        assert_eq!(value.get_dynamic("name").unwrap().as_str(), "zust");
        assert_eq!(value.get_dynamic("version").unwrap().as_int(), Some(1));
    }

    #[test]
    fn double_quoted_string_with_escapes() {
        let input = "msg: \"hello\\nworld\"\n";
        let (value, _) = <Dynamic as FromYaml>::from_yaml(input.as_bytes()).expect("parse");
        assert_eq!(value.get_dynamic("msg").unwrap().as_str(), "hello\nworld");
    }

    #[test]
    fn single_quoted_string_literal() {
        let input = "msg: 'a: b'\n";
        let (value, _) = <Dynamic as FromYaml>::from_yaml(input.as_bytes()).expect("parse");
        assert_eq!(value.get_dynamic("msg").unwrap().as_str(), "a: b");
    }

    #[test]
    fn quoted_round_trip_preserves_string_like_number() {
        // 字符串 "123" 必须 round-trip 仍然是字符串,而不是整数。
        let mut map = IndexMap::new();
        map.insert(SmolStr::from("id"), Dynamic::String("123".into()));
        let value = Dynamic::Map(Arc::new(RwLock::new(map)));
        let yaml = value.to_yaml_string();
        assert!(yaml.contains("id: \"123\""), "{yaml}");
        let (parsed, _) = <Dynamic as FromYaml>::from_yaml(yaml.as_bytes()).expect("parse");
        let id = parsed.get_dynamic("id").unwrap();
        assert_eq!(id.as_str(), "123");
        assert!(!id.is_int(), "id should stay string");
    }

    #[test]
    fn empty_collection_serialization() {
        let value = Dynamic::List(Arc::new(RwLock::new(Vec::new())));
        let mut buf = String::new();
        value.to_yaml(&mut buf);
        assert_eq!(buf, "[]");
    }
}
