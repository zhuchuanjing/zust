use super::Dynamic;

use std::ops::{Neg, Not};
impl Neg for Dynamic {
    type Output = Self;
    fn neg(self) -> Self::Output {
        use Dynamic::*;
        match self {
            I8(i) => I8(i.wrapping_neg()),
            I16(i) => I16(i.wrapping_neg()),
            I32(i) => I32(i.wrapping_neg()),
            I64(i) => I64(i.wrapping_neg()),
            F32(f) => F32(-f),
            F64(f) => F64(-f),
            _ => Null,
        }
    }
}

impl Not for Dynamic {
    type Output = Self;
    fn not(self) -> Self::Output {
        match self {
            Self::Bool(b) => Self::Bool(!b),
            Self::I8(i) => Self::I8(!i),
            Self::I16(i) => Self::I16(!i),
            Self::I32(i) => Self::I32(!i),
            Self::I64(i) => Self::I64(!i),
            Self::U8(i) => Self::U8(!i),
            Self::U16(i) => Self::U16(!i),
            Self::U32(i) => Self::U32(!i),
            Self::U64(i) => Self::U64(!i),
            _ => Self::Null,
        }
    }
}

use std::ops::{Add, Div, Mul, Rem, Sub};

fn int_fault(reason: &'static str) -> Dynamic {
    crate::set_fault(reason);
    Dynamic::Null
}

fn checked_i64(left: &Dynamic, right: &Dynamic, reason: &'static str) -> Option<(i64, i64)> {
    match (left.as_int(), right.as_int()) {
        (Some(left), Some(right)) => Some((left, right)),
        _ => {
            crate::set_fault(reason);
            None
        }
    }
}

impl Add for Dynamic {
    type Output = Self;
    fn add(self, rhs: Self) -> Self::Output {
        if self.is_list() {
            self.clone().append(rhs);
            return self;
        } else if rhs.is_list() {
            rhs.clone().append(self);
            return rhs;
        }
        match (self, rhs) {
            (Self::StringBuf(mut left), Self::StringBuf(right)) => {
                left.push_str(&right);
                return Self::StringBuf(left);
            }
            (Self::StringBuf(mut left), Self::String(right)) => {
                left.push_str(right.as_str());
                return Self::StringBuf(left);
            }
            (Self::StringBuf(mut left), right) => {
                left.push_str(&right.to_string());
                return Self::StringBuf(left);
            }
            (Self::String(left), Self::StringBuf(right)) => {
                let mut out = String::with_capacity(left.len() + right.len());
                out.push_str(left.as_str());
                out.push_str(&right);
                return Self::StringBuf(out);
            }
            (Self::String(left), Self::String(right)) => {
                let mut out = String::with_capacity(left.len() + right.len());
                out.push_str(left.as_str());
                out.push_str(right.as_str());
                return Self::StringBuf(out);
            }
            (Self::String(left), right) => {
                let right = right.to_string();
                let mut out = String::with_capacity(left.len() + right.len());
                out.push_str(left.as_str());
                out.push_str(&right);
                return Self::StringBuf(out);
            }
            (left, Self::StringBuf(right)) => {
                let left = left.to_string();
                let mut out = String::with_capacity(left.len() + right.len());
                out.push_str(&left);
                out.push_str(&right);
                return Self::StringBuf(out);
            }
            (left, Self::String(right)) => {
                let left = left.to_string();
                let mut out = String::with_capacity(left.len() + right.len());
                out.push_str(&left);
                out.push_str(right.as_str());
                return Self::StringBuf(out);
            }
            (left, right) => {
                if left.is_f64() || right.is_f64() {
                    return Dynamic::F64(left.as_float().unwrap_or(0.0) + right.as_float().unwrap_or(0.0));
                } else if left.is_f32() || right.is_f32() {
                    return Dynamic::F32(left.as_float().unwrap_or(0.0) as f32 + right.as_float().unwrap_or(0.0) as f32);
                }
                if left.is_int() || right.is_int() {
                    let Some((left, right)) = checked_i64(&left, &right, "整数加法类型或范围错误") else {
                        return Dynamic::Null;
                    };
                    return left.checked_add(right).map(Self::I64).unwrap_or_else(|| int_fault("整数加法溢出"));
                }
                if left.is_uint() || right.is_uint() {
                    let (Some(left), Some(right)) = (left.as_uint(), right.as_uint()) else {
                        return int_fault("无符号整数加法类型错误");
                    };
                    return left.checked_add(right).map(Self::U64).unwrap_or_else(|| int_fault("无符号整数加法溢出"));
                }
                if left.is_map() && right.is_map() {
                    left.append(right);
                }
                left
            }
        }
    }
}

impl Mul for Dynamic {
    type Output = Self;
    fn mul(self, rhs: Self) -> Self::Output {
        if self.is_f64() || rhs.is_f64() {
            return Dynamic::F64(self.as_float().unwrap_or(0.0) * rhs.as_float().unwrap_or(0.0));
        } else if self.is_f32() || rhs.is_f32() {
            return Dynamic::F32(self.as_float().unwrap_or(0.0) as f32 * rhs.as_float().unwrap_or(0.0) as f32);
        }
        if self.is_int() || rhs.is_int() {
            let Some((left, right)) = checked_i64(&self, &rhs, "整数乘法类型或范围错误") else {
                return Dynamic::Null;
            };
            return left.checked_mul(right).map(Self::I64).unwrap_or_else(|| int_fault("整数乘法溢出"));
        }
        if self.is_uint() || rhs.is_uint() {
            let (Some(left), Some(right)) = (self.as_uint(), rhs.as_uint()) else {
                return int_fault("无符号整数乘法类型错误");
            };
            return left.checked_mul(right).map(Self::U64).unwrap_or_else(|| int_fault("无符号整数乘法溢出"));
        }
        self
    }
}

impl Sub for Dynamic {
    type Output = Self;
    fn sub(self, rhs: Self) -> Self::Output {
        if self.is_float() && rhs.is_float() {
            if self.is_f64() || rhs.is_f64() {
                return Dynamic::F64(self.as_float().unwrap() - rhs.as_float().unwrap());
            } else if self.is_f32() || rhs.is_f32() {
                return Dynamic::F32(self.as_float().unwrap() as f32 - rhs.as_float().unwrap() as f32);
            }
        }
        if self.is_int() || rhs.is_int() {
            let Some((left, right)) = checked_i64(&self, &rhs, "整数减法类型或范围错误") else {
                return Dynamic::Null;
            };
            return left.checked_sub(right).map(Self::I64).unwrap_or_else(|| int_fault("整数减法溢出"));
        }
        if self.is_uint() || rhs.is_uint() {
            let (Some(left), Some(right)) = (self.as_uint(), rhs.as_uint()) else {
                return int_fault("无符号整数减法类型错误");
            };
            return left.checked_sub(right).map(Self::U64).unwrap_or_else(|| int_fault("无符号整数减法溢出"));
        }
        self
    }
}

impl Div for Dynamic {
    type Output = Self;
    fn div(self, rhs: Self) -> Self::Output {
        if self.is_float() && rhs.is_float() {
            if self.is_f64() || rhs.is_f64() {
                return Dynamic::F64(self.as_float().unwrap() / rhs.as_float().unwrap());
            } else if self.is_f32() || rhs.is_f32() {
                return Dynamic::F32(self.as_float().unwrap() as f32 / rhs.as_float().unwrap() as f32);
            }
        }
        if self.is_int() || rhs.is_int() {
            let Some((left, right)) = checked_i64(&self, &rhs, "整数除法类型或范围错误") else {
                return Dynamic::Null;
            };
            return match left.checked_div(right) {
                Some(value) => Self::I64(value),
                None => {
                    crate::set_fault("整数除零");
                    Self::Null
                }
            };
        }
        if self.is_uint() || rhs.is_uint() {
            let (Some(left), Some(right)) = (self.as_uint(), rhs.as_uint()) else {
                return int_fault("无符号整数除法类型错误");
            };
            return match left.checked_div(right) {
                Some(value) => Self::U64(value),
                None => {
                    crate::set_fault("无符号整数除零");
                    Self::Null
                }
            };
        }
        self
    }
}

impl Rem for Dynamic {
    type Output = Self;
    fn rem(self, rhs: Self) -> Self::Output {
        if self.is_int() || rhs.is_int() {
            let Some((left, right)) = checked_i64(&self, &rhs, "整数取余类型或范围错误") else {
                return Dynamic::Null;
            };
            return match left.checked_rem(right) {
                Some(value) => Self::I64(value),
                None => {
                    crate::set_fault("整数取余除零");
                    Self::Null
                }
            };
        }
        if self.is_uint() || rhs.is_uint() {
            let (Some(left), Some(right)) = (self.as_uint(), rhs.as_uint()) else {
                return int_fault("无符号整数取余类型错误");
            };
            return match left.checked_rem(right) {
                Some(value) => Self::U64(value),
                None => {
                    crate::set_fault("无符号整数取余除零");
                    Self::Null
                }
            };
        }
        self
    }
}

use std::ops::{Shl, Shr};

impl Shl for Dynamic {
    type Output = Self;
    fn shl(self, rhs: Self) -> Self::Output {
        use Dynamic::*;
        let Some(shift) = rhs.as_int().and_then(|value| u32::try_from(value).ok()).or_else(|| rhs.as_uint().and_then(|value| u32::try_from(value).ok())) else {
            return int_fault("位移数量类型或范围错误");
        };
        match self {
            I8(i) => i.checked_shl(shift).map(I8).unwrap_or_else(|| int_fault("左移溢出")),
            I16(i) => i.checked_shl(shift).map(I16).unwrap_or_else(|| int_fault("左移溢出")),
            I32(i) => i.checked_shl(shift).map(I32).unwrap_or_else(|| int_fault("左移溢出")),
            I64(i) => i.checked_shl(shift).map(I64).unwrap_or_else(|| int_fault("左移溢出")),
            U8(i) => i.checked_shl(shift).map(U8).unwrap_or_else(|| int_fault("左移溢出")),
            U16(i) => i.checked_shl(shift).map(U16).unwrap_or_else(|| int_fault("左移溢出")),
            U32(i) => i.checked_shl(shift).map(U32).unwrap_or_else(|| int_fault("左移溢出")),
            U64(i) => i.checked_shl(shift).map(U64).unwrap_or_else(|| int_fault("左移溢出")),
            _ => Dynamic::I64(0),
        }
    }
}

impl Shr for Dynamic {
    type Output = Self;
    fn shr(self, rhs: Self) -> Self::Output {
        use Dynamic::*;
        let Some(shift) = rhs.as_int().and_then(|value| u32::try_from(value).ok()).or_else(|| rhs.as_uint().and_then(|value| u32::try_from(value).ok())) else {
            return int_fault("位移数量类型或范围错误");
        };
        match self {
            I8(i) => i.checked_shr(shift).map(I8).unwrap_or_else(|| int_fault("右移溢出")),
            I16(i) => i.checked_shr(shift).map(I16).unwrap_or_else(|| int_fault("右移溢出")),
            I32(i) => i.checked_shr(shift).map(I32).unwrap_or_else(|| int_fault("右移溢出")),
            I64(i) => i.checked_shr(shift).map(I64).unwrap_or_else(|| int_fault("右移溢出")),
            U8(i) => i.checked_shr(shift).map(U8).unwrap_or_else(|| int_fault("右移溢出")),
            U16(i) => i.checked_shr(shift).map(U16).unwrap_or_else(|| int_fault("右移溢出")),
            U32(i) => i.checked_shr(shift).map(U32).unwrap_or_else(|| int_fault("右移溢出")),
            U64(i) => i.checked_shr(shift).map(U64).unwrap_or_else(|| int_fault("右移溢出")),
            _ => Dynamic::I64(0),
        }
    }
}

use std::ops::{BitAnd, BitOr, BitXor};
impl BitAnd for Dynamic {
    type Output = Self;
    fn bitand(self, rhs: Self) -> Self::Output {
        let ty = self.get_type() + rhs.get_type();
        let Ok(left) = ty.force(self) else {
            return int_fault("按位与左操作数类型错误");
        };
        let Ok(right) = ty.force(rhs) else {
            return int_fault("按位与右操作数类型错误");
        };
        match (left, right) {
            (Dynamic::I8(l), Dynamic::I8(r)) => Dynamic::I8(l & r),
            (Dynamic::I16(l), Dynamic::I16(r)) => Dynamic::I16(l & r),
            (Dynamic::I32(l), Dynamic::I32(r)) => Dynamic::I32(l & r),
            (Dynamic::I64(l), Dynamic::I64(r)) => Dynamic::I64(l & r),
            (Dynamic::U8(l), Dynamic::U8(r)) => Dynamic::U8(l & r),
            (Dynamic::U16(l), Dynamic::U16(r)) => Dynamic::U16(l & r),
            (Dynamic::U32(l), Dynamic::U32(r)) => Dynamic::U32(l & r),
            (Dynamic::U64(l), Dynamic::U64(r)) => Dynamic::U64(l & r),
            (_, _) => Dynamic::Null,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn negating_signed_min_values_does_not_panic() {
        assert_eq!(-Dynamic::I8(i8::MIN), Dynamic::I8(i8::MIN));
        assert_eq!(-Dynamic::I16(i16::MIN), Dynamic::I16(i16::MIN));
        assert_eq!(-Dynamic::I32(i32::MIN), Dynamic::I32(i32::MIN));
        assert_eq!(-Dynamic::I64(i64::MIN), Dynamic::I64(i64::MIN));
    }

    #[test]
    fn not_is_logical_for_bool_and_bitwise_for_ints() {
        assert_eq!(!Dynamic::Bool(false), Dynamic::Bool(true));
        assert_eq!(!Dynamic::I32(0b1010), Dynamic::I32(!0b1010));
        assert_eq!(!Dynamic::U32(0b1010), Dynamic::U32(!0b1010));
    }
}

impl BitOr for Dynamic {
    type Output = Self;
    fn bitor(self, rhs: Self) -> Self::Output {
        let ty = self.get_type() + rhs.get_type();
        let Ok(left) = ty.force(self) else {
            return int_fault("按位或左操作数类型错误");
        };
        let Ok(right) = ty.force(rhs) else {
            return int_fault("按位或右操作数类型错误");
        };
        match (left, right) {
            (Dynamic::I8(l), Dynamic::I8(r)) => Dynamic::I8(l | r),
            (Dynamic::I16(l), Dynamic::I16(r)) => Dynamic::I16(l | r),
            (Dynamic::I32(l), Dynamic::I32(r)) => Dynamic::I32(l | r),
            (Dynamic::I64(l), Dynamic::I64(r)) => Dynamic::I64(l | r),
            (Dynamic::U8(l), Dynamic::U8(r)) => Dynamic::U8(l | r),
            (Dynamic::U16(l), Dynamic::U16(r)) => Dynamic::U16(l | r),
            (Dynamic::U32(l), Dynamic::U32(r)) => Dynamic::U32(l | r),
            (Dynamic::U64(l), Dynamic::U64(r)) => Dynamic::U64(l | r),
            (_, _) => Dynamic::Null,
        }
    }
}

impl BitXor for Dynamic {
    type Output = Self;
    fn bitxor(self, rhs: Self) -> Self::Output {
        let ty = self.get_type() + rhs.get_type();
        let Ok(left) = ty.force(self) else {
            return int_fault("按位异或左操作数类型错误");
        };
        let Ok(right) = ty.force(rhs) else {
            return int_fault("按位异或右操作数类型错误");
        };
        match (left, right) {
            (Dynamic::I8(l), Dynamic::I8(r)) => Dynamic::I8(l ^ r),
            (Dynamic::I16(l), Dynamic::I16(r)) => Dynamic::I16(l ^ r),
            (Dynamic::I32(l), Dynamic::I32(r)) => Dynamic::I32(l ^ r),
            (Dynamic::I64(l), Dynamic::I64(r)) => Dynamic::I64(l ^ r),
            (Dynamic::U8(l), Dynamic::U8(r)) => Dynamic::U8(l ^ r),
            (Dynamic::U16(l), Dynamic::U16(r)) => Dynamic::U16(l ^ r),
            (Dynamic::U32(l), Dynamic::U32(r)) => Dynamic::U32(l ^ r),
            (Dynamic::U64(l), Dynamic::U64(r)) => Dynamic::U64(l ^ r),
            (_, _) => Dynamic::Null,
        }
    }
}
