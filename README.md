# Zust

Zust is a Rust-like scripting language and runtime written in Rust. It keeps the familiar shape of Rust syntax, but removes borrow checking and explicit mutability so scripts can stay compact, dynamic, and easy to generate or transform.

The project is close to a mature open-source release. The current crate version is `0.9.2`.

中文文档: [README.zh.md](README.zh.md)

## Design Ideas

Zust is designed around a small set of practical goals:

- **Rust-shaped, script-friendly syntax**: functions, structs, `impl`, ranges, blocks, and typed literals look familiar to Rust users, while variables remain freely assignable.
- **Dynamic values at the boundary**: the `dynamic` crate provides a `Dynamic` value model for lists, maps, structs, bytes, typed vectors, JSON, and MessagePack.
- **Optional static structure**: scripts can start dynamic and add type annotations where native code generation or GPU backends need stronger shape.
- **Compile scripts into native execution**: the `vm` crate compiles Zust modules with Cranelift and exposes raw function pointers for host Rust code.
- **One language, several execution targets**: the repository contains a JIT backend, SPIR-V generation, Metal source generation, and Vulkan execution helpers.
- **AI-ready but not app-specific**: the `llm` crate contains generic model-call utilities. Application/server code is intentionally outside this open-source snapshot.

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

### Values

```zust
let i = 42;
let f = 3.14f32;
let ok = true;
let text = "hello";
let nothing = null;

let list = [1, 2, 3];
let object = {name: "Zust", version: 0.9};
```

### Control Flow

```zust
for i in 0..10 {
    if i % 2 == 0 {
        continue;
    }
    print(i);
}

let label = if list.len() > 0 { "non-empty" } else { "empty" };
```

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
    p.len2()
}
```

### Imports

```zust
import("qsort", "qsort.zs");
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
