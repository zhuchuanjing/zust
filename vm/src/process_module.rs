//! `process::*` —— 子进程执行。
//!
//! - `process::run(cmd, args, opts) -> map` 同步执行命令并等待结束。
//! - `process::spawn(cmd, args, opts) -> {ok, id, pid, running}` 启动受监督进程；
//!   `process::poll(id)` 非阻塞查询，终态查询会消费 handle；
//!   `process::write_stdin(id, text, close)` 发送输入并可关闭 stdin；
//!   `process::read_output(id)` 读取进程尚未返回的新增 stdout/stderr；
//!   `process::terminate(id)` 强制结束并回收进程与输出。
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
use std::collections::HashMap;
use std::io::{Read, Write};
use std::path::{Component, Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::{Arc, LazyLock, Mutex};
use std::thread::{JoinHandle, ThreadId};
use std::time::{Duration, Instant};

const DEFAULT_TIMEOUT_MS: i64 = 60_000;
const DEFAULT_MAX_CHARS: usize = 16_000;
/// 轮询子进程退出的间隔。再小没有意义:try_wait 的成本远大于精度收益。
const POLL_INTERVAL: Duration = Duration::from_millis(10);

/// 子进程句柄的两种形态：pipe（std Command，stdin/stdout/stderr 三根管道）
/// 与 pty（portable-pty，master 读写端 + 合并输出流）。
enum ChildHandle {
    Pipe(Child),
    #[cfg(feature = "pty")]
    Pty {
        child: Box<dyn portable_pty::Child + Send>,
        master: Box<dyn portable_pty::MasterPty>,
    },
}

/// 归一化的退出结果：pipe 与 pty 的 ExitStatus 类型不同，统一成
/// (code, success)；pty 的信号死亡表现为 success=false。
struct ExitOutcome {
    code: i64,
    success: bool,
}

impl ChildHandle {
    fn try_wait(&mut self) -> std::io::Result<Option<ExitOutcome>> {
        match self {
            ChildHandle::Pipe(child) => Ok(child
                .try_wait()?
                .map(|status| ExitOutcome { code: status.code().unwrap_or(-1) as i64, success: status.success() })),
            #[cfg(feature = "pty")]
            ChildHandle::Pty { child, .. } => Ok(child
                .try_wait()?
                .map(|status| ExitOutcome { code: status.exit_code() as i64, success: status.success() })),
        }
    }

    fn kill(&mut self) -> std::io::Result<()> {
        match self {
            ChildHandle::Pipe(child) => child.kill(),
            #[cfg(feature = "pty")]
            ChildHandle::Pty { child, .. } => child.kill().map_err(|error| std::io::Error::other(error.to_string())),
        }
    }

    fn wait(&mut self) -> std::io::Result<ExitOutcome> {
        match self {
            ChildHandle::Pipe(child) => Ok(child
                .wait()
                .map(|status| ExitOutcome { code: status.code().unwrap_or(-1) as i64, success: status.success() })?),
            #[cfg(feature = "pty")]
            ChildHandle::Pty { child, .. } => Ok(child
                .wait()
                .map(|status| ExitOutcome { code: status.exit_code() as i64, success: status.success() })
                .map_err(|error| std::io::Error::other(error.to_string()))?),
        }
    }

    fn pid(&self) -> u32 {
        match self {
            ChildHandle::Pipe(child) => child.id(),
            #[cfg(feature = "pty")]
            ChildHandle::Pty { child, .. } => child.process_id().unwrap_or(0),
        }
    }
}

struct ManagedProcess {
    child: ChildHandle,
    stdin: Option<Box<dyn Write + Send>>,
    stdout: Arc<Mutex<Vec<u8>>>,
    stderr: Arc<Mutex<Vec<u8>>>,
    stdout_cursor: usize,
    stderr_cursor: usize,
    stdout_reader: Option<JoinHandle<()>>,
    stderr_reader: Option<JoinHandle<()>>,
    max_chars: usize,
    pid: u32,
    owner: ThreadId,
}

static NEXT_PROCESS_ID: AtomicI64 = AtomicI64::new(1);
static PROCESS_REGISTRY: LazyLock<Mutex<HashMap<i64, ManagedProcess>>> = LazyLock::new(|| Mutex::new(HashMap::new()));

extern "C" fn process_run(cmd: *const Dynamic, args: *const Dynamic, opts: *const Dynamic) -> *const Dynamic {
    let cmd = unsafe { (&*cmd).clone() };
    let args = unsafe { (&*args).clone() };
    let opts = unsafe { (&*opts).clone() };
    let result = run_command(&cmd, &args, &opts);
    let _ = root::add_value("local/zbuddy/last_process", result.clone());
    alloc_dynamic(result)
}

extern "C" fn process_spawn(cmd: *const Dynamic, args: *const Dynamic, opts: *const Dynamic) -> *const Dynamic {
    let cmd = unsafe { (&*cmd).clone() };
    let args = unsafe { (&*args).clone() };
    let opts = unsafe { (&*opts).clone() };
    alloc_dynamic(spawn_command(&cmd, &args, &opts))
}

#[cfg(feature = "pty")]
extern "C" fn process_spawn_pty(cmd: *const Dynamic, args: *const Dynamic, opts: *const Dynamic) -> *const Dynamic {
    let cmd = unsafe { (&*cmd).clone() };
    let args = unsafe { (&*args).clone() };
    let opts = unsafe { (&*opts).clone() };
    alloc_dynamic(spawn_pty_command(&cmd, &args, &opts))
}

#[cfg(feature = "pty")]
extern "C" fn process_resize_pty(id: *const Dynamic, rows: *const Dynamic, cols: *const Dynamic) -> *const Dynamic {
    let id = unsafe { (&*id).clone() };
    let rows = unsafe { (&*rows).clone() };
    let cols = unsafe { (&*cols).clone() };
    alloc_dynamic(resize_pty(&id, &rows, &cols))
}

extern "C" fn process_poll(id: *const Dynamic) -> *const Dynamic {
    let id = unsafe { &*id };
    alloc_dynamic(poll_process(id))
}

extern "C" fn process_terminate(id: *const Dynamic) -> *const Dynamic {
    let id = unsafe { &*id };
    alloc_dynamic(terminate_process(id))
}

extern "C" fn process_write_stdin(id: *const Dynamic, text: *const Dynamic, close: bool) -> *const Dynamic {
    let id = unsafe { &*id };
    let text = unsafe { &*text };
    alloc_dynamic(write_process_stdin(id, text, close))
}

extern "C" fn process_read_output(id: *const Dynamic) -> *const Dynamic {
    let id = unsafe { &*id };
    alloc_dynamic(read_process_output(id))
}

fn run_command(cmd: &Dynamic, args: &Dynamic, opts: &Dynamic) -> Dynamic {
    let Some(cmd) = as_text(cmd) else {
        return error_result("cmd must be string");
    };
    let Some(arg_list) = as_text_list(args) else {
        return error_result("args must be list of string");
    };

    // 参数级审批：执行前按真实 argv 匹配宿主策略（local/zbuddy/policy）。
    // allow 规则做 argv 前缀匹配（"cargo test" 匹配 ["cargo","test",...]），
    // 未命中时走 agent::ask 同一审批协议——人/白名单看到的是实际命令而非
    // 模型的自由文本描述。无策略节点时保持原行为（非沙箱场景不拦截）。
    if let Err(denied) = check_policy(&cmd, &arg_list) {
        return error_result(&denied);
    }

    let timeout_ms = opt_int(opts, "timeout_ms", DEFAULT_TIMEOUT_MS);
    let max_chars = opt_int(opts, "max_chars", DEFAULT_MAX_CHARS as i64).max(0) as usize;
    let cwd = match process_cwd(opts) {
        Ok(cwd) => cwd,
        Err(error) => return error_result(&error),
    };
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

    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(error) => return error_result(&format!("spawn failed: {cmd}: {error}")),
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

fn spawn_command(cmd: &Dynamic, args: &Dynamic, opts: &Dynamic) -> Dynamic {
    let Some(cmd) = as_text(cmd) else {
        return error_result("cmd must be string");
    };
    let Some(arg_list) = as_text_list(args) else {
        return error_result("args must be list of string");
    };
    if let Err(denied) = check_policy(&cmd, &arg_list) {
        return error_result(&denied);
    }

    let max_chars = opt_int(opts, "max_chars", DEFAULT_MAX_CHARS as i64).max(0) as usize;
    let cwd = match process_cwd(opts) {
        Ok(cwd) => cwd,
        Err(error) => return error_result(&error),
    };
    let env = opt_string_map(opts, "env");
    let mut command = Command::new(&cmd);
    command.args(&arg_list).stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped());
    if let Some(cwd) = cwd.as_deref() {
        command.current_dir(cwd);
    }
    if let Some(env) = env {
        for (key, value) in env {
            command.env(key, value);
        }
    }
    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(error) => return error_result(&format!("spawn failed: {cmd}: {error}")),
    };
    let pid = child.id();
    let stdin = child.stdin.take().map(|stdin| Box::new(stdin) as Box<dyn Write + Send>);
    let stdout_pipe = child.stdout.take();
    let stderr_pipe = child.stderr.take();
    let (stdout, stdout_reader) = capture_output(stdout_pipe);
    let (stderr, stderr_reader) = capture_output(stderr_pipe);
    let managed = ManagedProcess { child: ChildHandle::Pipe(child), stdin, stdout, stderr, stdout_cursor: 0, stderr_cursor: 0, stdout_reader: Some(stdout_reader), stderr_reader: Some(stderr_reader), max_chars, pid, owner: std::thread::current().id() };
    let id = NEXT_PROCESS_ID.fetch_add(1, Ordering::Relaxed);
    let Ok(mut registry) = PROCESS_REGISTRY.lock() else {
        return error_result("process registry poisoned");
    };
    registry.insert(id, managed);
    dynamic::map!("ok" => true, "id" => id, "pid" => pid as i64, "running" => true)
}

/// `process::spawn_pty(cmd, argv, opts)`：在伪终端里启动受监督子进程。
/// opts 额外字段 `rows`/`cols`（默认 24/80）。句柄与 spawn 同一注册表：
/// write_stdin 写 master（`\x03` 由内核行规程翻译为 SIGINT）、read_output
/// 读合并流（ONLCR 会把 `\n` 回显成 `\r\n`）、poll/terminate 同语义。
/// 终端语义（isatty、窗口尺寸、控制终端）由 pty 提供，无法用管道组合，
/// 是 process 模块的最窄补充。
#[cfg(feature = "pty")]
fn spawn_pty_command(cmd: &Dynamic, args: &Dynamic, opts: &Dynamic) -> Dynamic {
    let Some(cmd) = as_text(cmd) else {
        return error_result("cmd must be string");
    };
    let Some(arg_list) = as_text_list(args) else {
        return error_result("args must be list of string");
    };
    // 审批与 shell 拒绝与 pipe 路径完全一致：pty 不是绕过策略的新入口
    if let Err(denied) = check_policy(&cmd, &arg_list) {
        return error_result(&denied);
    }

    let rows = opt_int(opts, "rows", 24).max(1);
    let cols = opt_int(opts, "cols", 80).max(1);
    let max_chars = opt_int(opts, "max_chars", DEFAULT_MAX_CHARS as i64).max(0) as usize;
    let cwd = match process_cwd(opts) {
        Ok(cwd) => cwd,
        Err(error) => return error_result(&error),
    };
    let env_extra = opt_string_map(opts, "env");

    let pty_system = portable_pty::native_pty_system();
    let pair = match pty_system.openpty(portable_pty::PtySize { rows: rows as u16, cols: cols as u16, pixel_width: 0, pixel_height: 0 }) {
        Ok(pair) => pair,
        Err(error) => return error_result(&format!("openpty failed: {error}")),
    };

    let mut builder = portable_pty::CommandBuilder::new(&cmd);
    for arg in &arg_list {
        builder.arg(arg);
    }
    if let Some(cwd) = cwd.as_deref() {
        builder.cwd(cwd);
    }
    // 显式继承当前环境再叠加 opts.env：不依赖 crate 对默认环境的实现细节
    for (key, value) in std::env::vars() {
        builder.env(&key, &value);
    }
    if let Some(env_extra) = env_extra {
        for (key, value) in env_extra {
            builder.env(&key, &value);
        }
    }

    let child = match pair.slave.spawn_command(builder) {
        Ok(child) => child,
        Err(error) => return error_result(&format!("spawn failed: {cmd}: {error}")),
    };
    let pid = child.process_id().unwrap_or(0);
    let writer = pair.master.take_writer().ok().map(|writer| writer as Box<dyn Write + Send>);
    let reader = pair.master.try_clone_reader().ok();
    let (stdout, _stdout_reader) = capture_output(reader);
    // pty 只有 master 一条合并流；stderr 缓冲保持为空
    let (stderr, _stderr_reader) = capture_output(None::<std::io::Empty>);
    let managed = ManagedProcess {
        child: ChildHandle::Pty { child, master: pair.master },
        stdin: writer,
        stdout,
        stderr,
        stdout_cursor: 0,
        stderr_cursor: 0,
        // 不 join pty 读线程：子进程退出后，disowned 后代可能仍持有 pty，
        // master 读端不会 EOF，无限 join 会挂死回收路径
        stdout_reader: None,
        stderr_reader: None,
        max_chars,
        pid,
        owner: std::thread::current().id(),
    };
    let id = NEXT_PROCESS_ID.fetch_add(1, Ordering::Relaxed);
    let Ok(mut registry) = PROCESS_REGISTRY.lock() else {
        return error_result("process registry poisoned");
    };
    registry.insert(id, managed);
    dynamic::map!("ok" => true, "id" => id, "pid" => pid as i64, "running" => true, "pty" => true)
}

/// `process::resize_pty(id, rows, cols)`：更新窗口尺寸；子进程侧表现为
/// TIOCSWINSZ + SIGWINCH（Windows 为 ConPTY resize）。
#[cfg(feature = "pty")]
fn resize_pty(id: &Dynamic, rows: &Dynamic, cols: &Dynamic) -> Dynamic {
    let Some(id) = as_i64(id) else {
        return error_result("id must be integer");
    };
    let Some(rows) = as_i64(rows) else {
        return error_result("rows must be integer");
    };
    let Some(cols) = as_i64(cols) else {
        return error_result("cols must be integer");
    };
    if rows < 1 || cols < 1 {
        return error_result("rows and cols must be positive");
    }
    let Ok(mut registry) = PROCESS_REGISTRY.lock() else {
        return error_result("process registry poisoned");
    };
    let Some(managed) = registry.get_mut(&id) else {
        return error_result(&format!("unknown process id: {id}"));
    };
    let ChildHandle::Pty { master, .. } = &mut managed.child else {
        return error_result(&format!("process id {id} is not a pty"));
    };
    match master.resize(portable_pty::PtySize { rows: rows as u16, cols: cols as u16, pixel_width: 0, pixel_height: 0 }) {
        Ok(()) => dynamic::map!("ok" => true, "id" => id, "rows" => rows, "cols" => cols),
        Err(error) => error_result(&format!("pty resize failed: {error}")),
    }
}

fn poll_process(id: &Dynamic) -> Dynamic {
    let Some(id) = as_i64(id) else {
        return error_result("id must be integer");
    };
    let Ok(mut registry) = PROCESS_REGISTRY.lock() else {
        return error_result("process registry poisoned");
    };
    let Some(mut managed) = registry.remove(&id) else {
        return error_result(&format!("unknown process id: {id}"));
    };
    match managed.child.try_wait() {
        Ok(Some(status)) => finish_managed(id, managed, status, false),
        Ok(None) => {
            let pid = managed.pid;
            registry.insert(id, managed);
            dynamic::map!("ok" => true, "id" => id, "pid" => pid as i64, "running" => true)
        }
        Err(err) => {
            let _ = managed.child.kill();
            let _ = managed.child.wait();
            error_result(&format!("process poll failed: {err}"))
        }
    }
}

fn terminate_process(id: &Dynamic) -> Dynamic {
    let Some(id) = as_i64(id) else {
        return error_result("id must be integer");
    };
    let Ok(mut registry) = PROCESS_REGISTRY.lock() else {
        return error_result("process registry poisoned");
    };
    let Some(mut managed) = registry.remove(&id) else {
        return error_result(&format!("unknown process id: {id}"));
    };
    let killed = managed.child.kill().is_ok();
    match managed.child.wait() {
        Ok(status) => finish_managed(id, managed, status, killed),
        Err(err) => error_result(&format!("process terminate failed: {err}")),
    }
}fn write_process_stdin(id: &Dynamic, text: &Dynamic, close: bool) -> Dynamic {
    let Some(id) = as_i64(id) else {
        return error_result("id must be integer");
    };
    let Some(text) = as_text(text) else {
        return error_result("text must be string");
    };
    let Ok(mut registry) = PROCESS_REGISTRY.lock() else {
        return error_result("process registry poisoned");
    };
    let Some(managed) = registry.get_mut(&id) else {
        return error_result(&format!("unknown process id: {id}"));
    };
    let Some(stdin) = managed.stdin.as_mut() else {
        return error_result(&format!("stdin is closed for process id: {id}"));
    };
    if let Err(error) = stdin.write_all(text.as_bytes()).and_then(|_| stdin.flush()) {
        managed.stdin = None;
        return error_result(&format!("process stdin write failed: {error}"));
    }
    if close {
        managed.stdin = None;
    }
    dynamic::map!(
        "ok" => true,
        "id" => id,
        "pid" => managed.pid as i64,
        "running" => true,
        "stdin_closed" => close
    )
}

fn read_process_output(id: &Dynamic) -> Dynamic {
    let Some(id) = as_i64(id) else {
        return error_result("id must be integer");
    };
    let Ok(mut registry) = PROCESS_REGISTRY.lock() else {
        return error_result("process registry poisoned");
    };
    let Some(managed) = registry.get_mut(&id) else {
        return error_result(&format!("unknown process id: {id}"));
    };
    let running = match managed.child.try_wait() {
        Ok(status) => status.is_none(),
        Err(error) => return error_result(&format!("process output read failed: {error}")),
    };
    let (stdout, stdout_cut) = read_increment(&managed.stdout, &mut managed.stdout_cursor, managed.max_chars);
    let (stderr, stderr_cut) = read_increment(&managed.stderr, &mut managed.stderr_cursor, managed.max_chars);
    dynamic::map!(
        "ok" => true,
        "id" => id,
        "pid" => managed.pid as i64,
        "running" => running,
        "stdout" => stdout,
        "stdout_truncated" => stdout_cut,
        "stderr" => stderr,
        "stderr_truncated" => stderr_cut
    )
}

fn finish_managed(id: i64, mut managed: ManagedProcess, status: ExitOutcome, terminated: bool) -> Dynamic {
    #[cfg(feature = "pty")]
    let is_pty = matches!(managed.child, ChildHandle::Pty { .. });
    #[cfg(not(feature = "pty"))]
    let is_pty = false;
    // 被 SIGKILL 的进程其管道写端可能仍被 fork 出的后代持有（OpenSSH 的
    // ControlPersist master 会继承客户端管道）：读线程等不到 EOF，无限
    // join 会挂死回收路径。有界 drain 后放弃 join，直接快照缓冲。
    if is_pty || terminated {
        std::thread::sleep(Duration::from_millis(50));
    } else {
        if let Some(reader) = managed.stdout_reader.take() {
            let _ = reader.join();
        }
        if let Some(reader) = managed.stderr_reader.take() {
            let _ = reader.join();
        }
    }
    let stdout = captured_text(&managed.stdout);
    let stderr = captured_text(&managed.stderr);
    let (stdout, stdout_cut) = truncate_chars(&stdout, managed.max_chars);
    let (stderr, stderr_cut) = truncate_chars(&stderr, managed.max_chars);
    let code = status.code;
    dynamic::map!(
        "ok" => !terminated && status.success,
        "id" => id,
        "pid" => managed.pid as i64,
        "running" => false,
        "terminated" => terminated,
        "code" => code,
        "stdout" => stdout,
        "stdout_truncated" => stdout_cut,
        "stderr" => stderr,
        "stderr_truncated" => stderr_cut
    )
}

/// 回收当前 Zust 执行线程遗留的所有受监督子进程。
///
/// `process::spawn` 的显式 poll/terminate 仍是正常路径；宿主在程序结束（包括
/// panic/fault）时调用本函数兜底，避免提前 return 的模型程序把进程带到下一会话。
pub fn terminate_current_thread_processes() -> usize {
    let owner = std::thread::current().id();
    let managed = {
        let Ok(mut registry) = PROCESS_REGISTRY.lock() else {
            return 0;
        };
        let ids = registry.iter().filter_map(|(id, process)| (process.owner == owner).then_some(*id)).collect::<Vec<_>>();
        ids.into_iter().filter_map(|id| registry.remove(&id)).collect::<Vec<_>>()
    };
    let count = managed.len();
    for mut process in managed {
        let _ = process.child.kill();
        let _ = process.child.wait();
        // 与 finish_managed 的 terminate 路径同一理由：fd 可能被后代持有，
        // 有界 drain 代替无限 join
        std::thread::sleep(Duration::from_millis(50));
    }
    count
}

fn error_result(message: &str) -> Dynamic {
    // 字段形状与成功返回保持一致：调用方（模型程序）读 out.stdout / out.code
    // 不应拿到 Null（spawn 失败/参数错时 stdout 为空串、code 为 -1）
    dynamic::map!("ok" => false, "error" => message, "code" => -1i64, "stdout" => "", "stderr" => message)
}

fn read_all<R: Read>(mut pipe: Option<R>) -> Vec<u8> {
    let mut buffer = Vec::new();
    if let Some(pipe) = pipe.as_mut() {
        let _ = pipe.read_to_end(&mut buffer);
    }
    buffer
}

fn capture_output<R: Read + Send + 'static>(mut pipe: Option<R>) -> (Arc<Mutex<Vec<u8>>>, JoinHandle<()>) {
    let output = Arc::new(Mutex::new(Vec::new()));
    let writer = Arc::clone(&output);
    let reader = std::thread::spawn(move || {
        let Some(pipe) = pipe.as_mut() else { return };
        let mut chunk = [0u8; 8192];
        loop {
            let count = match pipe.read(&mut chunk) {
                Ok(0) | Err(_) => break,
                Ok(count) => count,
            };
            let Ok(mut bytes) = writer.lock() else { break };
            bytes.extend_from_slice(&chunk[..count]);
        }
    });
    (output, reader)
}

fn captured_text(output: &Arc<Mutex<Vec<u8>>>) -> String {
    let Ok(bytes) = output.lock() else { return String::new() };
    String::from_utf8_lossy(&bytes).to_string()
}

fn read_increment(output: &Arc<Mutex<Vec<u8>>>, cursor: &mut usize, max_chars: usize) -> (String, bool) {
    let Ok(bytes) = output.lock() else { return (String::new(), false) };
    if *cursor >= bytes.len() {
        return (String::new(), false);
    }
    let pending = &bytes[*cursor..];
    let consumed = match std::str::from_utf8(pending) {
        Ok(_) => pending.len(),
        // 保留被 read 系统调用截断的 UTF-8 尾部，下一次与后续字节一起解码。
        Err(error) if error.error_len().is_none() => error.valid_up_to(),
        // 子进程可能输出任意字节；遇到真正非法 UTF-8 时维持原有 lossy 语义。
        Err(_) => pending.len(),
    };
    let text = String::from_utf8_lossy(&pending[..consumed]).to_string();
    *cursor += consumed;
    truncate_chars(&text, max_chars)
}

fn as_text(value: &Dynamic) -> Option<String> {
    match value {
        Dynamic::String(text) => Some(text.to_string()),
        Dynamic::StringBuf(text) => Some(text.clone()),
        _ => None,
    }
}

fn as_i64(value: &Dynamic) -> Option<i64> {
    match value {
        Dynamic::I32(value) => Some(*value as i64),
        Dynamic::I64(value) => Some(*value),
        Dynamic::U32(value) => Some(*value as i64),
        Dynamic::U64(value) => i64::try_from(*value).ok(),
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

/// `cwd` 同时接受真实相对路径与 ROOT 目录挂载路径。`ws` / `ws/src`
/// 会解析到挂载的宿主目录，且拒绝 `..`、绝对路径和符号链接逃逸。
fn process_cwd(opts: &Dynamic) -> Result<Option<PathBuf>, String> {
    let Some(cwd) = opt_text(opts, "cwd") else {
        return Ok(None);
    };
    let lookup = if cwd.contains('/') { cwd.clone() } else { format!("{cwd}/") };
    let Ok((mount, relative)) = root::get_mount(&lookup) else {
        return Ok(Some(PathBuf::from(cwd)));
    };
    let root::Mount::Dir { base } = mount else {
        return Err(format!("cwd mount is not a directory: {cwd}"));
    };
    if Path::new(relative).components().any(|part| !matches!(part, Component::Normal(_) | Component::CurDir)) {
        return Err(format!("cwd escapes mount: {cwd}"));
    }
    let resolved = base.join(relative).canonicalize().map_err(|error| format!("cwd does not exist: {cwd}: {error}"))?;
    if !resolved.starts_with(&base) {
        return Err(format!("cwd escapes mount: {cwd}"));
    }
    Ok(Some(resolved))
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

pub const PROCESS_NATIVE: [(&str, &[Type], Type, *const u8); 6] = [
    ("run", &[Type::Any, Type::Any, Type::Any], Type::Any, process_run as *const u8),
    ("spawn", &[Type::Any, Type::Any, Type::Any], Type::Any, process_spawn as *const u8),
    ("poll", &[Type::Any], Type::Any, process_poll as *const u8),
    ("terminate", &[Type::Any], Type::Any, process_terminate as *const u8),
    ("write_stdin", &[Type::Any, Type::Any, Type::Bool], Type::Any, process_write_stdin as *const u8),
    ("read_output", &[Type::Any], Type::Any, process_read_output as *const u8),
];

#[cfg(feature = "pty")]
pub const PTY_NATIVE: [(&str, &[Type], Type, *const u8); 2] = [
    ("spawn_pty", &[Type::Any, Type::Any, Type::Any], Type::Any, process_spawn_pty as *const u8),
    ("resize_pty", &[Type::Any, Type::Any, Type::Any], Type::Any, process_resize_pty as *const u8),
];

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
        if let Dynamic::Map(map) = value { map.read().get(key).cloned().unwrap_or(Dynamic::Null) } else { Dynamic::Null }
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
        let result = run("false", vec![], Dynamic::Null);
        assert_eq!(field(&result, "ok"), Dynamic::Bool(false));
        assert_ne!(field(&result, "code"), Dynamic::I64(0));
    }

    #[test]
    fn timeout_kills_and_marks_timed_out() {
        let result = run("sleep", vec!["5"], opts(&[("timeout_ms", Dynamic::I64(100))]));
        assert_eq!(field(&result, "timed_out"), Dynamic::Bool(true));
        assert_eq!(field(&result, "ok"), Dynamic::Bool(false));
    }

    #[test]
    fn long_output_truncated_by_max_chars() {
        let text = "hello".repeat(20_000);
        let result = run("printf", vec!["%s", &text], opts(&[("max_chars", Dynamic::I64(100))]));
        assert_eq!(field(&result, "stdout_truncated"), Dynamic::Bool(true));
        let stdout = as_text(&field(&result, "stdout")).unwrap();
        assert!(stdout.starts_with("hello"));
        assert!(stdout.contains("[truncated"));
    }

    #[test]
    fn env_option_applies_without_shell() {
        let result = run("printenv", vec!["ZBUDDY_PROBE"], opts(&[("env", opts(&[("ZBUDDY_PROBE", Dynamic::from("42"))]))]));
        assert_eq!(as_text(&field(&result, "stdout")).unwrap(), "42\n");
    }

    #[test]
    fn bad_args_return_error_map() {
        let result = run_command(&Dynamic::from(1), &Dynamic::Null, &Dynamic::Null);
        assert_eq!(field(&result, "ok"), Dynamic::Bool(false));
        assert_eq!(field(&result, "error"), Dynamic::from("cmd must be string"));
        assert_eq!(field(&result, "stderr"), Dynamic::from("cmd must be string"));
    }

    #[test]
    fn truncate_chars_keeps_utf8_boundary() {
        let (text, cut) = truncate_chars("你好世界", 3);
        assert!(cut);
        assert!(text.starts_with("你好世"));
        assert!(text.contains("1 chars"));
    }

    #[test]
    fn supervised_process_can_be_polled_and_terminated() {
        let started = spawn_command(&Dynamic::from("sleep"), &Dynamic::list(vec![Dynamic::from("5")]), &Dynamic::Null);
        assert_eq!(field(&started, "ok"), Dynamic::Bool(true));
        let id = field(&started, "id");
        assert_eq!(field(&poll_process(&id), "running"), Dynamic::Bool(true));
        let stopped = terminate_process(&id);
        assert_eq!(field(&stopped, "running"), Dynamic::Bool(false));
        assert_eq!(field(&stopped, "terminated"), Dynamic::Bool(true));
    }

    #[test]
    fn supervised_process_accepts_stdin_until_closed() {
        let started = spawn_command(&Dynamic::from("tr"), &Dynamic::list(vec![Dynamic::from("a-z"), Dynamic::from("A-Z")]), &Dynamic::Null);
        assert_eq!(field(&started, "ok"), Dynamic::Bool(true));
        let id = field(&started, "id");
        let wrote = write_process_stdin(&id, &Dynamic::from("hello zust\n"), true);
        assert_eq!(field(&wrote, "ok"), Dynamic::Bool(true));
        assert_eq!(field(&wrote, "stdin_closed"), Dynamic::Bool(true));

        let terminal = loop {
            let state = poll_process(&id);
            if field(&state, "running") == Dynamic::Bool(false) {
                break state;
            }
            std::thread::sleep(Duration::from_millis(10));
        };
        assert_eq!(field(&terminal, "ok"), Dynamic::Bool(true));
        assert_eq!(field(&terminal, "stdout"), Dynamic::from("HELLO ZUST\n"));
    }

    #[test]
    fn supervised_process_exposes_incremental_output_before_exit() {
        let started = spawn_command(&Dynamic::from("cat"), &Dynamic::list(Vec::new()), &Dynamic::Null);
        assert_eq!(field(&started, "ok"), Dynamic::Bool(true));
        let id = field(&started, "id");
        assert_eq!(field(&write_process_stdin(&id, &Dynamic::from("hello zust\n"), false), "ok"), Dynamic::Bool(true));

        let mut streamed = String::new();
        for _ in 0..200 {
            let output = read_process_output(&id);
            assert_eq!(field(&output, "ok"), Dynamic::Bool(true));
            streamed.push_str(&as_text(&field(&output, "stdout")).unwrap());
            if streamed == "hello zust\n" {
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        assert_eq!(streamed, "hello zust\n");
        assert_eq!(field(&write_process_stdin(&id, &Dynamic::from(""), true), "stdin_closed"), Dynamic::Bool(true));

        let terminal = loop {
            let state = poll_process(&id);
            if field(&state, "running") == Dynamic::Bool(false) {
                break state;
            }
            std::thread::sleep(Duration::from_millis(10));
        };
        assert_eq!(field(&terminal, "stdout"), Dynamic::from("hello zust\n"));
    }

    #[test]
    fn supervised_processes_are_reaped_for_current_program_thread() {
        let started = spawn_command(&Dynamic::from("sleep"), &Dynamic::list(vec![Dynamic::from("5")]), &Dynamic::Null);
        assert_eq!(field(&started, "ok"), Dynamic::Bool(true));
        let id = field(&started, "id");
        assert_eq!(terminate_current_thread_processes(), 1);
        assert_eq!(field(&poll_process(&id), "ok"), Dynamic::Bool(false));
    }

    #[test]
    fn auto_approve_policy_allows_unlisted_argv() {
        let _ = root::add_value("local/zbuddy/policy", dynamic::map!("auto_approve" => true, "allow" => Vec::<String>::new()));
        let result = check_policy("unlisted-command", &["arbitrary-argument".to_string()]);
        let _ = root::remove("local/zbuddy/policy");
        assert_eq!(result, Ok(()));
    }

    #[cfg(all(unix, feature = "pty"))]
    fn pty_opts(pairs: &[(&str, Dynamic)]) -> Dynamic {
        opts(pairs)
    }

    #[cfg(all(unix, feature = "pty"))]
    fn wait_terminal(id: &Dynamic) -> Dynamic {
        loop {
            let state = poll_process(id);
            if field(&state, "running") == Dynamic::Bool(false) {
                return state;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    #[cfg(all(unix, feature = "pty"))]
    #[test]
    fn pty_child_sees_terminal_and_window_size() {
        let started = spawn_pty_command(
            &Dynamic::from("/bin/stty"),
            &Dynamic::list(vec![Dynamic::from("size")]),
            &pty_opts(&[("rows", Dynamic::I64(40)), ("cols", Dynamic::I64(120))]),
        );
        assert_eq!(field(&started, "ok"), Dynamic::Bool(true));
        let terminal = wait_terminal(&field(&started, "id"));
        assert_eq!(field(&terminal, "code"), Dynamic::I64(0));
        assert_eq!(as_text(&field(&terminal, "stdout")).unwrap().trim(), "40 120");
    }

    #[cfg(all(unix, feature = "pty"))]
    #[test]
    fn pty_resize_updates_kernel_window_size() {
        let started = spawn_pty_command(
            &Dynamic::from("/bin/sleep"),
            &Dynamic::list(vec![Dynamic::from("5")]),
            &pty_opts(&[("rows", Dynamic::I64(24)), ("cols", Dynamic::I64(80))]),
        );
        let id = field(&started, "id");
        let resized = resize_pty(&id, &Dynamic::I64(50), &Dynamic::I64(200));
        assert_eq!(field(&resized, "ok"), Dynamic::Bool(true));
        let stopped = terminate_process(&id);
        assert_eq!(field(&stopped, "terminated"), Dynamic::Bool(true));
    }

    #[cfg(all(unix, feature = "pty"))]
    #[test]
    fn pty_ctrl_c_byte_delivers_sigint_to_foreground_child() {
        let started = spawn_pty_command(&Dynamic::from("/bin/sleep"), &Dynamic::list(vec![Dynamic::from("30")]), &Dynamic::Null);
        assert_eq!(field(&started, "ok"), Dynamic::Bool(true));
        let id = field(&started, "id");
        assert_eq!(field(&write_process_stdin(&id, &Dynamic::from("\x03"), false), "ok"), Dynamic::Bool(true));
        let begin = Instant::now();
        let terminal = wait_terminal(&id);
        // 行规程把 ^C 翻译成 SIGINT：无需 terminate 就应秒级退出
        assert!(begin.elapsed() < Duration::from_secs(3));
        assert_eq!(field(&terminal, "running"), Dynamic::Bool(false));
    }

    #[cfg(all(unix, feature = "pty"))]
    #[test]
    fn pty_echoes_input_with_line_discipline() {
        let started = spawn_pty_command(&Dynamic::from("/bin/cat"), &Dynamic::list(Vec::new()), &Dynamic::Null);
        assert_eq!(field(&started, "ok"), Dynamic::Bool(true));
        let id = field(&started, "id");
        assert_eq!(field(&write_process_stdin(&id, &Dynamic::from("hi pty\n"), false), "ok"), Dynamic::Bool(true));
        let mut seen = String::new();
        for _ in 0..200 {
            let output = read_process_output(&id);
            seen.push_str(&as_text(&field(&output, "stdout")).unwrap());
            if seen.contains("hi pty") {
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        // 终端回显 + ONLCR：\n 变成 \r\n
        assert!(seen.contains("hi pty\r\n"), "line discipline echo missing: {seen:?}");
        assert_eq!(field(&write_process_stdin(&id, &Dynamic::from(""), true), "ok"), Dynamic::Bool(true));
        let terminal = wait_terminal(&id);
        assert_eq!(field(&terminal, "code"), Dynamic::I64(0));
    }

    #[cfg(all(unix, feature = "pty"))]
    #[test]
    fn pty_refuses_shell_executables_like_pipe_path() {
        let _ = root::add_value("local/zbuddy/policy", dynamic::map!("auto_approve" => true, "allow" => Vec::<String>::new()));
        let denied = spawn_pty_command(&Dynamic::from("/bin/bash"), &Dynamic::list(Vec::new()), &Dynamic::Null);
        let _ = root::remove("local/zbuddy/policy");
        assert_eq!(field(&denied, "ok"), Dynamic::Bool(false));
        let error = as_text(&field(&denied, "error")).unwrap();
        assert!(error.contains("shell"), "unexpected denial: {error}");
    }

    #[test]
    fn shell_command_detection_accepts_paths() {
        assert_eq!(shell_command("sh"), Some("sh"));
        assert_eq!(shell_command("/bin/bash"), Some("bash"));
        assert_eq!(shell_command("cargo"), None);
    }

    #[test]
    fn cwd_accepts_directory_mount_root_and_child() {
        let base = std::env::temp_dir().join(format!("zust-process-cwd-{}", std::process::id()));
        let child = base.join("src");
        std::fs::create_dir_all(&child).unwrap();
        let mount = format!("process_cwd_{}", std::process::id());
        let _ = root::mount_dir(&mount, base.to_str().unwrap()).unwrap();

        let root_cwd = process_cwd(&opts(&[("cwd", Dynamic::from(mount.clone()))])).unwrap().unwrap();
        let child_cwd = process_cwd(&opts(&[("cwd", Dynamic::from(format!("{mount}/src")))])).unwrap().unwrap();
        assert_eq!(root_cwd, base.canonicalize().unwrap());
        assert_eq!(child_cwd, child.canonicalize().unwrap());

        let _ = std::fs::remove_dir_all(base);
    }
}

/// argv 前缀匹配：rule 按空白拆成词序列，是 [cmd, args...] 的前缀即命中。
fn argv_matches(rule: &str, cmd: &str, args: &[String]) -> bool {
    let mut words = rule.split_whitespace();
    let Some(first) = words.next() else { return false };
    if first != cmd {
        return false;
    }
    let mut idx = 0;
    for word in words {
        if idx >= args.len() || args[idx] != word {
            return false;
        }
        idx += 1;
    }
    true
}

fn shell_command(cmd: &str) -> Option<&str> {
    let executable = Path::new(cmd).file_name().and_then(|name| name.to_str()).unwrap_or(cmd);
    matches!(executable, "sh" | "bash" | "zsh" | "dash" | "ksh" | "mksh" | "fish" | "csh" | "tcsh" | "pwsh" | "powershell").then_some(executable)
}

/// 策略检查：local/zbuddy/policy = {auto_approve: bool, allow: [规则...]}。
/// - 节点不存在：不拦截（非沙箱宿主未配置策略）
/// - auto_approve == true：受控测试场景直接放行
/// - allow 命中：放行
/// - 未命中：写 local/zbuddy/ask（question 固定 process、context 是完整 argv），
///   等 ask_reply——宿主白名单/web 人工批的是真实命令。超时/拒绝返回 Err。
fn check_policy(cmd: &str, args: &[String]) -> Result<(), String> {
    let Ok(policy) = root::get("local/zbuddy/policy") else {
        return Ok(());
    };
    if !policy.is_map() {
        return Ok(());
    }
    if let Some(executable) = shell_command(cmd) {
        return Err(format!("shell command is not available in the agent sandbox: {executable}"));
    }
    if policy.get_dynamic("auto_approve").and_then(|value| value.as_bool()) == Some(true) {
        return Ok(());
    }
    if let Some(allow) = policy.get_dynamic("allow") {
        if allow.is_list() {
            for idx in 0..allow.len() {
                if let Some(rule) = allow.get_idx(idx) {
                    if argv_matches(rule.as_str(), cmd, args) {
                        return Ok(());
                    }
                }
            }
        }
    }
    // 未命中：走审批协议（与 agent::ask 同节点，宿主已有处理逻辑）
    let argv = std::iter::once(cmd.to_string()).chain(args.iter().cloned()).collect::<Vec<_>>().join(" ");
    let _ = root::remove("local/zbuddy/ask_reply");
    let _ = root::add("local/zbuddy/ask", root::Object::Value(dynamic::map!("question" => "run command", "context" => argv.clone())));
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(300);
    loop {
        if let Ok(reply) = root::get("local/zbuddy/ask_reply") {
            if reply.is_map() {
                let _ = root::remove("local/zbuddy/ask_reply");
                let approved = reply.get_dynamic("approved").and_then(|v| v.as_bool()).unwrap_or(false);
                return if approved { Ok(()) } else { Err(format!("command denied by approval: {argv}")) };
            }
        }
        if std::time::Instant::now() >= deadline {
            let _ = root::remove("local/zbuddy/ask");
            return Err(format!("command approval timeout: {argv}"));
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
}
