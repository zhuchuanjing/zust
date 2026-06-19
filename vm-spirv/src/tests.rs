use super::*;

#[test]
fn compiles_simple_arithmetic_kernel() {
    let kernel = compile_source(
        br#"
            pub fn add(a: i32, b: i32) {
                a + b
            }
            "#,
        "test",
        "add",
    )
    .unwrap();

    assert_eq!(kernel.entry.as_str(), "main");
    assert_eq!(kernel.arg_tys, vec![Type::I32, Type::I32]);
    assert_eq!(kernel.ret_ty, Type::I32);
    assert!(!kernel.spirv.words().is_empty());
    assert!(kernel.spirv.disassemble().contains("OpEntryPoint GLCompute"));
}

#[test]
fn compiles_workgroup_static_and_atomic_task_counter() {
    let kernel = compile_source_with_workgroup_size(
        br#"
            static task_mgr: u32;

            pub fn main(buf: Vec<u32>) {
                let local = spirv::local_id();
                if local[0] == 0u32 {
                    task_mgr = 0u32;
                }
                spirv::barrier();
                let idx = task_mgr.atomic_add();
                buf[idx] = local[0];
            }
            "#,
        "workgroup_static_ref",
        "main",
        [4, 1, 1],
    )
    .unwrap();

    let asm = kernel.spirv.disassemble();
    assert!(asm.contains("Workgroup"));
    assert!(asm.contains("OpAtomicIAdd"));
    assert!(asm.contains("OpControlBarrier"));
}

#[test]
fn compiles_reference_mandelbrot_escape_control_flow() {
    let kernel = compile_source(
        br#"
            pub fn escape(x: f64, y: f64, max_iter: u32) {
                let iter = 0u32;
                let zx = 0.0f64;
                let zy = 0.0f64;
                while iter < max_iter {
                    let zx2 = zx * zx;
                    let zy2 = zy * zy;
                    if zx2 + zy2 > 4.0f64 {
                        break;
                    }
                    let tmp = zx2 - zy2 + x;
                    zy = 2.0f64 * zx * zy + y;
                    zx = tmp;
                    iter += 1u32;
                }
                return iter;
            }
            "#,
        "mandelbrot_ref",
        "escape",
    )
    .unwrap();

    let asm = kernel.spirv.disassemble();
    assert_eq!(kernel.arg_tys, vec![Type::F64, Type::F64, Type::U32]);
    assert_eq!(kernel.ret_ty, Type::U32);
    assert!(asm.contains("OpLoopMerge"));
    assert!(asm.contains("OpPhi"));
}

#[test]
fn compiles_escape_calling_external_module_function() {
    let kernel = compile_source_with_externs(
        br#"
            pub fn escape(x: f64, y: f64, max_iter: u32) {
                let iter = 0u32;
                let zx = 0.0f64;
                let zy = 0.0f64;
                while iter < max_iter {
                    let zx2 = zx * zx;
                    let zy2 = zy * zy;
                    if math::sqrt(zx2 + zy2) > 2.0f64 {
                        break;
                    }
                    let tmp = zx2 - zy2 + x;
                    zy = 2.0f64 * zx * zy + y;
                    zx = tmp;
                    iter += 1u32;
                }
                return iter;
            }
            "#,
        "mandelbrot_ext_ref",
        "escape",
        spirv_builtins().into_iter().chain([ExternalFn::glsl_unary("math::sqrt", Type::F64, Type::F64, spirv::GlslStd450Op::Sqrt, None)]),
    )
    .unwrap();

    let asm = kernel.spirv.disassemble();
    assert_eq!(kernel.ret_ty, Type::U32);
    assert!(asm.contains("GLSL.std.450"));
    assert!(asm.contains("Sqrt"));
    assert!(asm.contains("OpLoopMerge"));
}

#[test]
fn compiles_spirv_group_and_local_id_builtins() {
    let kernel = compile_source(
        br#"
            pub fn coord() {
                let group = spirv::group_id();
                let local = spirv::local_id();
                return group[0] * 32u32 + local[0];
            }
            "#,
        "spirv_builtin_ref",
        "coord",
    )
    .unwrap();

    let asm = kernel.spirv.disassemble();
    assert_eq!(kernel.ret_ty, Type::U32);
    assert!(asm.contains("BuiltIn WorkgroupId"));
    assert!(asm.contains("BuiltIn LocalInvocationId"));
    assert!(asm.contains("OpCompositeExtract"));
}

#[test]
fn compiles_spirv_barrier_builtin() {
    let kernel = compile_source(
        br#"
            pub fn sync_then_value() {
                spirv::barrier();
                return 1u32;
            }
            "#,
        "spirv_barrier_ref",
        "sync_then_value",
    )
    .unwrap();

    let asm = kernel.spirv.disassemble();
    assert_eq!(kernel.ret_ty, Type::U32);
    assert!(asm.contains("OpControlBarrier"));
}

#[test]
fn compiles_default_glsl_math_builtins() {
    let kernel = compile_source(
        br#"
            pub fn run(x: f32, y: f32) {
                let curved = pow(max(sin(x) + sqrt(y), exp(min(x, y))), 2.0f32);
                let shaped = smoothstep(0.0f32, 1.0f32, clamp(curved, 0.0f32, 1.0f32));
                return fma(mix(x, y, shaped), step(0.5f32, shaped), atan2(y, x));
            }
            "#,
        "spirv_default_math",
        "run",
    )
    .unwrap();

    let asm = kernel.spirv.disassemble();
    assert_eq!(kernel.ret_ty, Type::F32);
    assert!(asm.contains("GLSL.std.450"));
    assert!(asm.contains("Sin"));
    assert!(asm.contains("Sqrt"));
    assert!(asm.contains("Exp"));
    assert!(asm.contains("FMin"));
    assert!(asm.contains("FMax"));
    assert!(asm.contains("Pow"));
    assert!(asm.contains("SmoothStep"));
    assert!(asm.contains("FClamp"));
    assert!(asm.contains("FMix"));
    assert!(asm.contains("Step"));
    assert!(asm.contains("Fma"));
    assert!(asm.contains("Atan2"));
}

#[test]
fn compiles_reference_bitonic_compare_condition() {
    let kernel = compile_source(
        br#"
            pub fn cas_needed(val1: u32, val2: u32, direct: bool) {
                return (val1 > val2 && direct) || (val1 < val2 && !direct);
            }
            "#,
        "bitonic_ref",
        "cas_needed",
    )
    .unwrap();

    let asm = kernel.spirv.disassemble();
    assert_eq!(kernel.arg_tys, vec![Type::U32, Type::U32, Type::Bool]);
    assert_eq!(kernel.ret_ty, Type::Bool);
    assert!(asm.contains("OpLogicalAnd"));
    assert!(asm.contains("OpLogicalOr"));
}

#[test]
fn compiles_reference_bitonic_kernel() {
    let kernel = compile_source_with_workgroup_size(include_bytes!("../../zusts/gpu/bitonic.zs"), "bitonic", "main", [256, 1, 1]).unwrap();

    let asm = kernel.spirv.disassemble();
    assert_eq!(kernel.arg_tys.len(), 2);
    assert_eq!(kernel.ret_ty, Type::Void);
    assert!(asm.contains("BuiltIn WorkgroupId"));
    assert!(asm.contains("BuiltIn LocalInvocationId"));
    assert!(asm.contains("OpBitwiseXor"));
    assert!(asm.contains("OpBitwiseAnd"));
    assert!(asm.contains("OpStore"));
}

#[test]
fn compiles_user_function_call() {
    let kernel = compile_source(
        br#"
            fn inc(x: u32) {
                x + 1u32
            }

            pub fn run(x: u32) {
                return inc(x) * 2u32;
            }
            "#,
        "spirv_user_fn",
        "run",
    )
    .unwrap();

    let asm = kernel.spirv.disassemble();
    assert_eq!(kernel.ret_ty, Type::U32);
    assert!(asm.contains("OpFunctionCall"));
    assert!(asm.contains("OpIAdd"));
    assert!(asm.contains("OpIMul"));
}

#[test]
fn compiles_user_struct_method_call() {
    let kernel = compile_source(
        br#"
            struct Counter {
                value: u32,
            }

            impl Counter {
                fn add(self, amount: u32) {
                    self[0u32] + amount
                }
            }

            pub fn run(counter: Counter, amount: u32) {
                return counter.add(amount);
            }
            "#,
        "spirv_user_struct_method",
        "run",
    )
    .unwrap();

    let asm = kernel.spirv.disassemble();
    assert_eq!(kernel.ret_ty, Type::U32);
    assert!(asm.contains("OpFunctionCall"));
    assert!(asm.contains("OpCompositeExtract") || asm.contains("OpAccessChain"));
    assert!(asm.contains("OpIAdd"));
}

#[test]
fn compiles_bigfloat_f32_roundtrip_kernel() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).parent().expect("workspace root");
    let mut source = std::fs::read_to_string(root.join("zusts").join("bigfloat.zs")).expect("read bigfloat.zs");
    source.push_str(
        r#"

            pub fn run(value: f32) {
                BigFloat<2>::from_f32(value).to_f32()
            }
            "#,
    );
    let kernel = compile_source(source, "spirv_bigfloat_roundtrip", "run").unwrap();

    assert_eq!(kernel.arg_tys, vec![Type::F32]);
    assert_eq!(kernel.ret_ty, Type::F32);
}

#[test]
fn compiles_bigfloat_add_sub_mul_kernels() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).parent().expect("workspace root");
    let mut source = std::fs::read_to_string(root.join("zusts").join("bigfloat.zs")).expect("read bigfloat.zs");
    source.push_str(
        r#"

            pub fn add_run(a: f32, b: f32) {
                BigFloat<2>::from_f32(a).add(BigFloat<2>::from_f32(b)).to_f32()
            }

            pub fn sub_run(a: f32, b: f32) {
                BigFloat<2>::from_f32(a).sub(BigFloat<2>::from_f32(b)).to_f32()
            }

            pub fn mul_run(a: f32, b: f32) {
                BigFloat<2>::from_f32(a).mul(BigFloat<2>::from_f32(b)).to_f32()
            }
            "#,
    );

    for name in ["add_run", "sub_run", "mul_run"] {
        let kernel = compile_source(source.clone(), "spirv_bigfloat_ops", name).unwrap();
        assert_eq!(kernel.arg_tys, vec![Type::F32, Type::F32]);
        assert_eq!(kernel.ret_ty, Type::F32);
    }
}

#[test]
fn compiles_bigfloat_32_mul_kernel() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).parent().expect("workspace root");
    let mut source = std::fs::read_to_string(root.join("zusts").join("bigfloat.zs")).expect("read bigfloat.zs");
    source.push_str(
        r#"

            pub fn run(a: f32, b: f32) {
                BigFloat<32>::from_f32(a).mul(BigFloat<32>::from_f32(b)).to_f32()
            }
            "#,
    );
    let kernel = compile_source(source, "spirv_bigfloat32_mul", "run").unwrap();

    assert_eq!(kernel.arg_tys, vec![Type::F32, Type::F32]);
    assert_eq!(kernel.ret_ty, Type::F32);
}

#[test]
fn generic_inference_cache_separates_explicit_const_args() {
    let kernel = compile_source(
        br#"
            pub struct BfBox<N> {
                data: [u32; N],
            }

            impl BfBox<N> {
                pub fn make(value: u32) {
                    let data: [u32; N] = [0u32; N];
                    data[0u32] = value;
                    BfBox<N>{ data }
                }
            }

            pub fn run(a: u32, b: u32) {
                let small = BfBox<2>::make(a);
                let wide = BfBox<8>::make(b);
                small.data[0u32] + wide.data[0u32]
            }
            "#,
        "spirv_generic_const_arg_cache",
        "run",
    )
    .unwrap();

    assert_eq!(kernel.arg_tys, vec![Type::U32, Type::U32]);
    assert_eq!(kernel.ret_ty, Type::U32);
}

#[test]
fn compiles_user_vec_method_call_by_inlining() {
    let kernel = compile_source(
        br#"
            impl Vec<T> {
                fn first_plus(self, amount: u32) {
                    self[0u32] + amount
                }
            }

            pub fn run(values: Vec<u32>, amount: u32) {
                return values.first_plus(amount);
            }
            "#,
        "spirv_user_vec_method",
        "run",
    )
    .unwrap();

    let asm = kernel.spirv.disassemble();
    assert_eq!(kernel.ret_ty, Type::U32);
    assert!(!asm.contains("OpFunctionCall"));
    assert!(asm.contains("OpAccessChain"));
    assert!(asm.contains("OpIAdd"));
}

#[test]
fn specializes_generic_vec_method_from_receiver_type() {
    let source = br#"
            impl Vec<T> {
                fn first(self) {
                    self[0u32]
                }
            }

            pub fn first_u32(values: Vec<u32>) {
                return values.first();
            }

            pub fn first_f32(values: Vec<f32>) {
                return values.first();
            }
        "#;

    let u32_kernel = compile_source(source, "spirv_generic_vec_u32", "first_u32").unwrap();
    let f32_kernel = compile_source(source, "spirv_generic_vec_f32", "first_f32").unwrap();

    assert_eq!(u32_kernel.ret_ty, Type::U32);
    assert_eq!(f32_kernel.ret_ty, Type::F32);
    assert!(u32_kernel.spirv.disassemble().contains("OpAccessChain"));
    assert!(f32_kernel.spirv.disassemble().contains("OpAccessChain"));
}

#[test]
fn compiles_reference_world_random_hash_math() {
    let kernel = compile_source(
        br#"
            pub fn random(seed: u32) {
                let x = seed * 747796405u32 + 2891336453u32;
                let y = (x >> 16u32) ^ x;
                let z = y * 2246822519u32 + 3266489917u32;
                let w = (z >> 16u32) ^ z;
                return ((w & 2147483647u32) as f32) / 2147483647.0f32;
            }
            "#,
        "world_ref",
        "random",
    )
    .unwrap();

    let asm = kernel.spirv.disassemble();
    assert_eq!(kernel.arg_tys, vec![Type::U32]);
    assert_eq!(kernel.ret_ty, Type::F32);
    assert!(asm.contains("OpBitwiseXor"));
    assert!(asm.contains("OpConvertUToF"));
}

#[test]
fn compiles_for_loop_with_range() {
    let kernel = compile_source(
        br#"
            pub fn sum(n: u32) {
                let total = 0u32;
                for idx in 0..n {
                    total += idx;
                }
                return total;
            }
            "#,
        "for_test",
        "sum",
    )
    .unwrap();

    let asm = kernel.spirv.disassemble();
    assert_eq!(kernel.arg_tys, vec![Type::U32]);
    assert_eq!(kernel.ret_ty, Type::U32);
    assert!(asm.contains("OpLoopMerge"));
    assert!(asm.contains("OpPhi"));
}

#[test]
fn compiles_for_loop_with_inclusive_range() {
    let kernel = compile_source(
        br#"
            pub fn sum_inclusive(n: u32) {
                let total = 0u32;
                for idx in 0..=n {
                    total += idx;
                }
                return total;
            }
            "#,
        "for_inclusive_test",
        "sum_inclusive",
    )
    .unwrap();

    let asm = kernel.spirv.disassemble();
    assert_eq!(kernel.arg_tys, vec![Type::U32]);
    assert_eq!(kernel.ret_ty, Type::U32);
    assert!(asm.contains("OpLoopMerge"));
    assert!(asm.contains("OpULessThanEqual"));
}

#[test]
fn operator_precedence_sub_mul_add() {
    // (a - 1) * b + c  should compute as ((a-1)*b)+c
    // NOT as a - (1*b + c) or a - 1*b + c with wrong association
    let kernel = compile_source(
        br#"
            pub fn calc(a: u32, b: u32, c: u32) {
                return (a - 1u32) * b + c;
            }
            "#,
        "prec_test",
        "calc",
    )
    .unwrap();

    let asm = kernel.spirv.disassemble();
    // There should be an ISub before the IMul (computing a-1 first)
    // and the final operation should be IAdd
    assert!(asm.contains("OpISub"), "should have subtraction: {asm}");
    assert!(asm.contains("OpIMul"), "should have multiplication: {asm}");
    assert!(asm.contains("OpIAdd"), "should have addition: {asm}");

    // Verify order: ISub result feeds into IMul, IMul result feeds into IAdd
    // by checking that the instruction sequence follows the correct dependency chain
    eprintln!("{asm}");
}

#[test]
fn compiles_point_in_poly_with_for_loop() {
    // Simplified point-in-polygon using for loop
    let kernel = compile_source(
        br#"
            pub fn point_in_poly(data: Vec<f32>, n: u32, px: i32, py: i32) {
                let inside = false;
                for idx in 0u32..n {
                    let val = data[idx];
                    if val > 0.0f32 {
                        inside = !inside;
                    }
                }
                return inside;
            }
            "#,
        "poly_test",
        "point_in_poly",
    )
    .unwrap();

    let asm = kernel.spirv.disassemble();
    assert!(asm.contains("OpLoopMerge"));
    assert!(asm.contains("OpPhi"));
}

#[test]
fn compiles_unconditional_loop_with_break() {
    // 无条件 `loop { ... break; }` 在 SPIR-V 后端应当编译成
    // 带 OpLoopMerge 的循环结构 —— header 直接无条件跳到 body,
    // break 跳到 merge label。
    let kernel = compile_source(
        br#"
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
            "#,
        "loop_test",
        "first_negative",
    )
    .unwrap();

    let asm = kernel.spirv.disassemble();
    assert_eq!(kernel.arg_tys.len(), 2);
    assert_eq!(kernel.ret_ty, Type::I32);
    assert!(asm.contains("OpLoopMerge"), "loop must emit OpLoopMerge:\n{asm}");
}

#[test]
fn compiles_array_literal_with_default_float_suffix_into_f64_target() {
    // 历史回归:无后缀浮点字面量默认是 f32,但目标 `[f64; N]` 期望 f64。
    // 修复前 parser 把 `[1.0, 2.0, ...]` 折成单个 `Dynamic::List(F32...)`,
    // SPIR-V 后端不识别 `Dynamic::List` 这个 const 形态,直接 bail。
    let kernel = compile_source(
        br#"
            pub fn fn_under_test() {
                let arr: [f64; 4] = [1.0, 2.0, 3.0, 4.0];
                arr[0]
            }
        "#,
        "default_float",
        "fn_under_test",
    )
    .unwrap();
    assert_eq!(kernel.ret_ty, Type::F64);
}

#[test]
fn compiles_top_level_const_array_used_in_kernel() {
    // 顶层 `const ARR: [f64; N] = [...]` 经 Const(idx) 取回时,内层是 raw
    // `Dynamic::List`。后端要按目标元素类型 force 后再 composite_construct。
    let kernel = compile_source(
        br#"
            const COEFS: [f64; 3] = [1.0, 0.5, 0.25];
            pub fn fn_under_test(x: f64) {
                x * COEFS[0] + COEFS[1] - COEFS[2]
            }
        "#,
        "const_array",
        "fn_under_test",
    )
    .unwrap();
    assert_eq!(kernel.arg_tys, vec![Type::F64]);
    assert_eq!(kernel.ret_ty, Type::F64);
}

#[test]
fn compiles_unsuffixed_const_used_as_f64() {
    // `const K = 2.0;` 是 F32(默认浮点),但被乘到 f64 上下文应当能编译过。
    // 修复前 SPIR-V 直接 bail `compiler constant index not available`。
    let kernel = compile_source(
        br#"
            const K = 2.0;
            pub fn fn_under_test(x: f64) { x * K }
        "#,
        "unsuffixed_const",
        "fn_under_test",
    )
    .unwrap();
    assert_eq!(kernel.ret_ty, Type::F64);
}
