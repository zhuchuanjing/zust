# Zust vs Lua vs Python Benchmark

**build:** `cargo run --bin bench --release`  
**date:** 2026-06-11

| benchmark | Zust | Lua | lua/zust | Python | py/zust |
|---|---|---|---|---|---|
| fibonacci(35) recursive | 231ms | 318ms | 1.4x | 651ms | 2.8x |
| fibonacci iter 50M | 147ms | 365ms | 2.5x | 3.6s | 24.4x |
| sieve 100K | 224us | 7ms | 31.6x | 17ms | 77.1x |
| list push/sum 2M | 43ms | 22ms | 0.5x | 199ms | 4.6x |
| bintree depth 20 | 16ms | 28ms | 1.7x | 51ms | 3.1x |
| nested loops(2000) | 11ms | 152ms | 13.6x | 1.4s | 122.2x |
| float ops 20M | 50ms | 273ms | 5.5x | 1.9s | 37.4x |
| strcat x50000 | 718us | 44ms | 61.5x | 16ms | 22.9x |
| collatz(100K) | 24ms | 233ms | 9.8x | 878ms | 36.9x |
| pow mod 5M | 50ms | 801ms | 15.9x | 4.3s | 85.2x |
| gcd(5M) | 255ms | 514ms | 2.0x | 1.6s | 6.3x |
| prime check(500K) | 28ms | 229ms | 8.3x | 748ms | 26.9x |
| bubble sort 10K | 46ms | 801ms | 17.4x | — | — |
| map bracket get/set 200K | 91ms | 161ms | 1.8x | 110ms | 1.2x |
| mandelbrot 1000 | 30ms | 628ms | 21.3x | 3.5s | 119.0x |
| spectral norm 550 | 39ms | 186ms | 4.8x | 511ms | 13.2x |
| bit popcount 50M | 352ms | 2.0s | 5.6x | 12.0s | 34.0x |
| sequential fact 100M | 387ms | 504ms | 1.3x | 5.7s | 14.8x |
| string build x5000 | 54us | 11ms | 193.6x | 4ms | 76.3x |
| map bracket acc 200K | 41ms | 57ms | 1.4x | 72ms | 1.7x |
| struct field ops 20M | 61ms | 669ms | 10.9x | 2.8s | 45.8x |
| closure sum 50M | 349ms | 623ms | 1.8x | 3.1s | 9.0x |
| closure 16args 10M | 77ms | 580ms | 7.5x | 1.7s | 21.6x |
| vec add 100x500K | 28ms | 311ms | 11.0x | 2.5s | 86.7x |
| ackermann(3,6) | 2ms | 7ms | 4.1x | 8ms | 4.9x |
| quicksort 2K | 59us | 570us | 9.7x | — | — |
| matrix mul 40x40 x50 | 3ms | 87ms | 31.7x | 273ms | 99.4x |
| binary search 10K | 494us | 8ms | 15.6x | 22ms | 44.6x |
| random LCG 50M | 63ms | 437ms | 6.9x | 7.0s | 111.3x |
| array reverse 1K x10K | 9ms | 136ms | 15.0x | 1.1s | 122.4x |

**geometric mean (27 valid): Zust = 1.0x, Lua = 5.9x, Python = 21.9x**

注：bubble sort 和 quicksort 的 Python 版因大整数解析问题未跑通。绝对耗时取决于运行机器与负载;同一次运行内三种语言在相同环境下测得,故各列比值是稳定的可比信号。

## 近期优化（2026-06）

- **常量除数快路径**：除零安全守卫只在除数可能为 0/溢出时才生成;编译期已知非零的常量除数（如 `% 1000000007`、`/ 2`）走无守卫的 `*_imm`。div/mod 密集基准恢复并略超历史水平（collatz、pow mod）。
- **`Dynamic::Map` 底层 `BTreeMap` → `IndexMap`**：键访问 O(log n)→O(1),保留确定性插入顺序。两个 map 基准（get/set、access）从接近或慢于基线翻到稳定快于 Lua 与 Python。

## 已知可优化点

- **list push/sum（0.5x lua）**：列表已是扁平 `VecI64`（无锁、无装箱),瓶颈是每元素一次 native FFI 调用。进一步提速需把 `push`/`get_idx` 内联进 Cranelift IR 以消除逐元素调用。
