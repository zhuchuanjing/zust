pub fn bench(n: i64) {
    let acc = 0i64;
    let sum16 = |a, b, c, d, e, f, g, h, i, j, k, l, m, n_arg, o, p| {
        a + b + c + d + e + f + g + h + i + j + k + l + m + n_arg + o + p
    };
    for idx in 0..n {
        acc = acc + sum16(1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16);
    }
    acc
}
