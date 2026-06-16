use anyhow::Result;
use dynamic::Type;

fn main() -> Result<()> {
    let vm = vm::Vm::with_all()?;

    vm.jit.write().import_code(
        "demo",
        br#"
        pub fn add(a: i64, b: i64) {
            a + b
        }
        "#
        .to_vec(),
    )?;

    let (ptr, ret) = vm.jit.write().get_fn_ptr("demo::add", &[Type::I64, Type::I64])?;
    assert_eq!(ret, Type::I64);

    let add: extern "C" fn(i64, i64) -> i64 = unsafe { std::mem::transmute(ptr) };
    println!("40 + 2 = {}", add(40, 2));

    Ok(())
}
