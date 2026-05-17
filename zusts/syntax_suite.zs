// Zust syntax coverage derived from parser/src and compiler/src.
// Covered here: comments, import, literals, primitive and array types,
// const/static, let patterns, expression statements, blocks, if/else,
// while/loop/for, break/continue/return, functions, generics, structs,
// impl methods, associated calls, closures, lists, repeated arrays, dicts,
// ranges, indexing, field access, casts, unary ops, binary ops, and assigns.

import("syntax_imported", "syntax_imported.zs");

pub const CONST_ANSWER: i32 = 42i32;
pub static STATIC_NUM: i32 = 7i32;

struct HiddenPoint {
    x: i32,
    y: i32,
}

pub struct Point {
    x: i32,
    y: i32,
}

impl Point {
    pub fn sum(self: Point) {
        self.x + self.y
    }

    pub fn shift(self: Point, dx: i32, dy: i32) {
        Point{
            x: self.x + dx,
            y: self.y + dy,
        }
    }
}

pub struct Boxed<T> {
    value: T,
}

impl Boxed<T> {
    pub fn get(self: Boxed<T>) {
        self.value
    }
}

fn identity<T>(value: T) {
    value
}

fn add_i32(left: i32, right: i32) {
    left + right
}

fn no_value_return(flag: bool) {
    if flag {
        return;
    }
}

fn syntax_string_literals_only() {
    let text: string = "z\x75st";
    let raw: string = r#"raw text with "quotes""#;
    let escaped = "line\nnext";
    let dict = {"string-key": text, raw, escaped};
    dict
}

fn syntax_closure_literals_only() {
    let base = 10i32;
    let add_base = |value: i32| {
        value + base
    };
    add_base
}

pub fn test_literals_types_and_comments() {
    // line comment
    /*
       block comment
    */
    let boolean: bool = true;
    let int8: i8 = 1i8;
    let int16: i16 = 2i16;
    let int32: i32 = 3i32;
    let int64: i64 = 4i64;
    let uint8: u8 = 5u8;
    let uint16: u16 = 6u16;
    let uint32: u32 = 7u32;
    let uint64: u64 = 8u64;
    let float32: f32 = 1.5f32;
    let float64: f64 = 2.5f64;
    let text: string = "z\x75st";
    let raw: string = r#"raw text with "quotes""#;
    let hex = 0x10u32;
    let oct = 0o7u32;
    let bin = 0b1010u32;
    let arr: [u32; 1 + 1] = [hex, oct + bin] as [u32; 2];

    boolean
        && int8 == 1i8
        && int16 == 2i16
        && int32 == 3i32
        && int64 == 4i64
        && uint8 == 5u8
        && uint16 == 6u16
        && uint32 == 7u32
        && uint64 == 8u64
        && float32 < 2.0f32
        && float64 > 2.0f64
        && text.len() == 4
        && raw.len() > 10
        && arr[0] == 16u32
        && arr[1] == 17u32
        && CONST_ANSWER == 42i32
        && STATIC_NUM == 7i32
}

pub fn test_unary_binary_and_assigns() {
    let signed = -10i32;
    let logical = !false;
    let n = 5i32;
    n += 7i32;
    n -= 2i32;
    n *= 3i32;
    n /= 5i32;
    n %= 4i32;

    let bits = 0b1010u32;
    bits &= 0b1110u32;
    bits |= 0b0001u32;
    bits ^= 0b0011u32;
    bits <<= 1u32;
    bits >>= 1u32;

    signed == -10i32
        && logical
        && n == 2i32
        && bits == 8u32
        && (1i32 + 2i32 * 3i32 == 7i32)
        && (8i32 >> 1i32 == 4i32)
        && (1i32 < 2i32)
        && (2i32 <= 2i32)
        && (3i32 > 2i32)
        && (3i32 >= 3i32)
        && (3i32 != 4i32)
        && ((true && true) || false)
}

pub fn test_control_flow() {
    let picked = if true { 10i32 } else { 20i32 };
    let branch = if picked == 0i32 {
        0i32
    } else if picked == 10i32 {
        1i32
    } else {
        2i32
    };

    let total = 0i32;
    for idx in 0..5 {
        if idx == 3 {
            continue;
        }
        total += idx;
    }
    for idx in 1..=3 {
        total += idx;
    }
    while total < 15i32 {
        total += 1i32;
    }
    loop {
        total += 1i32;
        break;
    }

    no_value_return(true);
    branch == 1i32 && total == 16i32
}

pub fn test_patterns_lists_dicts_and_fields() {
    let (left, right) = (3i32, 4i32);
    let [first, second] = [5i32, 6i32];
    let _ = first;

    let label = 100i32;
    let data = {
        label,
        count: left + right,
        items: [1i32, 2i32, 3i32],
    };
    data.items.push(4i32);
    data.items[0] = data.items[1] + 10i32;
    data.extra = second;

    left == 3i32
        && right == 4i32
        && data.label == 100i32
        && data.count == 7i32
        && data.items.len() == 4
        && data.items[0] == 12i32
        && data.extra == 6i32
}

pub fn test_structs_impls_generics_and_assoc() {
    let p = Point{x: 1i32, y: 2i32};
    let q = p.shift(3i32, 4i32);
    let hidden = HiddenPoint{x: 8i32, y: 9i32};
    let boxed = Boxed<i32>{value: 11i32};
    let imported = syntax_imported::ImportedPair{left: 20i32, right: 22i32};

    p.sum() == 3i32
        && Point::sum(q) == 10i32
        && hidden.x + hidden.y == 17i32
        && boxed.get() == 11i32
        && identity(12i32) == 12i32
        && syntax_imported::imported_add(5i32, 6i32) == 11i32
        && imported.sum() == 42i32
        && syntax_imported::IMPORTED_CONST == 9i32
}

pub fn test_closures_arrays_ranges_and_calls() {
    let base = 10i32;
    let add_base = |value: i32| {
        value + base
    };

    let repeated: [u32; 3] = [0u32; 1 + 2];

    let open_sum = 0i32;
    for idx in 0..3 {
        open_sum += idx;
    }
    let closed_sum = 0i32;
    for idx in 0..=3 {
        closed_sum += idx;
    }

    add_base(5i32) == 15i32
        && add_i32(1i32, 2i32) == 3i32
        && repeated[0] == 0u32
        && repeated[1] == 0u32
        && repeated[2] == 0u32
        && open_sum == 3i32
        && closed_sum == 6i32
}
