pub fn bench<N>() {
    let arr: [i64; N] = [0i64; N];
    for i in 0i64..N {
        arr[i] = i * 2;
    }
    let sum = 0i64;
    for target in 0i64..N {
        let low = 0i64;
        let high = N - 1;
        let found = -1i64;
        while low <= high {
            let mid = (low + high) / 2;
            if arr[mid] == target * 2 {
                found = mid;
                low = high + 1;
            } else {
                if arr[mid] < target * 2 {
                    low = mid + 1;
                } else {
                    high = mid - 1;
                }
            }
        }
        sum = sum + found;
    }
    sum
}
