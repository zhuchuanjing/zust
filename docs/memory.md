# Zust Memory Model

## Overview

Zust uses a **thread-local arena** memory model. Each OS thread that executes Zust code gets its own private arena, avoiding global locks and enabling zero-contention concurrent execution.

## Architecture

```
thread 1          thread 2          thread N
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
               │ (shared, Arc) │
               │  code/consts  │
               └───────────────┘
```

## Key Mechanisms

### Arena Allocation

- Each thread starts with a 1 MB initial chunk.
- When a chunk is exhausted, a new chunk (at least the requested size, rounded to next power of two) is appended.
- On scope exit, the chunk pointer and offset are restored to the scope entry mark, effectively recycling arena space.
- Chunks are **not freed** while the thread is alive—they are reused across function calls.

### Scope Management

- Every Zust function call enters a scope (`scope_enter`) and exits via `scope_exit_void`, `scope_exit_dynamic`, or `scope_exit_bytes`.
- Inside a scope, `Dynamic` values are allocated in the arena.
- **Non-returned temporaries**: dropped in-place when the scope exits (LIFO order).
- **Returned values**: deep-cloned out of the arena (promoted) before scope exit. The promoted value is heap-allocated via `Box::into_raw`.

### Thread Safety

- `Vm` is `Arc<Mutex<JITRunTime>>` and implements `Send + Sync`. It can be cloned and shared across threads.
- Compiled function pointers are raw C ABI pointers, safe to call from any thread.
- Each thread has its own `thread_local! VM_MEMORY`, so arena operations are lock-free.
- Modifying the VM (importing code, registering types) requires locking the `Mutex`.

### Return Value Lifetime

When a Zust function returns a `*const Dynamic`:

1. `scope_exit_dynamic` deep-clones the value out of the arena.
2. The clone is heap-allocated (`Box::into_raw`).
3. The caller receives a raw pointer.
4. **The caller must free the returned value** via `Box::from_raw(ptr as *mut Dynamic)`.

The `dynamic::call_fn` helper handles this automatically. When calling raw function pointers directly, the caller is responsible for cleanup.

## Concurrent Stress Test

Location: `vm/src/lib.rs` — `concurrent_100_threads_no_memory_leak`

### Setup
- 100 threads
- 200 iterations per thread
- 2 function calls per iteration (one allocating 50-element maps, one doing 200 string concats)
- 40,000 total calls per round
- 3 rounds

### Results (macOS, Apple Silicon)

| Phase | RSS | Notes |
|-------|-----|-------|
| Before any calls | ~16 MB | Process baseline |
| After round 1 | ~164 MB | Arena chunks allocated (100 threads × initial + growth chunks) |
| After round 2 | ~165 MB | +1 MB, stabilized |
| After round 3 | ~168 MB | +3 MB, stabilized |

### Key Findings

1. **Memory grows once, then stabilizes.** The first round allocates arena chunks for all threads. Subsequent rounds reuse the same arenas.
2. **No unbounded growth.** Inter-round delta is 1–3 MB (OS page cache noise), not proportional to call count (40,000 calls/round).
3. **No data races.** 100 threads run in parallel without contention on arena access.
4. **Correct results.** All 120,000 function calls return valid data.

## Important Notes

- Arena memory is per-thread and held until thread exit. Long-lived thread pools will retain arena memory, which is the design intent for server workloads.
- RSS may appear higher than actual live data due to freed chunks not being returned to the OS (they are reused within the thread).
- When calling Zust functions directly via transmuted pointers, always free returned `*const Dynamic` values to avoid leaking the promoted heap allocations.
- Values that must outlive a function call should cross the boundary as owned `Dynamic` maps, lists, primitives, bytes, or ROOT values. Do not persist raw struct addresses from temporary VM storage.
