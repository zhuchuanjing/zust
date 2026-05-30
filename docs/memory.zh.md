# Zust 内存模型

## 概述

Zust 使用 **线程局部 arena** 内存模型。每个执行 Zust 代码的操作系统线程拥有独立的私有 arena，避免了全局锁，实现了零竞争的并发执行。

## 架构

```
线程 1            线程 2            线程 N
┌─────────┐       ┌─────────┐       ┌─────────┐
│VM_MEMORY│       │VM_MEMORY│       │VM_MEMORY│
│(arena)  │       │(arena)  │       │(arena)  │
│ chunks  │       │ chunks  │       │ chunks  │
│ dynamics│       │ dynamics│       │ dynamics│
│ scopes  │       │ scopes  │       │ scopes  │
└─────────┘       └─────────┘       └─────────┘
     │                 │                 │
     └─────────────────┼─────────────────┘
                       │
               ┌───────┴───────┐
               │      Vm       │
               │   (共享 Arc)   │
               │  代码/常量     │
               └───────────────┘
```

## 核心机制

### Arena 分配

- 每个线程以 1 MB 初始 chunk 启动。
- chunk 耗尽时追加新 chunk（大小至少为请求量，向上对齐到 2 的幂）。
- 作用域退出时，chunk 指针和偏移量恢复到作用域入口标记，实现 arena 空间复用。
- chunk **在线程存活期间不会释放**——它们在函数调用之间被复用。

### 作用域管理

- 每个 Zust 函数调用进入一个作用域（`scope_enter`），并通过 `scope_exit_void`、`scope_exit_dynamic` 或 `scope_exit_bytes` 退出。
- 作用域内，`Dynamic` 值在 arena 中分配。
- **非返回临时值**：作用域退出时原地 drop（LIFO 顺序）。
- **返回值**：作用域退出前从 arena 深拷贝（promote）。promote 后的值通过 `Box::into_raw` 分配在堆上。

### 线程安全

- `Vm` 为 `Arc<Mutex<JITRunTime>>`，实现了 `Send + Sync`，可 clone 并在多线程间共享。
- 编译后的函数指针是 C ABI 裸指针，可以在任何线程中安全调用。
- 每个线程有自己的 `thread_local! VM_MEMORY`，arena 操作无锁。
- 修改 VM（导入代码、注册类型）需要锁定 `Mutex`。

### 返回值生命周期

当 Zust 函数返回 `*const Dynamic` 时：

1. `scope_exit_dynamic` 将值深拷贝出 arena。
2. 拷贝在堆上分配（`Box::into_raw`）。
3. 调用方收到裸指针。
4. **调用方必须通过 `Box::from_raw(ptr as *mut Dynamic)` 释放返回值**。

`dynamic::call_fn` 辅助函数自动处理此过程。直接调用裸函数指针时，调用方负责清理。

## 并发压力测试

位置：`vm/src/lib.rs` — `concurrent_100_threads_no_memory_leak`

### 测试参数
- 100 线程
- 每线程 200 次迭代
- 每次迭代 2 次函数调用（一次分配 50 元素 map，一次做 200 次字符串拼接）
- 每轮 40,000 次调用
- 共 3 轮

### 结果（macOS，Apple Silicon）

| 阶段 | RSS | 说明 |
|-------|-----|------|
| 调用前 | ~16 MB | 进程基准 |
| 第 1 轮后 | ~164 MB | arena chunk 分配完成（100 线程 × 初始 + 扩展 chunk） |
| 第 2 轮后 | ~165 MB | +1 MB，已稳定 |
| 第 3 轮后 | ~168 MB | +3 MB，持续稳定 |

### 关键结论

1. **内存增长一次后稳定。** 第一轮为所有线程分配 arena chunk。后续轮次复用已有 arena。
2. **无无限增长。** 轮间增量 1–3 MB（操作系统 page cache 波动），与调用次数（每轮 40,000 次）不成比例。
3. **无数据竞争。** 100 线程并行执行，arena 访问无冲突。
4. **结果正确。** 全部 120,000 次函数调用返回有效数据。

## 注意事项

- Arena 内存是线程局部的，在线程退出前一直持有。长生命周期线程池会保留 arena 内存，这是服务器工作负载的设计意图。
- RSS 可能高于实际活跃数据，因为已释放的 chunk 不会归还操作系统（在 thread 内复用）。
- 通过 transmute 指针直接调用 Zust 函数时，务必释放返回的 `*const Dynamic` 值，防止 promote 后的堆分配泄漏。
- 需要跨调用长期存在的值，应以 owned `Dynamic` map、list、primitive、bytes 或 ROOT 值的形式跨边界。不要将临时 VM 存储中的裸 struct 地址持久化到长期容器中。
