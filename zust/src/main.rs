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
//!
//! 默认 `tree-sitter` feature 启用,会注入:
//!   - `source::scan_repo(path, project, run_id)` 扫描源码目录,按文件分组返回
//!     `{path, language, loc, units: [{kind, name, span, is_public, parent}]}`
//!   - `source::supported_languages()`  返回 List<String> 支持的语种清单
//! tree-sitter 解析在 Rust 内部做,不向 zust 暴露 node 原始接口。

use anyhow::{Context, Result};
use clap::Parser;
use dynamic::{Dynamic, ToJson, Type};
use std::path::PathBuf;
use std::sync::Arc;

#[derive(Debug, Parser)]
#[command(
    name = "zust",
    about = "通用 zust 脚本运行器:zust <script.zs> [args...]"
)]
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
    register_natives(&vm)?;

    let module = "zust_cli";
    let script_path = args
        .script
        .to_str()
        .context("script 路径不是 UTF-8")?
        .to_string();
    vm.jit
        .write()
        .compiler
        .import_file(module, &script_path)
        .with_context(|| format!("编译 {script_path} 失败"))?;

    let full_name = format!("{module}::{}", args.function);
    let (ptr, _ret_ty) = vm
        .jit
        .write()
        .get_fn_ptr(&full_name, &[Type::Any])
        .with_context(|| format!("找不到 {full_name}"))?;
    let run: extern "C" fn(*const Dynamic) -> *const Dynamic =
        unsafe { std::mem::transmute(ptr) };

    let ctx = build_context(&args);
    let ctx_ptr = alloc_dynamic(ctx);
    let result_ptr = run(ctx_ptr as *const Dynamic);
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
    let mut values: std::collections::BTreeMap<smol_str::SmolStr, Dynamic> =
        std::collections::BTreeMap::new();
    values.insert(
        "script".into(),
        Dynamic::from(args.script.to_string_lossy().to_string()),
    );
    values.insert("function".into(), Dynamic::from(args.function.clone()));
    let cwd = std::env::current_dir()
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_default();
    values.insert("cwd".into(), Dynamic::from(cwd));
    let arg_dyns: Vec<Dynamic> = args
        .script_args
        .iter()
        .map(|s| Dynamic::from(s.clone()))
        .collect();
    values.insert("args".into(), Dynamic::list(arg_dyns));
    Dynamic::map(values)
}

fn register_natives(vm: &vm::Vm) -> Result<()> {
    let mut jit = vm.jit.write();
    jit.add_native_module_ptr(
        "source",
        "scan_repo",
        &[Type::Any, Type::Any, Type::Any],
        Type::Any,
        scan_repo_native as *const u8,
    )?;
    jit.add_native_module_ptr(
        "source",
        "supported_languages",
        &[],
        Type::Any,
        supported_languages_native as *const u8,
    )?;
    Ok(())
}

extern "C" fn supported_languages_native() -> *const Dynamic {
    let langs: Vec<Dynamic> = scan::supported_languages()
        .into_iter()
        .map(|s| Dynamic::from(s.to_string()))
        .collect();
    let mut values = std::collections::BTreeMap::new();
    values.insert(smol_str::SmolStr::new("languages"), Dynamic::list(langs));
    let result = Dynamic::map(values);
    Box::into_raw(Box::new(result)) as *const Dynamic
}

extern "C" fn scan_repo_native(
    repo: *const Dynamic,
    project: *const Dynamic,
    run_id: *const Dynamic,
) -> *const Dynamic {
    let result = (|| -> Result<Dynamic> {
        let repo = unsafe { dynamic_string(repo) }.context("repo 参数为空")?;
        let project = unsafe { dynamic_string(project) }.context("project 参数为空")?;
        let run_id = unsafe { dynamic_string(run_id) }.context("run_id 参数为空")?;
        let result = scan::scan_repo(std::path::Path::new(&repo), &project, &run_id)?;
        Ok(result)
    })();
    match result {
        Ok(value) => Box::into_raw(Box::new(value)) as *const Dynamic,
        Err(err) => {
            let mut values = std::collections::BTreeMap::new();
            values.insert(smol_str::SmolStr::new("ok"), Dynamic::Bool(false));
            values.insert(
                smol_str::SmolStr::new("error"),
                Dynamic::from(format!("{err:#}")),
            );
            Box::into_raw(Box::new(Dynamic::map(values))) as *const Dynamic
        }
    }
}

unsafe fn dynamic_string(value: *const Dynamic) -> Option<String> {
    if value.is_null() {
        return None;
    }
    Some(unsafe { (&*value).as_str().to_string() })
}

mod scan;
