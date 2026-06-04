fn popcount(x: i64) {
    let n = x;
    n = n - ((n >> 1) & 0x5555555555555555i64);
    n = (n & 0x3333333333333333i64) + ((n >> 2) & 0x3333333333333333i64);
    n = (n + (n >> 4)) & 0x0F0F0F0F0F0F0F0Fi64;
    n = n + (n >> 8);
    n = n + (n >> 16);
    n = n + (n >> 32);
    n & 0x7Fi64
}

pub fn bench(n: i64) {
    let total = 0i64;
    for i in 0..n {
        total = total + popcount(i);
    }
    total
}
