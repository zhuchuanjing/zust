use crate::assert_err;

use super::Dynamic;
use anyhow::{Result, anyhow};

macro_rules! vec_to_json {
    ($buf:expr, $vec:expr, $variant:ident) => {{
        $buf.push('[');
        let mut once = ZOnce::new("", ",");
        for value in $vec.iter() {
            $buf.push_str(once.take());
            Dynamic::$variant(value.clone()).to_json($buf);
        }
        $buf.push(']');
    }};
}

pub(crate) fn skip_white(buf: &[u8]) -> Result<usize> {
    let mut pos = 0usize;
    while pos < buf.len() && (buf[pos] == b' ' || buf[pos] == b'\r' || buf[pos] == b'\t' || buf[pos] == b'\n') {
        pos += 1;
    }
    if pos < buf.len() { Ok(pos) } else { Err(anyhow!("no more data")) }
}

const TOKEN: &[u8] = b"01234567890.-+eEtruefalsenull"; //合法的数字和其他 json token(含科学计数法 e/E)
pub trait FromJson: Sized {
    fn from_json(buf: &[u8]) -> Result<(Self, usize)>;
    fn get_token(buf: &[u8]) -> Result<(&str, usize)> {
        let mut pos = 0usize;
        while pos < buf.len() && TOKEN.contains(&buf[pos]) {
            pos += 1;
        }
        Ok((std::str::from_utf8(&buf[..pos])?, pos))
    }

    fn get_string(buf: &[u8]) -> Result<(String, usize)> {
        let mut pos = 1usize;
        let mut vec = Vec::new();
        while pos < buf.len() && buf[pos] != b'"' {
            if buf[pos] == b'\\' {
                pos += 1;
                if pos == buf.len() {
                    return Err(anyhow!("uncomplete string"));
                }
                match buf[pos] {
                    b'\\' => vec.push(b'\\'),
                    b'"' => vec.push(b'\"'),
                    b'r' => vec.push(b'\r'),
                    b'n' => vec.push(b'\n'),
                    b't' => vec.push(b'\t'),
                    b'u' => {
                        if pos + 5 > buf.len() {
                            return Err(anyhow!("不完整的 unicode"));
                        }
                        let unicode_str = unsafe { std::str::from_utf8_unchecked(&buf[(pos + 1)..(pos + 5)]) };
                        let Ok(unicode) = u32::from_str_radix(unicode_str, 16) else {
                            return Err(anyhow!("非法 unicode 转义"));
                        };
                        pos += 4;
                        // JSON 规范:高代理(0xD800..=0xDBFF)后必须跟 \uXXXX 低代理
                        // (0xDC00..=0xDFFF),组合成完整 code point;孤立代理丢弃。
                        let code = if (0xD800..=0xDBFF).contains(&unicode) && buf.get(pos + 1..pos + 3) == Some(b"\\u") && pos + 7 <= buf.len() {
                            let low_str = unsafe { std::str::from_utf8_unchecked(&buf[(pos + 3)..(pos + 7)]) };
                            if let Ok(low) = u32::from_str_radix(low_str, 16)
                                && (0xDC00..=0xDFFF).contains(&low)
                            {
                                let combined = 0x10000 + ((unicode - 0xD800) << 10) + (low - 0xDC00);
                                pos += 6;
                                combined
                            } else {
                                unicode
                            }
                        } else {
                            unicode
                        };
                        if code < 0x80 {
                            vec.push(code as u8);
                        } else if let Some(ch) = char::from_u32(code) {
                            // encode_utf8 需要固定大小缓冲区,不能直接传 &mut Vec<u8>
                            // (Vec 借用的是当前长度 0 的切片,会 panic)。
                            let mut buf = [0u8; 4];
                            let s = ch.encode_utf8(&mut buf);
                            vec.extend_from_slice(s.as_bytes());
                        }
                    }
                    _ => {
                        return Err(anyhow!("unknow escape {}", buf[pos]));
                    }
                }
            } else {
                vec.push(buf[pos]);
            }
            pos += 1;
        }
        if pos < buf.len() {
            pos += 1;
        }
        Ok(String::from_utf8(vec).map(|s| (s, pos))?)
    }
}

pub trait ToJson {
    fn to_json(&self, buf: &mut String);
}

use indexmap::IndexMap;
use parking_lot::RwLock;
use smol_str::SmolStr;
use std::sync::Arc;

impl FromJson for Dynamic {
    fn from_json(buf: &[u8]) -> Result<(Self, usize)> {
        let mut pos = skip_white(buf)?;
        if buf[pos] == b'[' {
            pos += 1;
            pos += skip_white(&buf[pos..])?;
            let mut vec = Vec::<Self>::new();
            while buf[pos] != b']' {
                let (item, size) = Self::from_json(&buf[pos..])?;
                vec.push(item);
                pos += size;
                pos += skip_white(&buf[pos..])?;
                if buf[pos] == b',' {
                    pos += 1;
                    pos += skip_white(&buf[pos..])?;
                }
            }
            Ok((Dynamic::List(Arc::new(RwLock::new(vec))), pos + 1))
        } else if buf[pos] == b'{' {
            pos += 1;
            pos += skip_white(&buf[pos..])?;
            let mut map = IndexMap::new();
            while buf[pos] != b'}' {
                assert_err!(buf[pos] != b'"', anyhow!("need a string key {:?}", String::from_utf8_lossy(&buf[pos..])));
                let (key, size) = Self::get_string(&buf[pos..])?;
                pos += size;
                pos += skip_white(&buf[pos..])?;
                assert_err!(buf[pos] != b':', anyhow!("need a :"));
                pos += 1;
                pos += skip_white(&buf[pos..])?;
                let (item, size) = Self::from_json(&buf[pos..])?;
                map.insert(SmolStr::from(key), item);
                pos += size;
                pos += skip_white(&buf[pos..])?;
                if buf[pos] == b',' {
                    pos += 1;
                    pos += skip_white(&buf[pos..])?;
                }
            }
            Ok((Dynamic::Map(Arc::new(RwLock::new(map))), pos + 1))
        } else if buf[pos] == b'"' {
            let (s, size) = Self::get_string(&buf[pos..])?;
            Ok((SmolStr::from(s).into(), size))
        } else {
            let (token, size) = Self::get_token(&buf[pos..])?;
            if token == "true" {
                Ok((Dynamic::from(true), size))
            } else if token == "false" {
                Ok((Dynamic::from(false), size))
            } else if token == "null" {
                Ok((Dynamic::Null, size))
            } else if token.contains('.') || token.contains('e') || token.contains('E') {
                let v = token.parse::<f64>()?;
                Ok((Dynamic::from(v), size))
            } else {
                // 纯整数字面量;若失败(如溢出)退回 f64 而非直接报错。
                match token.parse::<i64>() {
                    Ok(v) => Ok((Dynamic::from(v), size)),
                    Err(_) => {
                        let v = token.parse::<f64>()?;
                        Ok((Dynamic::from(v), size))
                    }
                }
            }
        }
    }
}

impl ToJson for &str {
    fn to_json(&self, buf: &mut String) {
        let mut formatted = self.as_bytes().iter().fold(vec![b'\"'], |mut vec, ch| match ch {
            b'\"' => {
                vec.extend_from_slice(&[0x5c, 0x22]);
                vec
            }
            b'\\' => {
                vec.extend_from_slice(&[0x5c, 0x5c]);
                vec
            }
            b'\n' => {
                vec.extend_from_slice(&[0x5c, 0x6e]);
                vec
            }
            b'\r' => {
                vec.extend_from_slice(&[0x5c, 0x72]);
                vec
            }
            b'\t' => {
                vec.extend_from_slice(&[0x5c, 0x74]);
                vec
            }
            0x08 => {
                vec.extend_from_slice(&[0x5c, 0x62]); // \b
                vec
            }
            0x0c => {
                vec.extend_from_slice(&[0x5c, 0x66]); // \f
                vec
            }
            c if *c < 0x20 => {
                // 其它控制字符必须用 \uXXXX 转义(RFC 8259 §7),否则产生非法 JSON。
                vec.extend_from_slice(format!("\\u{:04x}", c).as_bytes());
                vec
            }
            _ => {
                vec.push(*ch);
                vec
            }
        });
        formatted.push(b'\"');
        buf.push_str(unsafe { std::str::from_utf8_unchecked(&formatted) });
    }
}

impl ToJson for i64 {
    fn to_json(&self, buf: &mut String) {
        buf.push_str(&self.to_string());
    }
}

use super::ZOnce;

impl ToJson for Dynamic {
    fn to_json(&self, buf: &mut String) {
        match self {
            Self::Iter { idx: _, keys: _, value: _ } => {}
            Self::Bool(b) => {
                if *b {
                    buf.push_str("true")
                } else {
                    buf.push_str("false")
                }
            }
            Self::F16(bits) => buf.push_str(&super::f16_to_f64(*bits).to_string()),
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
            Self::Null => buf.push_str("null"),
            Self::String(s) => s.as_str().to_json(buf),
            Self::StringBuf(s) => s.as_str().to_json(buf),
            Self::Bytes(vec) => vec_to_json!(buf, vec, U8),
            Self::VecI8(vec) => vec_to_json!(buf, vec, I8),
            Self::VecU16(vec) => vec_to_json!(buf, vec, U16),
            Self::VecI16(vec) => vec_to_json!(buf, vec, I16),
            Self::VecU32(vec) => vec_to_json!(buf, vec, U32),
            Self::VecI32(vec) => vec_to_json!(buf, vec, I32),
            Self::VecF32(vec) => vec_to_json!(buf, vec, F32),
            Self::VecU64(vec) => vec_to_json!(buf, vec, U64),
            Self::VecI64(vec) => vec_to_json!(buf, vec, I64),
            Self::VecF64(vec) => vec_to_json!(buf, vec, F64),
            Self::List(a) => {
                buf.push('[');
                let mut once = ZOnce::new("", ",");
                a.read().iter().for_each(|item| {
                    buf.push_str(once.take());
                    item.to_json(buf);
                });
                buf.push(']');
            }
            Self::Map(map) => {
                buf.push('{');
                let mut once = ZOnce::new("", ",");
                map.read().iter().for_each(|(k, v)| {
                    buf.push_str(once.take());
                    k.as_str().to_json(buf);
                    buf.push(':');
                    v.to_json(buf);
                });
                buf.push('}');
            }
            Self::StructView { .. } | Self::StructOwned { .. } => {
                buf.push('{');
                let mut once = ZOnce::new("", ",");
                self.keys().iter().for_each(|k| {
                    buf.push_str(once.take());
                    k.as_str().to_json(buf);
                    buf.push(':');
                    self.get_dynamic(k).unwrap_or(Dynamic::Null).to_json(buf);
                });
                buf.push('}');
            }
            Self::Custom(value) => {
                buf.push_str("{\"@custom\":");
                value.custom_type_name().to_json(buf);
                buf.push('}');
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dynamic_json_is_one_compact_roundtrippable_value() {
        let value = crate::map!("jsonrpc" => "2.0", "id" => 1i64, "params" => crate::map!("items" => vec![1i64, 2i64]));
        let mut text = String::new();
        value.to_json(&mut text);
        assert_eq!(text, r#"{"id":1,"jsonrpc":"2.0","params":{"items":[1,2]}}"#);
        assert!(!text.contains('\n'));
        let (decoded, consumed) = Dynamic::from_json(text.as_bytes()).unwrap();
        assert_eq!(consumed, text.len());
        assert_eq!(decoded, value);
    }
}
