use anyhow::Result;
use dynamic::Type;
use std::time::Instant;

fn main() -> Result<()> {
    let vm = vm::Vm::with_all()?;
    let dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"));

    println!("=== Bug 1: 递归函数类型推断错误 (已修复) ===\n");

    vm.jit.write().compiler.import_file("rec_demo", dir.join("zs/rec_bug_demo.zs").to_str().unwrap())?;

    let (ptr, ret) = vm.jit.write().get_fn_ptr("rec_demo::bench", &[Type::I64])?;
    println!("编译器推断的返回类型: {:?}", ret);
    println!("期望返回类型: I64\n");

    let f: extern "C" fn(i64) -> i64 = unsafe { std::mem::transmute(ptr) };

    println!("factorial(5) 正确值 = 120");
    let result = f(5);
    let status1 = if result == 120 { "✓ 已修复" } else { "✗ 仍然错误" };
    println!("Zust JIT 返回值  = {result}  {status1}\n");

    println!("修复: 预扫描 base case 返回类型作为种子，避免递归调用时种子为空返回 Any。\n");

    println!("=== Bug 2: 动态列表 JIT 正确性 (已修复) ===\n");

    vm.jit.write().compiler.import_file("dynlist_demo", dir.join("zs/dynlist_bug_demo.zs").to_str().unwrap())?;

    let (ptr, _ret) = vm.jit.write().get_fn_ptr("dynlist_demo::bench", &[Type::I64])?;
    let f: extern "C" fn(i64) -> i64 = unsafe { std::mem::transmute(ptr) };

    let n = 100000i64;

    let correct = n * (n - 1) / 2;
    println!("list sum(0..{n}) 正确值 = {correct}");

    let t0 = Instant::now();
    let result = f(n);
    let elapsed = t0.elapsed();

    let status2 = if result == correct { "✓ 已修复" } else { "✗ 仍然错误" };
    println!("Zust JIT 返回值  = {result}  {status2}");
    println!("耗时            = {:.0}ms (正确值 {correct})", elapsed.as_secs_f64() * 1000.0);

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
