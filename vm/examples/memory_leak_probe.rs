use anyhow::{Context, Result};
use dynamic::Type;
use std::process::Command;

fn rss_kb() -> Result<u64> {
    let pid = std::process::id().to_string();
    let output = Command::new("ps").args(["-o", "rss=", "-p", &pid]).output().context("run ps")?;
    let text = String::from_utf8(output.stdout).context("parse ps output as utf8")?;
    text.trim().parse::<u64>().context("parse rss kb")
}

fn print_rss(label: &str, base: u64) -> Result<u64> {
    let rss = rss_kb()?;
    println!("{label:<24} rss={rss:>10} KB delta={:>10} KB", rss.saturating_sub(base));
    Ok(rss)
}

fn main() -> Result<()> {
    if std::env::args().any(|arg| arg == "--drop-vm") {
        return drop_vm_probe();
    }
    if std::env::args().any(|arg| arg == "--drop-vm-thread") {
        return drop_vm_thread_probe();
    }

    let vm = vm::Vm::with_all()?;
    vm.jit.write().unwrap().import_code(
        "memory_probe",
        br#"
        pub fn setup() {
            root::add("local/memory_probe/payload", {
                value: 1,
                label: "abcdefghijklmnopqrstuvwxyz0123456789abcdefghijklmnopqrstuvwxyz0123456789",
                nums: [1, 2, 3, 4, 5, 6, 7, 8]
            });
            true
        }

        pub fn scalar_loop(n: i64) {
            let i = 0;
            let total = 0;
            while i < n {
                total = total + i;
                i = i + 1;
            }
            total
        }

        pub fn root_get_loop(n: i64) {
            let i = 0;
            let total = 0;
            while i < n {
                let item = root::get("local/memory_probe/payload");
                total = total + Any::to_i64(item.value);
                i = i + 1;
            }
            total
        }

        pub fn index_loop(n: i64) {
            let data = [
                {value: 1, label: "abcdefghijklmnopqrstuvwxyz0123456789"},
                {value: 2, label: "ABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789"}
            ];
            let i = 0;
            let total = 0;
            while i < n {
                let item = data[0];
                total = total + Any::to_i64(item.value);
                i = i + 1;
            }
            total
        }
        "#
        .to_vec(),
    )?;

    let (ptr, _) = vm.jit.write().unwrap().get_fn_ptr("memory_probe::setup", &[])?;
    let setup: extern "C" fn() -> bool = unsafe { std::mem::transmute(ptr) };
    anyhow::ensure!(setup(), "setup failed");

    let (ptr, _) = vm.jit.write().unwrap().get_fn_ptr("memory_probe::scalar_loop", &[Type::I64])?;
    let scalar: extern "C" fn(i64) -> i64 = unsafe { std::mem::transmute(ptr) };
    let (ptr, _) = vm.jit.write().unwrap().get_fn_ptr("memory_probe::root_get_loop", &[Type::I64])?;
    let root_get: extern "C" fn(i64) -> i64 = unsafe { std::mem::transmute(ptr) };
    let (ptr, _) = vm.jit.write().unwrap().get_fn_ptr("memory_probe::index_loop", &[Type::I64])?;
    let index: extern "C" fn(i64) -> i64 = unsafe { std::mem::transmute(ptr) };

    let iterations = std::env::args().nth(1).and_then(|arg| arg.parse::<i64>().ok()).unwrap_or(100_000);
    let rounds = std::env::args().nth(2).and_then(|arg| arg.parse::<usize>().ok()).unwrap_or(5);

    println!("iterations_per_round={iterations} rounds={rounds}");
    let base = print_rss("start", 0)?;

    for round in 1..=rounds {
        let before = rss_kb()?;
        let result = scalar(iterations);
        let after = print_rss(&format!("scalar round {round}"), base)?;
        println!("  result={result} round_delta={} KB", after.saturating_sub(before));
    }

    for round in 1..=rounds {
        let before = rss_kb()?;
        let result = root_get(iterations);
        let after = print_rss(&format!("root_get round {round}"), base)?;
        println!("  result={result} round_delta={} KB", after.saturating_sub(before));
    }

    for round in 1..=rounds {
        let before = rss_kb()?;
        let result = index(iterations);
        let after = print_rss(&format!("index round {round}"), base)?;
        println!("  result={result} round_delta={} KB", after.saturating_sub(before));
    }

    Ok(())
}

fn drop_vm_probe() -> Result<()> {
    let iterations = std::env::args().find_map(|arg| arg.strip_prefix("--iterations=").and_then(|value| value.parse::<i64>().ok())).unwrap_or(50_000);
    let rounds = std::env::args().find_map(|arg| arg.strip_prefix("--rounds=").and_then(|value| value.parse::<usize>().ok())).unwrap_or(4);

    println!("drop_vm_probe iterations_per_round={iterations} rounds={rounds}");
    let base = print_rss("start", 0)?;
    {
        let vm = vm::Vm::with_all()?;
        vm.jit.write().unwrap().import_code(
            "drop_probe",
            br#"
            pub fn setup() {
                root::add("local/drop_probe/payload", {
                    value: 1,
                    label: "abcdefghijklmnopqrstuvwxyz0123456789abcdefghijklmnopqrstuvwxyz0123456789",
                    nums: [1, 2, 3, 4, 5, 6, 7, 8]
                });
                true
            }

            pub fn root_get_loop(n: i64) {
                let i = 0;
                let total = 0;
                while i < n {
                    let item = root::get("local/drop_probe/payload");
                    total = total + Any::to_i64(item.value);
                    i = i + 1;
                }
                total
            }
            "#
            .to_vec(),
        )?;

        let (ptr, _) = vm.jit.write().unwrap().get_fn_ptr("drop_probe::setup", &[])?;
        let setup: extern "C" fn() -> bool = unsafe { std::mem::transmute(ptr) };
        anyhow::ensure!(setup(), "setup failed");

        let (ptr, _) = vm.jit.write().unwrap().get_fn_ptr("drop_probe::root_get_loop", &[Type::I64])?;
        let root_get: extern "C" fn(i64) -> i64 = unsafe { std::mem::transmute(ptr) };
        print_rss("after compile", base)?;
        for round in 1..=rounds {
            let before = rss_kb()?;
            let result = root_get(iterations);
            let after = print_rss(&format!("run round {round}"), base)?;
            println!("  result={result} round_delta={} KB", after.saturating_sub(before));
        }
        print_rss("before drop", base)?;
    }

    print_rss("after drop vm", base)?;
    std::thread::sleep(std::time::Duration::from_millis(100));
    print_rss("after sleep", base)?;
    Ok(())
}

fn drop_vm_thread_probe() -> Result<()> {
    let iterations = std::env::args().find_map(|arg| arg.strip_prefix("--iterations=").and_then(|value| value.parse::<i64>().ok())).unwrap_or(50_000);
    let rounds = std::env::args().find_map(|arg| arg.strip_prefix("--rounds=").and_then(|value| value.parse::<usize>().ok())).unwrap_or(4);
    let cycles = std::env::args().find_map(|arg| arg.strip_prefix("--cycles=").and_then(|value| value.parse::<usize>().ok())).unwrap_or(2);

    println!("drop_vm_thread_probe iterations_per_round={iterations} rounds={rounds} cycles={cycles}");
    let base = print_rss("start", 0)?;
    for cycle in 1..=cycles {
        let before = rss_kb()?;
        let handle = std::thread::spawn(move || -> Result<()> {
            let vm = vm::Vm::with_all()?;
            vm.jit.write().unwrap().import_code(
                "thread_drop_probe",
                br#"
                pub fn setup() {
                    root::add("local/thread_drop_probe/payload", {
                        value: 1,
                        label: "abcdefghijklmnopqrstuvwxyz0123456789abcdefghijklmnopqrstuvwxyz0123456789",
                        nums: [1, 2, 3, 4, 5, 6, 7, 8]
                    });
                    true
                }

                pub fn root_get_loop(n: i64) {
                    let i = 0;
                    let total = 0;
                    while i < n {
                        let item = root::get("local/thread_drop_probe/payload");
                        total = total + Any::to_i64(item.value);
                        i = i + 1;
                    }
                    total
                }
                "#
                .to_vec(),
            )?;
            let (ptr, _) = vm.jit.write().unwrap().get_fn_ptr("thread_drop_probe::setup", &[])?;
            let setup: extern "C" fn() -> bool = unsafe { std::mem::transmute(ptr) };
            anyhow::ensure!(setup(), "setup failed");
            let (ptr, _) = vm.jit.write().unwrap().get_fn_ptr("thread_drop_probe::root_get_loop", &[Type::I64])?;
            let root_get: extern "C" fn(i64) -> i64 = unsafe { std::mem::transmute(ptr) };
            for _ in 0..rounds {
                let _ = root_get(iterations);
            }
            print_rss(&format!("inside thread {cycle}"), base)?;
            Ok(())
        });
        handle.join().expect("probe thread panicked")?;
        let after = print_rss(&format!("after join {cycle}"), base)?;
        println!("  cycle_delta={} KB", after.saturating_sub(before));
    }
    std::thread::sleep(std::time::Duration::from_millis(100));
    print_rss("after final sleep", base)?;
    Ok(())
}
