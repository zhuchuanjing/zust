pub fn bench(n: i64) {
    let arr: [i64; 1000] = [0i64; 1000];
    for i in 0..1000 {
        arr[i] = i;
    }
    let total = 0i64;
    for _ in 0..n {
        let half = 500i64;
        for i in 0..half {
            let j = 999i64 - i;
            let tmp = arr[i];
            arr[i] = arr[j];
            arr[j] = tmp;
        }
        for i in 0..1000 {
            total = total + arr[i];
        }
    }
    total
}
