pub fn bench<N>() {
    let arr: [i64; N] = [0i64; N];
    for i in 0i64..N {
        let seed = i * 6364136223846793005i64 + 1i64;
        arr[i] = seed;
    }

    let lows: [i64; N] = [0i64; N];
    let highs: [i64; N] = [0i64; N];
    let top = 0i64;
    lows[top] = 0i64;
    highs[top] = N - 1;

    while top >= 0i64 {
        let low = lows[top];
        let high = highs[top];
        top -= 1i64;

        while low < high {
            let pivot = arr[high];
            let i = low - 1i64;
            for j in low..high {
                if arr[j] <= pivot {
                    i += 1i64;
                    let tmp = arr[i];
                    arr[i] = arr[j];
                    arr[j] = tmp;
                }
            }

            let pi = i + 1i64;
            let tmp = arr[pi];
            arr[pi] = arr[high];
            arr[high] = tmp;

            if pi - low < high - pi {
                if pi + 1i64 < high {
                    top += 1i64;
                    lows[top] = pi + 1i64;
                    highs[top] = high;
                }
                high = pi - 1i64;
            } else {
                if low < pi - 1i64 {
                    top += 1i64;
                    lows[top] = low;
                    highs[top] = pi - 1i64;
                }
                low = pi + 1i64;
            }
        }
    }

    let sum = 0i64;
    for idx in 0i64..N {
        sum = sum + arr[idx];
    }
    sum
}
