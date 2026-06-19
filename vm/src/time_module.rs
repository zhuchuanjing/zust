//! `time::*` —— 时间戳辅助。
//!
//! - `time::now() -> i64` 返回从 UNIX epoch 起的毫秒数。选毫秒(而不是秒)是因为
//!   绝大多数日志 / DB / HTTP 场景都按毫秒粒度记录;脚本要用秒只需 `/ 1000`。
//! - `time::format(fmt, tick) -> string` 用 chrono 的 strftime 风格格式化 UTC 时间。
//! - `time::parse(fmt, text) -> i64` 反向解析,失败返回 `-1`。
//!
//! 时间戳一律按 UTC 解读 —— 跨时区由调用方在脚本里自行处理(date / db 通常也都
//! 存 UTC)。
use crate::memory::alloc_dynamic;
use chrono::{NaiveDateTime, TimeZone, Utc};
use dynamic::{Dynamic, Type};

extern "C" fn time_now() -> i64 {
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis() as i64).unwrap_or(-1);
    now
}

extern "C" fn time_format(fmt: *const Dynamic, tick: i64) -> *const Dynamic {
    if fmt.is_null() {
        return alloc_dynamic(Dynamic::Null);
    }
    let fmt = unsafe { (&*fmt).as_str().to_string() };
    let secs = tick.div_euclid(1000);
    let nanos = (tick.rem_euclid(1000) as u32) * 1_000_000;
    let Some(dt) = Utc.timestamp_opt(secs, nanos).single() else {
        return alloc_dynamic(Dynamic::Null);
    };
    alloc_dynamic(dt.format(&fmt).to_string().into())
}

extern "C" fn time_parse(fmt: *const Dynamic, text: *const Dynamic) -> i64 {
    if fmt.is_null() || text.is_null() {
        return -1;
    }
    let fmt = unsafe { (&*fmt).as_str().to_string() };
    let text = unsafe { (&*text).as_str().to_string() };
    // 优先按"日期 + 时间"解析;没有时间字段时退回到"仅日期"。
    if let Ok(dt) = NaiveDateTime::parse_from_str(&text, &fmt) {
        return dt.and_utc().timestamp_millis();
    }
    if let Ok(date) = chrono::NaiveDate::parse_from_str(&text, &fmt) {
        if let Some(ts) = date.and_hms_opt(0, 0, 0) {
            return ts.and_utc().timestamp_millis();
        }
    }
    -1
}

pub const TIME_NATIVE: [(&str, &[Type], Type, *const u8); 3] = [
    ("now", &[], Type::I64, time_now as *const u8),
    ("format", &[Type::Any, Type::I64], Type::Any, time_format as *const u8),
    ("parse", &[Type::Any, Type::Any], Type::I64, time_parse as *const u8),
];
