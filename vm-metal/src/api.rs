use dynamic::Type;
use smol_str::SmolStr;
use std::rc::Rc;

#[derive(Debug, Clone)]
pub struct MetalModule {
    pub(crate) source: String,
}

impl MetalModule {
    pub fn source(&self) -> &str {
        &self.source
    }

    pub fn into_source(self) -> String {
        self.source
    }
}

#[derive(Debug, Clone)]
pub struct Kernel {
    pub metal: MetalModule,
    pub entry: SmolStr,
    pub arg_tys: Vec<Type>,
    pub ret_ty: Type,
    pub workgroup_size: [u32; 3],
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
    Builtin(BuiltinFn),
    MathUnary(&'static str),
    MathBinary(&'static str),
    MathFloatBinary(&'static str),
    MathTernary(&'static str),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum BuiltinFn {
    GroupId,
    LocalId,
    Barrier,
    AtomicAdd,
}

impl ExternalFn {
    pub fn builtin(full_name: impl Into<SmolStr>, arg_tys: Vec<Type>, ret_ty: Type, builtin: BuiltinFn) -> Self {
        Self { full_name: full_name.into(), arg_tys, ret_ty, kind: ExternalFnKind::Builtin(builtin) }
    }

    pub fn math_unary(full_name: impl Into<SmolStr>, arg_ty: Type, ret_ty: Type, name: &'static str) -> Self {
        Self { full_name: full_name.into(), arg_tys: vec![arg_ty], ret_ty, kind: ExternalFnKind::MathUnary(name) }
    }

    pub fn math_binary(full_name: impl Into<SmolStr>, arg_ty: Type, ret_ty: Type, name: &'static str) -> Self {
        Self { full_name: full_name.into(), arg_tys: vec![arg_ty.clone(), arg_ty], ret_ty, kind: ExternalFnKind::MathBinary(name) }
    }

    pub fn math_float_binary(full_name: impl Into<SmolStr>, arg_ty: Type, ret_ty: Type, name: &'static str) -> Self {
        Self { full_name: full_name.into(), arg_tys: vec![arg_ty.clone(), arg_ty], ret_ty, kind: ExternalFnKind::MathFloatBinary(name) }
    }

    pub fn math_ternary(full_name: impl Into<SmolStr>, arg_ty: Type, ret_ty: Type, name: &'static str) -> Self {
        Self { full_name: full_name.into(), arg_tys: vec![arg_ty.clone(), arg_ty.clone(), arg_ty], ret_ty, kind: ExternalFnKind::MathTernary(name) }
    }
}

pub fn metal_builtins() -> Vec<ExternalFn> {
    vec![
        ExternalFn::builtin("spirv::group_id", vec![], Type::Vec(Rc::new(Type::U32), 3), BuiltinFn::GroupId),
        ExternalFn::builtin("spirv::local_id", vec![], Type::Vec(Rc::new(Type::U32), 3), BuiltinFn::LocalId),
        ExternalFn::builtin("spirv::barrier", vec![], Type::Void, BuiltinFn::Barrier),
        ExternalFn::builtin("spirv::atomic_add", vec![Type::U32, Type::U32], Type::U32, BuiltinFn::AtomicAdd),
        ExternalFn::math_unary("abs", Type::F32, Type::F32, "abs"),
        ExternalFn::math_unary("sign", Type::F32, Type::F32, "sign"),
        ExternalFn::math_unary("floor", Type::F32, Type::F32, "floor"),
        ExternalFn::math_unary("ceil", Type::F32, Type::F32, "ceil"),
        ExternalFn::math_unary("round", Type::F32, Type::F32, "round"),
        ExternalFn::math_unary("round_even", Type::F32, Type::F32, "rint"),
        ExternalFn::math_unary("trunc", Type::F32, Type::F32, "trunc"),
        ExternalFn::math_unary("fract", Type::F32, Type::F32, "fract"),
        ExternalFn::math_unary("radians", Type::F32, Type::F32, "radians"),
        ExternalFn::math_unary("degrees", Type::F32, Type::F32, "degrees"),
        ExternalFn::math_unary("sin", Type::F32, Type::F32, "sin"),
        ExternalFn::math_unary("cos", Type::F32, Type::F32, "cos"),
        ExternalFn::math_unary("tan", Type::F32, Type::F32, "tan"),
        ExternalFn::math_unary("asin", Type::F32, Type::F32, "asin"),
        ExternalFn::math_unary("acos", Type::F32, Type::F32, "acos"),
        ExternalFn::math_unary("atan", Type::F32, Type::F32, "atan"),
        ExternalFn::math_unary("sinh", Type::F32, Type::F32, "sinh"),
        ExternalFn::math_unary("cosh", Type::F32, Type::F32, "cosh"),
        ExternalFn::math_unary("tanh", Type::F32, Type::F32, "tanh"),
        ExternalFn::math_unary("asinh", Type::F32, Type::F32, "asinh"),
        ExternalFn::math_unary("acosh", Type::F32, Type::F32, "acosh"),
        ExternalFn::math_unary("atanh", Type::F32, Type::F32, "atanh"),
        ExternalFn::math_unary("exp", Type::F32, Type::F32, "exp"),
        ExternalFn::math_unary("log", Type::F32, Type::F32, "log"),
        ExternalFn::math_unary("exp2", Type::F32, Type::F32, "exp2"),
        ExternalFn::math_unary("log2", Type::F32, Type::F32, "log2"),
        ExternalFn::math_unary("sqrt", Type::F32, Type::F32, "sqrt"),
        ExternalFn::math_unary("inverse_sqrt", Type::F32, Type::F32, "rsqrt"),
        ExternalFn::math_float_binary("atan2", Type::F32, Type::F32, "atan2"),
        ExternalFn::math_float_binary("pow", Type::F32, Type::F32, "pow"),
        ExternalFn::math_float_binary("step", Type::F32, Type::F32, "step"),
        ExternalFn::math_binary("min", Type::F32, Type::F32, "min"),
        ExternalFn::math_binary("max", Type::F32, Type::F32, "max"),
        ExternalFn::math_ternary("clamp", Type::F32, Type::F32, "clamp"),
        ExternalFn::math_ternary("mix", Type::F32, Type::F32, "mix"),
        ExternalFn::math_ternary("smoothstep", Type::F32, Type::F32, "smoothstep"),
        ExternalFn::math_ternary("fma", Type::F32, Type::F32, "fma"),
    ]
}
