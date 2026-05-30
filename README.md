# Zust

Zust is a Rust-like scripting language and runtime written in Rust. It keeps the familiar shape of Rust syntax, but removes borrow checking and explicit mutability so scripts can stay compact, dynamic, and easy to generate or transform.

Official website: [www.zust-lang.com](https://www.zust-lang.com)

The project is close to a mature open-source release. The workspace now contains separately versioned crates, with the VM crate at `0.9.36`, the dynamic crate at `0.9.8`, the compiler at `0.9.15`, the parser at `0.9.8`, and the editor-facing packages at `0.9.2`.

中文文档: [README.zh.md](README.zh.md)

## Recent Runtime Work

The VM-owned temporary memory work is implemented. VM-created `Any`/`Dynamic` values and generated struct storage are routed through a VM memory manager instead of scattered raw heap ownership. Each executing thread has a thread-local arena with function scopes; non-returned temporaries are dropped at scope exit, and returned values are promoted before they escape to Rust callers or ROOT.

Long-cycle probes now show stable memory after the first arena expansion. RSS can remain at the allocator high-water mark, especially in thread pools where each worker has its own arena, but repeated VM function calls do not show continuous `Dynamic` growth. This is intended for long-running server processes.

The current model is still an arena-based temporary owner, not a tracing GC. Values that must outlive a call should cross the boundary as owned `Dynamic` maps, lists, primitives, bytes, custom objects, or ROOT values. Do not persist raw generated struct addresses from temporary VM storage into long-lived containers.

Recent compiler/runtime fixes also include:

- Top-level `const` composite literals can reference previously declared const/static values, so tables like `const GEM_TABLE = [{ key: GEM_ATK }]` are folded at compile time.
- Function return inference writes inferred non-generic return types back into the function symbol table.
- Nested struct parameters returning structs support static field access at the call site.
- `std::log(value)` records a dynamic value through Rust logging with debug formatting.
- VM-internal memory and struct helper imports are registered directly by the runtime instead of being exposed through the script symbol table.

## Additional Documentation

- [Mandelbrot Deep Refinement and Zust BigFloat GPU Notes](MANDELBROT_REFINEMENT_BIGFLOAT_REPORT.en.md): English notes from a deep-zoom Mandelbrot experiment, covering GPU BigFloat precision, iteration count, sampling, and the server/client rendering split.
- [Mandelbrot 深度精化实验与 Zust BigFloat GPU 能力记录](MANDELBROT_REFINEMENT_BIGFLOAT_REPORT.zh.md): 中文版实验记录，说明高精度坐标、BigFloat GPU 渲染、Redis 状态保存和客户端交互规则。
- [BIGFLOAT_MANDELBROT.zh.md](BIGFLOAT_MANDELBROT.zh.md): BigFloat Mandelbrot implementation notes in Chinese.
- [GPU_DEVELOPMENT.zh.md](GPU_DEVELOPMENT.zh.md): GPU backend development notes in Chinese.

## Design Ideas

Zust is designed around a small set of practical goals:

- **Rust-shaped, script-friendly syntax**: functions, structs, `impl`, ranges, blocks, and typed literals look familiar to Rust users, while variables remain freely assignable.
- **Dynamic values at the boundary**: the `dynamic` crate provides a `Dynamic` value model for lists, maps, structs, bytes, typed vectors, JSON, and MessagePack.
- **Optional static structure**: scripts can start dynamic and add type annotations where native code generation or GPU backends need stronger shape.
- **Compile scripts into native execution**: the `vm` crate compiles Zust modules with Cranelift and exposes raw function pointers for host Rust code.
- **One language, several execution targets**: the repository contains a JIT backend, SPIR-V generation, Metal source generation, and Vulkan execution helpers.
- **AI-ready but not app-specific**: the `llm` crate contains generic model-call utilities. Application/server code is intentionally outside this open-source snapshot.

## Current Language Status

The checked-in syntax suite covers the core language surface now implemented by the parser, compiler, and VM:

- Line comments, block comments, escaped strings, raw strings, and numeric literals in decimal, hex, octal, and binary forms.
- Primitive types: `bool`, `string`, signed and unsigned integers from 8 to 64 bits, `f16`, `f32`, `f64`, tuples, dynamic lists/maps, fixed arrays, and GPU-oriented vector types.
- `let` bindings with identifier, tuple, list, wildcard, and typed patterns.
- `const`, `static`, public items, functions, generic functions, structs, generic structs, `impl` blocks, methods, and associated calls.
- Blocks, expression-oriented `if`/`else`, `for`, `while`, `loop`, `break`, `continue`, and `return`.
- Closures with typed parameters and captured values.
- Arithmetic, comparison, logical, bitwise, indexing, range, cast, assignment, and compound-assignment expressions.
- Imports across `.zs` files, including default `.zs` suffix inference for single-argument imports.

The language is intentionally pragmatic rather than fully Rust-compatible: there is no borrow checker, variables do not need explicit `mut`, and dynamic values remain the default boundary type for host modules.

## Language Overview

Zust source files use the `.zs` suffix.

```zust
fn add(a: i64, b: i64) {
    a + b
}

pub fn main() {
    let value = add(40, 2);
    print(value);
}
```

Functions return the last expression in a block when there is no trailing semicolon. Use `return;` or `return value;` when an early exit is clearer.

### Values

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
let repeated: [u32; 3] = [0u32; 1 + 2];
```

Numeric literals may use explicit suffixes such as `1i32`, `8u64`, or `3.14f32`, and integer literals support `0x`, `0o`, and `0b` prefixes.

String concatenation uses dynamic string conversion at runtime, so expressions such as `"" + idx`, `"" + level + " level"`, and `"" + map.value` are supported.

### Constants And Statics

```zust
pub const ANSWER: i32 = 42i32;
pub static DEFAULT_LIMIT: u32 = 1024u32;

pub const GEM_ATK = "atk";
pub const GEM_TABLE = [
    {key: GEM_ATK, score: 3i32},
];
```

Top-level `const` composite literals may reference constants and statics that have already been declared in the same module or an imported module.

### Patterns And Mutation

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

Variables and fields can be reassigned directly. Compound assignment operators such as `+=`, `-=`, `*=`, `/=`, `%=`, `&=`, `|=`, `^=`, `<<=`, and `>>=` are supported.

### Control Flow

```zust
for i in 0..10 {
    if i % 2 == 0 {
        continue;
    }
    print(i);
}

// for can iterate dynamic lists and maps directly:
for item in some_list { ... }
for value in some_map { ... }

let label = if list.len() > 0 { "non-empty" } else { "empty" };

let value = 0i32;
while value < 100 {
    value += 1;
}

loop {
    break;
}
```

`for in` iterates values directly over dynamic lists and maps. To iterate map keys, use `.keys()`. **`for in` does not iterate strings** (no character-level traversal).

### Language Limitations

Zust intentionally omits several Rust features. Known design differences and current limitations:

| Feature | Zust Behavior | Reason |
|---------|--------------|--------|
| `break value` | Not supported, only `break;` | `break` is a pure control-flow statement |
| `loop` as expression | Not supported | Use variable assignment instead |
| Block `{...}` as expression | `let y = { ... }` produces a parse error | Use `\|\|{...}()` immediate closure call |
| `struct`/`impl`/`const` inside functions | Not supported, top-level only | Compiler does not support local type definitions |
| Nested function (`fn` inside `fn`) | May trigger compiler crash in some cases | Hoist to top level or use closures |
| Integer overflow | Panics (does not wrap) | Safety policy, similar to Rust debug mode |
| `!` on float/Any | Not supported | Only bool (logical NOT) and int/uint (bitwise NOT) |
| `for ch in "hello"` | Does not iterate characters | Use `while` + `get_idx` instead |

### Structs And Impl Blocks

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

### Typed Receiver Method Calls

When the compiler can infer the receiver type, normal method syntax is enough:

```zust
let navmap = NavMap::new("map.svg", "grid.svg");
let path = navmap.get_path(start, stop, false);
```

Values that cross a dynamic boundary, such as `root::get`, handler arguments, or native `Any`/custom values, may only be known as `Any` at compile time. In that case, add a receiver type hint before the method name:

```zust
let navmap = root::get("local/world/newbie_village/navmap");
let path = navmap::<NavMap>::get_path(start, stop, false);
```

The `::<NavMap>::` part only tells the compiler where to look up the native method. It does not convert, clone, or otherwise change the underlying `Dynamic` value. Native/custom objects carried through `Dynamic` are intended for local VM use; JSON and MessagePack serialization cannot persist their in-process Rust state.

### Generics And Closures

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

pub fn demo_closure() {
    let base = 10i32;
    let add_base = |value: i32| {
        value + base
    };

    add_base(identity(5i32))
}

// closures can be called immediately:
pub fn immediate_closure() {
    let r = || { 1i32 + 2i32 }();
    r
}
```

### Imports

```zust
import("qsort", "qsort.zs");
import("syntax_imported");
```

When an import path is omitted by the caller, the compiler defaults to the `.zs` suffix.

### Generic Compile-Time Parameters

Zust supports compile-time type parameters for fixed-size data structures:

```zust
pub struct BigFloat<N> {
    sign: bool,
    exp: i32,
    data: [u32; N],
}
```

See [zusts/bigfloat.zs](zusts/bigfloat.zs) and the GPU Mandelbrot examples under [zusts/gpu](zusts/gpu).

## Runtime Modules

`Vm::with_all()` registers the standard runtime modules listed below. These functions use `Dynamic` at the boundary, so maps, lists, strings, bytes, and numbers can be passed directly from Zust scripts.

### `std`

The standard functions are available without a module prefix:

- `print(value)`: print a dynamic value.
- `log(value)`: write a dynamic value to Rust logs using debug formatting.
- `import(module, path)`: import another `.zs` file or a source object stored in `root`.
- `uuid()`: return a UUID string.
- `rand(start, stop)`: return a random integer or float between `start` and `stop`.

### `Any`

Dynamic values expose common methods:

- Type and copy helpers: `is_map()`, `is_list()`, `is_string()`, `is_null()`, `clone()`, `len()`, `keys()`, `to_string()`.
- List and string helpers: `push(value)`, `pop()`, `split(sep)`, `slice(start, stop, inclusive)`.
- Map and index helpers: `get_idx(idx)`, `set_idx(idx, value)`, `get_key(key)`, `set_key(key, value)`, `del_key(key)`, `contains(value)`, `starts_with(prefix)`.
- Iteration helpers: `iter()`, `next()`.
- Conversion helpers: `Any::from_i64`, `Any::to_i64`, `Any::from_bool`, `Any::to_bool`, `Any::from_f64`, `Any::to_f64`.

Most normal script syntax, such as `list[idx]`, `map.key`, `value.len()`, and dynamic arithmetic, is lowered through these helpers.

### `root`

`root` is an addressable object tree. The default mount is `local`, backed by memory. Redis mounts and a local Fjall-backed `fjall` mount are also supported.

```zust
root::add("local/user/1", {name: "Zust", points: 10});
let user = root::get("local/user/1");

root::add_list("local/events");
root::push("local/events", {kind: "login"});

root::add_map("local/users");
root::insert("local/users", "alice", {age: 20});
```

Functions:

- `root::mount(name, url)`: mount a Redis-backed root path.
- `root::mount_fjall(data_dir)`: mount a local Fjall-backed root path at `fjall`.
- `root::add(path, value)`, `root::get(path)`, `root::remove(path)`, `root::contains(path)`.
- `root::dir(path)`, `root::len(path)`.
- `root::add_list(path)`, `root::push(path, value)`, `root::get_idx(path, idx)`, `root::remove_idx(path, idx)`.
- `root::add_map(path)`, `root::insert(path, key, value)`, `root::get_key(path, key)`, `root::remove_key(path, key)`.
- `root::send(path, value)`, `root::send_idx(path, idx, value)`: send a message to a native or script handler.
- `root::add_fn(path, fn_name)`: register a compiled Zust function as a root handler.

### `http`

`http` provides a small dynamic HTTP client:

```zust
let page = http::get("https://example.com");

let response = http::request({
    method: "POST",
    url: "https://api.example.com/items",
    json: {name: "zust"},
    headers: {"x-client": "zust"}
});
```

Functions:

- `http::get(url)`.
- `http::post(url, body)`.
- `http::request(options)`.

Responses are maps with `status`, `ok`, `url`, `@headers`, and `body`. JSON bodies are decoded into `Dynamic`; text and bytes are returned as strings or bytes.

### `llm`

`llm` wraps generic model, image, audio, and TTS requests. The first argument is provider configuration; the later arguments are request payloads and optional notifier objects.

- `llm::complete(openai, value)`.
- `llm::image(openai, value, notifier)`.
- `llm::audio(openai, value)`.
- `llm::tts(openai, value)`.
- `llm::deep(openai, value, notifier)`: start an async completion task and notify progress through `root`.

### `db`

`db` uses `sqlx::AnyPool` and currently enables PostgreSQL and MySQL. Connection URLs are stored in `root`, usually under `local`.

```zust
root::add("local/db", "postgres://user@127.0.0.1/postgres");
```

When a database path is resolved, `db` first checks the complete path. If the complete path stores a URL, that path is the database connection name. If not, it walks upward until it finds a URL; the remaining suffix is the table name. For example, `local/db/user` uses the connection at `local/db` and table `user`.

Create and drop tables:

```zust
db::create("local/db/user", {
    id: "BIGINT PRIMARY KEY",
    name: "TEXT",
    email: "TEXT",

    "@indexes": [
        ["name"],
        {name: "uniq_user_email", columns: ["email"], unique: true}
    ]
});

db::drop("local/db/user");
```

Index keys may be `@index`, `@indexes`, `index`, `indexes`, or `索引`. Index definitions may be a string, a list of column names, or a map with `name`, `columns`, and `unique`.

Query and execute SQL:

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

Binding rules:

- `data` as a map binds named parameters such as `:id`.
- `data` as a list binds ordered `?` parameters.
- PostgreSQL placeholders are rewritten to `$1`, `$2`, and so on.
- MySQL uses `?` placeholders.
- `select` returns `List<Map>`.
- `exec` returns affected row count, or `-1` on failure.

Transactions use a list of `[sql, data]` steps. Each step shares the same binding rules as `exec`. The return value is the total affected row count; a failure rolls back the transaction and returns `-1`.

```zust
let changed = db::transaction("local/db", [
    ["insert into user (id, name) values (:id, :name)", {id: 1, name: "zhu"}],
    ["update user set name = ? where id = ?", ["zust", 1]]
]);
```

### `gpu`

`gpu` is the VM-facing GPU module registered by `Vm::with_all()`. It keeps the backend pieces split into three paths:

- `gpu::spirv_compile(options)` and `gpu::spirv_check(options)` compile/check Zust source to SPIR-V.
- `gpu::metal_compile(options)` and `gpu::metal_check(options)` compile/check Zust source to Metal source on macOS.
- `gpu::vulkan_run(options)` loads SPIR-V, binds buffers, dispatches Vulkan, and returns requested readbacks.
- `gpu::metal_run(options)` loads Metal source or compiles Zust source, dispatches Metal on macOS, and returns requested readbacks.

Shader compilation stays available without VM runtime execution backends. Enable `zust-vm` feature `vulkan` only when calling `gpu::vulkan_run`, and feature `metal` only when calling `gpu::metal_run`.

The compile options are dynamic maps with `source` or `path`, `module`, `fn`, `workgroup_size`, and optional `generic_args`. Runtime argument descriptors support scalar inputs, typed vector buffers, and raw `bytes` buffers for ABI-packed structs.

### `spirv`

The SPIR-V and Metal backends register GPU-oriented builtins for shader-style programs:

- `spirv::group_id() -> vec3<u32>`.
- `spirv::local_id() -> vec3<u32>`.
- `spirv::barrier()`.
- `spirv::atomic_add(value, delta)`.

See the GPU examples under [zusts/gpu](zusts/gpu).

## Minimal VM Example

The smallest host-side flow is:

1. Import Zust source code into the VM.
2. Ask the VM for a compiled function pointer.
3. Cast the pointer to an `extern "C"` function and call it.

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

Run the checked-in example:

```bash
cargo run -p vm --example minimal_vm
```

## Repository Layout

```text
zust/
├── dynamic/       Runtime value model, JSON, MessagePack, typed vectors
├── parser/        Hand-written lexer and recursive-descent parser
├── compiler/      AST to IR lowering, symbols, type inference
├── vm/            Cranelift JIT backend and host-facing VM API
├── vm-spirv/      SPIR-V code generation backend
├── vm-metal/      Metal shader source generation backend
├── vulkan/        Vulkan execution helpers for SPIR-V kernels
├── root/          Addressable object tree and storage abstractions
├── llm/           Generic LLM request helpers
├── zust-lsp/      Language server for diagnostics, hover, symbols, completion
├── zed-extension/ Zed editor extension and tree-sitter grammar wiring
└── zusts/         Example `.zs` scripts
```

## Example Scripts

- [zusts/test.zs](zusts/test.zs): broad language smoke example
- [zusts/qsort.zs](zusts/qsort.zs): quicksort over a typed vector
- [zusts/bigfloat.zs](zusts/bigfloat.zs): arbitrary-precision float implementation
- [zusts/gpu/bitonic.zs](zusts/gpu/bitonic.zs): GPU bitonic sort
- [zusts/gpu/pathfind.zs](zusts/gpu/pathfind.zs): GPU pathfinding example
- [zusts/gpu/mandelbrot.zs](zusts/gpu/mandelbrot.zs): Mandelbrot kernel

## Build And Check

```bash
cargo check --workspace
cargo run -p vm --example minimal_vm
cargo run -p zusts
```

SPIR-V, Metal, and Vulkan examples may require platform GPU support and driver setup.

## Editor Support

The repository includes:

- `zust-lsp`: a language server for `.zs` files
- `zed-extension`: a Zed dev extension
- `zed-extension/tree-sitter-zust`: tree-sitter grammar source

Build the language server:

```bash
cargo build -p zust-lsp
```

See [zed-extension/README.md](zed-extension/README.md) for Zed setup.

## License

See [LICENSE](LICENSE).
