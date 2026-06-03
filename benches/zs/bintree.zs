fn make_tree(depth: i64) {
    if depth <= 0 {
        return 1;
    }
    1 + make_tree(depth - 1) + make_tree(depth - 1)
}

pub fn bench(n: i64) {
    make_tree(n)
}
