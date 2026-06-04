pub fn bench(n: i64) {
    let x = 1.0f64;
    let y = 2.0f64;
    for i in 0..n {
        x = x * 1.000001f64 + y * 0.999999f64;
        y = y * 1.000001f64 - x * 0.999999f64;
    }
    (x + y) as i64
}
