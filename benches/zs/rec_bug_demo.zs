fn factorial(n: i64) {
    if n <= 1 {
        return 1;
    }
    n * factorial(n - 1)
}

pub fn bench(n: i64) {
    factorial(n)
}
