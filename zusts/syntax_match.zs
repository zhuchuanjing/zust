// match 表达式回归测试。
// 覆盖:字面量、通配符、or-pattern、tuple/list 解构、guard、struct 解构、嵌套。

pub struct Vec2 {
    x: i32,
    y: i32,
}

pub fn test_match_literal_int() {
    let r = match 2i32 {
        1i32 => 10i32,
        2i32 => 20i32,
        3i32 => 30i32,
        _ => -1i32,
    };
    r == 20i32
}

pub fn test_match_or_pattern() {
    let r = match 5i32 {
        1i32 | 2i32 | 5i32 => 1i32,
        _ => 0i32,
    };
    r == 1i32
}

pub fn test_match_wildcard_default() {
    let r = match 99i32 {
        1i32 => 10i32,
        2i32 => 20i32,
        _ => 999i32,
    };
    r == 999i32
}

pub fn test_match_tuple_destructure() {
    let r = match (3i32, 4i32) {
        (a, b) => a + b,
    };
    r == 7i32
}

pub fn test_match_list_with_rest() {
    let r = match [1i32, 2i32, 3i32, 4i32] {
        [a, b, ..rest] => a + b + rest.len(),
        _ => -1i32,
    };
    r == 5i32
}

pub fn test_match_list_exact() {
    let r = match [10i32, 20i32, 30i32] {
        [a, b, c] => a + b + c,
        _ => -1i32,
    };
    r == 60i32
}

pub fn test_match_struct_field_capture() {
    let p = Vec2{x: 3i32, y: 7i32};
    let r = match p {
        Vec2{x, y} => x * 100i32 + y,
    };
    r == 307i32
}

pub fn test_match_struct_literal_in_field() {
    let p = Vec2{x: 0i32, y: 5i32};
    let r = match p {
        Vec2{x: 0i32, y} => y + 1000i32,
        Vec2{x, y} => x + y,
    };
    r == 1005i32
}

pub fn test_match_guard() {
    let r = match 7i32 {
        n if n > 10i32 => 100i32,
        n if n > 5i32 => 50i32,
        _ => 0i32,
    };
    r == 50i32
}

pub fn test_match_string_literal() {
    let r = match "yes" {
        "yes" => 1i32,
        "no" => 0i32,
        _ => -1i32,
    };
    r == 1i32
}

pub fn test_match_bool() {
    let r = match true {
        true => 1i32,
        false => 0i32,
    };
    r == 1i32
}

pub fn test_match_as_statement_expression() {
    // 第一个 arm 不会命中,验证 fall-through 到第二个
    let r = match 0i32 {
        1i32 => 10i32,
        n => n + 7i32,
    };
    r == 7i32
}
