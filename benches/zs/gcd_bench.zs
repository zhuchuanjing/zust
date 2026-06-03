fn gcd(a: i64, b: i64) {
    let x = a;
    let y = b;
    while y != 0 {
        let t = y;
        y = x % y;
        x = t;
    }
    x
}

pub fn bench(n: i64) {
    let total = 0i64;
    for i in 0..n {
        total = total + gcd(i, n - i);
    }
    total
}
