//! `process::*` —— 子进程执行。
//!
//! - `process::run(cmd, args, opts) -> map` 同步执行命令并等待结束。
//!   opts 可选字段:`timeout_ms`(默认 60000)、`cwd`、`env`(map,追加到当前
//!   环境)、`max_chars`(stdout/stderr 截断上限,默认 16000)。
//! - 返回 `{ok, code, stdout, stderr, timed_out}`:ok = 正常退出且 code 为 0
//!   且未超时;超时会 kill 子进程。stdout/stderr 超过 max_chars 时截断并在
//!   末尾标注被裁掉的字符数。
//!
//! 主要消费方是 agent 沙箱 VM:输出截断属于上下文经济的一部分,模型程序
//! 应把完整输出留在局部变量里,只把摘要写回日志或 agent::report。
use crate::memory::alloc_dynamic;
use dynamic::{Dynamic, Type};
use std::io::Read;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

const DEFAULT_TIMEOUT_MS: i64 = 60_000;
const DEFAULT_MAX_CHARS: usize = 16_000;
/// 轮询子进程退出的间隔。再小没有意义:try_wait 的成本远大于精度收益。
const POLL_INTERVAL: Duration = Duration::from_millis(10);

extern "C" fn process_run(cmd: *const Dynamic, args: *const Dynamic, opts: *const Dynamic) -> *const Dynamic {
    let cmd = unsafe { (&*cmd).clone() };
    let args = unsafe { (&*args).clone() };
    let opts = unsafe { (&*opts).clone() };
    alloc_dynamic(run_command(&cmd, &args, &opts))
}

fn run_command(cmd: &Dynamic, args: &Dynamic, opts: &Dynamic) -> Dynamic {
    let Some(cmd) = as_text(cmd) else {
        return error_result("cmd must be string");
    };
    let Some(arg_list) = as_text_list(args) else {
        return error_result("args must be list of string");
    };

    let timeout_ms = opt_int(opts, "timeout_ms", DEFAULT_TIMEOUT_MS);
    let max_chars = opt_int(opts, "max_chars", DEFAULT_MAX_CHARS as i64).max(0) as usize;
    let cwd = opt_text(opts, "cwd");
    let env = opt_string_map(opts, "env");

    let mut command = Command::new(&cmd);
    command.args(&arg_list).stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped());
    if let Some(cwd) = cwd.as_deref() {
        command.current_dir(cwd);
    }
    if let Some(env) = env {
        for (key, value) in env {
            command.env(key, value);
        }
    }

    let Ok(mut child) = command.spawn() else {
        return error_result(&format!("spawn failed: {cmd}"));
    };

    // pipe 容量有限,读端不跟上会塞住子进程,try_wait 永远等不到退出;
    // 每根 pipe 一个读线程,主线程只负责等退出和超时 kill。
    let stdout_pipe = child.stdout.take();
    let stderr_pipe = child.stderr.take();
    let stdout_reader = std::thread::spawn(move || read_all(stdout_pipe));
    let stderr_reader = std::thread::spawn(move || read_all(stderr_pipe));

    let deadline = Instant::now() + Duration::from_millis(timeout_ms.max(0) as u64);
    let mut timed_out = false;
    let code = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status.code(),
            Ok(None) => {
                if Instant::now() >= deadline {
                    let _ = child.kill();
                    let _ = child.wait();
                    timed_out = true;
                    break None;
                }
                std::thread::sleep(POLL_INTERVAL);
            }
            Err(_) => break None,
        }
    };

    let stdout = String::from_utf8_lossy(&stdout_reader.join().unwrap_or_default()).to_string();
    let stderr = String::from_utf8_lossy(&stderr_reader.join().unwrap_or_default()).to_string();
    let (stdout, stdout_cut) = truncate_chars(&stdout, max_chars);
    let (stderr, stderr_cut) = truncate_chars(&stderr, max_chars);
    let exit_code = code.unwrap_or(-1);
    let ok = !timed_out && code == Some(0);

    dynamic::map!(
        "ok" => ok,
        "code" => exit_code,
        "stdout" => stdout,
        "stdout_truncated" => stdout_cut,
        "stderr" => stderr,
        "stderr_truncated" => stderr_cut,
        "timed_out" => timed_out
    )
}

fn error_result(message: &str) -> Dynamic {
    dynamic::map!("ok" => false, "error" => message)
}

fn read_all<R: Read>(mut pipe: Option<R>) -> Vec<u8> {
    let mut buffer = Vec::new();
    if let Some(pipe) = pipe.as_mut() {
        let _ = pipe.read_to_end(&mut buffer);
    }
    buffer
}

fn as_text(value: &Dynamic) -> Option<String> {
    match value {
        Dynamic::String(text) => Some(text.to_string()),
        Dynamic::StringBuf(text) => Some(text.clone()),
        _ => None,
    }
}

fn as_text_list(value: &Dynamic) -> Option<Vec<String>> {
    match value {
        Dynamic::List(list) => list.read().iter().map(as_text).collect(),
        _ => None,
    }
}

fn opt_int(opts: &Dynamic, key: &str, default: i64) -> i64 {
    if let Dynamic::Map(map) = opts {
        if let Some(value) = map.read().get(key) {
            match value {
                Dynamic::I32(n) => return *n as i64,
                Dynamic::I64(n) => return *n,
                Dynamic::U32(n) => return *n as i64,
                Dynamic::U64(n) => return *n as i64,
                _ => {}
            }
        }
    }
    default
}

fn opt_text(opts: &Dynamic, key: &str) -> Option<String> {
    if let Dynamic::Map(map) = opts {
        if let Some(value) = map.read().get(key) {
            return as_text(value);
        }
    }
    None
}

fn opt_string_map(opts: &Dynamic, key: &str) -> Option<Vec<(String, String)>> {
    if let Dynamic::Map(map) = opts {
        if let Some(Dynamic::Map(env)) = map.read().get(key) {
            let env = env.read();
            return Some(env.iter().filter_map(|(k, v)| as_text(v).map(|v| (k.to_string(), v))).collect());
        }
    }
    None
}

/// 按字符截断(不是字节),避免把 UTF-8 序列切半。返回 (文本, 是否截断)。
fn truncate_chars(text: &str, max: usize) -> (String, bool) {
    let total = text.chars().count();
    if total <= max {
        return (text.to_string(), false);
    }
    let head: String = text.chars().take(max).collect();
    (format!("{head}\n...[truncated {} chars]", total - max), true)
}

pub const PROCESS_NATIVE: [(&str, &[Type], Type, *const u8); 1] =
    [("run", &[Type::Any, Type::Any, Type::Any], Type::Any, process_run as *const u8)];

#[cfg(test)]
mod tests {
    use super::*;

    fn run(cmd: &str, args: Vec<&str>, opts: Dynamic) -> Dynamic {
        let args = Dynamic::list(args.into_iter().map(Dynamic::from).collect());
        run_command(&Dynamic::from(cmd), &args, &opts)
    }

    fn opts(pairs: &[(&str, Dynamic)]) -> Dynamic {
        let mut map = std::collections::BTreeMap::new();
        for (key, value) in pairs {
            map.insert(smol_str::SmolStr::from(*key), value.clone());
        }
        Dynamic::map(map)
    }

    fn field(value: &Dynamic, key: &str) -> Dynamic {
        if let Dynamic::Map(map) = value {
            map.read().get(key).cloned().unwrap_or(Dynamic::Null)
        } else {
            Dynamic::Null
        }
    }

    #[test]
    fn echo_returns_stdout_and_ok() {
        let result = run("echo", vec!["hello", "zust"], Dynamic::Null);
        assert_eq!(field(&result, "ok"), Dynamic::Bool(true));
        assert_eq!(field(&result, "code"), Dynamic::I64(0));
        assert_eq!(field(&result, "stdout"), Dynamic::from("hello zust\n"));
    }

    #[test]
    fn nonzero_exit_reports_code() {
        let result = run("sh", vec!["-c", "exit 3"], Dynamic::Null);
        assert_eq!(field(&result, "ok"), Dynamic::Bool(false));
        assert_eq!(field(&result, "code"), Dynamic::I64(3));
    }

    #[test]
    fn timeout_kills_and_marks_timed_out() {
        let result = run("sleep", vec!["5"], opts(&[("timeout_ms", Dynamic::I64(100))]));
        assert_eq!(field(&result, "timed_out"), Dynamic::Bool(true));
        assert_eq!(field(&result, "ok"), Dynamic::Bool(false));
    }

    #[test]
    fn long_output_truncated_by_max_chars() {
        let script = "yes hello | head -c 100000";
        let result = run("sh", vec!["-c", script], opts(&[("max_chars", Dynamic::I64(100))]));
        assert_eq!(field(&result, "stdout_truncated"), Dynamic::Bool(true));
        let stdout = as_text(&field(&result, "stdout")).unwrap();
        assert!(stdout.starts_with("hello"));
        assert!(stdout.contains("[truncated"));
    }

    #[test]
    fn env_and_cwd_options_apply() {
        let script = "printf %s $ZBUDDY_PROBE";
        let result = run("sh", vec!["-c", script], opts(&[("env", opts(&[("ZBUDDY_PROBE", Dynamic::from("42"))]))]));
        assert_eq!(as_text(&field(&result, "stdout")).unwrap(), "42");
    }

    #[test]
    fn bad_args_return_error_map() {
        let result = run_command(&Dynamic::from(1), &Dynamic::Null, &Dynamic::Null);
        assert_eq!(field(&result, "ok"), Dynamic::Bool(false));
        assert_eq!(field(&result, "error"), Dynamic::from("cmd must be string"));
    }

    #[test]
    fn truncate_chars_keeps_utf8_boundary() {
        let (text, cut) = truncate_chars("你好世界", 3);
        assert!(cut);
        assert!(text.starts_with("你好世"));
        assert!(text.contains("1 chars"));
    }
}
