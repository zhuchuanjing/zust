# Zust 语言与模块手册

本文是 Zust 语言、运行时与模块的完整说明,内容基于本仓库当前实现写就。每个结论都标注了源码位置,便于溯源和验证。

---

## 1. 项目总览

Zust 是一门**动态强类型** + JIT(Cranelift) 的脚本语言,语法高度对齐 Rust,但去掉了**所有权 / 借用检查**和**trait 系统**这两个最重的 Rust 概念,带 GPU 计算后端。

**定位关键词**:

- **类 Rust 语法**:`fn / let / if / for / while / loop / match / struct / impl / pub / const / static / import` 全保留;表达式风格、复合赋值、tuple/list/struct 字面量、`as` 类型转换、turbofish 泛型实参 —— 跟 Rust 同形。
- **动态**:函数参数和 `let` 不标注类型时默认 `Type::Any`,值跨边界用统一的 `Dynamic` 联合表达;变量可重赋值;集合(`Map / List`)的元素类型可在运行时变;字符串拼接 `"x=" + 42` 会在 `Type::Any` / `Type::Str` 边界自动转字符串(见 [dynamic/src/types.rs:82](../dynamic/src/types.rs))。
- **强类型**:每个 `Dynamic` 值在运行时携带具体类型 —— `Bool / I8..I64 / U8..U64 / F16..F64 / Str / Map / List / StructView / Custom / Bytes / VecI8.. VecF64` 等;原生函数和 native 方法按类型分发,**没有隐式回退到 `void *`**;算术/比较运算受 `dynamic/src/ops.rs` 的规则约束,类型不兼容会在运行时失败而不是静默放过。
- **可选静态标注**:在标注了 `i32 / f32 / [u32; N] / Boxed<T>` 等具体类型时,编译期会做类型推导并走 Cranelift JIT 的原生 ABI;否则走 Any 通路。GPU kernel 路径要求 100% 静态(数值 / 数组 / native struct),不允许 Any。
- **无所有权 / 借用检查**:没有 `&` / `&mut` / `'a` / `mut` / `Box / Rc / Arc` 的语法位置,变量直接重赋值;运行时由 VM 内部的 thread-local arena + scope 管理临时值寿命(见 [README.zh.md](../README.zh.md) "VM 托管临时内存")。
- **无 trait / impl Trait for Type / dyn**:`impl` 块只能给具体的 `struct` 加方法,不存在"为类型实现某接口"的概念;多态由 `Any` + 运行时分发完成;泛型只是单态化的"类型替换",没有 trait bound,所有约束都是结构性的(只要写得通就编得过)。

整体架构:

```
源码 (.zs) → parser → compiler → ┬─ vm (Cranelift JIT) → CPU 原生代码
                                  ├─ vm-spirv → SPIR-V 字节流 → vulkan
                                  └─ vm-metal → Metal Shading Language → Metal runtime
```

Workspace 一共 12 个 crate(见 [Cargo.toml](../Cargo.toml)):

| Crate | 职责 |
|---|---|
| `dynamic` | 类型系统 `Type` + 运行时值 `Dynamic`,JSON / MessagePack / 类型化向量 |
| `parser` | 词法 + 语法分析,产 `Stmt/Expr/Pattern` AST |
| `compiler` | 符号表、类型推导、`import` 解析、泛型替换 |
| `vm` | Cranelift JIT 运行时;原生函数 / 模块注册 |
| `vm-spirv` | Zust → SPIR-V 计算 shader 后端 |
| `vm-metal` | Zust → Metal Shading Language 后端 |
| `vulkan` | 用 vulkano 跑 SPIR-V |
| `zusts` | CLI / 回归测试 demo binary |
| `root` | 可寻址对象树(Memory / Redis / Fjall),tokio 桥接 |
| `llm` | LLM 调用助手:complete / image / audio / tts / deep |
| `zust-lsp` | tower-lsp 实现的语言服务 |
| `benches` | 基准 |

---

## 2. 语法

### 2.1 关键字与字面量

**保留字**(见 [parser/src/lib.rs:81](../parser/src/lib.rs)):

```
true false null
let if else match for in while loop
pub fn struct impl const static
continue return break
```

`match` 是表达式关键字;`as` 软关键字(类型转换),`import` / `spawn` 是软关键字(顶层 native 调用形式,不是真正的关键字 —— 它们只是注册在 `std::*` 下的 native 函数)。

**没有以下 Rust 关键字 / 语法**:

| 缺席项 | 替代或说明 |
|---|---|
| `mut` | 默认所有变量可重赋值,不需要 |
| `mod` / `use` / `extern crate` / `pub use` | 改用 `import("name", "path.zs")` 函数调用 |
| `&` / `&mut` / `*ptr` / `'lifetime` | 没有借用检查;Map / List / Struct 默认共享底层数据 |
| `Box` / `Rc` / `Arc` 语法位置 | 由 VM arena 管理寿命;长寿命跨边界值放进 ROOT |
| `trait` / `impl Trait for Type` / `dyn` / `where` | 没有 trait;多态走 `Any` |
| `?` 错误传播 / `?.` / `??` / `\|>` | 错误自己 if-return 处理;`null` 可以直接比较 |
| `unsafe` / `async fn` / `await` | 没有显式 unsafe;异步走回调闭包(见 `llm::deep` / `root::send`) |
| `enum` 变体 / `Result<T,E>` ADT | 用 map / 字符串 tag 自己拼;`Dynamic` 已经是巨型 enum |
| `loop { break value }` | `break` 是纯控制流,不能带值;用变量 |
| 块 `{ ... }` 作为右值表达式 | 用 `(\|\|{ ... })()` IIFE 替代 |

**字面量**:

```rust
// 数字(后缀决定类型,无后缀整数 = i32,无后缀浮点 = f32)
42i32        12u64       3.14f64       0xFFu32      0o777u32     0b1010u32

// 布尔与 null
true   false   null

// 字符串
"hello"
"with \n \t \r escapes"
"\x75 \u{1F600}"                            // \x 两位十六进制 / \u Unicode

// 原始字符串(不解释任何转义)
r"raw \n stays as backslash + n"            // 内容里没有 " 时,无需 #
r#"with "quotes" inside"#                   // 内容里有 ",两侧加 #
r##"contains "# inside"##                   // 内容里有 "#,两侧加 ##
                                            //   解析见 parser/src/lib.rs:604

// 列表(同构,元素类型自动推导)
[1i32, 2i32, 3i32]
[]
[0u32; 1 + 2]                               // 重复字面量,长度可为常量表达式

// map(键可省略走简写,字符串键要显式)
{ x: 1, y: 2 }
{ name, version }                           // 简写 = { name: name, version: version }
{ "key with spaces": 7 }
{}

// tuple(只在解构 / 构造时出现)
(1, 2)

// range(用于 for 和切片)
0..3        0..=3        arr[2..]      arr[..5]
```

注释:`// 单行` 和 `/* 块 */`。

### 2.2 变量

```rust
let x = 5i32;                       // 推导
let y: f64 = 3.14;                  // 显式标注
let arr: [u32; 1+1] = [1u32, 2u32]; // 长度可为常量表达式
x = 10i32;                          // 直接重赋值,不需要 mut
x += 1i32;                          // 复合赋值同 Rust
```

**默认整型** = `i32`,**默认浮点** = `f32`。未标注的函数参数类型为 `Any`(动态)。

### 2.3 控制流

`if` 是表达式:

```rust
let label = if x > 0 { "pos" } else if x == 0 { "zero" } else { "neg" };
```

`match` 是表达式(Rust 风格),desugar 到 if 链 + 临时变量,不引入新的 VM 指令:

```rust
let label = match value {
    0 | 1 => "small",                          // 字面量 + or-pattern
    Point { x: 0, y } => "on-axis " + y,        // struct 解构 + 字面量字段
    [head, ..tail] => "list of " + tail.len(),  // list 解构 + ..rest
    n if n > 100 => "big",                      // 守卫(guard)
    _ => "other",                               // 通配,放在最后兜底
};
```

支持的 pattern:
- 字面量:数字、`true`/`false`/`null`、字符串
- `_` 通配
- `Ident` 单变量绑定 / 简写 `field` 绑定 struct 字段
- `Struct { name }` / `Struct { name, other: pat }` 结构解构
- `(a, b)` tuple 解构
- `[a, b, c]` / `[a, b, ..rest]` list 解构
- `pat1 | pat2 | pat3` or-pattern(MVP:除第一个外的 alt 只允许字面量 / 通配)

不支持:`match` 语句位置 / 范围模式 `0..=9` / 绑定语法 `n @ pat` / 穷尽性检查(后续再加)。

#### 守卫和顺序

```rust
match x {
    0 => "zero",
    n if n < 0 => "neg " + (-n),
    n => "pos " + n,        // 落到这里时 n 已被绑定
}
```

- 守卫是 `if` 后跟任意表达式;守卫为真才执行 arm 体。
- arm 顺序按出现先后匹配,命中后短路。
- arm 体是普通表达式,最后表达式即 arm 值(整 match 也是表达式,值 = 命中 arm 的体值)。

#### 闭包 vs match

zust 的 `match` 接收闭包模式不在计划内 —— `match` 是表达式且 desugar 到 if 链,要在闭包里"延迟执行"用 closure 即可:

```rust
let choose = |x: i32| match x { 0 => "a", _ => "b" };
```

循环:

```rust
while cond { ... }
loop { ... }                        // 无条件;退出靠 break / return
for i in 0..10 { ... }
for v in list { ... }
for value in some_map { ... }       // map 直接迭代值
break;   continue;   return value;
return;                             // 无值返回
```

函数体最后一个表达式 = 隐式 return。

> **`loop` 实现状态**:三条 codegen 路径全支持。
> - CPU JIT(Cranelift):走 `gen_loop(cond=None, body, ...)`,见 [vm/src/rt.rs:1882](../vm/src/rt.rs)。
> - SPIR-V:`gen_loop` 与 `gen_while` 共享 `OpLoopMerge` 结构,只是 header 直接无条件 `OpBranch` 到 body,见 [vm-spirv/src/stmt.rs](../vm-spirv/src/stmt.rs) `gen_loop`。
> - Metal(MSL):lower 成 `while (true) { ... }`,见 [vm-metal/src/stmt.rs](../vm-metal/src/stmt.rs)。
>
> `loop` 是真正的关键字(在 [parser/src/lib.rs:81](../parser/src/lib.rs) 的 `KEYWORDS` 表里),所以不能再用作变量名;`break value` 仍然不支持(`break` 是纯控制流,要返回值在 loop 外部用变量赋值)。

### 2.4 函数

```rust
fn add(a: i32, b: i32) {              // 返回类型推导
    a + b
}

pub fn id<T>(value: T) {              // 泛型
    value
}

fn no_value(x: i32) {                 // 显式 return,无值
    if x < 0 { return; }
    print(x);
}
```

**闭包**:

```rust
let base = 10i32;
let add_base = |v: i32| { v + base };   // 自由变量按值捕获
add_base(5);

(|| { 42i32 })()                         // IIFE
```

闭包跨 native 边界传值时,运行时表示是 `Dynamic::Custom(ZustCallback)`,见 [vm/src/native.rs:17](../vm/src/native.rs)。这就是 `root::update(path, |v| v+1)` 这类 API 能直接接收闭包的实现基础。

**禁止**:函数体内嵌套 `fn / struct / impl / const / static`(parser 直接报错,见 [parser/src/stmt.rs:267](../parser/src/stmt.rs))。需要"局部类型 / 局部函数"用闭包 + map 拼。

> **多态怎么办**:zust 没有 trait,因此没有 `impl Add for T` / `impl Display for T` / `impl Iterator for T` 这种"为类型实现接口"的模式。要写"对任何容器都通用"的代码,把参数标成默认类型 `Any` 即可 —— 编译器会按 Any 通路出 native 方法分发。例如 `fn sum(xs)` 等价于 `fn sum(xs: Any)`,`xs.len()` / `xs.get_idx(i)` 在运行时按真实类型决定走哪条 native(`Map / List / List<i32> / List<f32> / Str / StringBuf / Custom` …),完整对照见 §4.2。

### 2.5 Struct / impl

```rust
pub struct Point { x: i32, y: i32 }     // 字段必须给类型

pub struct Boxed<T> { value: T }        // 泛型

impl Point {
    pub fn sum(self: Point) -> i32 {     // self 是显式参数
        self.x + self.y
    }
    pub fn origin() -> Point {           // 关联函数(无 self)
        Point { x: 0i32, y: 0i32 }
    }
}

let p = Point { x: 1i32, y: 2i32 };
let s1 = p.sum();                        // 方法调用
let s2 = Point::sum(p);                  // 关联调用
let z  = Point::origin();
let b  = Boxed::<i32> { value: 11i32 };  // 显式泛型实参
```

`impl` 块只能放方法 `fn`,不能再嵌 struct / const / static。

**和 Rust 的关键差异**:
- 没有 `impl Trait for Struct`,`impl` 后面只能直接接具体类型(可以带泛型实参,如 `impl Boxed<T> { ... }`)。
- 没有 `Self` 关键字 —— `self` 必须显式用类型签名(`self: Point`),关联函数省掉 `self` 即可。
- 没有 `derive` 宏;`==` / `print` / `to_string` 已经在 `Dynamic` 这一层默认实现,不需要 derive。
- 没有 `Drop` —— 临时值由 VM arena 在 scope 退出时统一释放,Custom 句柄通过 `Dynamic::Custom(Box<dyn ...>)` 的 Rust Drop 自然回收。

**Native struct**:所有字段类型都是数字 / `bool` / 嵌套 native struct 时,该 struct 走原生 ABI 内存布局,跟 Rust `#[repr(C)]` 等价 —— 这是 GPU 路径和未来 ECS 模块的关键性质。

### 2.6 Pattern

模式出现在 `let`、`for` 头部,以及 `match` 的 arm 里(见 [parser/src/pattern.rs:14](../parser/src/pattern.rs)):

```rust
let _ = first;                                // 通配
let n = 1;                                    // 单变量
let Point { x, y } = p;                        // struct 解构
let (a, b) = (1, 2);                          // tuple
let [first, second] = [3i32, 4i32];           // list
let [head, ..tail] = list;                    // 带 rest
```

`match` 的 pattern 复用同一套语法(加上字面量 / or-pattern / 守卫,见 §2.3)。

`let [..rest] = xs` 中 `rest` 的类型由 desugar 产出 —— zust 当前不静态推断切片长度,所以后续对 `rest` 的方法调用会通过 Any 通路,常见操作(`.len` / `.push` / `.iter`)都能直接用。

### 2.7 模块系统

无 `use` / `mod`,通过顶层 `import` 函数调用引入(见 [compiler/src/lib.rs:610](../compiler/src/lib.rs)):

```rust
import("syntax_imported", "syntax_imported.zs");   // 命名 + 路径
import("util");                                     // 单参 = util.zs
```

跨模块用 `::` 限定:

```rust
syntax_imported::imported_add(5i32, 6i32);
let pair = syntax_imported::ImportedPair { a: 1i32, b: 2i32 };
let n = syntax_imported::IMPORTED_CONST;
```

**只有 `pub` 项跨模块可见**:`pub fn` / `pub struct` / `pub const` / `pub static`。

### 2.8 表达式与运算

完整表见 [parser/src/expr.rs:18](../parser/src/expr.rs):

| 类别 | 运算符 |
|---|---|
| 算术 | `+ - * / %` |
| 位 | `& \| ^ << >>` |
| 逻辑 | `&& \|\|` |
| 比较 | `== != < > <= >=` |
| 一元 | `- !` |
| 复合赋值 | `+= -= *= /= %= &= \|= ^= <<= >>=` |
| 索引/字段 | `a[i]` / `a.b.c` / `a.x = ...` |
| 切片 | `a[s..e]` / `a[s..=e]` / `a[s..]` / `a[..e]` |
| 类型转换 | `value as i32`,`"123" as i32`,`"3.5" as f64` |
| 泛型实参 | `Boxed<i32> { ... }` / `f::<i32>(...)` |

**字符串拼接走 `+`**:`"x=" + 42` 自动把右侧转字符串(`Type::Any + Type::Str` 规则,见 [dynamic/src/types.rs:82](../dynamic/src/types.rs))。

map 的 `data.extra = 7` 会自动新增键(动态字段写)。

---

## 3. 类型系统

zust 是**动态强类型 + 可选静态标注**:运行时的每个 `Dynamic` 值都精确知道自己是 `I32` / `Map` / `StructView` 还是 `Custom`,但**编译期**类型可以"留空走 Any 通路":

| 标注情况 | 编译期 `Type` | 运行时表现 |
|---|---|---|
| `let x: i32 = 5i32;` | `Type::I32` | Cranelift JIT 直接生成原生 i32 指令 |
| `let x = 5i32;` | `Type::I32`(由字面量后缀推出) | 同上 |
| `let x = 5;` | `Type::I32`(默认整型 = i32) | 同上 |
| `let x = some_root_get();` | `Type::Any` | 运行时 `Dynamic` tag 携带真实类型,所有方法走 Any 分发 |
| `fn f(x) { ... }` | 参数 `Type::Any` | 同上 |
| `fn f(x: i32) { ... }` | 参数 `Type::I32` | 走原生 ABI |

**关键**:你**永远不需要**写"动态类型转静态"的转换语句 —— Any → 具体类型在调用 `Any::to_i64()` / `as i32` 时显式发生,Any → 方法分发由编译器自动展开。

`Type` 枚举的全部 variant(见 [dynamic/src/types.rs:18](../dynamic/src/types.rs)):

**占位**
- `Any` 动态 / 未知
- `Void` 无值
- `Iter` 迭代器标记

**标量**
- `Bool`
- `I8 I16 I32 I64`,`U8 U16 U32 U64`
- `F16 F32 F64`
- `Str`

**集合**
- `Map` —— 动态 KV(键 → Dynamic)
- `List(T)` —— 同构动态长度列表
- `Tuple([T])` —— 元素类型已知的元组
- `Vec(T, n)` —— GPU 用小向量(n ≤ 4)
- `Array(T, n)` —— 通用数组,长度是常量
- `ArrayParam(T, len_expr)` —— 长度由编译期表达式给出

**用户定义 / 符号**
- `Struct { params, fields }` —— 含可选泛型参数
- `Ident { name, params }` —— 解析阶段未消化的命名类型
- `Symbol { id, params }` —— 已挂在符号表上的具名类型 / 函数

**函数 / 编译期**
- `Fn { tys, ret }`
- `ConstInt(i64)` —— 常量类型参数(数组长度等)
- `ConstBinary { op, l, r }` —— 编译期算术(`Add Sub Mul Div Mod`)

**运行时值** `Dynamic`(见 [dynamic/src/lib.rs:326](../dynamic/src/lib.rs))在上述基础上多出几种"实现细节"variant:`StringBuf`(可变字符串)、`Bytes`、`VecI8/U16/I16/U32/I32/F32/U64/I64/F64`(类型化字节缓冲,GPU 友好)、`StructView` / `StructOwned`、`Custom`(Rust 注入的句柄,**ZustCallback 闭包就装在这里**)、`Iter`。

> 注:`Dynamic` 中**没有** `ZustCallback` 这个独立 variant,闭包统一装在 `Dynamic::Custom`。

---

## 4. 内置函数与模块

所有 native 通过 `add_native_*` 注册到 JIT 符号表,full_name 形如 `"模块::函数"`,Zust 端用 `模块::函数(...)` 调用。注册入口在 [vm/src/rt.rs:307](../vm/src/rt.rs)。

### 4.1 `std::*` —— 全局

直接调用,无前缀。表在 [vm/src/native.rs:1387](../vm/src/native.rs)、注册在 [vm/src/lib.rs:206](../vm/src/lib.rs):

| 函数 | 签名 | 说明 |
|---|---|---|
| `print(any)` | `(Any) -> Void` | println,接任意类型 |
| `log(any)` | `(Any) -> Void` | Rust `log::debug!` 格式输出 |
| `sqrt(f64) -> f64` | `(F64) -> F64` | 平方根 |
| `uuid() -> string` | `() -> Any` | 生成 UUID 字符串 |
| `rand(min, max) -> any` | `(Any, Any) -> Any` | 范围内随机数;两端类型决定结果是整型还是浮点 |
| `import(name, path)` | `(Any, Any) -> Bool` | 加载 `.zs` 模块,见 §2.7;单参形式由 parser 顶层识别 |
| `spawn(target, args_tuple)` | `(Any, Any) -> Bool` | 启动 OS 线程并调用 `target`;`target` 可以是函数名字符串或无捕获闭包,`args_tuple` 是 tuple 参数包(空 tuple `()` 表示无参),最多 16 个参数;返回值丢弃 |

### 4.2 `time::*` —— 时间戳辅助

`Vm::new()` / `Vm::with_all()` 默认注册,见 [vm/src/time_module.rs](../vm/src/time_module.rs)。所有 tick 都是从 UNIX epoch 起的**毫秒数(`i64`)**,需要秒就 `/ 1000`。时区一律按 UTC 处理。

| 函数 | 签名 | 说明 |
|---|---|---|
| `time::now()` | `() -> I64` | 当前时刻的毫秒级 unix 时间戳;失败(系统时钟早于 epoch)返回 `-1` |
| `time::format(fmt, tick)` | `(Any, I64) -> Any` | 用 strftime 风格的 `fmt` 把 `tick` 格式化成字符串(UTC);非法 tick 返回 `null` |
| `time::parse(fmt, text)` | `(Any, Any) -> I64` | 反向解析,优先按"日期 + 时间"解析,失败再退到"仅日期";彻底失败返回 `-1` |

```rust
let now_ms = time::now();                                            // 例如 1750348800000
let label  = time::format("%Y-%m-%d %H:%M:%S", now_ms);              // "2026-06-19 12:00:00"
let parsed = time::parse("%Y-%m-%d %H:%M:%S", "2020-01-02 03:04:05");// 1577934245000
```

### 4.3 `Any::*` —— 动态值方法

78 个,核心几组(完整在 [vm/src/native.rs:1395](../vm/src/native.rs)):

```rust
// 类型断言 / 转换
v.is_map()  v.is_list()  v.is_string()  v.is_null()
v.clone()                               // 深拷贝
v.to_i64() / to_bool() / to_f64() / to_string()
Any::from_i64(n) / from_u64(n) / from_bool(b) / from_f64(f)

// 容器通用
v.len()                                 // i32
v.keys()                                // map 键列表
v.push(x)   v.pop()
v.get_idx(i)   v.set_idx(i, x)
v.contains(x)   v.starts_with(prefix)
v.slice(start, end, inclusive)

// map / struct(get / set / get_key / set_key 是同一组别名)
v.get(key)   v.get_key(key)             // 取键 / 字段
v.set(k, val)   v.set_key(k, val)       // 写键 / 字段(新加 set 别名)
v.del_key(k)                            // 删键

// 字符串
v.split(sep)

// 类型化 list 零开销访问(对每种数值都有)
v.push_i32(x)   v.get_idx_i32(i) -> i32   v.set_idx_i32(i, x)
//   还有 _bool _u8 _i8 _u16 _i16 _u32 _u64 _i64 _f32 _f64 _str
v.data_ptr_u64() / data_ptr_i64() / data_ptr_f64()    // 给 GPU 上传用

// 迭代
let it = v.iter();
let next = it.next();
let (k, v) = it.next_pair();

// 动态二元
Any::binary(a, op_id, b)                // 整型 op_id 决定运算
Any::logic(a, op_id, b) -> bool
```

### 4.4 `root::*` —— 对象树

20 个 native(见 [vm/src/root_module.rs:115](../vm/src/root_module.rs)):

```rust
// 挂载
root::mount(name, kind)                 // memory / redis 等
root::mount_fjall(data_dir)

// 节点构造
root::add_list(path) -> bool
root::add_map(path) -> bool
root::add(path, value) -> bool
root::contains(path) -> bool
root::remove(path) -> any

// 查询
root::get(path)
root::dir(path) -> [string]
root::len(path)

// 列表
root::push(path, v)
root::get_idx(path, i)
root::remove_idx(path, i)

// map
root::insert(path, key, v)
root::get_key(path, k)
root::remove_key(path, k)

// 原子并发更新(闭包)
root::update(path, |v| v + 1) -> any                    // 节点值
root::update_key(path, key, |v| v + 1) -> any           // map 内 key

// 消息
root::send(path, msg) -> any
root::send_idx(path, i, msg) -> any
```

`root::update` / `root::update_key` 是**原子 read-modify-write**,同 path 的并发调用由 `Mount::get_mut` 内置桶锁(`scc::HashMap::update_sync`)串行化,无丢失更新。Memory mount 16 线程 × 10000 自增实测约 60k ops/sec、~17 μs/op,跟 `Arc<Mutex<i64>>` (~30M ops/sec) 的差距来自路径解析 + 哈希 + Dynamic 类型分发,是结构化目录系统该有的开销。

> 闭包跨 native 边界靠 `Dynamic::Custom(ZustCallback)`(`as_custom::<ZustCallback>().call1(arg)`),实现见 [vm/src/root_module.rs](../vm/src/root_module.rs) 的 `root_update` / `root_update_key`。

### 4.5 `db::*` —— SQL(`feature = "db"`)

5 个 native(见 [vm/src/db_module.rs:737](../vm/src/db_module.rs)):

```rust
db::create(name, schema) -> bool
db::drop(name) -> bool
db::select(name, query, args) -> any        // 返回行列表
db::exec(name, sql, args) -> i64            // 影响行数
db::transaction(name, [[sql, args], ...]) -> i64
```

### 4.6 `http::*`(`feature = "http"`)

见 [vm/src/http_module.rs:985](../vm/src/http_module.rs):

```rust
http::request(config) -> any                // 通用,config 是 map
http::get(url) -> any
http::post(url, body) -> any
http::upload(url, file, fields) -> any      // 需要 feature="llm"
http::serve(config) -> any                  // 启动 HTTP server
```

### 4.7 `llm::*`(`feature = "llm"`)

见 [vm/src/llm_module.rs:124](../vm/src/llm_module.rs):

```rust
llm::complete(model, prompt) -> any         // 文本补全
llm::image(model, prompt, opts) -> any      // 图像生成
llm::audio(model, audio) -> any             // ASR
llm::tts(model, text) -> any                // TTS
llm::deep(model, prompt, opts) -> any       // 深度推理
```

### 4.8 `oss::*`(`feature = "llm"`)

见 [vm/src/oss_module.rs:44](../vm/src/oss_module.rs):

```rust
oss::upload(bucket, key, data) -> any
oss::signed_url(bucket, key) -> any
```

### 4.9 `gpu::*`(`feature = "gpu"`)

见 [vm/src/gpu_module.rs:32](../vm/src/gpu_module.rs):

```rust
gpu::spirv_compile(config) -> any           // 输入 {path/source, fn_name, workgroup_size}
gpu::spirv_check(config) -> any             // 仅做静态检查
gpu::metal_compile(config) -> any
gpu::metal_check(config) -> any
gpu::vulkan_run(config) -> any              // 跑编译好的 SPIR-V
gpu::metal_run(config) -> any
```

---

## 5. CLI 用法 (`zusts`)

`zusts` 当前**不是**带子命令的通用 CLI,而是一个**固定的回归 demo binary**(见 [zusts/src/main.rs:43](../zusts/src/main.rs)):

它做的事:
1. `Vm::with_all()` 起一个开启所有 feature 的 VM
2. 加载固定列表的 `.zs` 文件(`test.zs`、`qsort.zs`、`syntax_suite.zs`、`syntax_edge.zs` 等)
3. 跑硬编码的 bool 测试列表 `SYNTAX_BOOL_TESTS` / `SYNTAX_EDGE_BOOL_TESTS`
4. 跑 `gpu/pathfind.zs` 端到端 GPU pipeline,输出 `pathfind.png`

**真正的 zust runtime 是 `zust-vm` crate**。从 Rust 跑 `.zs` 脚本:

```rust
use vm::Vm;
use dynamic::Type;

let vm = Vm::with_all()?;
vm.jit.write().compiler.import_file("my", "/abs/path/to/my.zs")?;
// 或 import_code 直接喂字符串:
//   vm.jit.write().compiler.import_code("my", source.as_bytes().to_vec())?;

// 取函数指针并调用
let (ptr, ret_ty) = vm.jit.write().get_fn_ptr("my::compute", &[Type::I32, Type::I32])?;
let f: extern "C" fn(i32, i32) -> i32 = unsafe { std::mem::transmute(ptr) };
let r = f(3, 5);
```

如果要"通用 CLI"(`zusts run script.zs --fn main`),需要自己加 `clap` 子命令把上面的 `import + get_fn_ptr` 串起来。

---

## 6. GPU 后端

### 6.1 用户视角

CPU 和 GPU 用同一份 Zust 源码,只是某个函数被指定为 kernel 入口。Kernel 必须只用支持的子集:数值、`Vec/Array`、native struct、循环、分支(不能用 `Map` / `List` / 闭包 / 字符串)。

```rust
// path/find.zs
pub fn main(grid: [f32; N], params: PathParams) -> [f32; N] {
    // ...
}
```

```rust
// 在 zust 脚本里编译
let kernel = gpu::spirv_compile({
    path: "path/find.zs",
    fn_name: "main",
    workgroup_size: [64, 1, 1],
});
gpu::vulkan_run({ kernel: kernel, args: [...] });
```

或在 Rust 宿主端直接:

```rust
let kernel = vm_spirv::compile_file_with_workgroup_size(
    "path/find.zs", "main", [64, 1, 1])?;
// kernel.spirv: Vec<u32>,丢给 vulkano
```

### 6.2 编译路径(SPIR-V)

见 [vm-spirv/src/lib.rs:81](../vm-spirv/src/lib.rs):

1. `Compiler::new()` + `register_externs(spirv_builtins())`(sin/cos/sqrt 等 GPU 内置)
2. `compiler.import_file(...)` —— 复用 CPU 路径同款 parser + 类型推导
3. `specialize_entry_function` 代入泛型实参,得到具体 arg 类型 + body
4. `infer_fn_with_params` 推断返回类型
5. `collect_type_defs / collect_user_fns / collect_workgroup_statics` 收集 kernel 闭包
6. `SpirvCompiler::compile_kernel(...)` 输出 SPIR-V 字节流
7. 返回 `Kernel { spirv: Vec<u32>, entry: "main", arg_tys, ret_ty }`

### 6.3 编译路径(Metal)

见 [vm-metal/src/lib.rs:80](../vm-metal/src/lib.rs)。流程一致,最后输出 **Metal Shading Language 源码字符串**,入口名 `zust_main`。`feature = "runtime"` 时 [vm-metal/src/runtime.rs](../vm-metal/src/runtime.rs) 提供把 MSL 源码即时编译并跑在 Apple Metal 上的 `Runtime / MetalBuffer / Args`。

### 6.4 与 VM 的关系

- `zust-vm` 的 Cranelift JIT 只管 CPU
- `vm-spirv` / `vm-metal` 不依赖 `vm`,直接用 `compiler` + `dynamic` + `parser`,产物是字节流/字符串
- Zust 脚本里的 `gpu::*` 是 thin wrapper —— [vm/src/gpu_module.rs](../vm/src/gpu_module.rs) 负责把 `Dynamic` 入参转给 `vm-spirv` / `vm-metal`,把结果回包成 `Dynamic`

---

## 7. 速查与示例

### "动态 vs 强类型"一图流

```
源码 ──parser──► AST ──compiler/infer──┬─► 标注完整 → Type::I32 / F32 / 具体 struct → Cranelift 原生指令
                                       │
                                       └─► 留空     → Type::Any                      → Any::* native 分发
                                                                                       │
                                运行时 Dynamic tag ◄──────────────────────────────────┘
                                  ├─ I32 / I64 / F32 / F64 / Bool / Str / Map / List / StructView / Custom / Bytes / VecF32 ...
                                  └─ 类型不匹配 → DynamicErr / panic,**不会静默继续**
```

**强类型的两条边界**:
- `Any::to_i64(s)` 当 `s` 不是数字时会 panic,而不是返回 `0`。**只有 `as` 类型转换**遇到非法字符串会回退到 `0`(见 README.zh.md 的 `"123" as i32` 说明)。
- 算术 `Any::binary(a, op, b)` 在 `a / b` 类型不可加(如 `bool + Map`)时立即报错,不做"宽松转换"。

### 模块目录速查

```rust
// 全局
print(x)  log(x)  sqrt(f)  uuid()  rand(min, max)
import("name", "path.zs")        // 顶层 加载模块
spawn(target, args_tuple)        // 起 OS 线程

// 时间
time::now()                                   // 当前 unix 毫秒
time::format("%Y-%m-%d %H:%M:%S", tick)        // -> "2026-06-19 12:00:00"
time::parse("%Y-%m-%d %H:%M:%S", text)         // -> i64,失败 -1

// 容器
v.len()  v.push(x)  v.pop()  v.get_idx(i)  v.set_idx(i, x)
v.keys()  v.get(k)  v.set(k, x)  v.contains(x)
v.iter()  it.next()

// root
root::add(p, v)        root::get(p)         root::insert(p, k, v)
root::update(p, f)     root::update_key(p, k, f)        // 原子 RMW
root::send(p, msg)

// db
db::create(name, schema)  db::select(name, q, args)  db::exec(name, sql, args)

// http
http::get(url)  http::post(url, body)  http::serve(cfg)

// llm
llm::complete(model, prompt)  llm::tts(model, text)

// gpu
gpu::spirv_compile(cfg)  gpu::vulkan_run(cfg)
```

### 完整最小例子

```rust
// hello.zs
import("util");

pub struct Point { x: f32, y: f32 }

impl Point {
    pub fn dot(self: Point, other: Point) -> f32 {
        self.x * other.x + self.y * other.y
    }
}

pub fn main() {
    let p = Point { x: 1.0f32, y: 2.0f32 };
    let q = Point { x: 3.0f32, y: 4.0f32 };
    print("dot = " + p.dot(q));     // dot = 11

    // 高阶 + 闭包
    let scale = 10i32;
    let scaled = [1i32, 2i32, 3i32];
    for i in 0..scaled.len() {
        scaled.set_idx(i, scaled.get_idx(i) * scale);
    }
    print(scaled);

    // root 原子更新
    root::add("local/state/count", 0i32);
    let next = root::update("local/state/count", |v| { v + 1 });
    print("count = " + next);                  // count = 1

    root::add_map("local/state/users");
    root::insert("local/state/users", "alice", 100i32);
    root::update_key("local/state/users", "alice", |v| { v + v });
    print(root::get_key("local/state/users", "alice"));    // 200
}
```

### 常见坑

1. **数字字面量后缀**:无后缀整数 = `i32`,无后缀浮点 = `f32`。如果目标位置是 `f64` / `[f64; N]` / `Vec<f64>` 等更宽类型,编译器会自动按目标元素类型 `force` 转换(数组字面量、`const` 数组、被显式标注的 `let` 都已覆盖,见 [compiler/src/lib.rs](../compiler/src/lib.rs) `eval` 的 Typed+Array/Vec 分支以及对应 GPU 后端的回退路径);极少数情况下仍建议加显式后缀以让阅读者一眼看出目标类型。
2. **没有 `mut`**:所有变量都可重赋值,不需要也写不出 `let mut`。
3. **没有 trait / impl Trait for X**:`impl` 后面只能接具体 struct;要"对任意类型通用"用 `Any` 形参,运行时再分发。
4. **没有 `&` / 借用**:Map / List / Struct 跨函数传时是引用语义共享底层 buffer;真要拷贝写 `v.clone()`(深拷贝)。
5. **`fn` 体内不能再开 `struct/impl/const`**:嵌套结构请提到顶层。
6. **泛型实参在调用点要用 `::<T>`**:`Boxed::<i32>{...}` / `f::<i32>(...)` 这种 turbofish。
7. **kernel 子集**:GPU 入口里不能出现 `Map / List / String / 闭包`,只允许 native struct / 数组 / 数值。
8. **`zusts` 这个 binary 是 demo**,不是用户级 CLI;真正的运行时入口是 `vm::Vm`。
9. **`root::update` 的闭包是 zust 闭包,不是 Rust 闭包**:它在 native 侧通过 `Dynamic::Custom(ZustCallback)` 跨边界,所以可以自由捕获 zust 上下文里的变量,但**不要在闭包里再调 `root::update` 同一个 path**(会自旋等同一把 scc 桶锁,等价于死锁)。
10. **`break value` / `loop` 当表达式 / `{}` 当表达式都不行**:`break;` 是纯控制流,要返回值用变量或 `(\|\|{ ... })()` IIFE。
11. **空 tuple `()` 是合法值**,但**没有** `unit type` 类型别名 —— 函数无返回时返回 `Type::Void`,在 zust 里看到的是 `null`。
12. **`for in str`** 不会按字符迭代:zust 字符串是 `SmolStr` 包装,要逐字符走用 `while + s.get_idx(i)`。
