use anyhow::{Context, Result};
use dynamic::Type;
use std::path::PathBuf;
use std::process::Command;
use std::time::Instant;

struct Bench {
    name: &'static str,
    desc: &'static str,
    size: i64,
    is_generic: bool,
}

const BENCHMARKS: &[Bench] = &[
    Bench { name: "fib_rec", desc: "fibonacci(35) recursive", size: 35, is_generic: false },
    Bench { name: "fib_iter", desc: "fibonacci iter 50M      ", size: 50_000_000, is_generic: false },
    Bench { name: "sieve", desc: "sieve 100K             ", size: 100_000, is_generic: true },
    Bench { name: "list_ops", desc: "list push/sum 2M       ", size: 2_000_000, is_generic: false },
    Bench { name: "bintree", desc: "bintree depth 20       ", size: 20, is_generic: false },
    Bench { name: "nbody", desc: "nested loops(2000)     ", size: 2_000, is_generic: false },
    Bench { name: "float_ops", desc: "float ops 20M          ", size: 20_000_000, is_generic: false },
    Bench { name: "strcat", desc: "strcat x50000          ", size: 50_000, is_generic: false },
    Bench { name: "collatz", desc: "collatz(100K)          ", size: 100_000, is_generic: false },
    Bench { name: "pow_mod", desc: "pow mod 5M             ", size: 5_000_000, is_generic: false },
    Bench { name: "gcd_bench", desc: "gcd(5M)                ", size: 5_000_000, is_generic: false },
    Bench { name: "prime_count", desc: "prime check(500K)      ", size: 500_000, is_generic: false },
    Bench { name: "sort", desc: "bubble sort 10K         ", size: 10_000, is_generic: true },
    Bench { name: "map_ops", desc: "map bracket get/set 200K", size: 200_000, is_generic: false },
    Bench { name: "mandelbrot", desc: "mandelbrot 1000        ", size: 1000, is_generic: false },
    Bench { name: "spectral_norm", desc: "spectral norm 550      ", size: 550, is_generic: false },
    Bench { name: "bit_count", desc: "bit popcount 50M       ", size: 50_000_000, is_generic: false },
    Bench { name: "seq_fact", desc: "sequential fact 100M   ", size: 100_000_000, is_generic: false },
    Bench { name: "string_build", desc: "string build x5000     ", size: 5_000, is_generic: false },
    Bench { name: "map_access", desc: "map bracket acc 200K   ", size: 200_000, is_generic: false },
    Bench { name: "struct_ops", desc: "struct field ops 20M   ", size: 20_000_000, is_generic: false },
    Bench { name: "closure_sum", desc: "closure sum 50M        ", size: 50_000_000, is_generic: false },
    Bench { name: "closure_16args", desc: "closure 16args 10M     ", size: 10_000_000, is_generic: false },
    Bench { name: "vec_add", desc: "vec add 100x500K       ", size: 500_000, is_generic: false },
    Bench { name: "ackermann", desc: "ackermann(3,6)         ", size: 6, is_generic: false },
    Bench { name: "quicksort", desc: "quicksort 2K           ", size: 2000, is_generic: true },
    Bench { name: "matrix_mul", desc: "matrix mul 40x40 x50   ", size: 50, is_generic: false },
    Bench { name: "binary_search", desc: "binary search 10K       ", size: 10_000, is_generic: true },
    Bench { name: "random_gen", desc: "random LCG 50M         ", size: 50_000_000, is_generic: false },
    Bench { name: "array_reverse", desc: "array reverse 1K x10K  ", size: 10_000, is_generic: false },
];

struct LangResult {
    exec_ms: f64,
    result: Option<i64>,
    error: Option<String>,
}

fn main() -> Result<()> {
    let benches_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let filters: Vec<String> = std::env::args().skip(1).collect();
    let selected: Vec<&Bench> = BENCHMARKS.iter().filter(|bench| filters.is_empty() || filters.iter().any(|filter| bench.name.contains(filter) || bench.desc.contains(filter))).collect();
    if selected.is_empty() {
        anyhow::bail!("no benchmark matched filters: {}", filters.join(", "));
    }

    print!("\nloading Zust VM...");
    let t0 = Instant::now();
    let vm = vm::Vm::with_all()?;
    println!(" {:.0}ms", t0.elapsed().as_secs_f64() * 1000.0);

    for b in &selected {
        let path = benches_dir.join("zs").join(format!("{}.zs", b.name));
        let t0 = Instant::now();
        vm.jit.write().unwrap().compiler.import_file(b.name, path.to_str().context("invalid path")?).with_context(|| format!("import {}.zs", b.name))?;
        println!("  compiled {}.zs ({:.0}ms)", b.name, t0.elapsed().as_secs_f64() * 1000.0);
    }

    let baseline_lua = measure_baseline("lua", &benches_dir.join("lua")).unwrap_or(25.0);
    let baseline_py = measure_baseline("python3", &benches_dir.join("py")).unwrap_or(35.0);
    println!("baseline: lua={:.0}ms  python={:.0}ms\n", baseline_lua, baseline_py);

    let mut all: Vec<(&Bench, LangResult, LangResult, LangResult)> = Vec::new();

    for b in selected {
        print!("{:28} ", b.desc.trim());

        let zs = run_zust(&vm, b);
        let lua = run_script("lua", &benches_dir.join("lua"), b.name, b.size, baseline_lua);
        let py = run_script("python3", &benches_dir.join("py"), b.name, b.size, baseline_py);

        print_row(&zs, &lua, &py);
        all.push((b, zs, lua, py));
    }

    print_summary(&all);
    Ok(())
}

fn run_zust(vm: &vm::Vm, b: &Bench) -> LangResult {
    if b.is_generic {
        let fn_name = format!("{}::bench", b.name);
        let (ptr, _ret) = match vm.jit.write().unwrap().get_fn_ptr_with_params(&fn_name, &[], &[Type::ConstInt(b.size)]) {
            Ok(r) => r,
            Err(e) => return LangResult { exec_ms: 0.0, result: None, error: Some(format!("compile: {e}")) },
        };
        let bench_fn: extern "C" fn() -> i64 = unsafe { std::mem::transmute(ptr) };

        bench_fn();

        let t0 = Instant::now();
        let result = bench_fn();
        let exec_ms = t0.elapsed().as_secs_f64() * 1000.0;

        LangResult { exec_ms, result: Some(result), error: None }
    } else {
        let fn_name = format!("{}::bench", b.name);
        let (ptr, _ret) = match vm.jit.write().unwrap().get_fn_ptr(&fn_name, &[Type::I64]) {
            Ok(r) => r,
            Err(e) => return LangResult { exec_ms: 0.0, result: None, error: Some(format!("compile: {e}")) },
        };
        let bench_fn: extern "C" fn(i64) -> i64 = unsafe { std::mem::transmute(ptr) };

        bench_fn(1);

        let t0 = Instant::now();
        let result = bench_fn(b.size);
        let exec_ms = t0.elapsed().as_secs_f64() * 1000.0;

        LangResult { exec_ms, result: Some(result), error: None }
    }
}

fn measure_baseline(cmd: &str, dir: &PathBuf) -> Result<f64> {
    let ext = if cmd.contains("python") { "py" } else { "lua" };
    let script = dir.join(format!("_baseline_empty.{ext}"));
    std::fs::write(&script, "print(0)\n").ok();

    let mut times = Vec::new();
    for _ in 0..10 {
        let t0 = Instant::now();
        if let Ok(out) = Command::new(cmd).arg(&script).output() {
            if out.status.success() {
                times.push(t0.elapsed().as_secs_f64() * 1000.0);
            }
        }
    }
    let _ = std::fs::remove_file(&script);
    if times.len() >= 3 {
        times.sort_by(|a, b| a.partial_cmp(b).unwrap());
        let mid = times.len() / 2;
        Ok(times[mid])
    } else {
        Ok(30.0)
    }
}

fn run_script(cmd: &str, dir: &PathBuf, name: &str, size: i64, baseline_ms: f64) -> LangResult {
    let ext = if cmd.contains("python") { "py" } else { "lua" };
    let path = dir.join(format!("{name}.{ext}"));
    let t0 = Instant::now();
    match Command::new(cmd).arg(&path).arg(size.to_string()).output() {
        Ok(out) => {
            let total_ms = t0.elapsed().as_secs_f64() * 1000.0;
            let exec_ms = (total_ms - baseline_ms).max(0.01);
            if out.status.success() {
                let stdout = String::from_utf8_lossy(&out.stdout).trim().to_string();
                match stdout.parse::<i64>() {
                    Ok(val) => LangResult { exec_ms, result: Some(val), error: None },
                    Err(_) => LangResult { exec_ms, result: None, error: Some(format!("parse: {}", truncate_chars(&stdout, 40))) },
                }
            } else {
                let stderr = String::from_utf8_lossy(&out.stderr).trim().to_string();
                LangResult { exec_ms: total_ms, result: None, error: Some(stderr.chars().take(80).collect()) }
            }
        }
        Err(e) => LangResult { exec_ms: 0.0, result: None, error: Some(format!("{e}")) },
    }
}

fn print_row(zs: &LangResult, lua: &LangResult, py: &LangResult) {
    let zs_str = if zs.error.is_none() { format!("{:>7}", format_ms(zs.exec_ms)) } else { "   ERR  ".into() };
    let lua_str = if lua.error.is_none() { format!("{:>7}", format_ms(lua.exec_ms)) } else { "   ERR  ".into() };
    let py_str = if py.error.is_none() { format!("{:>7}", format_ms(py.exec_ms)) } else { "   ERR  ".into() };

    print!("zust {}  lua {}  py {}", zs_str, lua_str, py_str);

    let mut notes = Vec::new();
    if let Some(ref e) = zs.error {
        notes.push(format!("zust: {}", truncate_chars(e, 30)));
    }
    if let Some(ref e) = lua.error {
        notes.push(format!("lua: {}", truncate_chars(e, 30)));
    }
    if let Some(ref e) = py.error {
        notes.push(format!("py: {}", truncate_chars(e, 30)));
    }

    if zs.error.is_none() && lua.error.is_none() && py.error.is_none() {
        let zr = zs.result.unwrap();
        let lr = lua.result.unwrap();
        let pr = py.result.unwrap();

        if zr != lr {
            notes.push(format!("zs/lua: {}!={}", zr, lr));
        }
        if zr != pr {
            notes.push(format!("zs/py: {}!={}", zr, pr));
        }

        if zs.exec_ms > 0.0 {
            print!("  lua/zs {:4.1}x  py/zs {:4.1}x", lua.exec_ms / zs.exec_ms, py.exec_ms / zs.exec_ms);
        }
    }

    if !notes.is_empty() {
        print!("  [{}]", notes.join(" | "));
    }
    println!();
}

fn truncate_chars(text: &str, max_chars: usize) -> String {
    text.chars().take(max_chars).collect()
}

fn print_summary(results: &[(&Bench, LangResult, LangResult, LangResult)]) {
    println!("\n╔══════════════════════════════════════════════════════════════════════════════════════╗");
    println!("║                       Zust vs Lua vs Python  Performance Summary                    ║");
    println!("╠══════════════════════════════════╦══════════════╦══════════════╦════════════════════╣");
    println!("║ benchmark                        ║ Zust         ║ Lua          ║ Python   lua  py  ║");
    println!("╠══════════════════════════════════╬══════════════╬══════════════╬════════════════════╣");

    let mut geomean_lua = 1.0f64;
    let mut geomean_py = 1.0f64;
    let mut count = 0;

    for (b, zs, lua, py) in results {
        let zs_ok = zs.error.is_none() && zs.exec_ms >= 0.1;
        let lua_ok = lua.error.is_none() && lua.exec_ms >= 0.1;
        let py_ok = py.error.is_none() && py.exec_ms >= 0.1;

        let zs_str = if zs.error.is_none() { format_ms(zs.exec_ms) } else { "ERR".into() };
        let lua_str = if lua.error.is_none() { format_ms(lua.exec_ms) } else { "ERR".into() };
        let py_str = if py.error.is_none() { format_ms(py.exec_ms) } else { "ERR".into() };

        let ratio_str = if zs_ok && lua_ok && py_ok {
            let rl = lua.exec_ms / zs.exec_ms;
            let rp = py.exec_ms / zs.exec_ms;
            geomean_lua *= rl;
            geomean_py *= rp;
            count += 1;
            format!("{:4.1}x {:4.1}x", rl, rp)
        } else {
            String::from("  ---   ")
        };

        println!("║ {:<32}  ║ {:>8}    ║ {:>8}    ║ {:>8}    {:>11} ║", b.desc.trim(), zs_str, lua_str, py_str, ratio_str);
    }

    println!("╠══════════════════════════════════╩══════════════╩══════════════╩════════════════════╣");
    if count > 0 {
        let gm_lua = geomean_lua.powf(1.0 / count as f64);
        let gm_py = geomean_py.powf(1.0 / count as f64);
        println!("║  geometric mean ({} benchmarks)    Zust = 1.0x,   Lua = {:.1}x,   Python = {:.1}x                    ║", count, gm_lua, gm_py);
    }
    println!("╚══════════════════════════════════════════════════════════════════════════════════════╝");
}

fn format_ms(ms: f64) -> String {
    if ms < 0.01 {
        format!("{:.1}us", ms * 1000.0)
    } else if ms < 1.0 {
        format!("{:.0}us", ms * 1000.0)
    } else if ms < 1000.0 {
        format!("{:.0}ms", ms)
    } else {
        format!("{:.1}s", ms / 1000.0)
    }
}
