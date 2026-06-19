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

#[test]
fn compiles_unconditional_loop_to_while_true() {
    // 无条件 `loop { ... break; }` 在 Metal 后端应当 lower 成 MSL 的
    // `while (true) { ... break; }`,break/continue 直接复用 C 控制流。
    let source = br#"
        pub fn first_negative(data: Vec<i32>, n: u32) {
            let i = 0u32;
            let result = -1i32;
            loop {
                if i >= n {
                    break;
                }
                if data[i] < 0i32 {
                    result = data[i];
                    break;
                }
                i += 1u32;
            }
            return result;
        }
    "#;
    let kernel = compile_source(source.to_vec(), "loop_test", "first_negative").unwrap();
    let msl = kernel.metal.source();
    assert!(msl.contains("while (true)"), "loop must lower to `while (true)`:\n{msl}");
    assert!(msl.contains("break;"));
}

#[test]
fn compiles_array_literal_with_default_float_suffix_into_f64_target() {
    // 历史回归:`[1.0, 2.0, ...]` 默认 F32 → 目标 `[f64; N]`。修复前 Metal 后端
    // 拿到 raw `Dynamic::List` 直接 bail。
    let source = br#"
        pub fn fn_under_test() {
            let arr: [f64; 4] = [1.0, 2.0, 3.0, 4.0];
            arr[0]
        }
    "#;
    let kernel = compile_source(source.to_vec(), "default_float", "fn_under_test").unwrap();
    let msl = kernel.metal.source();
    assert!(msl.contains("double"), "expected double in MSL:\n{msl}");
}

#[test]
fn compiles_top_level_const_array_used_in_kernel() {
    let source = br#"
        const COEFS: [f64; 3] = [1.0, 0.5, 0.25];
        pub fn fn_under_test(x: f64) {
            x * COEFS[0] + COEFS[1] - COEFS[2]
        }
    "#;
    let kernel = compile_source(source.to_vec(), "const_array", "fn_under_test").unwrap();
    assert_eq!(kernel.arg_tys, vec![Type::F64]);
    assert_eq!(kernel.ret_ty, Type::F64);
}
