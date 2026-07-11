# Zust

Zust 是一个用 Rust 编写的类 Rust 脚本语言和运行时。它保留了 Rust 语法中清晰、结构化的部分，同时去掉借用检查和显式可变性约束，让脚本更短、更动态，也更适合由工具或模型生成、改写。

官方网站：[www.zust-lang.com](https://www.zust-lang.com)

项目已经接近成熟的开源版本。当前 workspace 内各 crate 独立发版：VM crate 为 `0.9.106`，root crate 为 `0.9.25`，dynamic crate 为 `0.9.25`，compiler 为 `0.9.47`，parser 为 `0.9.26`，SPIR-V 后端为 `0.9.12`，Metal 后端为 `0.9.14`，编辑器相关包为 `0.9.2`。

English: [README.md](README.md)

## 最近运行时工作

VM 托管临时内存的工作已经完成。VM 创建的 `Any`/`Dynamic` 值和生成结构体存储现在统一走 VM 内存管理器，不再散落 raw heap ownership。每个执行线程都有自己的 thread-local arena 和函数 scope；非返回临时值会在 scope 退出时释放，返回值在逃逸给 Rust 调用方或 ROOT 前会被 promote。

长周期测试显示，第一次 arena 扩展之后内存会保持稳定。RSS 可能停留在 allocator 高水位，尤其是线程池中每个 worker 都有自己的 arena，但重复执行 VM 函数不会出现持续的 `Dynamic` 增长。这一模型可以用于长期运行的服务器进程。

当前模型仍然是 arena-based 临时 owner，不是 tracing GC。需要跨调用长期存在的值，应以 owned `Dynamic` map、list、primitive、bytes、custom object 或 ROOT 值的形式跨边界。不要把临时 VM 存储中的生成结构体裸地址持久写入长期容器。

最近的 compiler/runtime 修复还包括：

- 顶层 `const` composite literal 可以引用前面声明的 const/static 值，例如 `const GEM_TABLE = [{ key: GEM_ATK }]` 会在编译期折叠。
- 函数返回类型推断会把非泛型函数的推断返回类型写回函数符号表。
- 嵌套结构体参数返回结构体时，调用点可以正常做静态字段访问。
- `std::log(value)` 会用 Rust log 以 debug 格式记录动态值。
- VM 内部内存和结构体 helper 由运行时直接注册，不再暴露到脚本符号表。
- 嵌套闭包捕获正确解析——闭包内部定义的闭包能正确捕获外层变量。
- 支持字符串到数字的类型转换：`"123" as i32`、`"3.14" as f64`。
- parser 在同一 scope 内拒绝重复声明。
- dict shorthand 字段：`{name}` 等价于 `{name: name}`。
- `std::sqrt(value)` 计算 `f64` 的平方根。
- `std::sleep(ms)` 阻塞当前执行线程指定毫秒数。
- `std::env(name)` 读取进程环境变量；变量不存在或不是合法 unicode 时返回 `null`。
- `std::spawn(target, args)` 启动独立 OS 线程；回调闭包支持最多 24 个参数。

## 设计思路

Zust 的设计围绕几个现实目标展开：

- **Rust 形状，脚本体验**：函数、结构体、`impl`、range、代码块、类型字面量都接近 Rust，但变量可以自由赋值。
- **动态值作为边界模型**：`dynamic` crate 提供统一的 `Dynamic` 值，覆盖 list、map、struct、bytes、typed vector、JSON、MessagePack 等场景。
- **可选的静态结构**：脚本可以先以动态方式书写，在需要 JIT、GPU 或明确 ABI 时再加入类型标注。
- **脚本编译成本地函数**：`vm` crate 使用 Cranelift 编译 Zust 模块，并把函数指针暴露给宿主 Rust 代码调用。
- **同一语言，多种执行目标**：仓库包含 JIT 后端、SPIR-V 生成、Metal 源码生成和 Vulkan 执行辅助。
- **面向 AI，但不绑定应用**：`llm` crate 保留通用模型调用能力；具体应用服务器代码不在本次开源范围内。

## 当前语言状态

仓库内的语法套件已经覆盖了 parser、compiler 和 VM 目前实现的核心语言面：

- 行注释、块注释、转义字符串、原始字符串，以及十进制、十六进制、八进制、二进制数字字面量。
- 基本类型：`bool`、`string`、8 到 64 位有符号/无符号整数、`f16`、`f32`、`f64`、tuple、动态 list/map、固定数组和面向 GPU 的向量类型。
- `let` 绑定支持标识符、tuple、list、通配符和带类型标注的模式。
- `const`、`static`、公开项、函数、泛型函数、结构体、泛型结构体、`impl`、方法和关联调用。
- 代码块、表达式形式的 `if`/`else`、`for`、`while`、`loop`、`break`、`continue` 和 `return`。
- 带类型参数并能捕获外部值的闭包，包括嵌套闭包捕获。
- 算术、比较、逻辑、位运算、索引、range、类型转换、赋值和复合赋值表达式。
- 字符串到数字的类型转换：`"123" as i32`、`"3.14" as f64`。
- dict shorthand 字段：`{name}` 等价于 `{name: name}`。
- 空 tuple `()` 作为合法表达式。
- 跨 `.zs` 文件导入；单参数 `import` 会默认补全 `.zs` 后缀。
- 同一 scope 内拒绝重复声明。

Zust 的目标不是完整兼容 Rust，而是保留 Rust 的结构感并服务脚本场景：没有借用检查，变量不需要显式 `mut`，宿主模块边界默认使用动态值。

## 语言说明

Zust 源文件使用 `.zs` 后缀。

```zust
fn add(a: i64, b: i64) {
    a + b
}

pub fn main() {
    let value = add(40, 2);
    print(value);
}
```

函数在没有尾随分号时会返回代码块最后一个表达式。需要提前退出时，可以使用 `return;` 或 `return value;`。

### 基本值

```zust
let i = 42;
let f = 3.14f32;
let ok = true;
let text = "hello";
let raw = r#"hello "Zust""#;
let nothing = null;

let list = [1, 2, 3];
let object = {name: "Zust", version: 0.9};
let pair = (1i32, 2i32);
let empty = ();
let repeated: [u32; 3] = [0u32; 1 + 2];

// dict shorthand: {name} 等价于 {name: name}
let version = 0.9;
let short = {name, version};
```

数字字面量可以带显式后缀，例如 `1i32`、`8u64`、`3.14f32`；整数字面量支持 `0x`、`0o` 和 `0b` 前缀。浮点数字面量支持科学计数法：`1e-3f32`、`1.797e308f64`。

字符串拼接会在运行时使用动态字符串转换，因此支持 `"" + idx`、`"" + level + "级"`、`"" + map.value` 这类写法。原始字符串允许内嵌引号和反斜杠：`r#"hello "Zust""#` 或 `r##"内含 "# 的文本"##`。

字符串到数字的类型转换通过 `as` 完成：`"123" as i32` 得到 `123`，`"3.14" as f64` 得到 `3.14`。无效数字转为 `0`。

### 常量和静态值

```zust
pub const ANSWER: i32 = 42i32;
pub static DEFAULT_LIMIT: u32 = 1024u32;

pub const GEM_ATK = "atk";
pub const GEM_TABLE = [
    {key: GEM_ATK, score: 3i32},
];
```

顶层 `const` composite literal 可以引用同一模块或已导入模块中更早声明的常量和静态值。

### 模式和修改

```zust
let (left, right) = (3i32, 4i32);
let [first, second] = [5i32, 6i32];
let _ = first;

let data = {
    label: "point",
    items: [1i32, 2i32, 3i32],
};

data.items.push(4i32);
data.items[0] = data.items[1] + 10i32;
data.extra = second;
```

变量和字段可以直接重新赋值。语言也支持 `+=`、`-=`、`*=`、`/=`、`%=`、`&=`、`|=`、`^=`、`<<=`、`>>=` 等复合赋值运算符。

多重赋值（不带 `let` 的解构）也支持 tuple 和 list：

```zust
let a = 1i32;
let b = 2i32;
(a, b) = (b, a);   // 交换值

let arr = [10i32, 20i32, 30i32];
[arr[0], arr[2]] = [arr[2], arr[0]];  // 通过索引交换
```

### 控制流

```zust
for i in 0..10 {
    if i % 2 == 0 {
        continue;
    }
    print(i);
}

// for 可以直接迭代动态 list 和 map 的值：
for item in [1, 2, 3] {
    print(item);
}
for value in {"a": 1, "b": 2} {
    print(value);    // 打印值：1, 2
}

let label = if list.len() > 0 { "non-empty" } else { "empty" };

let value = 0i32;
while value < 100 {
    value += 1;
}

loop {
    break;
}
```

`for in` 在动态 list/map 上直接迭代值；迭代 map 的 key 请用 `.keys()`：

```zust
let map = {"a": 1, "b": 2};
for key in map.keys() {
    print(key);   // 打印键："a", "b"
}
```

**`for in` 不迭代字符串**（不会按字符遍历）。

### 语言限制

Zust 有意不实现 Rust 的全部语法特性，以下是已知的设计差异和当前限制：

| 特性 | Zust 行为 | 原因 |
|------|----------|------|
| `break value` | 不支持，只能 `break;` | `break` 是纯控制流语句 |
| `loop` 作为表达式 | 不支持 | 同上，用变量赋值替代 |
| 代码块 `{...}` 作为表达式 | 直接写 `let y = { ... }` 报错 | 用 `\|\|{...}()` 即调闭包替代 |
| `struct`/`impl`/`const` 在函数内 | 不支持，只能顶层定义 | 编译器不支持局部类型定义 |
| 嵌套函数 (`fn` 在 `fn` 内) | 部分场景触发编译器崩溃 | 提取到顶层或用闭包 |
| 整数溢出 | panic（不回绕） | 安全策略，类似 Rust debug |
| `!` 对 float/Any | 不支持 | 仅 bool（逻辑取反）和 int/uint（按位取反） |
| `for ch in "hello"` | 不迭代字符 | 用 `while` + `get_idx` 替代 |

### 结构体和 impl

```zust
struct Point {
    x: f64,
    y: f64,
}

impl Point {
    pub fn len2(self: Point) {
        self.x * self.x + self.y * self.y
    }
}

pub fn demo() {
    let p = Point{x: 3.0, y: 4.0};
    p.len2() == Point::len2(p)
}
```

### 带接收者类型提示的方法调用

当编译器能确定接收者类型时，普通方法调用就可以工作：

```zust
let navmap = NavMap::new("map.png", "grid.png");
let path = navmap.get_path(start_x, start_y, stop_x, stop_y, false);
```

如果值来自动态边界，例如 `root::get`、handler 参数，或者 native `Any`/custom 值，编译期可能只知道它是 `Any`。这种情况下，可以在方法名前加接收者类型提示：

```zust
let navmap = root::get("local/world/newbie_village/navmap");
let path = navmap::<NavMap>::get_path(start_x, start_y, stop_x, stop_y, false);
```

这里的 `::<NavMap>::` 只是在告诉编译器去哪里查找 native 方法，不会转换、克隆或修改底层的 `Dynamic` 值。通过 `Dynamic` 携带的 native/custom 对象只适合在本地 VM 进程内使用；JSON 和 MessagePack 不能持久化它们持有的 Rust 进程内状态。

### 泛型和闭包

```zust
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

fn const_value<N>() {
    N
}

pub fn demo_closure() {
    let base = 10i32;
    let add_base = |value: i32| {
        value + base
    };

    add_base(identity(5i32))
}

// 嵌套闭包正确捕获外层变量：
pub fn demo_nested_closure() {
    let label = "test";
    |path: string| {
        let done = |ok: bool| {
            if ok { label + ":" + path } else { "missing" }
        };
        done(true)
    }("file.png")
}

pub fn demo_const_generic() {
    const_value::<4>()
}

// 闭包可以立即调用：
pub fn immediate_closure() {
    let r = || { 1i32 + 2i32 }();
    r
}
```

### 导入模块

```zust
import("qsort", "qsort.zs");
import("syntax_imported");
```

当调用侧省略导入路径时，编译器默认使用 `.zs` 后缀。

### 编译期泛型参数

Zust 支持编译期类型参数，可用于固定大小数组和高精度数值类型：

```zust
pub struct BigFloat<N> {
    sign: bool,
    exp: i32,
    data: [u32; N],
}
```

可以参考 [zusts/bigfloat.zs](zusts/bigfloat.zs) 以及 [zusts/gpu](zusts/gpu) 下的 Mandelbrot 示例。

## 运行时 Native 模块

`zust-vm` 默认不启用扩展 feature。`Vm::new()` 注册 core 能力：VM 内存运行时、`std`、`Any`、`Vec` 和 `root`。`Vm::with_all()` 会注册当前编译进来的全部能力；启用 `full` 时会包含 `http`、`db`、`llm`、`candle` 和 `gpu`。`oss` 跟随 `llm` feature 注册；`candle` 注册本地 Candle 模型执行入口；`http::upload` 只有同时启用 `http` 和 `llm` 时可用。`vulkan` 和 `metal` 是 GPU 运行后端 feature，分别建立在 `gpu` 之上。

下面这些 native 模块和辅助类型都以 `Dynamic` 作为主要边界，因此 Zust 脚本可以直接传 map、list、字符串、数字和 bytes。

### 标准函数

标准函数不需要模块前缀：

- `print(value)`：打印动态值。
- `log(value)`：用 Rust log 以 debug 格式记录动态值。
- `sqrt(value)`：返回 `f64` 的平方根。
- `uuid()`：返回 UUID 字符串。
- `rand(start, stop)`：返回 `start` 到 `stop` 之间的随机整数或浮点数。
- `import(module, path)`：导入另一个 `.zs` 文件，或导入存放在 `root` 中的源码对象。
- `spawn(target, args)`：启动独立 OS 线程并调用 `target`。`target` 可以是函数名字符串或无捕获的闭包。`args` 始终是 tuple 参数包：`spawn("job::run", ())` 无参调用，`spawn(|x, y| { print(x + y); }, (10, 20))` 带两个参数调用。spawn 函数通过动态 `Any` 边界接收参数（最多 16 个），返回值被忽略。

```zust
print({ok: true});
let id = uuid();
let n = rand(1, 100);
import("world", "scripts/world.zs");

spawn("job::run", ());
spawn(|x, y| {
    root::add("local/result", x + y);
}, (10, 20));
```

### Native 回调

当闭包通过 `Any` 参数传递给 native 函数时，VM 会将其封装为 `Dynamic::Custom(ZustCallback)`。Rust native 代码可以 downcast 并保存它，之后通过 `callback.call0()`、`callback.call1(value)` 或 `callback.call(vec![...])` 调用。回调闭包最多支持 24 个显式动态参数，例如 `button.on_pressed(|| { label.set_text("clicked!"); })` 和 `dialog.on_file_selected(|path| { label.set_text(path); })`。捕获变量会 deep clone 为动态值，因此标量和 native custom 对象在原 Zust 调用返回后仍可使用。

### Any 动态值方法

动态值暴露这些成员方法和静态转换函数：

- 构造和类型辅助：`Any::null()`、`is_map()`、`is_list()`、`is_string()`、`is_null()`、`clone()`。
- 长度和转换辅助：`len()`、`keys()`、`to_string()`、`Any::from_i64(value)`、`Any::to_i64(value)`、`Any::from_bool(value)`、`Any::to_bool(value)`、`Any::from_f64(value)`、`Any::to_f64(value)`。
- 序列化辅助：`to_yaml()`、`from_yaml()`，详见下方的 [序列化](#序列化) 一节。
- List 和字符串辅助：`push(value)`、`pop()`、`split(sep)`、`slice(start, stop, inclusive)`。
- Map 和索引辅助：`get_idx(idx)`、`set_idx(idx, value)`、`get(key)`、`get_key(key)`、`set_key(key, value)`、`del_key(key)`、`contains(value)`、`starts_with(prefix)`。
- 迭代辅助：`iter()`、`next()`。
- 动态表达式辅助：`Any::binary(left, op, right)`、`Any::logic(left, op, right)`，主要由编译器生成调用。

普通脚本语法，例如 `list[idx]`、`map.key`、`value.len()` 和动态算术，都会降到这些辅助方法上。

```zust
let data = {name: "zust", tags: ["vm", "native"]};

if data.is_map() && data.contains("name") {
    print(data.get("name"));
}

data.tags.push("script");
let count = data.tags.len();

let items = data.keys();       // 获取 map 所有键
let s = data.to_string();      // 字符串表示
let first = data.tags.get_idx(0);
```

### 序列化

动态值可以转成 YAML 字符串、再解析回来 —— 这是 Zust 暴露在脚本层的结构化
序列化方式。输出风格针对 LLM 做了取舍：block 风格优先、字符串尽量裸出、
多行字符串用 `|` literal block scalar 表示，**不输出 anchor / alias**（这两
个东西语言模型经常搞错）。

- `value.to_yaml()`：把 `Dynamic` 渲染成 YAML 字符串。
- `text.from_yaml()`：把 YAML 字符串解析成 `Dynamic`，解析失败时返回 `null`。

YAML 1.2 的标量规则：整数、浮点（`3.14`、`-1.5e3`）、布尔（`true` /
`false`）、`null` 都能识别。形如数字或保留字（`"123"`、`"yes"`、`"true"`
等）的字符串会自动加引号，round-trip 时仍是字符串而不是被误读成数值。
支持 block mapping（`key: value`）、block sequence（`- item`）以及
compact block sequence（`- name: alice` 后跟 `  age: 30` 续行）。注释
（`# ...`）和空行在解析时会被忽略。

JSON、MessagePack、Markdown 也在 `dynamic` crate 里有对应的 Rust trait，
方便原生代码使用；脚本层目前只暴露了 YAML。

```zust
let data = {
    user: {name: "alice", age: 30},
    tags: ["rust", "zust"],
    active: true,
    id: "123"          // 注意是字符串,不是整数 —— 加引号后保留
};

let yaml = data.to_yaml();
print(yaml);
// active: true
// tags:
//   - rust
//   - zust
// user:
//   age: 30
//   name: alice
// id: "123"

let back = yaml.from_yaml();
assert(back.get("id").is_string());   // round-trip 后仍是字符串
assert(back.get("user").get("age") == 30);
```

### Vec 辅助类型

`Vec` 是 VM 底层向量辅助类型，主要用于编译后的代码和 GPU 数据路径：

- `Vec::swap(vec, i, j)`：交换两个 `i32` 槽位。
- `Vec::get_idx(vec, idx)`：读取一个 `i32` 槽位。

### root

`root` 是运行期对象树。默认 `local` mount 是内存；也支持 Redis mount 和本地 Fjall-backed `fjall` mount。

```zust
root::add("local/user/1", {name: "Zust", points: 10});
let user = root::get("local/user/1");

root::add_list("local/events");
root::push("local/events", {kind: "login"});

root::add_map("local/users");
root::insert("local/users", "alice", {age: 20});
```

`root` 节点里的 List / Map 是对象树节点,成员修改必须通过 `root` API 原地写到节点上。`root::get(path)` 返回的是当前值的副本/快照;对这个返回值调用 `push`、`pop`、`set_key` 等只会修改副本,不会写回 ROOT。

```zust
root::add_list("local/events");
root::push("local/events", {kind: "login"});     // 正确: 修改 ROOT 节点

let events = root::get("local/events");
events.push({kind: "logout"});                   // 错误: 只改 events 副本
```

函数：

- `root::mount(name, url)`：挂载 Redis-backed root path。
- `root::mount_fjall(name, data_dir)`：把本地 Fjall 存储挂载到指定 ROOT 路径名 `name`。
- `root::mount_dir(name, host_dir)`：把本地文件系统目录挂载到指定 ROOT 路径名 `name`。
- `root::add(path, value)`、`root::get(path)`、`root::remove(path)`、`root::contains(path)`。
- `root::dir(path, all)`：`all` 为 `false` 时列出下一级子项名，为 `true` 时递归列出相对 `path` 的全部子项目路径。`root::len(path)`、`root::keys(path)`。
- `root::add_list(path)`、`root::push(path, value)`、`root::get_idx(path, idx)`、`root::remove_idx(path, idx)`。
- `root::add_map(path)`、`root::insert(path, key, value)`、`root::get_key(path, key)`、`root::remove_key(path, key)`。
- `root::send(path, value)`、`root::send_idx(path, idx, value)`：向 native handler 或 Zust handler 发送消息。
- `root::add_fn(path, fn_name)`：把已编译的 Zust 函数注册成 ROOT handler。

### http

`http` 提供动态 HTTP client、Zust 分发的 HTTP server，以及直接 OSS 上传入口。

#### HTTP 客户端

```zust
let page = http::get("https://example.com");

let response = http::request({
    method: "POST",
    url: "https://api.example.com/items",
    json: {name: "zust"},
    headers: {"x-client": "zust"}
});
```

常用函数：

- `http::get(url)`。
- `http::post(url, body)`。
- `http::request(options)`。
- `http::upload(config, object_name, bytes)`：用显式传入的 OSS 配置 map 把 `Vec<u8>` bytes 上传到 OSS，返回 `{ok, object_name, oss_url, url}`。

响应是 map，包含 `status`、`ok`、`url`、`@headers` 和 `body`。JSON 响应会解码成 `Dynamic`，文本和二进制分别返回字符串或 bytes。

#### HTTP Server 和 WebSocket

```zust
root::add_fn("local/http/post/echo", "app::echo");

pub fn echo(req) {
    {
        ok: true,
        method: req["@method"],
        path: req["@path"],
        query: req["@query"],
        body: req,
    }
}

pub fn start() {
    http::serve({
        host: "0.0.0.0:8080",
        ws: "/ws",
        upload: "/upload",
        static: "public",
    })
}
```

`http::serve(config)` 启动 HTTP server。`config.host` 是 `"host:port"` 地址字符串。`config.ws` 是 WebSocket 路径或路径 list。`config.upload` 是 multipart 上传路径或路径 list，不需要上传时传空字符串 `""` 或不传。`config.static` 是一个静态目录，或静态目录 list。

HTTP API 按 ROOT 分发：`/api/foo` 会映射到 `local/http/{method}/foo`，其中 `method` 为小写。

请求 payload 会包含 `@method`、`@path`、`@header`、可选 `@query`，以及成功解析的 JSON body 字段。handler 默认返回 JSON；可以用 `@status`、`@content-type` 和 `@body` 控制状态码、content type 和原始响应 body。

当 `config.upload` 设置后，server 会在这些路径接收 multipart `POST`，按 boundary 纯字节扫描解析，不把文件内容转成字符串。解析结果直接按 `{name: bytes}` 合并到 payload，然后分发到 `local/http/upload`。

```zust
root::add_fn("local/http/upload", "app::upload");

pub fn upload(req) {
    let oss = root::get("local/oss");
    let saved = http::upload(oss, "uploads/input.bin", req.file);
    {
        ok: saved.ok,
        url: saved.url,
    }
}
```

`static` 可以是挂载到 `/` 的目录字符串，也可以是目录 list，或显式配置挂载路径：

```zust
http::serve({
    host: "0.0.0.0:8080",
    ws: ["/ws", "/socket"],
    upload: ["/upload", "/file"],
    static: [
        "public",
        {path: "/assets", dir: "assets"},
    ],
});
```

当 `ws` 设置后，连接会挂载到 `local/ws` 下；可选 handler 路径为 `local/ws_handlers/auth`、`connect`、`message` 和 `disconnect`。二进制 WebSocket 消息按 MessagePack 解码；文本消息优先按 JSON 解码，否则作为字符串。

```zust
root::add_fn("local/ws_handlers/message", "app::ws_message");

pub fn ws_message(req) {
    let idx = req.idx;
    root::send_idx("local/ws", idx, {
        idx: idx,
        type: "echo",
        message: req.message,
    });
}
```

### llm

`llm` 封装文本、图片、语音识别和 TTS 请求。第一个参数是模型配置，`url`（endpoint）和 `model`（模型名）都是必备字段，`zust-llm` 不会从模型名前缀推断 endpoint，每个配置都必须显式指向你实际要打的 provider URL。

```zust
let model = {
    url: "https://api.deepseek.com",
    model: "deepseek-chat",
    key: root::get("local/keys/deepseek"),
};

let answer = llm::complete(model, "用一句话介绍 Zust。");

let vision = llm::complete(model, {
    text: "这张图里有什么？",
    image: "https://example.com/image.png",
});

let task_id = llm::deep(model, {prompt: "写一份长报告"}, |result| {
    root::add("local/llm/report", result);
});
```

可灵官方 API 的凭证是 Access Key 和 Secret Key。配置里传 `access_key` 和 `secret_key`，`zust-llm` 会为创建任务和轮询任务生成可灵要求的 HS256 JWT Bearer token。

```zust
let kling = {
    kind: "kling_image_generation",
    url: "https://api-singapore.klingai.com",
    access_key: root::get("local/keys/kling/access_key"),
    secret_key: root::get("local/keys/kling/secret_key"),
};

let image = llm::image(kling, {
    prompt: "清晨的安静山村",
    model_name: "kling-v2-1",
}, |result| {
    root::add("local/llm/image", result);
});
```

常用函数：

- `llm::complete(model, value)`：文本、图片 URL、视频 URL 等多模态输入，返回 `Dynamic`。
- `llm::image(model, value, callback)`：图片生成或编辑，返回本地任务 id，完成后结果会传给 callback closure。
- `llm::audio(model, value)`：语音识别，输入可用 URL 或 bytes，输出文字。
- `llm::tts(model, value)`：输入文字或 `{text/input: ...}`，输出音频 bytes 或音频 URL。
- `llm::deep(model, value, callback)`：启动异步补全任务，完成后结果会传给 callback closure。

大体积二进制输入应优先上传到对象存储，再把 URL 传给支持 URL 输入的模型，避免把大 payload 塞进 LLM 请求体。

### candle

`candle` 在 Rust 进程内用 Candle 执行小规模本地模型。它不下载模型文件；调用时显式传入 tokenizer、config 和 safetensors 权重的本地路径。embedding 入口当前支持 BERT-compatible 模型，以及 KaLM-Embedding-V2.5 这类 Qwen2 embedding 模型。

```zust
let embedder = candle::load_embedder({
    model: "models/all-MiniLM-L6-v2/model.safetensors",
    tokenizer: "models/all-MiniLM-L6-v2/tokenizer.json",
    config: "models/all-MiniLM-L6-v2/config.json",
    max_len: 256,
    normalize: true,
});

let result = candle::embed(embedder, ["hello Zust", "local embeddings"]);
```

函数：

- `candle::load_embedder(options)`：加载本地 BERT-compatible embedding 模型，返回可复用的 native embedder 对象。
- `candle::embed(embedder, input)`：执行已加载的 embedder。`input` 是字符串或字符串列表。成功返回 `{ok, model, count, dim, embeddings}`，失败返回 `{ok: false, error}`。
- `candle::embed(options, input)`：一次性调用形式，在同一次调用中从本地路径加载并执行 embedding。

KaLM-Embedding-V2.5 可以先下载到本地，再直接指向这些文件：

```zust
let kalm = candle::load_embedder({
    model: "models/KaLM-embedding-multilingual-mini-instruct-v2.5/model.safetensors",
    tokenizer: "models/KaLM-embedding-multilingual-mini-instruct-v2.5/tokenizer.json",
    config: "models/KaLM-embedding-multilingual-mini-instruct-v2.5/config.json",
    max_len: 512,
    output_dim: 896,
    normalize: true,
});

let query = "Instruct: Given a query, retrieve documents that answer the query\nQuery: What is Zust?";
let result = candle::embed(kalm, [query, "Zust is a Rust-like scripting language."]);
```

### oss

`oss` 模块提供直接的 Aliyun OSS 上传入口，适合 LLM 工作流里的图片、音频、视频和其他大文件。

先把配置写入 ROOT，然后在调用 OSS 函数时显式传入配置值：

```zust
root::add("local/oss", {
    access_id: "...",
    access_key: "...",
    region: "cn-hangzhou",
    bucket: "my-bucket",
});
```

直接上传 `Vec<u8>` bytes：

```zust
let oss = root::get("local/oss");
let uploaded = oss::upload(oss, "llm/input/audio.wav", audio_bytes);

let audio_text = llm::audio(model, {
    url: uploaded.url,
});
```

`http::upload(config, object_name, bytes)` 是 HTTP 模块里暴露的同一个直接上传入口，适合已经在服务端 HTTP 流程里使用时调用。

`oss::signed_url(config, input)` 为已有 `oss:://...` 对象 URL 生成临时 HTTP 访问 URL。可以直接传对象 URL，也可以传 `{oss_url, expires}` 设置过期时间。

```zust
let url = oss::signed_url(oss, uploaded.oss_url);
let longer = oss::signed_url(oss, {oss_url: uploaded.oss_url, expires: 3600});
```

函数：

- `oss::upload(config, object_name, bytes)`：使用显式传入的 OSS 配置 map 上传 `Vec<u8>` bytes，返回 `{ok, object_name, oss_url, url}`。
- `oss::signed_url(config, input)`：把 `oss:://...` 转成临时 HTTP URL，默认 600 秒。

### db

`db` 使用 `sqlx::AnyPool`，当前支持 PostgreSQL 和 MySQL。连接 URL 存在 `root` 中，通常放在 `local/db`。

```zust
root::add("local/db", {
    url: "mysql://user:pass@127.0.0.1:3306/app",
    max_connections: 10,
});
```

解析数据库路径时，`db` 会先检查完整路径。如果完整路径存的是连接 URL，这个路径就是连接名；否则它会向上查找连接配置，剩下的路径后缀作为表名。例如 `local/db/user` 使用 `local/db` 的连接，表名是 `user`。

函数：

- `db::create(path, fields)`：创建表，`path` 通常是表路径，例如 `local/db/user`。
- `db::drop(path)`：删除表。
- `db::select(path, sql, data)`：查询 SQL，返回 `List<Map>`。
- `db::exec(path, sql, data)`：执行写入 SQL，返回影响行数，失败返回 `-1`。
- `db::transaction(path, steps)`：在一个事务里执行多条 SQL，返回总影响行数，失败回滚并返回 `-1`。

创建和删除表：

```zust
db::create("local/db/user", {
    id: "BIGINT PRIMARY KEY",
    name: "VARCHAR(64)",
    email: "VARCHAR(128)",

    "@indexes": [
        ["name"],
        {name: "uniq_user_email", columns: ["email"], unique: true}
    ]
});

db::drop("local/db/user");
```

查询和写入：

```zust
let rows = db::select(
    "local/db",
    "select id, name from user where id = :id",
    {id: 1}
);

let changed = db::exec(
    "local/db",
    "update user set name = ? where id = ?",
    ["new-name", 1]
);
```

绑定规则：

- `data` 是 map 时绑定 `:id` 这样的命名参数。
- `data` 是 list 时绑定顺序 `?` 参数。
- PostgreSQL 占位符会重写为 `$1`、`$2`。
- MySQL 使用 `?` 占位符。

事务使用 `[sql, data]` step 列表，每个 step 的绑定规则和 `db::exec` 一样：

```zust
let changed = db::transaction("local/db", [
    ["insert into user (id, name) values (:id, :name)", {id: 1, name: "zhu"}],
    ["update user set name = ? where id = ?", ["zust", 1]]
]);
```

### gpu

`gpu` 是 `Vm::with_all()` 注册的 GPU-facing 模块。它把后端入口拆成编译、检查和运行：

- `gpu::spirv_compile(options)`：把 Zust shader 编译成 SPIR-V，返回 words、bytes、disassembly 等信息。
- `gpu::spirv_check(options)`：只检查 SPIR-V 编译，不返回完整模块。
- `gpu::metal_compile(options)`：在 macOS 上把 Zust shader 编译成 Metal source。
- `gpu::metal_check(options)`：只检查 Metal 编译。
- `gpu::vulkan_run(options)`：加载 SPIR-V、绑定 buffer、dispatch Vulkan，并返回 readback。
- `gpu::metal_run(options)`：加载 Metal source 或从 Zust 编译后 dispatch Metal，并返回 readback。

编译 shader 不需要 VM 的执行后端。只有调用 `gpu::vulkan_run` 时才需要打开 `zust-vm` 的 `vulkan` feature；只有调用 `gpu::metal_run` 时才需要打开 `metal` feature。

`options` 是普通动态 map，常用字段包括 `source` 或 `path`、`module`、`fn`、`workgroup_size`、`groups` 和 `args`。运行参数支持标量输入、typed vector buffer，以及用于结构体 ABI 参数的原始 `bytes` buffer。

```zust
let checked = gpu::spirv_check({
    path: "zusts/gpu/mandelbrot.zs",
    module: "mandelbrot",
    fn: "main",
    workgroup_size: [8, 8, 1],
});

let compiled = gpu::spirv_compile({
    source: "pub fn main() { ... }",
    module: "kernel",
    fn: "main",
    workgroup_size: [16, 16, 1],
    generic_args: [32u32],
});
```

## 最小 VM 示例

`Vm` 是 JIT 运行时的薄封装。`jit` 字段是 `pub` 的，宿主代码直接访问编译器和 JIT：

1. 把 Zust 源码导入 VM。
2. 向 JIT 请求已编译的函数指针。
3. 将函数指针转换成 `extern "C"` 函数并调用。

```rust
use anyhow::Result;
use dynamic::Type;

fn main() -> Result<()> {
    let vm = vm::Vm::with_all()?;

    vm.jit.write().unwrap().import_code(
        "demo",
        br#"
        pub fn add(a: i64, b: i64) {
            a + b
        }
        "#
        .to_vec(),
    )?;

    let (ptr, ret) = vm.jit.write().unwrap().get_fn_ptr("demo::add", &[Type::I64, Type::I64])?;
    assert_eq!(ret, Type::I64);

    let add: extern "C" fn(i64, i64) -> i64 = unsafe { std::mem::transmute(ptr) };
    println!("40 + 2 = {}", add(40, 2));

    Ok(())
}
```

运行仓库中的最小示例：

```bash
cargo run -p vm --example minimal_vm
```

## 目录结构

```text
zust/
├── dynamic/       运行时动态值模型、JSON、MessagePack、typed vector
├── parser/        手写词法分析和递归下降解析器
├── compiler/      AST 到 IR 的编译、符号表、类型推断
├── vm/            Cranelift JIT 后端和 VM 宿主 API
├── vm-spirv/      SPIR-V 代码生成后端
├── vm-metal/      Metal shader 源码生成后端
├── vulkan/        SPIR-V kernel 的 Vulkan 执行辅助
├── root/          可寻址对象树和存储抽象
├── llm/           通用 LLM 请求辅助
├── zust-lsp/      `.zs` 文件的语言服务器
├── zed-extension/ Zed 编辑器扩展和 tree-sitter grammar 接入
└── zusts/         `.zs` 示例脚本
```

## 示例脚本

- [zusts/test.zs](zusts/test.zs)：综合语言冒烟示例
- [zusts/qsort.zs](zusts/qsort.zs)：typed vector 上的快速排序
- [zusts/bigfloat.zs](zusts/bigfloat.zs)：任意精度浮点实现
- [zusts/gpu/bitonic.zs](zusts/gpu/bitonic.zs)：GPU 双调排序
- [zusts/gpu/pathfind.zs](zusts/gpu/pathfind.zs)：GPU 路径查找示例
- [zusts/gpu/mandelbrot.zs](zusts/gpu/mandelbrot.zs)：Mandelbrot kernel

## 构建和检查

```bash
cargo check --workspace
cargo run -p vm --example minimal_vm
cargo run -p zusts
```

SPIR-V、Metal、Vulkan 相关示例可能需要平台 GPU 支持和驱动环境。

## 编辑器支持

仓库内包含：

- `zust-lsp`：面向 `.zs` 文件的语言服务器
- `zed-extension`：Zed dev extension
- `zed-extension/tree-sitter-zust`：tree-sitter grammar 源码

构建语言服务器：

```bash
cargo build -p zust-lsp
```

Zed 安装方式见 [zed-extension/README.md](zed-extension/README.md)。

## 许可证

见 [LICENSE](LICENSE)。
