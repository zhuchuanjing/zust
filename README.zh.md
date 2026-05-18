# Zust

Zust 是一个用 Rust 编写的类 Rust 脚本语言和运行时。它保留了 Rust 语法中清晰、结构化的部分，同时去掉借用检查和显式可变性约束，让脚本更短、更动态，也更适合由工具或模型生成、改写。

当前开源版本号为 `0.9.2`，已经接近成熟版本。

English: [README.md](README.md)

## 设计思路

Zust 的设计围绕几个现实目标展开：

- **Rust 形状，脚本体验**：函数、结构体、`impl`、range、代码块、类型字面量都接近 Rust，但变量可以自由赋值。
- **动态值作为边界模型**：`dynamic` crate 提供统一的 `Dynamic` 值，覆盖 list、map、struct、bytes、typed vector、JSON、MessagePack 等场景。
- **可选的静态结构**：脚本可以先以动态方式书写，在需要 JIT、GPU 或明确 ABI 时再加入类型标注。
- **脚本编译成本地函数**：`vm` crate 使用 Cranelift 编译 Zust 模块，并把函数指针暴露给宿主 Rust 代码调用。
- **同一语言，多种执行目标**：仓库包含 JIT 后端、SPIR-V 生成、Metal 源码生成和 Vulkan 执行辅助。
- **面向 AI，但不绑定应用**：`llm` crate 保留通用模型调用能力；具体应用服务器代码不在本次开源范围内。

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

### 基本值

```zust
let i = 42;
let f = 3.14f32;
let ok = true;
let text = "hello";
let nothing = null;

let list = [1, 2, 3];
let object = {name: "Zust", version: 0.9};
```

### 控制流

```zust
for i in 0..10 {
    if i % 2 == 0 {
        continue;
    }
    print(i);
}

let label = if list.len() > 0 { "non-empty" } else { "empty" };
```

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
    p.len2()
}
```

### 导入模块

```zust
import("qsort", "qsort.zs");
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

## 最小 VM 示例

最小宿主流程是：

1. 把 Zust 源码导入 VM。
2. 向 VM 请求已编译的函数指针。
3. 将函数指针转换成 `extern "C"` 函数并调用。

```rust
use anyhow::Result;
use dynamic::Type;

fn main() -> Result<()> {
    let vm = vm::Vm::with_all()?;

    vm.import_code(
        "demo",
        br#"
        pub fn add(a: i64, b: i64) {
            a + b
        }
        "#
        .to_vec(),
    )?;

    let compiled = vm.get_fn("demo::add", &[Type::I64, Type::I64])?;
    assert_eq!(compiled.ret_ty(), &Type::I64);

    let add: extern "C" fn(i64, i64) -> i64 = unsafe { std::mem::transmute(compiled.ptr()) };
    println!("40 + 2 = {}", add(40, 2));

    Ok(())
}
```

运行仓库中的最小示例：

```bash
cargo run -p vm --example minimal_vm
```

## GPU VM 模块

`Vm::with_all()` 会注册 `gpu` 模块，把现有 GPU 后端整理成三条入口：

- `gpu::spirv_compile(options)` / `gpu::spirv_check(options)`：编译或检查 `Zust -> SPIR-V`。
- `gpu::metal_compile(options)` / `gpu::metal_check(options)`：在 macOS 上编译或检查 `Zust -> Metal`。
- `gpu::vulkan_run(options)`：加载 SPIR-V、绑定 buffer，并通过 Vulkan dispatch。
- `gpu::metal_run(options)`：在 macOS 上加载 Metal shader 或从 Zust 编译后 dispatch。

`options` 是普通动态 map，常用字段包括 `source` 或 `path`、`module`、`fn`、`workgroup_size`、`groups` 和 `args`。运行参数支持标量输入、typed vector buffer，以及用于结构体 ABI 参数的原始 `bytes` buffer。

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
