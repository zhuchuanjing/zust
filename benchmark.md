# Zust vs Lua vs Python Benchmark

**build:** `cargo run --bin bench --release`  
**date:** 2025-06-08

| benchmark | Zust | Lua | lua/zust | Python | py/zust |
|---|---|---|---|---|---|
| fibonacci(35) recursive | 235ms | 336ms | 1.4x | 686ms | 2.9x |
| fibonacci iter 50M | 183ms | 402ms | 2.2x | 3.3s | 18.1x |
| sieve 100K | 226us | 7ms | 31.2x | 15ms | 65.5x |
| list push/sum 2M | 43ms | 23ms | 0.5x | 183ms | 4.3x |
| bintree depth 20 | 16ms | 30ms | 1.8x | 52ms | 3.2x |
| nested loops(2000) | 12ms | 157ms | 12.7x | 1.4s | 115.7x |
| float ops 20M | 55ms | 287ms | 5.2x | 1.8s | 32.8x |
| strcat x50000 | 935us | 41ms | 44.3x | 15ms | 16.2x |
| collatz(100K) | 26ms | 232ms | 8.9x | 872ms | 33.5x |
| pow mod 5M | 48ms | 951ms | 19.7x | 4.0s | 82.2x |
| gcd(5M) | 279ms | 514ms | 1.8x | 1.6s | 5.7x |
| prime check(500K) | 20ms | 228ms | 11.2x | 916ms | 45.0x |
| bubble sort 10K | 75ms | 899ms | 11.9x | — | — |
| map bracket get/set 200K | 83ms | 178ms | 2.1x | 144ms | 1.7x |
| mandelbrot 1000 | 30ms | 627ms | 21.0x | 4.2s | 138.9x |
| spectral norm 550 | 40ms | 195ms | 4.9x | 534ms | 13.5x |
| bit popcount 50M | 412ms | 1.9s | 4.7x | 10.8s | 26.1x |
| sequential fact 100M | 410ms | 501ms | 1.2x | 5.8s | 14.1x |
| string build x5000 | 56us | 17ms | 310.8x | 1ms | 21.1x |
| map bracket acc 200K | 75ms | 79ms | 1.1x | 107ms | 1.4x |
| struct field ops 20M | 66ms | 670ms | 10.1x | 3.0s | 45.1x |
| closure sum 50M | 364ms | 660ms | 1.8x | 3.6s | 9.9x |
| closure 16args 10M | 86ms | 657ms | 7.6x | 1.5s | 17.8x |
| vec add 100x500K | 29ms | 307ms | 10.7x | 2.2s | 76.0x |
| ackermann(3,6) | 2ms | 2ms | 1.1x | 7ms | 3.9x |
| quicksort 2K | 62us | 1ms | 22.1x | — | — |
| matrix mul 40x40 x50 | 3ms | 92ms | 35.1x | 402ms | 152.6x |
| binary search 10K | 494us | 7ms | 14.7x | 32ms | 63.8x |
| random LCG 50M | 70ms | 443ms | 6.3x | 7.0s | 99.8x |
| array reverse 1K x10K | 9ms | 133ms | 15.1x | 949ms | 107.8x |

**geometric mean (27 valid): Zust = 1.0x, Lua = 5.5x, Python = 21.4x**

注：bubble sort 和 quicksort 的 Python 版因大整数解析问题未跑通。
