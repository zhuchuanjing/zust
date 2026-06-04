pub fn bench(n: i64) {
    let sz = 40i64;
    let a: [f64; 1600] = [0.0f64; 1600];
    let b: [f64; 1600] = [0.0f64; 1600];
    let c: [f64; 1600] = [0.0f64; 1600];
    for i in 0..sz {
        for j in 0..sz {
            a[i * sz + j] = ((i * j) as f64) * 0.01f64;
            b[i * sz + j] = ((i + j) as f64) * 0.005f64;
        }
    }
    for _ in 0..n {
        for i in 0..sz {
            for k in 0..sz {
                let aik = a[i * sz + k];
                for j in 0..sz {
                    c[i * sz + j] = c[i * sz + j] + aik * b[k * sz + j];
                }
            }
        }
    }
    c[0] as i64
}
