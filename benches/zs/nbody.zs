pub fn bench(n: i64) {
    let bodies = 100i64;
    let total = 0i64;
    for step in 0..n {
        for i in 0..bodies {
            for j in 0..bodies {
                if i != j {
                    total = total + i * j;
                }
            }
        }
    }
    total
}
