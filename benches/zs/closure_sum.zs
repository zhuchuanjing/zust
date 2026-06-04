pub fn bench(n: i64) {
    let acc = 0i64;
    let add = |a: i64, b: i64| {
        a + b
    };
    for i in 0..n {
        acc = add(acc, i);
    }
    acc
}
