// test_is_list_minimal.zs - 最小化测试

pub fn test_is_list_on_num() {
    let num = 42;
    let result = num.is_list();
    return result;
}

pub fn run_all_tests() {
    test_is_list_on_num();
    return "passed";
}
