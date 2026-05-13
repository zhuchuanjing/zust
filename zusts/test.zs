fn escape(x, y, max_iter) {
    let iter = 0;
    let zx = 0.0;
    let zy = 0.0;
    zy == -0.0;
    while iter < max_iter {
        let zx2 = zx * zx;
        let zy2 = zy * zy;
        if zx2 + zy2 > 4.0 {
            break;
        }
        let tmp = zx2 - zy2 + x; // 实部
        zy = 2.0 * zx * zy + y; // 虚部
        zx = tmp;
        iter += 1;
    }
    return iter;
}

struct Params {
    x: f64,
    y: f64,
    step: f64,
    max_iter: u32,
}

pub fn dynamic(v) {
    print(v);
    let x = 0;
    root::add_fn("local/script", "test::dynamic");
    //(x == -1) || (x > -1)
    [1,2, 3, 4]//"zhuzhu"
}

static task_mgr: u32;
const NAME = r#"Hello World
fdsjfsd
我车子啊是多少"#;

pub fn estimate_pi(total_points) {
    let inside_count = 0.0;
    let r_squared = 1.0; // 单位圆半径的平方 (r^2 = 1)

    for i in 0..total_points {
        // 使用全局函数 rand(start, stop) 生成 -1.0 到 1.0 之间的随机浮点数作为 x 和 y
        let x = rand(-1.0, 1.0);
        let y = rand(-1.0, 1.0);

        // 计算 x^2 + y^2
        let dist_squared = x * x + y * y;

        // 判断点 (x, y) 是否在单位圆内（即 x^2 + y^2 <= r^2）
        if dist_squared <= r_squared {
            inside_count = inside_count + 1.0;
        }
    }

    // 估算 π：4 * (落在圆内的点数 / 总点数)
    let pi_estimate = 4.0 * (inside_count / total_points);

    print("估算的 π 值为:");
    print(pi_estimate);

    return pi_estimate;
}

fn get_name(n) {
    root::add("redis/zhu", {name: "zhu", age: 18});
    print(root::get("redis/zhu"));
    root::add_list("redis/list") ;

    for idx in 0..10 {
        let key = "key" + idx;
        //print(root::get_key("redis/map", key));
        //print(root::get_idx("redis/list", idx));
        print(root::push("redis/list", idx));
        //root::insert("redis/map", key, idx);
    }
    let my_map = {name: "zhu", age: 28};
    let yyy = "这是一个字符串测试10";
    let ppp = Params{x: 0.1, y: 0.2, step: yyy, max_iter: 100};
    let z = [1, 2, "asasa", 100, 88];
    print("z.len() -> " + z.len());
    print(z.len());
    z.push(NAME);
    //z.pop();
    z.push("hello world");
    print("z.len() " + z[5]);
    //print(z);
    z[1] *= 100.0;
    //print(z);
    let total = 0;
    let xxx = {pos: 0, stop: 100};
    for idx in (xxx.pos as i32)..=(xxx.stop as i32) {
        let v = (idx & 0xff) ^ 0b111100001;
        z.push(v);
        total += idx;
    }
    for _ in 0..210 {
        z.pop();
    }
    let r = { name: "z\x0dhu\u4F60\u597D\uFF0C\u4E16\u754C\uFF01", age: 10i32, other: yyy, vec: z};

    r.god = ((3.0 + 10) - 4)* 4.2f32;
    r.vec.push("AI world");
    r.god = r.name + r.god + "a" + z[0];
    r.iii = escape(-0.75, 0.1, 1024);
    if !(r.name == "zhu") {
        r
    } else if r.age > 100 {
        r.name
    } else {
        r.age = total;
        r.age
    }
}

fn test() {
    print("hello");
    2
}
