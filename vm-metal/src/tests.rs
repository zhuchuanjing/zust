use super::*;

#[test]
fn compiles_bitonic_to_metal_source() {
    let source = std::fs::read(format!("{}/../zusts/gpu/bitonic.zs", env!("CARGO_MANIFEST_DIR"))).unwrap();
    let kernel = compile_source_with_workgroup_size(source, "bitonic", "main", [256, 1, 1]).unwrap();
    assert!(kernel.metal.source().contains("kernel void zust_main"));
    assert!(kernel.metal.source().contains("device uint*"));
}

#[test]
fn compiles_pathfind_to_metal_source() {
    let source = std::fs::read(format!("{}/../zusts/gpu/pathfind.zs", env!("CARGO_MANIFEST_DIR"))).unwrap();
    let kernel = compile_source_with_workgroup_size(source, "pathfind", "main", [32, 32, 1]).unwrap();
    assert!(kernel.metal.source().contains("struct PathParams"));
    assert!(kernel.metal.source().contains("device float*"));
}

#[test]
fn emits_padding_for_zust_struct_layout() {
    let kernel = compile_source(
        br#"
        pub struct Params {
            a: u32,
            b: u32,
            c: u32,
        }

        pub fn main(params: Params) {
            return params.a + params.b + params.c;
        }
        "#,
        "metal_struct_padding",
        "main",
    )
    .unwrap();

    let source = kernel.metal.source();
    assert!(source.contains("struct Params"));
    assert!(source.contains("uint a;"));
    assert!(source.contains("uint b;"));
    assert!(source.contains("uint c;"));
    assert!(source.contains("array<uchar, 4> _zust_pad0;"));
}

#[test]
fn compiles_default_math_builtins_to_metal_source() {
    let kernel = compile_source(
        br#"
        pub fn run(x: f32, y: f32) {
            let curved = pow(max(sin(x) + sqrt(y), exp(min(x, y))), 2.0f32);
            let shaped = smoothstep(0.0f32, 1.0f32, clamp(curved, 0.0f32, 1.0f32));
            return fma(mix(x, y, shaped), step(0.5f32, shaped), atan2(y, x));
        }
        "#,
        "metal_default_math",
        "run",
    )
    .unwrap();

    let source = kernel.metal.source();
    assert!(source.contains("sin("));
    assert!(source.contains("sqrt("));
    assert!(source.contains("exp("));
    assert!(source.contains("min("));
    assert!(source.contains("max("));
    assert!(source.contains("pow("));
    assert!(source.contains("smoothstep("));
    assert!(source.contains("clamp("));
    assert!(source.contains("mix("));
    assert!(source.contains("step("));
    assert!(source.contains("fma("));
    assert!(source.contains("atan2("));
}

#[test]
fn compiles_atomic_add_receiver_and_global_fallback_to_metal_source() {
    let kernel = compile_source_with_workgroup_size(
        br#"
        static task_mgr: u32;

        pub fn main(buf: Vec<u32>) {
            let local = spirv::local_id();
            if local[0] == 0u32 {
                task_mgr = 0u32;
            }
            spirv::barrier();
            let first = task_mgr.atomic_add();
            let second = atomic_add(task_mgr, 2u32);
            buf[first] = second;
        }
        "#,
        "metal_atomic_add_fallback",
        "main",
        [4, 1, 1],
    )
    .unwrap();

    let source = kernel.metal.source();
    assert!(source.contains("threadgroup atomic_uint"));
    assert!(source.matches("atomic_fetch_add_explicit(&zust_static_").count() >= 2);
    assert!(source.contains("threadgroup_barrier"));
}
