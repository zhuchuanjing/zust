pub fn bench(n: i64) {
    let l = [];
    for i in 0..=n {
        l.push(true);
    }

    let ok0 = l.get_idx(0) == true;
    let ok1 = l.get_idx(1) == true;
    let ok5 = l.get_idx(5) == true;

    l[0] = false;
    l[1] = false;

    let changed0 = l.get_idx(0) == false;
    let changed1 = l.get_idx(1) == false;
    let still5 = l.get_idx(5) == true;

    let count = 0i64;
    for p in 2..=n {
        if l.get_idx(p) == true {
            count = count + 1;
        }
    }

    if !ok0 { return 1; }
    if !ok1 { return 2; }
    if !ok5 { return 3; }
    if !changed0 { return 4; }
    if !changed1 { return 5; }
    if !still5 { return 6; }
    count
}
