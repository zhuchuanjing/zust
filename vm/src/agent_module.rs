//! `agent::*` —— 沙箱程序与受信宿主的同步协议。
//!
//! 全部经 root 树上的固定节点通信（M1 通道；远程化时换成正式消息通道）：
//! - `agent::report(value)`：异步推送摘要，追加到 `local/zbuddy/reports`。
//!   模型看不见 VM 变量——需要模型在下一轮看到的事实，用 report 或
//!   verdict.context 带出。
//! - `agent::ask(question, context) -> {approved, reason}`：同步审批。
//!   写 `local/zbuddy/ask` 后阻塞轮询 `local/zbuddy/ask_reply`，宿主按
//!   profile.approvals 白名单判定；超时按拒绝处理（fail-closed）。
//! - `agent::checkpoint(label) -> {ok, id}`：同步快照请求，宿主对工作区
//!   做目录快照后写 `checkpoint_done`。
//! - `agent::rollback(id) -> {ok}`：同步回滚到指定快照。
//!
//! 同步语义靠"写请求 → 轮询回执"达成；等待都带超时，超时即失败返回，
//! 不允许程序在宿主缺席时无限挂起。
use crate::memory::alloc_dynamic;
use dynamic::{Dynamic, Type};
use std::time::{Duration, Instant};

const ASK_PATH: &str = "local/zbuddy/ask";
const ASK_REPLY_PATH: &str = "local/zbuddy/ask_reply";
const CHECKPOINT_PATH: &str = "local/zbuddy/checkpoint";
const CHECKPOINT_DONE_PATH: &str = "local/zbuddy/checkpoint_done";
const ROLLBACK_PATH: &str = "local/zbuddy/rollback";
const ROLLBACK_DONE_PATH: &str = "local/zbuddy/rollback_done";
const REPORTS_PATH: &str = "local/zbuddy/reports";

const POLL: Duration = Duration::from_millis(20);
const ASK_TIMEOUT: Duration = Duration::from_secs(300);
const SNAPSHOT_TIMEOUT: Duration = Duration::from_secs(30);

extern "C" fn agent_report(value: *const Dynamic) -> *const Dynamic {
    let value = unsafe { (&*value).clone() };
    if root::push(REPORTS_PATH, value.clone()).is_err() {
        let _ = root::add_list(REPORTS_PATH);
        let _ = root::push(REPORTS_PATH, value);
    }
    alloc_dynamic(Dynamic::Bool(true))
}

extern "C" fn agent_ask(question: *const Dynamic, context: *const Dynamic) -> *const Dynamic {
    let question = unsafe { (&*question).clone() };
    let context = unsafe { (&*context).clone() };
    let _ = root::remove(ASK_REPLY_PATH);
    let _ = root::add(ASK_PATH, root::Object::Value(dynamic::map!("question" => question, "context" => context)));
    let reply = wait_map(ASK_REPLY_PATH, ASK_TIMEOUT).unwrap_or_else(|| {
        // 宿主缺席/超时按拒绝处理：审批能力 fail-closed
        dynamic::map!("approved" => false, "reason" => "ask timeout, denied by default")
    });
    let _ = root::remove(ASK_REPLY_PATH);
    alloc_dynamic(reply)
}

extern "C" fn agent_checkpoint(label: *const Dynamic) -> *const Dynamic {
    let label = unsafe { (&*label).clone() };
    let _ = root::remove(CHECKPOINT_DONE_PATH);
    let _ = root::add(CHECKPOINT_PATH, root::Object::Value(dynamic::map!("label" => label)));
    let done = wait_map(CHECKPOINT_DONE_PATH, SNAPSHOT_TIMEOUT)
        .unwrap_or_else(|| dynamic::map!("ok" => false, "error" => "checkpoint timeout"));
    let _ = root::remove(CHECKPOINT_DONE_PATH);
    alloc_dynamic(done)
}

extern "C" fn agent_rollback(id: *const Dynamic) -> *const Dynamic {
    let id = unsafe { (&*id).clone() };
    let _ = root::remove(ROLLBACK_DONE_PATH);
    let _ = root::add(ROLLBACK_PATH, root::Object::Value(dynamic::map!("id" => id)));
    let done = wait_map(ROLLBACK_DONE_PATH, SNAPSHOT_TIMEOUT)
        .unwrap_or_else(|| dynamic::map!("ok" => false, "error" => "rollback timeout"));
    let _ = root::remove(ROLLBACK_DONE_PATH);
    alloc_dynamic(done)
}

/// 轮询直到节点出现 map 或超时；节点非 map（null/缺失）继续等。
fn wait_map(path: &str, timeout: Duration) -> Option<Dynamic> {
    let deadline = Instant::now() + timeout;
    loop {
        if let Ok(value) = root::get(path) {
            if value.is_map() {
                return Some(value);
            }
        }
        if Instant::now() >= deadline {
            return None;
        }
        std::thread::sleep(POLL);
    }
}

pub const AGENT_NATIVE: [(&str, &[Type], Type, *const u8); 4] = [
    ("report", &[Type::Any], Type::Bool, agent_report as *const u8),
    ("ask", &[Type::Any, Type::Any], Type::Any, agent_ask as *const u8),
    ("checkpoint", &[Type::Any], Type::Any, agent_checkpoint as *const u8),
    ("rollback", &[Type::Any], Type::Any, agent_rollback as *const u8),
];

#[cfg(test)]
mod tests {
    use super::*;

    fn field(value: &Dynamic, key: &str) -> Dynamic {
        if let Dynamic::Map(map) = value {
            map.read().get(key).cloned().unwrap_or(Dynamic::Null)
        } else {
            Dynamic::Null
        }
    }

    /// ask 的超时拒绝路径：宿主不写 ask_reply 时 fail-closed。
    /// 用最短可测超时不可注入——改为直接测 wait_map 的超时返回 None。
    /// ask/checkpoint 的超时 fail-closed 路径：宿主缺席时不允许无限挂起。
    /// report 的追加语义由脚本层端到端覆盖（全局 root 树在并行单测间共享，
    /// 单测断言 list 长度不稳定）。
    #[test]
    fn wait_map_times_out_without_host() {
        let _ = root::remove("local/zbuddy/test_absent");
        let start = Instant::now();
        let result = wait_map("local/zbuddy/test_absent", Duration::from_millis(80));
        assert!(result.is_none());
        assert!(start.elapsed() >= Duration::from_millis(70));
    }
}
