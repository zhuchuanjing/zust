// 1000 线程并发跑 spectral_norm(550)
//
// 用法: cargo run --release -p zust-bench --bin spectral_concurrent -- [threads] [warmup_iters]
//
// 默认: 1000 线程,每线程 1 次。

use anyhow::{Context, Result};
use dynamic::Type;
use std::path::PathBuf;
use std::sync::atomic::{AtomicI64, AtomicUsize, Ordering};
use std::sync::{Arc, Barrier};
use std::time::Instant;

const SIZE: i64 = 550;

fn main() -> Result<()> {
    let threads: usize = std::env::args().nth(1).and_then(|s| s.parse().ok()).unwrap_or(1000);
    let iters: usize = std::env::args().nth(2).and_then(|s| s.parse().ok()).unwrap_or(1);

    let benches_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    print!("loading Zust VM...");
    let t0 = Instant::now();
    let vm = vm::Vm::with_all()?;
    println!(" {:.0}ms", t0.elapsed().as_secs_f64() * 1000.0);

    let path = benches_dir.join("zs").join("spectral_norm.zs");
    let t0 = Instant::now();
    vm.jit
        .write()
        .compiler
        .import_file("spectral_norm", path.to_str().context("invalid path")?)
        .context("import spectral_norm.zs")?;
    println!("compiled spectral_norm.zs ({:.0}ms)", t0.elapsed().as_secs_f64() * 1000.0);

    let (ptr, _ret) = vm
        .jit
        .write()
        .get_fn_ptr("spectral_norm::bench", &[Type::I64])
        .context("get_fn_ptr spectral_norm")?;
    let fn_ptr_usize = ptr as usize;

    // warmup: single-threaded baseline
    let bench_fn: extern "C" fn(i64) -> i64 = unsafe { std::mem::transmute(ptr) };
    bench_fn(SIZE);
    let mut single_runs = Vec::new();
    for _ in 0..3 {
        let t0 = Instant::now();
        let r = bench_fn(SIZE);
        single_runs.push((t0.elapsed().as_secs_f64() * 1000.0, r));
    }
    let min_single_ms = single_runs.iter().map(|(t, _)| *t).fold(f64::INFINITY, f64::min);
    let single_result = single_runs[0].1;
    println!(
        "single-threaded warmup: min={:.2}ms result={} (3 runs: {})",
        min_single_ms,
        single_result,
        single_runs.iter().map(|(t, _)| format!("{t:.1}ms")).collect::<Vec<_>>().join(", ")
    );

    println!("\n>>> concurrent: {threads} threads × {iters} iter(s) = {} total calls", threads * iters);

    let cpus = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(0);
    println!("available_parallelism = {cpus}");

    // barrier 让所有线程同时起跑(测整体 wall-clock + 测内核调度承压)
    let barrier = Arc::new(Barrier::new(threads));
    let mismatch = Arc::new(AtomicUsize::new(0));
    let total_calls = Arc::new(AtomicUsize::new(0));
    let max_thread_ms = Arc::new(AtomicI64::new(0));

    let t_total = Instant::now();
    let mut handles = Vec::with_capacity(threads);
    for tid in 0..threads {
        let barrier = Arc::clone(&barrier);
        let mismatch = Arc::clone(&mismatch);
        let total_calls = Arc::clone(&total_calls);
        let max_thread_ms = Arc::clone(&max_thread_ms);
        let _ = tid;
        handles.push(std::thread::spawn(move || {
            barrier.wait();
            let f: extern "C" fn(i64) -> i64 = unsafe { std::mem::transmute(fn_ptr_usize as *const ()) };
            let t0 = Instant::now();
            for _ in 0..iters {
                let r = f(SIZE);
                total_calls.fetch_add(1, Ordering::Relaxed);
                if r != single_result {
                    mismatch.fetch_add(1, Ordering::Relaxed);
                }
            }
            let elapsed_us = t0.elapsed().as_micros() as i64;
            max_thread_ms.fetch_max(elapsed_us, Ordering::Relaxed);
        }));
    }
    for h in handles {
        h.join().unwrap();
    }
    let total_ms = t_total.elapsed().as_secs_f64() * 1000.0;
    let max_us = max_thread_ms.load(Ordering::Relaxed);
    let mism = mismatch.load(Ordering::Relaxed);
    let calls = total_calls.load(Ordering::Relaxed);

    println!("\n=== Results ===");
    println!("total wall-clock:        {total_ms:>10.2}ms");
    println!("slowest single thread:   {:>10.2}ms", max_us as f64 / 1000.0);
    println!("total calls:             {calls}");
    println!("mismatches:              {mism}");
    println!("aggregate throughput:    {:.0} calls/sec", calls as f64 / (total_ms / 1000.0));
    let speedup = (min_single_ms * threads as f64 * iters as f64) / total_ms;
    println!("speedup vs serial:       {speedup:.2}x  (vs {} CPUs)", cpus);
    println!("efficiency:              {:.1}%", speedup / cpus.max(1) as f64 * 100.0);
    Ok(())
}
