fn fib(n: i64) {
    if n <= 1 {
        return n;
    }
    fib(n - 1) + fib(n - 2)
}

pub fn bench(n: i64) {
    fib(n)
}
