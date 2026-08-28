//! `stdio::*` —— 当前 Zust 宿主进程的最小文本传输。
//!
//! 逐行 API 服务 JSONL；精确 UTF-8 字节读写服务 Content-Length 等 framing。
//! 模块不解析 JSON、JSON-RPC 或任何上层协议；调用方使用 Dynamic 与普通 Zust
//! 控制流完成解码、校验和分发。它不进 core，由宿主按信任边界显式注册。

use crate::memory::alloc_dynamic;
use dynamic::{Dynamic, Type};
use std::io::{Read, Write};

extern "C" fn stdio_read_line() -> *const Dynamic {
    let mut line = String::new();
    match std::io::stdin().read_line(&mut line) {
        Ok(0) | Err(_) => alloc_dynamic(Dynamic::Null),
        Ok(_) => {
            if line.ends_with('\n') {
                line.pop();
                if line.ends_with('\r') {
                    line.pop();
                }
            }
            alloc_dynamic(Dynamic::from(line))
        }
    }
}

extern "C" fn stdio_write_line(text: *const Dynamic) -> bool {
    if text.is_null() {
        return false;
    }
    let text = match unsafe { &*text } {
        Dynamic::String(text) => text.as_str(),
        Dynamic::StringBuf(text) => text.as_str(),
        _ => return false,
    };
    let mut stdout = std::io::stdout().lock();
    stdout.write_all(text.as_bytes()).and_then(|_| stdout.write_all(b"\n")).and_then(|_| stdout.flush()).is_ok()
}

extern "C" fn stdio_read_exact(byte_count: i64) -> *const Dynamic {
    if byte_count < 0 {
        return alloc_dynamic(Dynamic::Null);
    }
    let Ok(byte_count) = usize::try_from(byte_count) else {
        return alloc_dynamic(Dynamic::Null);
    };
    let mut bytes = vec![0u8; byte_count];
    if std::io::stdin().lock().read_exact(&mut bytes).is_err() {
        return alloc_dynamic(Dynamic::Null);
    }
    match String::from_utf8(bytes) {
        Ok(text) => alloc_dynamic(Dynamic::from(text)),
        Err(_) => alloc_dynamic(Dynamic::Null),
    }
}

extern "C" fn stdio_write(text: *const Dynamic) -> bool {
    if text.is_null() {
        return false;
    }
    let text = match unsafe { &*text } {
        Dynamic::String(text) => text.as_str(),
        Dynamic::StringBuf(text) => text.as_str(),
        _ => return false,
    };
    let mut stdout = std::io::stdout().lock();
    stdout.write_all(text.as_bytes()).and_then(|_| stdout.flush()).is_ok()
}

pub const STDIO_NATIVE: [(&str, &[Type], Type, *const u8); 4] = [
    ("read_line", &[], Type::Any, stdio_read_line as *const u8),
    ("write_line", &[Type::Any], Type::Bool, stdio_write_line as *const u8),
    ("read_exact", &[Type::I64], Type::Any, stdio_read_exact as *const u8),
    ("write", &[Type::Any], Type::Bool, stdio_write as *const u8),
];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stdio_surface_stays_protocol_neutral() {
        assert_eq!(STDIO_NATIVE.len(), 4);
        assert_eq!(STDIO_NATIVE[0].0, "read_line");
        assert_eq!(STDIO_NATIVE[1].0, "write_line");
        assert_eq!(STDIO_NATIVE[2].0, "read_exact");
        assert_eq!(STDIO_NATIVE[3].0, "write");
    }
}
