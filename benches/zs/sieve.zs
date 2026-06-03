pub fn bench(n: i64) {
    let limit = n;
    let is_prime = [true; limit + 1];
    if limit >= 0 {
        is_prime[0] = false;
    }
    if limit >= 1 {
        is_prime[1] = false;
    }
    let count = 0i64;
    for p in 2i64..=limit {
        if is_prime[p] == true {
            count = count + 1;
            let step = p;
            let j = p * p;
            while j <= limit {
                is_prime[j] = false;
                j = j + step;
            }
        }
    }
    count
}
