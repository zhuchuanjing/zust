pub fn bench(n: i64) {
    let m = {};
    for i in 0..n {
        let key = "key_" + "" + i;
        m[key] = i;
    }
    let sum = 0i64;
    for i in 0..n {
        let key = "key_" + "" + i;
        sum = sum + m[key];
    }
    sum
}
