use anyhow::Result;
use dynamic::Type;
use std::path::PathBuf;
use std::time::Instant;

fn main() -> Result<()> {
    let vm = vm::Vm::with_all()?;
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));

    vm.import("sieve", dir.join("zs/sieve.zs").to_str().unwrap())?;

    let compiled = vm.get_fn("sieve::bench", &[Type::I64])?;
    println!("return type: {:?}", compiled.ret_ty());
    let f: extern "C" fn(i64) -> i64 = unsafe { std::mem::transmute(compiled.ptr()) };

    for n in [10i64, 100, 1000, 10000, 100000] {
        let correct = count_primes(n);
        let t0 = Instant::now();
        let result = f(n);
        let ms = t0.elapsed().as_secs_f64() * 1000.0;
        let status = if result == correct { "OK" } else { "WRONG" };
        println!("n={:>6}  zust={:>6}  correct={:>6}  {status}  {:.0}ms", n, result, correct, ms);
    }

    Ok(())
}

fn count_primes(n: i64) -> i64 {
    let mut is_prime = vec![true; (n + 1) as usize];
    if n >= 0 { is_prime[0] = false; }
    if n >= 1 { is_prime[1] = false; }
    let mut count = 0;
    for p in 2..=n as usize {
        if is_prime[p] {
            count += 1;
            let step = p;
            let mut j = p * p;
            while j <= n as usize {
                is_prime[j] = false;
                j += step;
            }
        }
    }
    count
}
