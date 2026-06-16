//! 运行期错误标志。
//!
//! JIT 编译出的脚本代码无法像普通 Rust 函数那样返回 `Result`,因此用一个线程
//! 局部标志记录"本次脚本执行中发生过运行期错误"(例如整数除零)。脚本入口的
//! 隔离边界在调用结束后用 [`take_fault`] 读取并清空它,把崩溃降级为一次失败的
//! 调用,而不是杀掉整个进程。
//!
//! 这个标志放在最底层的 `dynamic` crate,使得动态值运算([`crate::Dynamic`] 的
//! 除法/取余)和 VM 生成的守卫代码可以写同一个标志位。

use std::cell::Cell;

thread_local! {
    static RUNTIME_FAULT: Cell<Option<&'static str>> = const { Cell::new(None) };
}

/// 记录一次运行期错误。只保留第一个原因(最早发生的错误信息最有用)。
pub fn set_fault(reason: &'static str) {
    RUNTIME_FAULT.with(|fault| {
        if fault.get().is_none() {
            fault.set(Some(reason));
        }
    });
}

/// 读取并清空当前线程的运行期错误标志。
pub fn take_fault() -> Option<&'static str> {
    RUNTIME_FAULT.with(|fault| fault.take())
}

/// 是否已经发生过运行期错误(不清空)。
pub fn has_fault() -> bool {
    RUNTIME_FAULT.with(|fault| fault.get().is_some())
}
