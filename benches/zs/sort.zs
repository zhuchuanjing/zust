pub fn bench<N>() {
    let items: [i64; N] = [0i64; N];
    for i in 0i64..N {
        let seed = i * 6364136223846793005i64 + 1i64;
        items[i] = seed;
    }
    for i in 0i64..N {
        let limit = N - i - 1;
        for j in 0i64..limit {
            if items[j] > items[j + 1] {
                let a = items[j];
                let b = items[j + 1];
                items[j] = b;
                items[j + 1] = a;
            }
        }
    }
    let sum = 0i64;
    for i in 0i64..N {
        sum = sum + items[i];
    }
    sum
}
