#![no_main]

//! Fuzz 目标:把任意字节喂给 Zust 解析器,断言它永不 panic、不崩溃、不卡死。
//!
//! 解析循环有界(B2 的递归深度守卫 + 这里的语句计数上限),因此恶意/畸形输入
//! 只会得到 Ok 或 Err,不会打爆调用栈或无限循环。
//!
//! 运行(需 nightly + cargo-fuzz):`cargo +nightly fuzz run parse`

use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let mut parser = parser::Parser::new(data.to_vec());
    let mut count = 0u32;
    loop {
        match parser.stmt(false) {
            Ok(_) => {
                count += 1;
                if parser.is_eof() || count > 10_000 {
                    break;
                }
            }
            Err(_) => break,
        }
    }
});
