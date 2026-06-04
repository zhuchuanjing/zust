pub fn bench(n: i64) {
    let result = 1i64;
    let m = 1000000007i64;
    for i in 1..=n {
        result = (result * i) % m;
    }
    result
}
