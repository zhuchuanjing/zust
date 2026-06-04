fn eval_a(i: i64, j: i64) {
    1.0f64 / ((i + j) * (i + j + 1) / 2 + i + 1) as f64
}

fn multiply_av(n: i64, v: [f64; 550]) {
    let result: [f64; 550] = [0.0f64; 550];
    for i in 0..n {
        let sum = 0.0f64;
        for j in 0..n {
            sum = sum + eval_a(i, j) * v[j];
        }
        result[i] = sum;
    }
    result
}

fn multiply_atv(n: i64, v: [f64; 550]) {
    let result: [f64; 550] = [0.0f64; 550];
    for i in 0..n {
        let sum = 0.0f64;
        for j in 0..n {
            sum = sum + eval_a(j, i) * v[j];
        }
        result[i] = sum;
    }
    result
}

pub fn bench(n: i64) {
    let u: [f64; 550] = [1.0f64; 550];
    let v: [f64; 550] = [0.0f64; 550];
    for _ in 0..10 {
        v = multiply_av(n, u);
        u = multiply_atv(n, v);
    }
    let vbv = 0.0f64;
    let vv = 0.0f64;
    for i in 0..n {
        vbv = vbv + u[i] * v[i];
        vv = vv + v[i] * v[i];
    }
    let result = sqrt(vbv / vv);
    (result * 1000000.0f64) as i64
}
