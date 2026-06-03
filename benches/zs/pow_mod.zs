pub fn bench(n: i64) {
    let result = 1i64;
    for i in 0..n {
        let base = (i % 100i64) + 2i64;
        let exp = (i % 31i64) + 1i64;
        let m = 1000000007i64;
        let r = 1i64;
        let b = base;
        let e = exp;
        while e > 0 {
            if e % 2i64 == 1i64 {
                r = (r * b) % m;
            }
            b = (b * b) % m;
            e = e / 2i64;
        }
        result = (result + r) % m;
    }
    result
}
