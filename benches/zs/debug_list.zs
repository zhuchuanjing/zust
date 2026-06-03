pub fn bench(n: i64) {
    let l = [];
    for i in 0..=n {
        l.push(i);
    }

    let sum = 0i64;
    for _ in 0..1000 {
        sum = sum + l.get_idx(n / 2);
    }
    sum
}
