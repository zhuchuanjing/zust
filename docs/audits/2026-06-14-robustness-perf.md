# Zust 鲁棒性 / 正确性 / 性能 / 报告

提交：`fix/robustness-correctness`

## TL;DR

| 类别 | 数量 | 描述 |
|------|------|------|
| 真 bug 修复 | 7 | `Type::PartialEq` panic、`f16` 不支持、嵌套 `fn` panic、`\x` off-by-one、`else` 静默吞错、float 后缀越界、未知函数兜底 |
| 严格化 | 1 | 未知函数调用改成 `Err` 报告，`Symbol::Null` 兜底移除 |
| 性能 | 2 | `consts` IndexMap + 稳定名字 ID；`SymbolTable.modules` IndexMap + 简化 `get_id` |
| 语言层 | 4 | 拒绝 impl body 嵌套 decl；移除 `String` const 静默降级 warn；`let x = { ... }` 块表达式；list pattern `..rest` / `[..all]` |
| 锁定原语 | 1 | `std::sync::RwLock` → `parking_lot::RwLock`，所有 `read/write().unwrap()` 简化（设计决策见 [docs/dynamic-sync-design.md](dynamic-sync-design.md)）|
| 错误定位 | 1 | `ParserErr` 统一携带 `Span`（[Span:start, Span:end]）|
| 清理 | 1 | vm / 编译器其它 `.unwrap()` 转 `?` 失败传播 |

**测试**：`cargo test --workspace` → **290 passed**（无回归）。`cargo run -p zusts` → 全跑通。

## 已修改 / 新增的 crate

| Crate | 旧 | 新 | 改动 |
|-------|-----|-----|------|
| `zust-parser` | 0.9.15 | 0.9.16 | `ParserErr::Spanned{message, span}` + 便捷构造 `at(msg, pos)`；pattern `..rest` / `[..all]` |
| `zust-compiler` | 0.9.34 | 0.9.35 | `get_id` 重写 + 改用 IndexMap；`format_compile_error` 接 `ParserErr.span()`；impl 体内拒绝 struct/impl/const/static |
| `zust-dynamic` | 0.9.16 | 0.9.16 | `F16` 用 `half` crate；`RwLock` → `parking_lot::RwLock`；`pub use parking_lot::RwLock` |
| `zust-vm` | 0.9.75 | 0.9.76 | `jit` / `native_symbols` 同步改 `parking_lot`；清掉 `jit.write().unwrap()`；`v.unwrap()` 改成 `anyhow!` 错误传播 |

## 详细改动

### Phase 0 — 真正的 bug

| 编号 | 改动 | 文件 | 验证 |
|------|------|------|------|
| **F0.1'** | `Type::PartialEq` 函数比较返回 `false` 而非 panic；类型推断走独立 `merge_return_type` 错误路径 | [dynamic/src/types.rs:143](dynamic/src/types.rs) | 4 个新测试 |
| **F0.2** | `Dynamic::F16(u16)` 实现 + `f16_to_f64`/`f64_to_f16` 用 `half` crate；`force`/`as_float`/`is_native` 同步支持 | [dynamic/src/lib.rs:11-22](dynamic/src/lib.rs) | 4 个新测试覆盖 1.0 / 0.5 / subnormal / inf |
| **F0.3** | parser 阶段拒绝嵌套 `fn`（去掉 compiler `panic!`）| [parser/src/stmt.rs:233-242](parser/src/stmt.rs) | 4 个新测试 |
| **F0.4** | `\x` 转义 off-by-one + hex 字符校验 + 越界值校验 | [parser/src/lib.rs:543-562](parser/src/lib.rs) | 3 个新测试 |
| **F0.5** | `else` 解析错误用 `?` 上抛，去掉 `.ok()` 静默吞 | [parser/src/stmt.rs:213-223](parser/src/stmt.rs) | 1 个新测试 |
| **F0.6** | `float_literal` 整数后缀范围 / 整数性校验 | [parser/src/lib.rs:653-690](parser/src/lib.rs) | 3 个新测试 |
| **F0.7** | 未知函数调用严格报错，去掉 `Symbol::Null` 兜底 | [compiler/src/lib.rs:1661-1667](compiler/src/lib.rs) | 1 个新测试 |

### Phase 0b — F0.2b 严格化

`half = "2.7.1"` 加入 `zust-dynamic` 依赖。所有 F16 路径（force / as_float / 序列化 / `Debug`）走 `half::f16::from_bits / to_bits / from_f64 / to_f64`。subnormal、signaling NaN、Inf 全部 round-trip 正确。

### Phase 1 — 性能

| 编号 | 改动 | 文件 | 收益 |
|------|------|------|------|
| **F1.2** | `consts: Vec<Dynamic>` → `IndexMap<SmolStr, Dynamic>`，键是 `Debug` 渲染（字符串字面量用 `str:` 前缀防与同名 native 类型冲突）；`get_const` O(1) | [compiler/src/lib.rs](compiler/src/lib.rs) | 字段访问 `obj.foo` 从 O(n) 降到 O(1) |
| **F1.3** | `SymbolTable.modules: BTreeMap<BTreeMap>` → `IndexMap<IndexMap>`；`get_id` 重写避开反复 `format!` 拼字符串 + 多 modules 扫描 | [compiler/src/symbol.rs](compiler/src/symbol.rs) | 跨模块脚本符号查找去掉字符串分配 |

**撤回**：
- **F1.1** closure 单次编译：高风险，重写 IR 后嵌套闭包语义错。
- **F1.4** state clone → length index：破坏 bigfloat 多态推断。

### Phase 2 — 语言层

| 编号 | 改动 | 文件 | 验证 |
|------|------|------|------|
| **F0.3b** | impl body 拒绝 struct/impl/const/static（fn 仍允许） | [parser/src/stmt.rs:243-252](parser/src/stmt.rs) | 2 个新测试 |
| **F2.5** | 移除 `String` const 静默降级 `log::warn!`；保留 String fallback 路径但不再 warn | [compiler/src/lib.rs](compiler/src/lib.rs) | 现有 test 验证 |
| **F2.1** | `let x = { stmts; expr }` 块表达式（`ExprKind::Stmt` 包装）；dict 优先级保持 | [parser/src/stmt.rs:267-274](parser/src/stmt.rs) | 2 个新测试 |
| **F2.3** | list pattern `[a, b, ..rest]` / `[..all]`；rest 绑定为 `expr[prefix_count..]` 切片 | [parser/src/pattern.rs](parser/src/pattern.rs) + [parser/src/stmt.rs](parser/src/stmt.rs) | 2 个新测试 |

### F1.6 — 锁原语替换

完整设计决策见 **[docs/dynamic-sync-design.md](dynamic-sync-design.md)**。

`std::sync::RwLock` → `parking_lot::RwLock`：
- 所有 `Arc<RwLock<...>>` 内部从 std 改为 parking_lot
- 所有 `.read().unwrap()` / `.write().unwrap()` 简化为 `.read()` / `.write()`（parking_lot guard 不再是 `Result`）
- vm crate 同步改 `jit` / `native_symbols`
- llm / dynamic / json / msgpack 等下游调用方全部跟进

**关键决策**（避免历史错误）：**不改 `Arc` → `RefCell`**。`RefCell::clone` 隐式深拷贝 IndexMap，会让 `let other = map.clone()` / `+` / `set` / 函数参数传递全部从 O(1) 退化成 O(n)。详细理由见设计文档。

### F3.1 — ParserErr 携带 span

`ParserErr` 改成单一 variant：

```rust
pub enum ParserErr {
    Spanned { message: String, span: Span },
}
```

`ParserErr::new(message, span)` 和 `ParserErr::at(message, pos)`（便捷构造）覆盖所有 `?` 上抛路径。`format_compile_error` 在 [compiler/src/lib.rs:963-983](compiler/src/lib.rs) 现在 downcast `ParserErr` 拿 span 输出精确的"第 N 行 第 M 列"。`parse_code` 同样 downcast。

`SpannedParseError` 保留作为 `take()` 等需要单独传 `pos` 的路径（实测用 `Span::new(pos, pos)` 等价）。

### F3.2 — vm 其它 unwrap 清理

- 删 `jit.write().unwrap()` / `jit.read().unwrap()` (~15 处)
- `vm/src/rt.rs:281` inlined native callback 改用 `.and_then` 传播 `Result<_, anyhow::Error>` 而不是 panic
- 移除测试代码里 `use std::sync::RwLock;` 死 import
- binary.rs / context.rs 里 codegen 路径的 `unwrap()` 保留（这些是 hot path，类型已经被前一步推断过，不引入噪声 if-let）

## 设计文档

**[docs/dynamic-sync-design.md](dynamic-sync-design.md)** 解释：

1. `Dynamic::clone` 故意浅拷贝的设计意图（`Arc::clone` 不是并发，是值语义）
2. 跨线程路径审计（spawn / Task / WebSocket mpsc / root::send / closure 拒绝带 capture）— **没有任何 Dynamic 真的跨线程**
3. 为什么 `parking_lot::RwLock` 不是 `RefCell`（F1.6 差点踩坑的反思）
4. 未来"性能优化"提案必须证明不会破坏 `Clone` 语义 — 没有 benchmark 之前假设改动是错的

## 验证

```
$ cargo test --workspace
test result: ok. 290 passed (24 suites)

$ cargo run -p zusts
>>> 测试完成 <<<
[test_recursive_bug::run_all_tests] 结果: String("recursive tests passed")
编译 pathfind.zs -> SPIR-V (769 words) 耗时 2.5ms
迷宫: 32x32 cells, 图像: 512x512
... 路径在迭代 1000 找到 (距离 1000)
```

## 建议的下一步

- 性能优化后续：`SymbolTable::get_id` 的剩余 `format!` 路径可加 `HashMap` 二级索引
- 写 micro-bench 量化 parking_lot 替换收益
- 合并 F0.4 / F0.5 / F0.6 / F0.7 之后，parser 已无 panic / 静默吞错路径；可以删 README 里的"已知限制"章节
- 继续推 F2.6 字符串插值 / F2.7 match / F2.8 type alias / F2.9 `?` 运算符

## 提交 / push / 发布

按 `Co-Authored-By: Claude <noreply@anthropic.com>` 约定提交到 `fix/robustness-correctness`，push 到 origin，版本号：

- `zust-parser` 0.9.15 → **0.9.16**
- `zust-compiler` 0.9.34 → **0.9.35**
- `zust-dynamic` 0.9.16（dep 升级到 parking_lot 0.12.3）
- `zust-vm` 0.9.75 → **0.9.76**（dep 升级到 parking_lot 0.12.3）

后续 crates.io 发布按 `cargo publish -p zust-parser` 等顺序，依赖先发布。
