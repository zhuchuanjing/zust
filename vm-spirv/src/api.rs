use dynamic::Type;
use rspirv::{binary::Disassemble, dr::Module};
use smol_str::SmolStr;
use std::rc::Rc;

#[derive(Debug, Clone)]
pub struct SpirvModule {
    pub(crate) words: Vec<u32>,
    pub(crate) module: Module,
}

impl SpirvModule {
    pub fn words(&self) -> &[u32] {
        &self.words
    }

    pub fn into_words(self) -> Vec<u32> {
        self.words
    }

    pub fn disassemble(&self) -> String {
        self.module.disassemble()
    }
}

#[derive(Debug, Clone)]
pub struct Kernel {
    pub spirv: SpirvModule,
    pub entry: SmolStr,
    pub arg_tys: Vec<Type>,
    pub ret_ty: Type,
}

#[derive(Debug, Clone)]
pub struct ExternalFn {
    pub full_name: SmolStr,
    pub arg_tys: Vec<Type>,
    pub ret_ty: Type,
    pub kind: ExternalFnKind,
}

#[derive(Debug, Clone)]
pub enum ExternalFnKind {
    GlslUnary { float_op: spirv::GlslStd450Op, signed_int_op: Option<spirv::GlslStd450Op> },
    GlslBinary { float_op: spirv::GlslStd450Op, signed_int_op: spirv::GlslStd450Op, unsigned_int_op: spirv::GlslStd450Op },
    GlslFloatBinary { op: spirv::GlslStd450Op },
    GlslFloatTernary { op: spirv::GlslStd450Op },
    Builtin(BuiltinFn),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum BuiltinFn {
    GroupId,
    LocalId,
    Barrier,
    AtomicAdd,
}

impl ExternalFn {
    pub fn glsl_unary(full_name: impl Into<SmolStr>, arg_ty: Type, ret_ty: Type, float_op: spirv::GlslStd450Op, signed_int_op: Option<spirv::GlslStd450Op>) -> Self {
        Self { full_name: full_name.into(), arg_tys: vec![arg_ty], ret_ty, kind: ExternalFnKind::GlslUnary { float_op, signed_int_op } }
    }

    pub fn glsl_binary(full_name: impl Into<SmolStr>, arg_ty: Type, ret_ty: Type, float_op: spirv::GlslStd450Op, signed_int_op: spirv::GlslStd450Op, unsigned_int_op: spirv::GlslStd450Op) -> Self {
        Self { full_name: full_name.into(), arg_tys: vec![arg_ty.clone(), arg_ty], ret_ty, kind: ExternalFnKind::GlslBinary { float_op, signed_int_op, unsigned_int_op } }
    }

    pub fn glsl_float_binary(full_name: impl Into<SmolStr>, arg_ty: Type, ret_ty: Type, op: spirv::GlslStd450Op) -> Self {
        Self { full_name: full_name.into(), arg_tys: vec![arg_ty.clone(), arg_ty], ret_ty, kind: ExternalFnKind::GlslFloatBinary { op } }
    }

    pub fn glsl_float_ternary(full_name: impl Into<SmolStr>, arg_ty: Type, ret_ty: Type, op: spirv::GlslStd450Op) -> Self {
        Self { full_name: full_name.into(), arg_tys: vec![arg_ty.clone(), arg_ty.clone(), arg_ty], ret_ty, kind: ExternalFnKind::GlslFloatTernary { op } }
    }

    pub fn builtin(full_name: impl Into<SmolStr>, arg_tys: Vec<Type>, ret_ty: Type, builtin: BuiltinFn) -> Self {
        Self { full_name: full_name.into(), arg_tys, ret_ty, kind: ExternalFnKind::Builtin(builtin) }
    }
}

pub fn spirv_builtins() -> Vec<ExternalFn> {
    vec![
        ExternalFn::builtin("spirv::group_id", vec![], Type::Vec(Rc::new(Type::U32), 3), BuiltinFn::GroupId),
        ExternalFn::builtin("spirv::local_id", vec![], Type::Vec(Rc::new(Type::U32), 3), BuiltinFn::LocalId),
        ExternalFn::builtin("spirv::barrier", vec![], Type::Void, BuiltinFn::Barrier),
        ExternalFn::builtin("spirv::atomic_add", vec![Type::U32, Type::U32], Type::U32, BuiltinFn::AtomicAdd),
        ExternalFn::glsl_unary("abs", Type::F32, Type::F32, spirv::GlslStd450Op::FAbs, Some(spirv::GlslStd450Op::SAbs)),
        ExternalFn::glsl_unary("sign", Type::F32, Type::F32, spirv::GlslStd450Op::FSign, Some(spirv::GlslStd450Op::SSign)),
        ExternalFn::glsl_unary("floor", Type::F32, Type::F32, spirv::GlslStd450Op::Floor, None),
        ExternalFn::glsl_unary("ceil", Type::F32, Type::F32, spirv::GlslStd450Op::Ceil, None),
        ExternalFn::glsl_unary("round", Type::F32, Type::F32, spirv::GlslStd450Op::Round, None),
        ExternalFn::glsl_unary("round_even", Type::F32, Type::F32, spirv::GlslStd450Op::RoundEven, None),
        ExternalFn::glsl_unary("trunc", Type::F32, Type::F32, spirv::GlslStd450Op::Trunc, None),
        ExternalFn::glsl_unary("fract", Type::F32, Type::F32, spirv::GlslStd450Op::Fract, None),
        ExternalFn::glsl_unary("radians", Type::F32, Type::F32, spirv::GlslStd450Op::Radians, None),
        ExternalFn::glsl_unary("degrees", Type::F32, Type::F32, spirv::GlslStd450Op::Degrees, None),
        ExternalFn::glsl_unary("sin", Type::F32, Type::F32, spirv::GlslStd450Op::Sin, None),
        ExternalFn::glsl_unary("cos", Type::F32, Type::F32, spirv::GlslStd450Op::Cos, None),
        ExternalFn::glsl_unary("tan", Type::F32, Type::F32, spirv::GlslStd450Op::Tan, None),
        ExternalFn::glsl_unary("asin", Type::F32, Type::F32, spirv::GlslStd450Op::Asin, None),
        ExternalFn::glsl_unary("acos", Type::F32, Type::F32, spirv::GlslStd450Op::Acos, None),
        ExternalFn::glsl_unary("atan", Type::F32, Type::F32, spirv::GlslStd450Op::Atan, None),
        ExternalFn::glsl_unary("sinh", Type::F32, Type::F32, spirv::GlslStd450Op::Sinh, None),
        ExternalFn::glsl_unary("cosh", Type::F32, Type::F32, spirv::GlslStd450Op::Cosh, None),
        ExternalFn::glsl_unary("tanh", Type::F32, Type::F32, spirv::GlslStd450Op::Tanh, None),
        ExternalFn::glsl_unary("asinh", Type::F32, Type::F32, spirv::GlslStd450Op::Asinh, None),
        ExternalFn::glsl_unary("acosh", Type::F32, Type::F32, spirv::GlslStd450Op::Acosh, None),
        ExternalFn::glsl_unary("atanh", Type::F32, Type::F32, spirv::GlslStd450Op::Atanh, None),
        ExternalFn::glsl_unary("exp", Type::F32, Type::F32, spirv::GlslStd450Op::Exp, None),
        ExternalFn::glsl_unary("log", Type::F32, Type::F32, spirv::GlslStd450Op::Log, None),
        ExternalFn::glsl_unary("exp2", Type::F32, Type::F32, spirv::GlslStd450Op::Exp2, None),
        ExternalFn::glsl_unary("log2", Type::F32, Type::F32, spirv::GlslStd450Op::Log2, None),
        ExternalFn::glsl_unary("sqrt", Type::F32, Type::F32, spirv::GlslStd450Op::Sqrt, None),
        ExternalFn::glsl_unary("inverse_sqrt", Type::F32, Type::F32, spirv::GlslStd450Op::InverseSqrt, None),
        ExternalFn::glsl_float_binary("atan2", Type::F32, Type::F32, spirv::GlslStd450Op::Atan2),
        ExternalFn::glsl_float_binary("pow", Type::F32, Type::F32, spirv::GlslStd450Op::Pow),
        ExternalFn::glsl_float_binary("step", Type::F32, Type::F32, spirv::GlslStd450Op::Step),
        ExternalFn::glsl_binary("min", Type::F32, Type::F32, spirv::GlslStd450Op::FMin, spirv::GlslStd450Op::SMin, spirv::GlslStd450Op::UMin),
        ExternalFn::glsl_binary("max", Type::F32, Type::F32, spirv::GlslStd450Op::FMax, spirv::GlslStd450Op::SMax, spirv::GlslStd450Op::UMax),
        ExternalFn::glsl_float_ternary("clamp", Type::F32, Type::F32, spirv::GlslStd450Op::FClamp),
        ExternalFn::glsl_float_ternary("mix", Type::F32, Type::F32, spirv::GlslStd450Op::FMix),
        ExternalFn::glsl_float_ternary("smoothstep", Type::F32, Type::F32, spirv::GlslStd450Op::SmoothStep),
        ExternalFn::glsl_float_ternary("fma", Type::F32, Type::F32, spirv::GlslStd450Op::Fma),
    ]
}
