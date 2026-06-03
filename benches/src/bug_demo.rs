use anyhow::Result;
use dynamic::Type;
use std::time::Instant;

fn main() -> Result<()> {
    let vm = vm::Vm::with_all()?;
    let dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));

    println!("=== Bug 1: 递归函数类型推断错误 ===\n");

    vm.import("rec_demo", dir.join("zs/rec_bug_demo.zs").to_str().unwrap())?;

    let compiled = vm.get_fn("rec_demo::bench", &[Type::I64])?;
    println!("编译器推断的返回类型: {:?}", compiled.ret_ty());
    println!("期望返回类型: I64\n");

    let f: extern "C" fn(i64) -> i64 = unsafe { std::mem::transmute(compiled.ptr()) };

    println!("factorial(5) 正确值 = 120");
    let result = f(5);
    println!("Zust JIT 返回值  = {}  ← 错误! (垃圾指针值)\n", result);

    println!("原因: 编译器中递归调用 factorial(n-1) 时，类型推断占位符未解析，");
    println!("函数返回类型被推断为 Any/Dynamic 而非 i64。");
    println!("JIT 返回的是堆上 Dynamic 对象的指针，被错误解释为 i64。\n");

    println!("=== Bug 2: 动态列表 JIT 性能/正确性 ===\n");

    vm.import("dynlist_demo", dir.join("zs/dynlist_bug_demo.zs").to_str().unwrap())?;

    let compiled = vm.get_fn("dynlist_demo::bench", &[Type::I64])?;
    let f: extern "C" fn(i64) -> i64 = unsafe { std::mem::transmute(compiled.ptr()) };

    let n = 100000i64;

    let correct = n * (n - 1) / 2;
    println!("list sum(0..{n}) 正确值 = {correct}");

    let t0 = Instant::now();
    let result = f(n);
    let elapsed = t0.elapsed();

    println!("Zust JIT 返回值  = {result}  ← 错误!");
    println!("耗时            = {:.0}ms (正确值 {correct})", elapsed.as_secs_f64() * 1000.0);

    println!("\n原因: 动态列表 [] 在 JIT 中每次 get_idx 都走运行时分发，");
    println!("且类型擦除导致 sum + Any 产生类型混乱，累积错误。\n");

    // Verify with a Lua equivalent for timing comparison
    use std::process::Command;
    let lua_script = dir.join("lua").join("_dynlist_test.lua");
    std::fs::write(&lua_script, format!("local l={{}} for i=0,{n}-1 do l[i]=i end local s=0 for i=0,{n}-1 do s=s+l[i] end print(s)"))?;

    let t0 = Instant::now();
    let out = Command::new("lua").arg(&lua_script).output()?;
    let lua_elapsed = t0.elapsed();
    let lua_result: i64 = String::from_utf8_lossy(&out.stdout).trim().parse().unwrap();
    println!("对比 Lua: 结果={lua_result} (正确), 耗时={:.0}ms", lua_elapsed.as_secs_f64() * 1000.0);
    let _ = std::fs::remove_file(&lua_script);

    Ok(())
}
