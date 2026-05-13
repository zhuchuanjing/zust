use anyhow::Result;
use dynamic::Type;

fn main() -> Result<()> {
    vm::import_code(
        "demo",
        br#"
        pub fn add(a: i64, b: i64) {
            a + b
        }
        "#
        .to_vec(),
    )?;

    let (fn_ptr, ret_ty) = vm::get_fn("demo::add", &[Type::I64, Type::I64])?;
    assert_eq!(ret_ty, Type::I64);

    let add: extern "C" fn(i64, i64) -> i64 = unsafe { std::mem::transmute(fn_ptr) };
    println!("40 + 2 = {}", add(40, 2));

    Ok(())
}
