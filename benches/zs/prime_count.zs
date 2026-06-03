fn is_prime(n: i64) {
    if n < 2 { return false; }
    let i = 2i64;
    while i * i <= n {
        if n % i == 0 { return false; }
        i = i + 1;
    }
    true
}

pub fn bench(n: i64) {
    let count = 0i64;
    for x in 2..=n {
        if is_prime(x) == true {
            count = count + 1;
        }
    }
    count
}
