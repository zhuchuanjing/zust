# Zust vs Lua vs Python Benchmark

**build:** `cargo run --bin bench --release`  
**date:** 2026-06-17

| benchmark | Zust | Lua | lua/zust | Python | py/zust |
|---|---|---|---|---|---|
| fibonacci(35) recursive | 131ms | 315ms | 2.4x | 638ms | 4.9x |
| fibonacci iter 50M | 163ms | 374ms | 2.3x | 3.5s | 21.6x |
| sieve 100K | 214us | 8ms | 36.9x | 13ms | 62.3x |
| list push/sum 2M | 38ms | 23ms | 0.6x | 168ms | 4.4x |
| list push only 2M | 32ms | 13ms | 0.4x | 72ms | 2.2x |
| list sum x5 2M | 36ms | 60ms | 1.7x | 584ms | 16.4x |
| bintree depth 20 | 5ms | 34ms | 6.3x | 46ms | 8.7x |
| nested loops(2000) | 11ms | 155ms | 13.6x | 1.3s | 111.7x |
| float ops 20M | 50ms | 270ms | 5.4x | 1.7s | 34.6x |
| strcat x50000 | 1ms | 44ms | 41.2x | 15ms | 14.0x |
| collatz(100K) | 24ms | 230ms | 9.8x | 820ms | 34.7x |
| pow mod 5M | 58ms | 770ms | 13.2x | 3.9s | 65.9x |
| gcd(5M) | 186ms | 496ms | 2.7x | 1.6s | 8.7x |
| prime check(500K) | 32ms | 235ms | 7.3x | 748ms | 23.4x |
| bubble sort 10K | 48ms | 676ms | 14.1x | — | — |
| map bracket get/set 200K | 41ms | 120ms | 2.9x | 89ms | 2.2x |
| mandelbrot 1000 | 31ms | 626ms | 20.4x | 3.9s | 127.5x |
| spectral norm 550 | 6ms | 190ms | 31.8x | 495ms | 82.8x |
| bit popcount 50M | 49ms | 1.9s | 38.2x | 10.8s | 221.5x |
| sequential fact 100M | 368ms | 500ms | 1.4x | 5.7s | 15.6x |
| string build x5000 | 54us | 14ms | 261.2x | 2ms | 41.4x |
| map bracket acc 200K | 42ms | 61ms | 1.5x | 61ms | 1.4x |
| struct field ops 20M | 62ms | 675ms | 10.9x | 2.7s | 43.6x |
| closure sum 50M | 24ms | 601ms | 25.2x | 2.9s | 120.5x |
| closure 16args 10M | 8ms | 562ms | 73.5x | 1.5s | 193.4x |
| vec add 100x500K | 26ms | 309ms | 11.7x | 2.2s | 85.3x |
| ackermann(3,6) | 897us | 7ms | 7.9x | 95us | 0.1x |
| quicksort 2K | 55us | 931us | 16.9x | — | — |
| matrix mul 40x40 x50 | 3ms | 89ms | 32.9x | 274ms | 101.4x |
| binary search 10K | 702us | 10ms | 14.1x | 14ms | 19.8x |
| random LCG 50M | 62ms | 422ms | 6.8x | 6.6s | 106.6x |
| array reverse 1K x10K | 9ms | 139ms | 15.7x | 1.2s | 130.8x |

**geometric mean (28 valid): Zust = 1.0x, Lua = 7.7x, Python = 28.7x**

注：

- bubble sort 和 quicksort 的 Python 版未跑通；Lua 有有效数据，因此表格仍保留 Lua/Zust 对比。
- string build 和 ackermann 的部分耗时低于 summary 的稳定计入阈值，因此不进入几何平均，但表格保留实际测得比例。
- 绝对耗时取决于运行机器与负载；同一次运行内三种语言在相同环境下测得，因此各列比值是稳定的可比信号。

## 近期变化（2026-06）

- 新增 `list push only 2M` 与 `list sum x5 2M`，把 list 写入和读取/求和拆开观察。当前 push-only 仍慢于 Lua，sum-repeat 已快于 Lua。
- 闭包、bit popcount、spectral norm、bintree 等计算项在本轮结果中提升明显。
- `Dynamic::Map` 相关两个 map 基准继续稳定快于 Lua 与 Python。

## 已知可优化点

- **list push only（0.4x lua）**：列表已是扁平 `VecI64`，瓶颈仍是每元素一次 native FFI 调用。进一步提速需要把 `push`/`get_idx` 等路径内联进 Cranelift IR，消除逐元素调用成本。
