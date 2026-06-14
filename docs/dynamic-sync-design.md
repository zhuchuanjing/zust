# Dynamic Clone & Synchronization Design

## TL;DR

- `Dynamic::clone` is **shallow** for `List`/`Map`. Local operations on lists/maps are O(1) refcount bumps, not deep copies.
- `Dynamic::deep_clone` is the only way to take a value across a thread boundary safely.
- The locking primitive is `parking_lot::RwLock`, never `parking_lot::Mutex` and never `RefCell` for shared state.
- `Arc<RwLock<...>>` exists for two reasons that look like concurrency reasons but are actually value-semantics reasons — read on.

These rules are not negotiable. Future "performance optimizations" that propose changing them are almost certainly wrong; see "What almost went wrong" below.

## Why `Dynamic::clone` is shallow

`Dynamic` is defined as:

```rust
pub enum Dynamic {
    List(Arc<RwLock<Vec<Dynamic>>>),
    Map(Arc<RwLock<IndexMap<SmolStr, Dynamic>>>),
    // ... other variants
}
```

The `#[derive(Clone)]` on this enum synthesizes:

```rust
List(l) => List(Arc::clone(l)),  // refcount + 1, not a copy of the Vec
Map(m)  => Map(Arc::clone(m)),   // refcount + 1, not a copy of the IndexMap
```

This is **intentional**. It preserves two invariants Zust's runtime relies on:

1. **Local operators are cheap.** `a + b` for two list values does:
   ```rust
   self.clone().append(rhs);  // Arc::clone (atomic increment)
   return self;               // return the original
   ```
   If `clone` were a deep copy, every `+` on a list would copy the entire list. That is not what we want on a hot path.

2. **Cross-thread transfer is explicit.** The only way to get an independent copy of a `List` or `Map` is `deep_clone`. This makes the cost of crossing a thread boundary visible at the call site, and it matches the only thread boundary that actually exists in the runtime: spawning a detached OS thread (see "Where the boundary actually is" below).

The corollary: **a `Dynamic::clone` is only safe to read on the thread that owns the cloned handle.** Two threads sharing an `Arc` would race on the contents. Since nothing in the runtime shares an `Arc` across threads, this is fine.

## Where the thread boundary actually is

`Dynamic` is not shared across threads. The full list of paths that could in theory share it, and what they actually do:

| Path | Cross-thread? | What happens |
|------|---------------|--------------|
| Main-thread JIT runs map operations | No | `RwLock` is uncontended but pays the atomic-load + lock-acquire cost anyway |
| `std::spawn`/`std::thread::spawn` on a script function | No | `args.deep_clone()` before the move into the new thread |
| `start_task` / `Object::Task` / `Object::ThreadTask` | No | `info: Dynamic` is moved; `Object::value()` returns `info.deep_clone()` |
| `http::WebSocket` mpsc channel | No | `Dynamic` is moved through the channel; consumer serializes to msgpack before any cross-thread use |
| `root::send` | No | `msg: Dynamic` is moved into the handler; handlers run sequentially on the dispatch stack |
| `root::add` / `Object::Value` in a memory mount | No | Stored in `scc::HashMap`; read via `read_sync` which hands a `&Object` to a closure that calls `value()` → `deep_clone()` |
| Closure captures across spawn | N/A | The runtime rejects this with `"spawn closure does not support captures yet"`. Cranelift SSA values can't be serialized anyway |

The conclusion from auditing every `Arc::clone` site: **no `Dynamic` value is ever reachable from two threads at the same time**. The `Arc<RwLock<...>>` exists purely to give `Dynamic::clone` cheap shallow-copy semantics, not to synchronize concurrent access.

## Why `parking_lot::RwLock` and not `RefCell` or `std::sync::RwLock`

A previous attempt to "optimize" this by switching to `RefCell<IndexMap>` would have been a regression. Here's the trap:

`RefCell<T>` provides `RefCell::clone(&self) -> Self` which automatically calls `T::clone` on the inner value. So:

```rust
// before
Map(Arc<RwLock<IndexMap<...>>>)  // clone = Arc::clone (cheap)

// after (DO NOT DO THIS)
Map(RefCell<IndexMap<...>>)      // clone = full IndexMap clone (O(n))
```

The change looks like a perf win because you drop the atomic + lock acquire. It is actually a perf regression because every `let other = map.clone();` in user code now deep-copies the map. Every `+` operator on a map, every function-call argument, every closure capture, every for-loop body — they all just got 10–100× slower for non-trivial maps.

The right way to remove the lock overhead is to switch the lock primitive, not the synchronization model. `parking_lot::RwLock` is a drop-in replacement for `std::sync::RwLock` that uses futexes directly instead of going through the OS mutex API. We get the same `Arc`/`RwLock`/`Dynamic::clone` semantics, just with a faster lock. As a bonus, the guards are not `Result`-wrapped, so we delete `~30` `.unwrap()` calls on `read()`/`write()` that could only ever panic on a poisoned lock (and parking_lot guards can't be poisoned).

## Why `Arc<RwLock<...>>` and not `Box<RwLock<...>>` or `RefCell<...>`

The `Arc` is not there for sharing across threads — see above. It's there so that `Dynamic::clone` can produce a cheap, value-typed handle that the caller can use without thinking about ownership. If we changed it to `Box<RwLock<...>>`, every `clone` would have to be `deep_clone` (or every `clone` would have to be a move, which is not what `derive(Clone)` gives you). We went around this once and the result was slower and more confusing, not faster.

## What almost went wrong

When the F1.6 audit surfaced the lock cost on `Dynamic::Map` operations, the obvious-looking fix was to replace `Arc<RwLock<...>>` with `RefCell<...>` — drop the atomic, drop the lock. It compiles, it passes tests, and it is **wrong**:

- The original `clone` is `Arc::clone` (atomic increment).
- The "optimized" `clone` is `RefCell::clone` which **deep-copies** the contents.
- Every local operator (`+`, `append`, field assignment, function call argument, closure capture) now does a full O(n) copy of the list/map.
- Net effect: read paths slightly faster, write paths slightly faster, but **every other path 10–100× slower**.

The principle: when the question is "can we make the lock cheaper?", the answer is almost always "yes — use a better lock", not "drop the lock and break `Clone`". Read the structure of `#[derive(Clone)]` carefully before changing shared state shape.

## Summary

- Local `Dynamic` operations are cheap. Keep them cheap by keeping `Arc<RwLock<...>>` shallow-clone.
- Cross-thread transfers are explicit `deep_clone`. Keep them explicit and rare.
- `parking_lot::RwLock` is the lock; not `RefCell`, not `std::sync::RwLock`, not `Arc<Mutex<...>>`.
- Any future proposal to switch to `RefCell` for performance, to drop the `Arc`, or to change `clone` semantics, must come with a benchmark that measures the **hot operator path** (`+`, `set`, function call), not just isolated read/write. Without that, assume the change is wrong.
