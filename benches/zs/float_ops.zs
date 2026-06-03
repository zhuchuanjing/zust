pub fn bench(n: i64) {
    let x = 1.0;
    let y = 2.0;
    for i in 0..n {
        x = x * 1.000001 + y * 0.999999;
        y = y * 1.000001 - x * 0.999999;
    }
    (x + y) as i64
}
