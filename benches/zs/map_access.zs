pub fn bench(n: i64) {
    let m = {};
    for i in 0..n {
        let key = "" + i;
        m[key] = i;
    }
    let sum = 0i64;
    for i in 0..n {
        let key = "" + i;
        sum = sum + m[key];
    }
    sum
}
