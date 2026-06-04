pub fn bench(n: i64) {
    let hw = n / 2;
    let total = 0i64;
    let nf = n as f64;
    let hwf = hw as f64;
    for y in 0..n {
        let yf = y as f64;
        let ci = (yf - hwf) / (0.5f64 * nf);
        for x in 0..n {
            let xf = x as f64;
            let cr = 1.5f64 * (xf - hwf) / (0.5f64 * nf);
            let zr = 0.0f64;
            let zi = 0.0f64;
            let k = 0i64;
            while k < 50 && zr * zr + zi * zi < 4.0f64 {
                let zr2 = zr * zr;
                let zi2 = zi * zi;
                zi = 2.0f64 * zr * zi + ci;
                zr = zr2 - zi2 + cr;
                k = k + 1;
            }
            total = total + k;
        }
    }
    total
}
