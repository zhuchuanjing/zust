# Zust 语言审计报告

**审查版本**: vm 0.9.74 / compiler 0.9.32 / parser 0.9.13 / dynamic 0.9.14
**审查范围**: parser (parser/), compiler (compiler/), dynamic (dynamic/), vm 边界
**审查日期**: 2026-06-14

---

## 0. 总览

Zust 是一个 Rust 形状的脚本语言，运行时基于 Cranelift JIT，配有 SPIR-V / Metal / Vulkan
后端，以及 http / llm / db / gpu 等原生模块。语法表面已经收敛，但底层实现里仍残留
一些 panic、安静吞错和不一致。下面按 **致命 / 中等 / 建议** 三档列出，并给出复现与
建议改法。

报告中所有路径都是相对 `/Volumes/zhu/zust/` 的相对路径，便于点击跳转。

---

## 1. 真正的 Bug(优先级 P0 / P1)

### 1.1 `Type::PartialEq` 在函数返回类型不一致时直接 panic
[P0] [dynamic/src/types.rs:143-152](dynamic/src/types.rs)

```rust
(Type::Fn { tys: t1, ret: r1 }, Type::Fn { tys: t2, ret: r2 }) => {
    if t1 == t2 {
        if r1 != r2 {
            panic!("函数返回类型不一致")
        }
        true
    } else { false }
}
```

函数签名比较时，一旦参数一致但返回类型不同就 panic。`Type` 的 `PartialEq` 是
`#[derive(PartialEq)]` 之外的"手工"实现，这一处 `panic!` 把它从「值比较」退化成了
「运行时炸弹」。任何让两个 `Fn { ret }` 走到这条分支(常见于泛型实例化差异)的路径
都会让 host 进程崩溃。编译器内部多处用 `==` 比较 `Type`(infer 里的 `merge_range_bound_types`
后的 `Type + Type`，以及返回类型合并)，是受影响的实际路径。

**修法**：把 `panic!` 换成 `r1 == r2`，并把 `PartialEq` 文档化为"宽松比较"或拆出
`Type::fn_signature_eq` 显式函数。

### 1.2 `f16` 类型没有 `Dynamic` 表示，parser 接受但运行时炸
[P0] [parser/src/lib.rs:68](parser/src/lib.rs) / [parser/src/lib.rs:601-612](parser/src/lib.rs)
[dynamic/src/types.rs:127](dynamic/src/types.rs)

`TYPES` 里包含 `("f16", Type::F16)`，`get_type` 也认 `f16`，但 `numeric_suffix` 里
显式跳过 `F16`，`Dynamic` 枚举里也根本没有 `F16` 变体。一旦脚本写了 `let x: f16 = 1.0f16;`
或 `1.0f16`，后续要落到 `Dynamic::F16` 的地方会 panic 或静默失败。

**修法**：要么 `cargo build` 时关掉 `f16` 关键字并改写文档，要么在 `dynamic` 里
提供 `Dynamic::F16(half::f16)` 并把 `force`、`get_type` 都补齐。

### 1.3 嵌套 `fn` 触发编译期 panic
[P0] [compiler/src/lib.rs:1772-1781](compiler/src/lib.rs)

parser 允许在 `fn body` 里写 `fn`，但编译器到这里直接 `panic!("nested functions are not supported here")`。
README 把这条列为"may trigger compiler crash"——这其实不是 may，是一定。

```rust
fn outer() {
    fn inner() { 1 }   // ← panic
}
```

**修法**：在 parser 的 `function_body` / `block` 里直接拒绝(返回明确的
"结构体 / impl / const / fn 不能定义在函数内"错误)。这样错误信息会落在用户能看到
的地方，而不是 host 进程崩溃。

### 1.4 `\x` 转义解析长度条件 off-by-one
[P1] [parser/src/lib.rs:542-550](parser/src/lib.rs)

```rust
b'x' => {
    self.pos += 1;
    if self.pos + 2 < self.buf.len() {
        let start = self.pos;
        self.pos += 2;
        let hex = &self.buf[start..self.pos];
        ...
    }
}
```

`self.pos + 2 < self.buf.len()` 应当是 `<=`(或者 `<` 配 `>= 2`)。结果：
`"...\x41"`(字符串末尾正好 2 位 hex)会被静默吞掉，整段变成空字符串。
`"abc\x"` 同样。

**修法**：把判断改为 `self.pos + 2 <= self.buf.len()`，并且在读到非 hex 字符时返回
"非法 \\x 转义"。

### 1.5 `else if` / `else` 解析错误被 `.ok()` 静默吞掉
[P1] [parser/src/stmt.rs:213-214](parser/src/stmt.rs)

```rust
if self.keyword("else").is_ok() {
    self.whitespace()?;
    if self.keyword("if").is_ok() { self.if_block().map(Box::new).ok() }
    else { self.block().map(Box::new).ok() }
}
```

`if_block` / `block` 失败时 `.ok()` 直接把 `Err` 转成 `None`，导致 `if cond { ... } else
something_bad` 静默丢失 else 分支，且位置停留在错误处，主解析循环下一个 `stmt` 解析
时会报一个莫名其妙的"未结束的表达式"。

**修法**：用 `?` 上抛或 `else return Err(...)` 保留诊断信息。

### 1.6 `Float::Literal` 不校验后缀越界
[P1] [parser/src/lib.rs:652-667](parser/src/lib.rs)

`int_literal` 显式校验 `magnitude > max_allowed` 并报错；`float_literal` 把
`1e30i8` / `1e30u8` 直接 `as i8` 静默截断成 `i8::MAX`。同样是不一致：`9999999999999u8`
会报错，`1e15u8` 不会。

**修法**：在 `float_literal` 入口对 `is_int` 后缀做范围检查并报错。

### 1.7 `Type::Fn` `Add`(`operator+`)在参数列不一致时 panic
[P1] [dynamic/src/types.rs:143](dynamic/src/types.rs)

`Type + Type` 用于推断混合宽度算术结果。`PartialEq` 的同一处 `panic!` 也会通过
`==` 触发(参见 1.1)。`(i32, f32) + (i32, i32)` 就会触发。

**修法**：同 1.1。

### 1.8 编译器对未解析标识符的 fallback 注入符号表
[P2] [compiler/src/lib.rs:1629-1636](compiler/src/lib.rs)

```rust
Err(e) => {
    if let ExprKind::Ident(ident) = &obj.kind {
        let fn_id = if ident.contains("::") { self.symbols.add_global(ident.clone(), Symbol::Null) }
                    else { self.symbols.add(ident.clone(), Symbol::Null) };
        Ok(Expr::new(ExprKind::Call { obj: Box::new(Expr::new(ExprKind::Id(fn_id, None), ...)), params }, ...))
    }
}
```

调用了一个不存在的函数时，编译器不仅不报错，还把 `Symbol::Null` 加到全局符号表里，
掩盖错误。下次再调用同一个名字就静默走通——用户的 typo 永远得不到诊断。

**修法**：移除 fallback，或者在 release 编译模式下保留(并打 warn)，在 debug / check
模式下报"未知函数"。

---

## 2. 性能瓶颈(P2)

### 2.1 闭包 body 被编译两次
[compiler/src/lib.rs:1478-1492](compiler/src/lib.rs)

```rust
let _ = self.compile_fn(names.as_slice(), &mut tys.clone(), *body.clone(), &mut local_cap)?;
// ...
let mut compiled = self.compile_fn(names.as_slice(), &mut tys.clone(), *body.clone(), &mut Capture::default())?;
```

第一次调用纯粹是为了触发 `cap` 的 side effect；第二次才真正产出 `compiled`。
每次 closure 都付两倍 compile 成本，并且 body AST 被克隆两次。`compile_fn` 内部
又要 `take_local_state`/`restore_local_state` + 4 次重复推断循环(见下)。

**修法**：让 `compile_fn` 返回 `(cap_used, compiled_stmts)`，或者显式走两遍只对
cap 做追踪。

### 2.2 闭包符号每解析一次就 add 一次
[compiler/src/lib.rs:1496](compiler/src/lib.rs)

```rust
let name = SmolStr::from(format!("__closure_{}_{}", expr.span.start, expr.span.end));
let fn_id = self.symbols.add(name, Symbol::Fn { ... });
```

任何在源码同一 span 上重复出现的闭包都会撞名(目前会被 IndexMap 的 `insert_full`
覆盖，但旧 symbol 还在 modules map 里挂空)。

**修法**：给 closure 一个去重计数器后缀，或在 add 之前先 `get_id`。

### 2.3 `infer_fn` 的 4-iteration fixed-point + 多次 state clone
[compiler/src/infer.rs:840-891](compiler/src/infer.rs)

```rust
for _ in 0..4 {
    let saved_state = self.take_local_state();   // 5 个 Vec take
    ...
    let pass_local_type_hints = self.collect_local_type_hints();
    self.restore_local_state(saved_state);
    ...
}
```

每个被推断的函数都会被推断最多 4 遍，每一遍：
- `take_local_state` 把 `frames / names / tys / list_elem_states / arg_counts` 整个
  掏空(`mem::take`)；
- 再 `restore_local_state` 装回；
- `collect_local_type_hints` 又对 `tys` 做一次遍历。

当 `tys` / `names` 长 ~1000 时(大函数很正常)，每次推断要 5k+ 元素的 Vec 复制。
递归 / 互递归的泛型函数又触发 `infer_fn` 多次。

**修法**：把 state 用 `Rc<RefCell<...>>` 共享 / 用 frame 索引代替完整克隆 / 把
`take_local_state` 改成只 save 当前 frame 长度。

### 2.4 `SymbolTable::get_id` 是 O(M) 字符串查找
[compiler/src/symbol.rs:313-334](compiler/src/symbol.rs)

`infer_expr` 几乎每一行都调用 `self.symbols.get_id(name)`。每次都：
1. `IndexMap::get_index_of` (O(1))；
2. 遍历 `roots` 拼 `{root}::{name}` 然后再 `get_index_of`；
3. 遍历所有 `modules` 在每个 `BTreeMap` 里查名字；
4. 切分 `::` 二次尝试。

多模块脚本里这是不可忽略的开销，并且拼字符串 `format!` 还会反复分配。

**修法**：在 `add` 时维护一个 `name → (module, id)` 的二级 `HashMap`(`HashMap<SmolStr, u32>`)；
失效时机只有 add 模块 / pop 模块，复杂度可降到 O(1)。

### 2.5 `eval` 路径大量临时 `Dynamic` 克隆
[compiler/src/lib.rs:1644-1660](compiler/src/lib.rs)

```rust
let mut v = Vec::new();
for (idx, item) in list.iter().enumerate() {
    if item.is_value() {
        v.push(item.clone().value().unwrap());  // 克隆
    } else { ... }
}
let list = Expr::new(ExprKind::Const(self.get_const(Dynamic::list(v))), expr.span);
```

列表 / dict 字面量在编译期就构造完整的 `Dynamic::list`，运行时再 `get_const`
取回，每个元素都过一遍 clone。对于大数组这是一次性 O(n) 开销，但仍可以靠
intern 优化。

### 2.6 `BinaryOp::Idx` 双重语义 + 每次访问都拼 const string
[parser/src/expr.rs:408](parser/src/expr.rs) / [compiler/src/lib.rs:1063-1064](compiler/src/lib.rs)

`obj.field` 全部 lowering 成 `Expr { Binary(Idx, obj, Const("field")) }`，再下层
`get_const(Dynamic::String(key.into()))` 反复在 `consts` 表里查。`consts` 是
`Vec<Dynamic>`，查找是线性 (`position`) —— 每条字段访问都是 O(K)。

**修法**：给 `consts` 配一个 `HashMap<Dynamic, usize>` 索引，或在 `Expr` 里直接
保留 `SmolStr` 而不是 `ExprKind::Const(idx)`。

---

## 3. 语言设计 / 局限(P1 / P2)

### 3.1 block `{ ... }` 不能作为表达式
README 已经记录这条 (`let y = { ... }` → 解析错误)。对 Rust 用户非常别扭。
**建议**：在 stmt 里像 `closure` 那样 fallback 处理；或单独加 `let y = (|| { ... })();` 的 lint。

### 3.2 `for ch in "abc"` 不支持
README 记录：`for in` 不迭代字符串。
**建议**：在 `infer_range_expr` 里加 `Type::Str → Type::list_any()` 的展开路径，
或为字符串显式提供 `for_in_chars()` 辅助。

### 3.3 `break value` / `loop` 作为表达式缺失
README 已记录。

### 3.4 `let` 关键字不能重复，但允许 `let x = ...; x = ...; let x = ...` ?
**确认**：parser 已经在 `declare_pattern_symbols` 拒绝同 scope 重复声明(`lib.rs:984-1003` 测试)。
但 `let x = 1; let mut ...` 风格的 `mut` 不存在 —— 一切都是可变的。**这是设计意图**，
但 README 没把"`mut` 故意省略"明确写进 "Design Ideas"。

### 3.5 `const` / `static` 中的 `String` 静默降级
[compiler/src/lib.rs:1530-1534](compiler/src/lib.rs) / [compiler/src/lib.rs:1708-1726](compiler/src/lib.rs)

`let name: string = "literal"` 只打 `log::warn!` 然后把约束去掉变成 `Any`。
**建议**：要么支持 `const` 里真正的 `String`(在 dynamic 里已经有 `Dynamic::String`，
只需要存进 `Const { value, ty }`)，要么报错而不是 warn。

### 3.6 泛型仅支持 const-int 维度
[parser/src/lib.rs:325-418](parser/src/lib.rs) / [compiler/src/lib.rs:106-108](compiler/src/lib.rs)

`struct BigFloat<N>` 写满了 `i32 + N - 1`、`u32 as i32` 等的 ugly cast，
因为 `N` 必须能落到 i32。GPU 上要的多维 / 类型维度完全不支持。
**建议**：把 `ConstInt` 扩成 `ConstType { kind: Int | Float | Bool, value }`，
让 `pub struct Matrix<M, N>` 这种结构也能落地。

### 3.7 `fns` 推断记忆化按 `(generic_args, fn_tys)` 维度
[compiler/src/infer.rs:770-825](compiler/src/infer.rs)

`fns: BTreeMap<u32, Vec<(Vec<Type>, Vec<Type>, FnInferRet)>>`，按 fn_id 分桶，
按 `(generic_args, fn_tys)` 比较。`Type::PartialEq` 在 1.1 那一处 panic 也会让
这条记忆化彻底无效 —— 任何返回类型不一致的实例化都会触发 host panic。

### 3.8 closure 类型推断限制
[compiler/src/lib.rs:1478](compiler/src/lib.rs)

closure 参数列表只能从字面量 signature 推，没有 closure-trait inference，
所以 `let f = |x| x; f(1)` 里 `f` 是 generic，但 `f` 被存到 `Vec<fn(i64)>` 时
不能 infer。
**建议**：在 capture / 赋值时把 closure 当作单态化函数特化。

### 3.9 `is_any()` 把 `Fn { ret = Any }` 也算 Any
[dynamic/src/types.rs:285-291](dynamic/src/types.rs)

```rust
pub fn is_any(&self) -> bool {
    match self {
        Self::Any => true,
        Self::Fn { tys: _, ret } => ret.is_any(),
        _ => false,
    }
}
```

返回类型还是 `Any` 的函数被认为本身就是 `Any`。这通常无害，但和 `Type::Add`
的"`Any + Anything = Any`"规则叠加后，会让 `(Fn{Any} + Int)` → `Any`，
进而丢失"这是函数"的元信息，影响 `expr_calls_fn` 之类的判断。

---

## 4. Parser / 编译器的健壮性

### 4.1 大量 `.unwrap()` 与 `.expect()`
- [parser/src/expr.rs:287, 299, 300, 852, 868, 882](parser/src/expr.rs)
- [compiler/src/lib.rs:1044, 1494, 1653, 1779](compiler/src/lib.rs)
- [vm/src/lib.rs: 多处 .unwrap()](vm/src/lib.rs)

parser 的 .unwrap() 多半发生在 `compact()` / `binary_op` 里，理论上走不到 panic，
但 fail-safe 应该返回错误而不是 panic。`compiler/src/lib.rs:1779` 那个
"nested functions" 的 panic 是上文 1.3。`vm/src/lib.rs:323` 的
`root::get(path).unwrap()` 会让 host 进程 crash，应该返回 `Result`。

### 4.2 错误信息位置丢失
[parser/src/stmt.rs:213-214](parser/src/stmt.rs) / [parser/src/stmt.rs:331-343](parser/src/stmt.rs)

parser 静默吞错时，`pos` 留在错误处，后续 `?` 上抛虽然能拿到错误，但 span 已经指
向下一个语句，错误位置错位。
**建议**：统一改成 `?` + 显式 Span，把位置信息塞进 `ParserErr` 变体里。

### 4.3 fuzzer 覆盖率良好但缺 spec 测试
[parser/src/lib.rs:917-965](parser/src/lib.rs) 有 4000 次随机输入 fuzz，
但没有针对 `let f = MyType<MyGeneric::Variant>` / `MyStruct < 1 > { ... }` /
`return value` / `break value` 的定向回归测试。可以在 fuzz corpus 里固化这些
"曾经报错过"的 input。

### 4.4 `is_eof()` 与 `whitespace()` 的 `?` 交错
parser 里大量 `self.whitespace()?` 后紧跟 `self.is_eof()`，但 `whitespace` 已经
跳过空白；`is_eof` 只看 `pos >= len`。在 `try_parse!` 回滚 `pos` 后，偶尔会
出现 `is_eof` 返回 true 但当前字符实际不是 EOF(因为 `whitespace` 已经跳过
换行)。这种情况在 `parser/src/lib.rs:724-727` 触发，导致 "未结束的表达式"。

---

## 5. 可改进方向(中长期)

### 5.1 解析器架构

- 当前是单文件 hand-written recursive descent，深度上限 128 已经处理。短
  期收益不大，但增加：
  - **更友好的错误恢复**(在 `;` / `}` / 同步关键字上同步)；
  - **token 缓存**(目前每次 `keyword` / `just` 都重新切字符串比较)；
  - **expression 的 Pratt / shunting-yard 化**：`expr_with_min_weight` 嵌套已经
    非常深(参见 700+ 行的 `expr.rs:714-887`)，拆成左结合 + 优先级表更清晰。

### 5.2 编译器

- 把 `Compiler` 拆成 `Inferer` / `Lowerer` / `ConstFolder` 三个阶段。`infer.rs`
  976 行，`lib.rs` 1821 行，单文件过 1000 行已经不利于 diff 评审。
- 引入"调用点缓存"：当前每次 `infer_fn_with_params` 都重新 `take_local_state`，
  应该用 `Rc<RefCell<ScopeStack>>` 让 type hints 跨调用共享。
- `consts: Vec<Dynamic>` 改成 `HashMap<Dynamic, usize>`(参见 2.6)。
- 取消"unknown fn → Symbol::Null" 的兜底(参见 1.8)。

### 5.3 动态值层

- `Dynamic` 大量用 `Arc<RwLock<...>>` 包裹 list / map / struct。每个 `is_list()` /
  `len()` 都过 read lock。当前是 single-threaded VM，但 callback / spawn 路径
  可能有并发访问。建议加一个**单线程快路径**(`&mut` 引用)以及**无锁版本**(`Cow`)，
  让纯单线程函数不走 RwLock。

- `Dynamic::get_type()` 在 `Self::List(items)` 里 `read().unwrap().iter()` 然后对每
  个元素再 `get_type()`，嵌套 list 时是 O(n) 加锁。建议懒求值 + 缓存(动态 list
  一旦类型稳定下来就 memo)。

### 5.4 性能优化机会

- `inferred_local_type_hints` 是 `BTreeMap<u32, Vec<(Vec<Type>, Vec<Type>, Vec<Option<Type>>)>>`，
  同一个 fn 反复实例化时(泛型)每次都复制 vec。应该用 `Rc<...>` 共享。
- `take_local_state` 用 `mem::take` 后 `restore_local_state` 直接赋值 (`self.frames = state.0`)，
  即 `Vec::new()` 后 `extend` —— 这是 O(N)，且每次推断跑 4 次 + 子调用多次。
  改用 frame index + Vec 长度 truncate 就足够。
- `indexmap` 已经引入到 SymbolTable 和 Dynamic::Map，可以继续：
  - `Compiler::names` (Vec<SmolStr>) → 偶尔 `iter().rev().find(...)` 是 O(N)，可
    以为当前 scope 维护一个 IndexMap<SmolStr, u32>。
  - `tys: Vec<Type>` 同理，locate 用线性扫描。

### 5.5 语言层

- 加 `?` 运算符或 try-block：当前错误处理靠 return null + 调用方检查，体验差。
- 加 `match`：`if cond { a } else { b } else if ... else { c }` 在四五个分支
  时已经很难读。
- 字符串插值：当前 `"" + x + " level"` 重复写 `"" +`，可以加 `"${x} level"`。
- pattern 中加 `..rest`：`let [first, ..rest] = items`，目前 `PatternKind::List`
  有 `has_rest: bool` 但 parser 不解析 `..`。
- 加 enum / sum type：当前用 `Type::Symbol { id }` + tagged union 是 workaround。
- 加 type alias：`type Bytes = Vec<u8>`。
- `pub` / 模块可见性：目前 `pub` 只是符号标记，没有真正的可见性检查 —— 任何
  文件 `import` 进来后都能调到非 pub 函数。

### 5.6 工具链

- `zust-lsp` 和 `zed-extension` 都依赖同一份 AST。可以把 parser crate 共享给
  LSP 之外再加一层"`check_code`(只解析+语义检查，不生成 IR)"的轻量入口。
- 没有 incremental compilation：`import_code` 每次都 `self.clear()` 然后
  整体重编译。多文件项目每次热重载都是 O(files × size)。

---

## 6. P0 / P1 / P2 汇总

| 级别 | 条目 | 文件 |
|------|------|------|
| **P0** | `Type::PartialEq` 函数返回类型 panic | [dynamic/src/types.rs:143-152](dynamic/src/types.rs) |
| **P0** | `f16` 字面量无 Dynamic 后端 | [parser/src/lib.rs:601](parser/src/lib.rs) |
| **P0** | 嵌套 `fn` 触发 host panic | [compiler/src/lib.rs:1779](compiler/src/lib.rs) |
| **P1** | `\x` 转义长度 off-by-one | [parser/src/lib.rs:542](parser/src/lib.rs) |
| **P1** | `else` 解析错误被 `.ok()` 吞掉 | [parser/src/stmt.rs:213-214](parser/src/stmt.rs) |
| **P1** | `float_literal` 不校验后缀越界 | [parser/src/lib.rs:652](parser/src/lib.rs) |
| **P1** | `Type::Add` 经过 `PartialEq` 也会 panic | [dynamic/src/types.rs:143](dynamic/src/types.rs) |
| **P1** | `String` const 静默降级为 Any | [compiler/src/lib.rs:1530-1534](compiler/src/lib.rs) |
| **P1** | 块表达式 / 字符串迭代 / `break value` 缺失 | [README.md:199-213](README.md) |
| **P2** | 闭包 body 编译两次 | [compiler/src/lib.rs:1478-1492](compiler/src/lib.rs) |
| **P2** | closure symbol 重名潜在冲突 | [compiler/src/lib.rs:1496](compiler/src/lib.rs) |
| **P2** | `infer_fn` 4 次迭代 + 全量 state clone | [compiler/src/infer.rs:840-891](compiler/src/infer.rs) |
| **P2** | `SymbolTable::get_id` O(M) 字符串拼装 | [compiler/src/symbol.rs:313-334](compiler/src/symbol.rs) |
| **P2** | `consts` 线性查找 | [compiler/src/lib.rs:771](compiler/src/lib.rs) |
| **P2** | 未知函数 fallback 到 `Symbol::Null` | [compiler/src/lib.rs:1629-1636](compiler/src/lib.rs) |
| **P2** | `.unwrap()` / `.expect()` 多处 | [parser/src/expr.rs](parser/src/expr.rs) / [vm/src/lib.rs](vm/src/lib.rs) |
| **P2** | parser / 编译器 / Dynamic 共享 RwLock 但单线程 | [dynamic/src/lib.rs](dynamic/src/lib.rs) |

---

## 7. 测试与回归建议

- 把 1.1 / 1.2 / 1.3 三个 P0 bug 各写一个最小复现的 `.zs` 进
  `zusts/bug_tests/`；现有的 `test_recursive_bug.zs` / `test_is_list_minimal.zs`
  可以再加：
  - `test_type_fn_panic.zs` —— 比较 `Type::Fn { ret = I32 }` 与 `Type::Fn { ret = F32 }`；
  - `test_f16_lacks_dynamic.zs` —— `let x = 1.0f16; print(x);`；
  - `test_nested_fn_panic.zs` —— `fn outer() { fn inner() {} }`。
- 在 parser 单测里加 `deeply_nested_blocks_with_recovery`，验证错误恢复能正
  确继续解析后面的语句，而不是污染下一个 stmt 的起始位置。
- 编译器加 `#[bench]` 跑 `BigFloat<N>` / Mandelbrot 类型推断，量化 2.3 / 2.4
  优化前后的改进。

---

## 8. 一句话总结

Zust 的语法表面已经"差不多可以用了"，但**底层在 `Type::PartialEq`、`f16` 后端、
嵌套函数 panic、`\x` 边界**这四件事上还有真 bug；性能上 **闭包二次编译、
state 全量克隆、`SymbolTable::get_id` 字符串拼装**是头号瓶颈；语言层最大的
缺憾是 **block-as-expr、字符串迭代、`break value`** 这几个 Rust 用户最自然
期待的特性仍然缺失。