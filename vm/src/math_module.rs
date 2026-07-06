//! `math::*` —— 浮点数学函数。
//!
//! 为什么是独立模块而不是 Any 方法:数值运算不是对象方法,`x.sin()` 读起来
//! 不如 `math::sin(x)` 自然,而且 `std::sqrt` 已经确立了顶层函数的先例。
//!
//! 所有三角函数按**弧度**(不是度),与 Rust 标准库一致。脚本要把度转弧度用
//! `deg * math::pi() / 180.0`。
//!
//! 单参数函数(`sin/cos/...`)直接转调 `f64::` 同名方法;`floor/ceil/round`
//! 返 `f64`(不是 i64),脚本侧需要整数时用 `as i64` 转换 —— 这跟 `std::sqrt`
//! 的处理一致,避免在这里硬塞类型转换。
use dynamic::Type;

// 单参 f64 -> f64,转调 f64 同名方法。
macro_rules! math_unary {
    ($name:ident, $method:ident) => {
        extern "C" fn $name(value: f64) -> f64 {
            f64::$method(value)
        }
    };
}

math_unary!(sin, sin);
math_unary!(cos, cos);
math_unary!(tan, tan);
math_unary!(asin, asin);
math_unary!(acos, acos);
math_unary!(atan, atan);
math_unary!(ln, ln);
math_unary!(log10, log10);
math_unary!(exp, exp);
math_unary!(sqrt, sqrt);
math_unary!(abs, abs);
math_unary!(floor, floor);
math_unary!(ceil, ceil);
math_unary!(round, round);

extern "C" fn atan2(y: f64, x: f64) -> f64 {
    f64::atan2(y, x)
}

extern "C" fn pow(base: f64, exp: f64) -> f64 {
    f64::powf(base, exp)
}

extern "C" fn max(a: f64, b: f64) -> f64 {
    a.max(b)
}

extern "C" fn min(a: f64, b: f64) -> f64 {
    a.min(b)
}

extern "C" fn pi() -> f64 {
    std::f64::consts::PI
}

pub const MATH_NATIVE: &[(&str, &[Type], Type, *const u8)] = &[
    ("sin", &[Type::F64], Type::F64, sin as *const u8),
    ("cos", &[Type::F64], Type::F64, cos as *const u8),
    ("tan", &[Type::F64], Type::F64, tan as *const u8),
    ("asin", &[Type::F64], Type::F64, asin as *const u8),
    ("acos", &[Type::F64], Type::F64, acos as *const u8),
    ("atan", &[Type::F64], Type::F64, atan as *const u8),
    ("ln", &[Type::F64], Type::F64, ln as *const u8),
    ("log10", &[Type::F64], Type::F64, log10 as *const u8),
    ("exp", &[Type::F64], Type::F64, exp as *const u8),
    ("sqrt", &[Type::F64], Type::F64, sqrt as *const u8),
    ("abs", &[Type::F64], Type::F64, abs as *const u8),
    ("floor", &[Type::F64], Type::F64, floor as *const u8),
    ("ceil", &[Type::F64], Type::F64, ceil as *const u8),
    ("round", &[Type::F64], Type::F64, round as *const u8),
    ("atan2", &[Type::F64, Type::F64], Type::F64, atan2 as *const u8),
    ("pow", &[Type::F64, Type::F64], Type::F64, pow as *const u8),
    ("max", &[Type::F64, Type::F64], Type::F64, max as *const u8),
    ("min", &[Type::F64, Type::F64], Type::F64, min as *const u8),
    ("pi", &[], Type::F64, pi as *const u8),
];
