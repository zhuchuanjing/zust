//! 通用 zust 脚本运行器
//!
//! 用法:
//!   zust <script.zs> [args...]
//!
//! 第一个位置参数是 .zs 脚本路径,后续所有参数都进 `ctx.args` 数组传给脚本。
//!
//! zust 脚本的 `main(ctx)` 函数会被调用,`ctx` 至少包含:
//!   - `args`:   List<String>,所有命令行参数(脚本名后的部分)
//!   - `script`: String,本次执行的 .zs 路径
//!   - `cwd`:    String,当前工作目录

use anyhow::{Context, Result};
use clap::Parser;
use dynamic::{Dynamic, ToJson, Type};
use std::collections::BTreeMap;
use std::path::PathBuf;

#[derive(Debug, Parser)]
#[command(name = "zust", about = "通用 zust 脚本运行器:zust <script.zs> [args...]")]
struct Args {
    /// 要执行的 .zs 脚本路径
    script: PathBuf,

    /// 备用入口(默认 main)
    #[arg(long, default_value = "main")]
    function: String,

    /// 透传给 .zs 脚本的其余参数
    #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
    script_args: Vec<String>,
}

fn main() -> Result<()> {
    let args = Args::parse();

    let vm = vm::Vm::with_all()?;

    let module = "zust_cli";
    let script_path = args.script.to_str().context("script 路径不是 UTF-8")?.to_string();
    vm.jit.write().compiler.import_file(module, &script_path).with_context(|| format!("编译 {script_path} 失败"))?;

    let full_name = format!("{module}::{}", args.function);
    let (ptr, _ret_ty) = vm.jit.write().get_fn_ptr(&full_name, &[Type::Any]).with_context(|| format!("找不到 {full_name}"))?;
    let run: extern "C" fn(*const Dynamic) -> *const Dynamic = unsafe { std::mem::transmute(ptr) };

    let ctx = build_context(&args);
    let ctx_ptr = alloc_dynamic(ctx);
    let result_ptr = run(ctx_ptr);
    if !result_ptr.is_null() {
        let result = unsafe { Box::from_raw(result_ptr as *mut Dynamic) };
        let mut out = String::new();
        result.to_json(&mut out);
        println!("{out}");
    }
    Ok(())
}

fn alloc_dynamic(value: Dynamic) -> *const Dynamic {
    Box::into_raw(Box::new(value)) as *const Dynamic
}

fn build_context(args: &Args) -> Dynamic {
    let mut values: BTreeMap<smol_str::SmolStr, Dynamic> = BTreeMap::new();
    values.insert("script".into(), Dynamic::from(args.script.to_string_lossy().to_string()));
    values.insert("function".into(), Dynamic::from(args.function.clone()));
    let cwd = std::env::current_dir().map(|p| p.to_string_lossy().to_string()).unwrap_or_default();
    values.insert("cwd".into(), Dynamic::from(cwd));
    let arg_dyns: Vec<Dynamic> = args.script_args.iter().map(|s| Dynamic::from(s.clone())).collect();
    values.insert("args".into(), Dynamic::list(arg_dyns));
    Dynamic::map(values)
}
