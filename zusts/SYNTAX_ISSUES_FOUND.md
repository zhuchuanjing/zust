# Zust 语法/编译语义问题报告

## 测试范围

- **43 个 Positive 测试** 通过 (来自 `zusts/syntax_suite.zs`、`syntax_edge.zs`、`syntax_match.zs`)
- **36 个 Stress 测试** 通过 (我自己写的)
- **13 个 Negative/错误检测 测试**
- **5 个未闭合边界 测试**

## ✅ 工作正常的语法特性

- 字面量: int(8/16/32/64), float(32/64), bool, str
- 类型注解: `let x: i32 = ...`
- const/static/let
- 函数声明(`pub fn`)
- struct/impl/方法/关联函数
- 泛型参数
- 闭包(只支持块体,见下文)
- 数组、字典、列表、范围
- 控制流: if/while/for/loop/break/continue/return
- match 字面量/通配符/元组/列表/struct/guard/string/bool
- 运算符: 算术、逻辑、位运算、复合赋值
- 嵌套 fn 拒绝(明确报错)
- 未定义标识符检测(明确报错)
- 类型不匹配(显式注解时检测,如 `let x: i32 = "hello"`)

## ⚠️ 发现的真实问题

### 问题 1: Parser 在 EOF 时静默吞掉错误 [严重]

**位置**: `compiler/src/lib.rs:1110-1112` (`parse_code`)

```rust
match p.stmt(false) {
    Ok(stmt) => stmts.push(stmt),
    Err(e) => {
        if p.is_eof() {
            return Ok(stmts);  // BUG: 在 EOF 吞掉错误
        }
        ...
    }
}
```

**影响场景**:
```zust
fn ok() { 42 }
fn bad() { let x = 1;     // ← 缺 } — 应报错,但被静默丢弃
```
运行结果: 仅 `fn ok` 被编译,`fn bad` 默默消失,**用户完全不知道有错误**。

**测试用例** (`int_plus_str`, `unclosed_str`, `unclosed_block`, `unclosed_paren`, `unclosed_bracket`):
- `let s = "hello` (未闭合字符串) → 0 stmts,无错误
- `let x = 1;` (未闭合块) → 0 stmts,无错误
- `foo(1, 2` (未闭合括号) → 0 stmts,无错误
- `[1, 2, 3` (未闭合方括号) → 0 stmts,无错误

**根因**: `parse_code` 是 partial/import_source 也能用的公共 API,而 `import_file` 走的是它。
**影响严重性**: 高 — 完整 `.zs` 文件编译时,文件末尾的语法错误对用户隐藏。

### 问题 2: 缺少运行时类型算术检查 [严重]

**测试用例**:
```zust
fn main() { 1 + "hello" }     // 静默接受
fn main() { 1 - "hello" }     // 静默接受
fn main() { 1i32 * 1.5f64 }   // 静默接受
```

但:
```zust
fn main() { let x: i32 = "hello" }  // 拒绝(显式标注)
```

**位置**: `compiler/src/infer.rs`(类型推断模块)。推断可能过松:二元运算符两侧的类型没有相互约束检查 — 推测允许字符串到 Dynamic 的隐式转换,推断结果落到 Any/Dynamic。

**影响严重性**: 中-高 — 用户写的明显错误不会在编译期被捕获,而是推到运行时(可能 panic 或返回 null)。

### 问题 3: 不支持字节字符串字面量 [设计问题]

```zust
let s = b"hello"  // 解析错误: not code block
```

其他 Rust 风格的语言(Swift、Rust、Kotlin)一般都有字节字符串支持。Zust 没有。

**严重性**: 低 — 不影响核心功能。

### 问题 4: 闭包不支持单行表达式体 [设计问题]

**测试用例**:
```rust
let f = |x: i32| x + 1;     // 错误
let f = |x: i32| { x + 1 }; // OK
```

zust 只支持块体闭包。Rust、Kotlin、Swift 都允许这种简写形式。

**严重性**: 低 — 设计选择,但降低代码简洁性。

### 问题 5: `vm::get_fn_ptr` 的 panic 路径 [小问题]

**位置**: `vm/src/lib.rs:440, 458`:
```rust
other => panic!("expected integer-like return, got {other:?}"),
```

当用户用错的方式调用 JIT 函数指针(返回类型不匹配),VM 会 panic 而不是返回运行时错误。

**严重性**: 低 — 一般使用路径不会触发,只有 FFI 集成代码或类型错误编译才会遇到。

## 📊 测试覆盖总结

| 类型 | 数量 | 结果 |
|---|---|---|
| Positive (语法正确) | 79 | 79/79 通过 |
| Negative (应有错误) | 13 | 9/13 正确报错, 4 个静默 |
| Edge (边界) | 5 | 1 严重 + 4 无错 |

## 建议修复优先级

| 优先级 | 问题 | 影响 |
|---|---|---|
| **P0** | Parser EOF 吞错 | 完整文件末尾错误不可见 |
| **P0** | 算术类型检查 | int + str 等明显错误编译期不报 |
| P3 | 字节字符串 | 设计补全 |
| P3 | 单行闭包 | 设计补全 |
| P3 | panic 路径 | FFI/错误调用栈 panic |
