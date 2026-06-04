pub fn bench(n: i64) {
    let size = 100i64;
    let a: [f64; 100] = [0.0f64; 100];
    for i in 0..size {
        a[i] = (i as f64) * 1.5f64;
    }
    let total = 0.0f64;
    for _ in 0..n {
        for i in 0..size {
            total = total + a[i];
        }
    }
    total as i64
}
