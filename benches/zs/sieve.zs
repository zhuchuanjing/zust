pub fn bench<N>() {
    let is_prime = [true; N];
    is_prime[0] = false;
    is_prime[1] = false;
    let count = 0i64;
    for p in 2i64..N {
        if is_prime[p] == true {
            count = count + 1;
            let step = p;
            let j = p * p;
            while j < N {
                is_prime[j] = false;
                j = j + step;
            }
        }
    }
    count
}
