pub fn bench(n: i64) {
    let s = "";
    let sep = ",";
    let chunk = "hello";
    for i in 0..n {
        if i > 0 {
            s = s + sep;
        }
        s = s + chunk;
    }
    s.len() as i64
}
