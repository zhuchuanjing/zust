fn ack(m: i64, n: i64) {
    if m == 0 {
        return n + 1;
    }
    if n == 0 {
        return ack(m - 1, 1);
    }
    ack(m - 1, ack(m, n - 1))
}

pub fn bench(n: i64) {
    ack(3, n)
}
