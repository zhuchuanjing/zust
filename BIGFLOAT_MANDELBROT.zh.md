# Zust BigFloat 与 Mandelbrot 计算说明

这份文档说明 Zust 中 `BigFloat` 的实现方式，以及它如何用于 Mandelbrot 分形计算并编译到 GPU 后端执行。

相关源码：

- `zusts/bigfloat.zs`
- `zusts/gpu/mandelbrot.zs`
- `zusts/gpu/mandelbrot_f32.zs`
- `zusts/gpu/mandelbrot_bigfloat2.zs`
- `zusts/gpu/mandelbrot_bigfloat4.zs`
- `zusts/gpu/mandelbrot_bigfloat8.zs`

## 1. 为什么 Mandelbrot 需要 BigFloat

Mandelbrot 集的核心迭代公式是：

```text
z = z * z + c
```

其中：

```text
z = zx + zy * i
c = x + y * i
```

展开成实数计算：

```text
zx_next = zx * zx - zy * zy + x
zy_next = 2 * zx * zy + y
```

每次迭代后判断：

```text
zx * zx + zy * zy > 4
```

如果条件成立，说明这个点已经逃逸。逃逸所需迭代次数可以用来生成颜色；如果达到最大迭代次数仍未逃逸，通常认为它在 Mandelbrot 集内部或边界附近。

普通 `f32` 可以画出基础分形，但当画面持续放大时，坐标间距会越来越小，`f32` 精度很快不够。相邻像素可能映射到几乎相同的坐标，细节会丢失。`BigFloat` 的作用就是用更高精度保存和计算这些坐标。

## 2. BigFloat 的数据结构

`zusts/bigfloat.zs` 中定义了高精度浮点数：

```zust
pub struct BigFloat<N> {
    sign: bool,
    exp: i32,
    data: [u32; N],
}
```

它由三部分组成：

```text
sign    符号位，false 表示正数，true 表示负数
exp     指数
data    N 个 u32 limb，保存有效数字
```

可以把它理解成一个以 `2^32` 为底的大数：

```text
value = sign * sum(data[i] * (2^32)^(exp + i))
```

其中 `N` 是编译期泛型参数，表示有效数字数组的长度：

```zust
BigFloat<2>
BigFloat<4>
BigFloat<8>
BigFloat<32>
```

`N` 越大，精度越高，计算成本也越高。

## 3. BigFloat 的基础构造

`BigFloat` 提供了几个基础构造函数：

```zust
BigFloat<N>::zero()
BigFloat<N>::from_u32(value)
BigFloat<N>::from_f32(value)
```

例如：

```zust
let zx = bigfloat::BigFloat<4>::from_f32(0.0f32);
let two = bigfloat::BigFloat<4>::from_u32(2u32);
let radius = bigfloat::BigFloat<4>::from_u32(4u32);
```

`from_f32` 会把普通浮点数转换为 `BigFloat<N>`。`to_f32` 则把高精度数转回 `f32`，主要用于最终输出颜色值。

## 4. 加减法实现

`BigFloat` 的加减法需要先对齐指数。

两个数可以抽象成：

```text
a = data_a * base^exp_a
b = data_b * base^exp_b
```

如果指数不同，就要把 limb 放到相同的指数位置后再做加减。

底层有两个辅助函数：

```zust
pub fn bf_add_carry(a: u32, b: u32)
pub fn bf_sub_borrow(a: u32, b: u32, borrow: u32)
```

它们分别处理：

```text
u32 加法进位
u32 减法借位
```

`add` 的逻辑大致是：

```text
如果两个数同号：
    绝对值相加，符号不变
如果两个数异号：
    比较绝对值
    大的减小的
    符号取绝对值更大的那个数
```

`sub` 则可以看成加上相反数。

## 5. 乘法实现

`BigFloat` 中有两套乘法：

```zust
fn mul_schoolbook(...)
fn mul_ss(...)
```

较小的 `N` 使用普通竖式乘法：

```text
每个 limb 与另一个数的每个 limb 相乘
累加到结果数组
处理进位
```

由于 `u32 * u32` 会产生超过 `u32` 的结果，代码中使用：

```zust
pub fn bf_mul_u32_wide(a: u32, b: u32)
```

这个函数把一个 `u32` 拆成两个 16 bit 部分，计算出 64 bit 乘积的低 32 位和高 32 位。

当 `N >= 32` 且 `N` 是 2 的幂时，`mul` 会走 `mul_ss`，使用 NTT 思路做快速卷积：

```text
拆成 16 bit 数字
用两个模数做 NTT
点乘
逆 NTT
CRT 合并
恢复成 u32 limb
```

这说明 `bigfloat.zs` 不只是一个语法示例，而是在 Zust 中实现了较完整的高精度数值算法。

## 6. Mandelbrot 的 f32 版本

普通 `f32` 版本位于 `zusts/gpu/mandelbrot_f32.zs`：

```zust
while iter < params.max_iter && (zx * zx + zy * zy) <= 4.0f32 {
    let x_next = zx * zx - zy * zy + x0;
    zy = 2.0f32 * zx * zy + y0;
    zx = x_next;
    iter += 1u32;
}
```

这个版本速度快，但深度放大时精度有限。

## 7. Mandelbrot 的 BigFloat 版本

BigFloat 版本会把 `zx`、`zy`、`x`、`y` 换成高精度数：

```zust
let zx = bigfloat::BigFloat<4>::from_f32(0.0f32);
let zy = bigfloat::BigFloat<4>::from_f32(0.0f32);
let x_bf = bigfloat::BigFloat<4>::from_f32(x);
let y_bf = bigfloat::BigFloat<4>::from_f32(y);
```

迭代公式变成：

```zust
let zx2 = zx.mul(zx);
let zy2 = zy.mul(zy);
let radius2 = zx2.add(zy2);

if radius2.gt(escape_radius2) {
    break;
}

let tmp = zx2.sub(zy2).add(x_bf);
let next_zy = two_bf.mul(zx).mul(zy).add(y_bf);

zy = next_zy;
zx = tmp;
```

数学公式没有变化，只是普通浮点运算：

```text
+
-
*
>
```

被替换成了 BigFloat 方法：

```text
add
sub
mul
gt
```

## 8. GPU 上如何并行计算

Mandelbrot 图像里的每个像素都可以独立计算，所以它非常适合 GPU。

Zust kernel 中通过：

```zust
let group = spirv::group_id();
let local = spirv::local_id();
```

得到当前 GPU 线程位置。

然后计算像素坐标：

```zust
let px = group[0] * 16u32 + local[0];
let py = group[1] * 16u32 + local[1];
```

每个 GPU 线程负责一个像素：

```zust
let pos = py * sample_width + px;
buf[pos] = escape_value_bigfloat4(x, y, params.max_iter);
```

最终 `buf` 是一个 `Vec<f32>`，保存每个像素的逃逸值，用于生成图像颜色。

## 9. 不同 Mandelbrot 版本的意义

仓库里有多个 Mandelbrot 版本：

```text
mandelbrot_f32.zs          普通 f32 版本
mandelbrot_bigfloat2.zs    BigFloat<2>
mandelbrot_bigfloat4.zs    BigFloat<4>
mandelbrot_bigfloat8.zs    BigFloat<8>
mandelbrot.zs              泛型 BigFloat<N> 版本
```

可以这样理解：

```text
f32              最快，但精度最低
BigFloat<2>      精度更高，成本较低
BigFloat<4>      更适合深一些的放大
BigFloat<8>      精度更高，但计算更重
BigFloat<N>      用编译期参数选择精度
```

## 10. 运行示例

生成 SPIR-V：

```bash
cargo run -p vm-spirv --example mandelbrot
```

通过 Vulkan 执行：

```bash
cargo run -p vulkan --example run_mandel
```

通过 Metal 执行：

```bash
cargo run -p vm-metal --example run_mandel
```

也可以通过环境变量选择不同版本：

```bash
MANDEL_ZS=zusts/gpu/mandelbrot_bigfloat8.zs \
MANDEL_MODULE=mandelbrot_bigfloat8 \
MANDEL_OUTPUT=mand-bigfloat8.png \
cargo run -p vulkan --example run_mandel
```

## 11. 小结

`bigfloat.zs` 展示了 Zust 可以写复杂数值库：结构体、泛型、数组、循环、方法、比较、加减乘都可以在语言内完成。

Mandelbrot 示例则展示了另一层能力：这些 Zust 数值程序不只是能在 CPU 上解释或 JIT，也可以编译成 GPU kernel，在 Vulkan 或 Metal 上执行。

这正好体现了 Zust 的定位：用动态、可生成的脚本语言表达复杂计算，再把关键路径编译到高性能后端。
