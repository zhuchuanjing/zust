pub fn bench(n: i64) {
    let seed = 12345i64;
    let total = 0i64;
    for _ in 0..n {
        seed = seed * 1103515245i64 + 12345i64;
        seed = seed & 0x7fffffff;
        total = total + (seed & 0xff);
    }
    total
}
