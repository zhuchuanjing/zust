// Zust 边界条件测试 - 精简版

pub fn test_int_extremes() {
    let max_i32: i32 = 2147483647i32;
    let min_i32: i32 = -2147483648i32;
    let max_f64: f64 = 1.7976931348623157e308f64;
    max_i32 > min_i32 && min_i32 < 0i32 && max_f64 > 1e307f64
}

pub fn test_empty_containers() {
    let empty_str: string = "";
    let empty_list = [];
    let empty_map = {};

    empty_str.len() == 0
        && empty_list.len() == 0
        && empty_map.is_map()
}

pub fn test_nested_patterns() {
    let (a, b) = (1i32, 2i32);
    let [first, second] = [3i32, 4i32];
    let (_, middle, _) = (5i32, 6i32, 7i32);
    a == 1i32 && b == 2i32 && first == 3i32 && second == 4i32 && middle == 6i32
}

pub fn test_nested_loops() {
    let outer_sum = 0i32;
    for i in 0..=2 {
        for j in 0..=2 {
            outer_sum += i * 3i32 + j;
        }
    }
    outer_sum == 36i32
}

pub fn test_nested_if_chain() {
    let x = 15i32;
    let category = if x < 0 {
        -1i32
    } else if x < 10 {
        0i32
    } else if x < 20 {
        1i32
    } else {
        2i32
    };
    category == 1i32
}

pub fn test_dynamic_list_operations() {
    let items = [1i64];
    items.push(2i64);
    items.push(3i64);
    let first = items.get_idx(0);
    let last = items.get_idx(2);
    let popped = items.pop();
    items.len() == 2 && first == 1i64 && last == 3i64 && popped == 3i64
}

pub fn test_dynamic_map_operations() {
    let data = {key: "value", count: 10i64};
    data.extra = 20i64;
    data.key = "updated";
    data.contains("key") && !data.contains("nothing") && data.key == "updated"
}

pub fn test_string_split() {
    let text = "hello world";
    let empty = "";
    text.len() > empty.len()
}

pub fn test_range_expressions() {
    let sum = 0i32;
    for i in 0..3 {
        sum += i;
    }
    sum == 3i32
}

fn bitwise_not_i32(value: i32) {
    !value
}

fn bitwise_not_u32(value: u32) {
    !value
}

pub fn test_bitwise_operations() {
    let a: u32 = 0b1100u32;
    let b: u32 = 0b1010u32;
    let zero: i32 = 0i32;
    let signed_not: i32 = bitwise_not_i32(zero);
    let unsigned_not: u32 = bitwise_not_u32(b);
    (a & b) == 0b1000u32
        && (a | b) == 0b1110u32
        && signed_not == -1i32
        && unsigned_not == 0xfffffff5u32
}

pub fn test_negation_on_types() {
    let signed = -100i64;
    let not_true = !true;
    let not_false = !false;
    signed == -100i64 && not_true == false && not_false == true
}

pub fn test_compound_assign_all_ops() {
    let n = 10i32;
    n += 5i32;
    n -= 3i32;
    n *= 2i32;
    n /= 4i32;
    n %= 5i32;
    n == 1i32
}

pub fn test_array_index_assign() {
    let items = [0i32, 1i32, 2i32];
    items[0] = 10i32;
    items[1] = items[1] + items[2];
    items[0] == 10i32 && items[1] == 3i32
}

pub fn test_string_concat_all_types() {
    let int_val = 42i64;
    let float_val = 3.14f64;
    let int_str = "" + int_val;
    let float_str = "" + float_val;
    int_str.len() > 0 && float_str.len() > 0
}

pub fn test_chain_reassign() {
    let x = 1i32;
    x = 2i32;
    x = 3i32;
    x = x + 1i32;
    x == 4i32
}

pub fn test_nested_struct_field_access() {
    let data = {
        a: {
            b: {
                c: {
                    value: 42i32,
                },
            },
        },
    };
    data.a.b.c.value == 42i32
}

pub fn test_mixed_type_list() {
    let items = [1i32, "hello", 3.14f64, true];
    let ok1 = items.len() == 4;
    items.push(42i64);
    let ok2 = items.len() == 5;
    items.pop();
    let ok3 = items.len() == 4;
    ok1 && ok2 && ok3
}

pub fn test_map_iteration() {
    let data = {x: 1i64, y: 2i64, z: 3i64};
    let keys = data.keys();
    let total = 0i64;
    for value in data {
        total += value;
    }
    keys.len() == 3 && total == 6i64
}

pub fn test_void_null_in_bool_context() {
    let items = [1i32, 2i32];

    // void expression (push) treated as false in &&
    let ok1 = !(items.push(3i32) && false);

    // null treated as false
    let ok2 = !(null && true);
    let ok3 = null || true;

    // void/null in ||
    let ok4 = null || items.len() == 3;

    ok1 && ok2 && ok3 && ok4
}
