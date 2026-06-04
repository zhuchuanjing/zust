struct Vec3 {
    x: f64,
    y: f64,
    z: f64,
}

pub fn bench(n: i64) {
    let v = Vec3{x: 1.0f64, y: 2.0f64, z: 3.0f64};
    let total = 0.0f64;
    for _ in 0..n {
        let sum = v.x + v.y + v.z;
        v.x = v.x + 0.001f64;
        v.y = v.y + 0.002f64;
        v.z = v.z + 0.003f64;
        total = total + sum;
    }
    total as i64
}
