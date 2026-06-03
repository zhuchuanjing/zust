pub fn bench(n: i64) {
    let total = 0i64;
    for start in 1..=n {
        let x = start;
        while x != 1 {
            if x % 2 == 0 {
                x = x / 2;
            } else {
                x = 3 * x + 1;
            }
            total = total + 1;
        }
    }
    total
}
