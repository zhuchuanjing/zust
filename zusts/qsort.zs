fn partition(vec: Vec<i32>, low: i64, high: i64) {
    let pivot_index = low + (high - low) / 2;
    vec.swap(pivot_index, high);    //把 pivot 移到末尾
    let pivot = vec[high];

    let store_index = low;
    for i in low..high {
        if vec[i] <= pivot {
            vec.swap(i, store_index);
            store_index += 1;
        }
    }
    vec.swap(store_index, high);
    store_index
}

pub fn sort_range(vec: Vec<i32>, low: i64, high: i64) {
    if low >= high {
        return;
    }
    let p = partition(vec, low, high);
    if p > 0 && p - low < high - p {
        sort_range(vec, low, p - 1);
        sort_range(vec, p + 1, high);
    } else {
        sort_range(vec, p + 1, high);
        sort_range(vec, low, p - 1);
    }
}
