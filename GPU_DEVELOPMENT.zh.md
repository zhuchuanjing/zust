# Zust 显卡可执行程序开发说明

这份文档说明如何用 Zust 编写可以在显卡上执行的程序。Zust 的 GPU 开发流程可以理解为：

```text
.zs 源码
  -> Zust 编译器
  -> SPIR-V 或 Metal shader
  -> Vulkan / Metal runtime
  -> GPU 执行
```

Zust 不只是一个类 Rust 脚本语言。它可以把 `.zs` 文件中的计算逻辑编译成 GPU kernel，让同一门语言既能表达动态脚本，也能落到高性能显卡计算。

## 1. 适合开发的程序类型

Zust 的 GPU 后端适合编写大量并行、数据密集型的计算程序，例如：

- 图像生成，例如 Mandelbrot 分形
- 并行数组处理
- GPU 排序，例如 bitonic sort
- 路径查找、距离场、网格计算
- 高精度数值计算
- 由 AI 生成或改写的动态计算 kernel

仓库中的 GPU 示例位于：

```text
zusts/gpu/bitonic.zs
zusts/gpu/pathfind.zs
zusts/gpu/poly.zs
zusts/gpu/mandelbrot.zs
zusts/gpu/mandelbrot_f32.zs
zusts/gpu/mandelbrot_bigfloat2.zs
zusts/gpu/mandelbrot_bigfloat4.zs
zusts/gpu/mandelbrot_bigfloat8.zs
```

## 2. GPU kernel 的基本结构

GPU 程序通常写成一个公开的入口函数：

```zust
pub fn main(data: Vec<u32>) {
    let group = spirv::group_id();
    let local = spirv::local_id();

    let i = group[0] * 256u32 + local[0];

    data[i] = data[i] + 1u32;
}
```

其中：

```text
pub fn main(...)       GPU kernel 入口
Vec<u32>               GPU buffer
spirv::group_id()      当前 workgroup 编号
spirv::local_id()      当前线程在 workgroup 内的位置
```

真实程序中一般需要边界检查：

```zust
pub fn main(len: u32, data: Vec<u32>) {
    let group = spirv::group_id();
    let local = spirv::local_id();

    let i = group[0] * 256u32 + local[0];

    if i < len {
        data[i] = data[i] + 1u32;
    }
}
```

## 3. 使用结构体传递参数

复杂 kernel 通常会把参数放进结构体中：

```zust
struct Params {
    len: u32,
    scale: f32,
}

pub fn main(params: Params, input: Vec<f32>, output: Vec<f32>) {
    let group = spirv::group_id();
    let local = spirv::local_id();

    let i = group[0] * 256u32 + local[0];

    if i < params.len {
        output[i] = input[i] * params.scale;
    }
}
```

宿主 Rust 侧也需要定义相同布局的结构体：

```rust
#[repr(C)]
#[derive(Clone, Copy, Debug)]
struct Params {
    len: u32,
    scale: f32,
}
```

如果走 Vulkan runtime，参数结构体通常还需要满足 `vulkano::buffer::BufferContents`。如果走 Metal runtime，参数结构体通常需要满足 `bytemuck::Pod` 和 `bytemuck::Zeroable`。

## 4. 编译为 SPIR-V

SPIR-V 是 Vulkan 可以执行的 shader 中间格式。Zust 的 `vm-spirv` crate 可以把 `.zs` 编译成 SPIR-V kernel。

Rust 侧编译示例：

```rust
use vm_spirv::compile_file_with_workgroup_size;

let kernel = compile_file_with_workgroup_size(
    "zusts/gpu/bitonic.zs",
    "bitonic",
    "main",
    [256, 1, 1],
)?;
```

参数含义：

```text
"zusts/gpu/bitonic.zs"  Zust 源文件路径
"bitonic"               模块名
"main"                  入口函数名
[256, 1, 1]             每个 workgroup 的线程布局
```

也可以直接运行仓库示例，生成 `.spv` 和 `.spvasm`：

```bash
cargo run -p vm-spirv --example bitonic
```

输出文件：

```text
bitonic.spv       二进制 SPIR-V
bitonic.spvasm    可读的 SPIR-V 反汇编文本
```

`.spvasm` 适合调试，用来确认 kernel 是否正确生成了计算入口、buffer 访问和控制流。

## 5. 用 Vulkan 执行 SPIR-V

仓库中的 `vulkan` crate 提供了运行 SPIR-V kernel 的辅助 runtime。

基本流程：

```rust
use vm_spirv::compile_file_with_workgroup_size;
use vulkan::Runtime;

let kernel = compile_file_with_workgroup_size(
    "zusts/gpu/bitonic.zs",
    "bitonic",
    "main",
    [256, 1, 1],
)?;

let mut runtime = Runtime::new()?;
let mut args = runtime.args();

let params = args.add_input(BitonicParams {
    len: data.len() as u32,
    k,
    j,
    ascend: 1,
})?;

let data_buf = args.add_vec::<u32>(
    data.len() as u64,
    |buf| buf.copy_from_slice(&data),
)?;

runtime.prepare(kernel.spirv.words(), args)?;
runtime.run([workgroup_count, 1, 1])?;
```

关键步骤：

```text
Runtime::new()          初始化 Vulkan runtime
runtime.args()          创建参数绑定集合
args.add_input(...)     添加普通输入参数
args.add_vec(...)       添加数组 buffer
runtime.prepare(...)    创建 compute pipeline
runtime.run(...)        dispatch 到 GPU 执行
data_buf.read()?        从 GPU buffer 读回结果
```

运行仓库中的 Vulkan 示例：

```bash
cargo run -p vulkan --example run_bitonic
cargo run -p vulkan --example run_poly
cargo run -p vulkan --example run_pathfind
cargo run -p vulkan --example run_mandel
```

这些示例会把 Zust GPU 程序编译成 SPIR-V，然后通过 Vulkan 在显卡上执行。

## 6. 编译为 Metal shader

在 macOS 和 Apple GPU 上，可以使用 `vm-metal` 后端。它会把 Zust kernel 编译成 Metal shader source。

示例：

```rust
let source = std::fs::read("zusts/gpu/bitonic.zs")?;

let kernel = vm_metal::compile_source_with_workgroup_size(
    source,
    "bitonic",
    "main",
    [256, 1, 1],
)?;

std::fs::write("bitonic.metal", kernel.metal.source())?;
```

运行 Metal 示例：

```bash
cargo run -p vm-metal --example bitonic
cargo run -p vm-metal --example run_mandel
```

Metal 路线适合 Apple 平台；Vulkan 路线适合具备 Vulkan 驱动和运行时的环境。

## 7. Workgroup 与 dispatch

GPU 计算通常分为两层：

```text
workgroup size    每个 workgroup 中有多少线程
dispatch groups   启动多少个 workgroup
```

例如编译时指定：

```rust
[256, 1, 1]
```

表示每个 workgroup 有 256 个线程。

运行时指定：

```rust
runtime.run([workgroup_count, 1, 1])?;
```

表示启动 `workgroup_count` 个 workgroup。

Zust kernel 内部通常这样计算全局下标：

```zust
let group = spirv::group_id();
let local = spirv::local_id();
let i = group[0] * 256u32 + local[0];
```

如果处理二维图像，可以这样写：

```zust
let group = spirv::group_id();
let local = spirv::local_id();

let px = group[0] * 16u32 + local[0];
let py = group[1] * 16u32 + local[1];
let pos = py * width + px;
```

## 8. 一个二维图像 kernel 示例

下面是一个简化的二维图像计算 kernel：

```zust
struct Params {
    width: u32,
    height: u32,
}

pub fn main(params: Params, image: Vec<f32>) {
    let group = spirv::group_id();
    let local = spirv::local_id();

    let x = group[0] * 16u32 + local[0];
    let y = group[1] * 16u32 + local[1];

    if x < params.width && y < params.height {
        let pos = y * params.width + x;
        image[pos] = (x as f32) / (params.width as f32);
    }
}
```

这个 kernel 中，每个 GPU 线程负责写一个像素。

## 9. Mandelbrot 示例

Mandelbrot 是 Zust GPU 后端的典型示例。它把每个像素映射到复平面坐标，然后独立迭代：

```text
z = z * z + c
```

对应的普通 `f32` 版本位于：

```text
zusts/gpu/mandelbrot_f32.zs
```

高精度 BigFloat 版本位于：

```text
zusts/gpu/mandelbrot.zs
zusts/gpu/mandelbrot_bigfloat2.zs
zusts/gpu/mandelbrot_bigfloat4.zs
zusts/gpu/mandelbrot_bigfloat8.zs
```

运行 Vulkan Mandelbrot：

```bash
cargo run -p vulkan --example run_mandel
```

运行 Metal Mandelbrot：

```bash
cargo run -p vm-metal --example run_mandel
```

也可以通过环境变量选择不同的 `.zs` 文件：

```bash
MANDEL_ZS=zusts/gpu/mandelbrot_bigfloat8.zs \
MANDEL_MODULE=mandelbrot_bigfloat8 \
MANDEL_OUTPUT=mand-bigfloat8.png \
cargo run -p vulkan --example run_mandel
```

## 10. 编写 GPU 程序的建议

写 Zust GPU kernel 时，建议遵守这些规则：

- 入口函数使用 `pub fn main(...)`。
- 参数尽量使用数值、结构体和 `Vec<T>`。
- 每个 GPU 线程根据 `group_id` 和 `local_id` 计算自己的数据下标。
- 访问 `Vec<T>` 前先做边界检查。
- 宿主 Rust 侧结构体使用 `#[repr(C)]`。
- 保持 Rust 侧参数顺序与 Zust kernel 参数顺序一致。
- workgroup size 在编译 kernel 时指定。
- dispatch group 数量在运行时指定。
- GPU kernel 中尽量避免字符串、动态 object、打印输出等脚本侧能力。
- 对复杂算法，先写简单版本，再逐步加入结构体、二维网格、泛型或高精度数值。

## 11. 常见问题

### 找不到 Vulkan

如果运行 Vulkan 示例时报错：

```text
no local Vulkan library/DLL
```

说明当前环境没有可用 Vulkan 运行时。需要安装 Vulkan SDK、平台驱动，或在 macOS 上配置 MoltenVK。

### 找不到 GPU 设备

如果报错：

```text
no Vulkan physical device available
```

说明 Vulkan runtime 没有枚举到可用显卡设备。需要检查驱动、Vulkan loader、ICD 配置。

### 创建 pipeline 失败

如果 SPIR-V 编译成功但 Vulkan pipeline 创建失败，通常需要检查：

- kernel 参数类型是否和 Rust 侧 buffer 布局一致
- 结构体是否使用 `#[repr(C)]`
- 参数顺序是否一致
- GPU 是否支持 kernel 使用的类型或特性

### 输出全是零

如果 kernel 执行后输出全是零，通常需要检查：

- dispatch group 数量是否足够
- 下标计算是否越界或没有覆盖目标区域
- 是否忘记写输出 buffer
- 是否写入了错误的 buffer
- 输入参数是否正确更新

## 12. 推荐开发流程

推荐按下面顺序开发：

```text
1. 写一个最小 `.zs` kernel
2. 用 `vm-spirv` 或 `vm-metal` 编译
3. 检查 `.spvasm` 或生成的 `.metal` source
4. 在 Rust 宿主中准备参数和 buffer
5. dispatch 到 GPU 执行
6. 读回结果并验证
7. 再逐步加入复杂逻辑
```

对于复杂计算，可以先写 CPU 或简单 `f32` 版本，确认算法正确后，再迁移到 BigFloat、二维 workgroup 或更复杂的数据结构。

## 13. Zust 在 GPU 开发中的意义

Zust 的 GPU 后端让一门脚本语言具备了高性能执行路径：

```text
AI 或用户生成计算逻辑
  -> Zust 表达 kernel
  -> 编译成 SPIR-V / Metal
  -> GPU 执行
  -> 结果返回应用
```

这对 AI 时代尤其有价值。模型可以生成或改写计算逻辑，Zust 负责提供结构化、可编译、可执行的中间表达，最终把关键计算放到显卡上运行。

Zust 因此可以作为动态计算系统中的 GPU kernel 语言：上层足够灵活，下层能够落到真实硬件。
