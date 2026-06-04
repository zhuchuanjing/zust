fn partition<N>(arr: [i64; N], low: i64, high: i64) {
    let pivot = arr[high];
    let i = low - 1;
    for j in low..high {
        if arr[j] <= pivot {
            i = i + 1;
            let tmp = arr[i];
            arr[i] = arr[j];
            arr[j] = tmp;
        }
    }
    let tmp = arr[i + 1];
    arr[i + 1] = arr[high];
    arr[high] = tmp;
    i + 1
}

fn sort_range<N>(arr: [i64; N], low: i64, high: i64) {
    if low < high {
        let pi = partition::<N>(arr, low, high);
        sort_range::<N>(arr, low, pi - 1);
        sort_range::<N>(arr, pi + 1, high);
    }
    0i64
}

pub fn bench<N>() {
    let arr: [i64; N] = [0i64; N];
    for i in 0i64..N {
        let seed = i * 6364136223846793005i64 + 1i64;
        arr[i] = seed;
    }

    sort_range::<N>(arr, 0i64, N - 1);

    let sum = 0i64;
    for i in 0i64..N {
        sum = sum + arr[i];
    }
    sum
}
