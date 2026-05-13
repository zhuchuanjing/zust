// test_recursive_bug.zs - 复现递归函数类型推断问题

// ---- 阶乘递归函数 ----
pub fn test_factorial() {
    print("=== 阶乘递归测试 ===");
    
    fn factorial(n) {
        if n <= 1 {
            1                    // 推断为 I32
        } else {
            n * factorial(n - 1) // 由于递归推断占位，返回 Any
        }
    }
    
    let fact_5 = factorial(5);
    print("5! = " + fact_5);
    
    return fact_5;
}

// ---- 斐波那契递归函数 ----
pub fn test_fibonacci() {
    print("=== 斐波那契递归测试 ===");
    
    fn fibonacci(n) {
        if n <= 0 {
            0                    // 推断为 I32
        } else if n == 1 {
            1                    // 推断为 I32
        } else {
            fibonacci(n - 1) + fibonacci(n - 2)  // 推断为 Any
        }
    }
    
    print("fibonacci(0) = " + fibonacci(0));
    print("fibonacci(1) = " + fibonacci(1));
    print("fibonacci(2) = " + fibonacci(2));
    print("fibonacci(5) = " + fibonacci(5));
    
    return fibonacci(5);
}

// ---- 运行测试 ----
pub fn run_all_tests() {
    print(">>> 开始运行递归类型推断测试 <<<");
    
    test_factorial();
    test_fibonacci();
    
    print(">>> 测试完成 <<<");
    return "recursive tests passed";
}
