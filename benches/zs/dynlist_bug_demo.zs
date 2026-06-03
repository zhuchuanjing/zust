pub fn bench(n: i64) {
    let l = [];
    for i in 0..n {
        l.push(i);
    }
    let sum = 0i64;
    for i in 0..n {
        sum = sum + l.get_idx(i);
    }
    sum
}
