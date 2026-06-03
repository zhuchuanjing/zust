pub fn bench(n: i64) {
    let a = 0i64;
    let b = 1i64;
    for _ in 0..n {
        let tmp = a;
        a = b;
        b = (tmp + b) % 1000000007i64;
    }
    a
}
