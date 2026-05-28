use super::{JITRunTime, context::BuildContext};
use cranelift::prelude::*;
use cranelift_module::{DataDescription, Module};
use dynamic::{Dynamic, Type};
use parser::{BinaryOp, Expr};

use anyhow::{Result, anyhow};

impl JITRunTime {
    fn any_to_string(&mut self, ctx: &mut BuildContext, vt: (Value, Type)) -> Result<Value> {
        let value = self.convert(ctx, vt, Type::Any)?;
        self.call(ctx, self.get_method(&Type::Any, "to_string")?, vec![value]).map(|(v, _)| v)
    }

    fn any_logic(&mut self, ctx: &mut BuildContext, left: Value, op: BinaryOp, right: Value) -> Result<(Value, Type)> {
        let op = ctx.builder.ins().iconst(types::I32, i32::from(op) as i64);
        self.call(ctx, self.get_method(&Type::Any, "logic")?, vec![left, op, right])
    }

    fn any_binary(&mut self, ctx: &mut BuildContext, left: Value, op: BinaryOp, right: Value) -> Result<(Value, Type)> {
        let op = ctx.builder.ins().iconst(types::I32, i32::from(op) as i64);
        self.call(ctx, self.get_method(&Type::Any, "binary")?, vec![left, op, right])
    }

    fn struct_to_dynamic(&mut self, ctx: &mut BuildContext, base: Value, ty: &Type) -> Result<Value> {
        let Type::Struct { params: _, fields: _ } = ty else {
            return Err(anyhow!("不是结构体 {:?}", ty));
        };
        let id = self.module.declare_anonymous_data(true, false)?;
        let mut desc = DataDescription::new();
        let ty_ptr = Box::into_raw(Box::new(ty.clone()));
        desc.define((ty_ptr as i64).to_le_bytes().into());
        self.module.define_data(id, &desc)?;
        let ty_data = self.module.declare_data_in_func(id, &mut ctx.builder.func);
        let ty_addr = ctx.builder.ins().global_value(crate::ptr_type(), ty_data);
        let ty_ptr = ctx.builder.ins().load(crate::ptr_type(), MemFlags::new(), ty_addr, 0);
        let fn_id = self.struct_from_ptr_fn.ok_or_else(|| anyhow!("VM struct Dynamic runtime is not registered"))?;
        let fn_ref = self.get_fn_ref(ctx, fn_id);
        let call_inst = ctx.builder.ins().call(fn_ref, &[base, ty_ptr]);
        Ok(ctx.builder.inst_results(call_inst)[0])
    }

    pub(crate) fn bool_value(&mut self, ctx: &mut BuildContext, vt: (Value, Type)) -> Result<Value> {
        if vt.1.is_bool() {
            Ok(vt.0)
        } else if vt.1.is_any() {
            self.call(ctx, self.get_method(&Type::Any, "to_bool")?, vec![vt.0]).map(|(v, _)| v)
        } else if vt.1.is_int() || vt.1.is_uint() {
            Ok(ctx.builder.ins().icmp_imm(IntCC::NotEqual, vt.0, 0))
        } else if vt.1.is_f32() {
            let zero = ctx.builder.ins().f32const(0.0);
            Ok(ctx.builder.ins().fcmp(FloatCC::NotEqual, vt.0, zero))
        } else if vt.1.is_f64() {
            let zero = ctx.builder.ins().f64const(0.0);
            Ok(ctx.builder.ins().fcmp(FloatCC::NotEqual, vt.0, zero))
        } else {
            Err(anyhow!("cannot convert {:?} to bool", vt.1))
        }
    }

    pub fn convert(&mut self, ctx: &mut BuildContext, vt: (Value, Type), ty: Type) -> Result<Value> {
        let vt = if matches!(vt.1, Type::Symbol { .. }) {
            let resolved = self.compiler.symbols.get_type(&vt.1).unwrap_or_else(|_| vt.1.clone());
            (vt.0, resolved)
        } else {
            vt
        };
        if vt.1 != ty {
            if ty.is_any() {
                if self.is_opaque_custom_ty(&vt.1) {
                    return Ok(vt.0);
                } else if vt.1.is_struct() {
                    return self.struct_to_dynamic(ctx, vt.0, &vt.1);
                } else if vt.1.is_bool() {
                    return self.call(ctx, self.get_method(&Type::Any, "from_bool")?, vec![vt.0]).map(|(v, _)| v);
                } else if vt.1.is_uint() {
                    let v = if vt.1.width() < 8 { ctx.builder.ins().uextend(types::I64, vt.0) } else { vt.0 };
                    return self.call(ctx, self.get_method(&Type::Any, "from_i64")?, vec![v]).map(|(v, _)| v);
                } else if vt.1.is_int() {
                    let v = if vt.1.width() < 8 { ctx.builder.ins().sextend(types::I64, vt.0) } else { vt.0 };
                    return self.call(ctx, self.get_method(&Type::Any, "from_i64")?, vec![v]).map(|(v, _)| v);
                } else if vt.1.is_f32() {
                    let v = ctx.builder.ins().fpromote(types::F64, vt.0);
                    return self.call(ctx, self.get_method(&Type::Any, "from_f64")?, vec![v]).map(|(v, _)| v);
                } else if vt.1.is_f64() {
                    return self.call(ctx, self.get_method(&Type::Any, "from_f64")?, vec![vt.0]).map(|(v, _)| v);
                } else if vt.1.is_str() {
                    return Ok(vt.0);
                } else if matches!(vt.1, Type::Symbol { .. }) {
                    return Ok(vt.0);
                }
            } else if vt.1.is_any() {
                if ty.is_bool() {
                    return self.call(ctx, self.get_method(&Type::Any, "to_bool")?, vec![vt.0]).map(|(v, _)| v);
                } else if ty.is_str() {
                    return self.call(ctx, self.get_method(&Type::Any, "to_string")?, vec![vt.0]).map(|(v, _)| v);
                } else if ty.is_int() | ty.is_uint() {
                    let (v, _) = self.call(ctx, self.get_method(&Type::Any, "to_i64")?, vec![vt.0])?;
                    return Ok(match ty.width() {
                        1 => ctx.builder.ins().ireduce(types::I8, v),
                        2 => ctx.builder.ins().ireduce(types::I16, v),
                        4 => ctx.builder.ins().ireduce(types::I32, v),
                        _ => v,
                    });
                } else if ty.is_f32() {
                    let v = self.call(ctx, self.get_method(&Type::Any, "to_f64")?, vec![vt.0]).map(|(v, _)| v)?;
                    return Ok(ctx.builder.ins().fdemote(types::F32, v));
                } else if ty.is_f64() {
                    return self.call(ctx, self.get_method(&Type::Any, "to_f64")?, vec![vt.0]).map(|(v, _)| v);
                } else {
                    return Ok(vt.0);
                }
            } else if ty.is_str() {
                return self.any_to_string(ctx, vt);
            } else if ty.is_int() || ty.is_uint() {
                if vt.1.is_f32() || vt.1.is_f64() {
                    let target = crate::get_type(&ty)?;
                    if ty.is_uint() {
                        return Ok(ctx.builder.ins().fcvt_to_uint(target, vt.0));
                    } else if ty.is_int() {
                        return Ok(ctx.builder.ins().fcvt_to_sint(target, vt.0));
                    }
                }
                if vt.1.is_int() || vt.1.is_uint() || vt.1.is_bool() {
                    let target = crate::get_type(&ty)?;
                    let actual = ctx.builder.func.dfg.value_type(vt.0);
                    if actual == target {
                        return Ok(vt.0);
                    }
                    if actual.is_int() && target.is_int() {
                        if actual.bits() > target.bits() {
                            return Ok(ctx.builder.ins().ireduce(target, vt.0));
                        }
                        if actual.bits() < target.bits() {
                            return if vt.1.is_int() { Ok(ctx.builder.ins().sextend(target, vt.0)) } else { Ok(ctx.builder.ins().uextend(target, vt.0)) };
                        }
                    }
                }
                if vt.1.is_str() {
                    let (v, _) = self.call(ctx, self.get_method(&Type::Any, "to_i64")?, vec![vt.0])?;
                    return Ok(match ty.width() {
                        1 => ctx.builder.ins().ireduce(types::I8, v),
                        2 => ctx.builder.ins().ireduce(types::I16, v),
                        4 => ctx.builder.ins().ireduce(types::I32, v),
                        _ => v,
                    });
                }
            } else if ty.is_f32() {
                if vt.1.is_int() {
                    return Ok(ctx.builder.ins().fcvt_from_sint(types::F32, vt.0));
                } else if vt.1.is_uint() {
                    return Ok(ctx.builder.ins().fcvt_from_uint(types::F32, vt.0));
                } else if vt.1.is_f64() {
                    return Ok(ctx.builder.ins().fdemote(types::F32, vt.0));
                }
            } else if ty.is_f64() {
                if vt.1.is_int() {
                    return Ok(ctx.builder.ins().fcvt_from_sint(types::F64, vt.0));
                } else if vt.1.is_uint() {
                    return Ok(ctx.builder.ins().fcvt_from_uint(types::F64, vt.0));
                } else if vt.1.is_f32() {
                    return Ok(ctx.builder.ins().fpromote(types::F64, vt.0));
                }
            } else if let Type::Symbol { id: _, params: _ } = ty {
                log::info!("convert {:?} -> {:?}", vt, ty);
                return Ok(vt.0); //结构类型 可以看作 External 类型
            }
            if vt.1.is_bool() {
                let v = ctx.builder.ins().sextend(types::I64, vt.0);
                return self.call(ctx, self.get_method(&Type::Any, "from_i64")?, vec![v]).map(|(v, _)| v);
            }
            log::error!("未实现 {:?} {:?}", vt, ty); //暂时还没有实现 struct 的 初始化
            Ok(vt.0)
        } else {
            Ok(vt.0)
        }
    }

    pub(crate) fn binary(&mut self, ctx: &mut BuildContext, left: (Value, Type), op: BinaryOp, right: &Expr) -> Result<(Value, Type)> {
        //处理可以计算的简单情形
        if matches!(op, BinaryOp::And | BinaryOp::Or) {
            return self.short_circuit_logic(ctx, left, op, right);
        }
        let right_ty_hint = if right.is_value() { right.clone().value().ok().map(|v| v.get_type()) } else { self.get_dynamic(right).map(|v| v.get_type()) };
        let right = if right.is_value() {
            let right = right.clone().value()?;
            if right.is_f32() {
                (ctx.builder.ins().f32const(right.as_float().unwrap() as f32), Type::F32)
            } else if right.is_f64() {
                (ctx.builder.ins().f64const(right.as_float().unwrap() as f64), Type::F64)
            } else if left.1.is_any() {
                if right.is_int() {
                    (ctx.builder.ins().iconst(types::I64, right.as_int().unwrap()), Type::I64)
                } else if right.is_null() {
                    self.call(ctx, self.get_method(&Type::Any, "null")?, vec![])?
                } else {
                    ctx.get_const(&right)?
                }
            } else {
                return self.binary_imm(ctx, left, op, right);
            }
        } else {
            self.eval(ctx, right)?.get(ctx).ok_or_else(|| anyhow!("没有返回值: {:?}", right))?
        };
        let right_ty = right_ty_hint.as_ref().unwrap_or(&right.1);
        let ty = if (op.is_add() || op.is_logic()) && (left.1.is_str() || right.1.is_str() || right_ty.is_str()) {
            Type::Str
        } else if (op.is_add() || op.is_logic()) && (left.1.is_any() || right.1.is_any()) {
            Type::Any
        } else {
            left.1.clone() + right.1.clone()
        }; //为了支持字符串的加法需要单独处理
        if ty.is_str() && op.is_add() {
            let left = self.convert(ctx, left, Type::Any)?;
            let right = self.convert(ctx, right, Type::Any)?;
            let result = self.any_binary(ctx, left, op, right)?.0;
            return Ok((result, ty));
        }
        let left = self.convert(ctx, left, ty.clone())?;
        let right = self.convert(ctx, right, ty.clone())?;
        if ty.is_any() {
            if op.is_logic() {
                return self.any_logic(ctx, left, op, right);
            } else {
                return self.any_binary(ctx, left, op, right);
            }
        }
        if ty.is_str() && op.is_logic() {
            return self.any_logic(ctx, left, op, right);
        }
        match op {
            BinaryOp::Add | BinaryOp::AddAssign => {
                if ty.is_int() || ty.is_uint() {
                    return Ok((ctx.builder.ins().iadd(left, right), ty));
                } else if ty.is_float() {
                    return Ok((ctx.builder.ins().fadd(left, right), ty));
                } else if ty.is_str() {
                    let result = self.any_binary(ctx, left, op, right)?.0;
                    return Ok((result, ty));
                }
            }
            BinaryOp::Sub | BinaryOp::SubAssign => {
                if ty.is_int() || ty.is_uint() {
                    return Ok((ctx.builder.ins().isub(left, right), ty));
                } else if ty.is_float() {
                    return Ok((ctx.builder.ins().fsub(left, right), ty));
                }
            }
            BinaryOp::Mul | BinaryOp::MulAssign => {
                if ty.is_int() || ty.is_uint() {
                    return Ok((ctx.builder.ins().imul(left, right), ty));
                } else if ty.is_float() {
                    return Ok((ctx.builder.ins().fmul(left, right), ty));
                }
            }
            BinaryOp::Div | BinaryOp::DivAssign => {
                if ty.is_int() {
                    return Ok((ctx.builder.ins().sdiv(left, right), ty));
                } else if ty.is_uint() {
                    return Ok((ctx.builder.ins().udiv(left, right), ty));
                } else if ty.is_float() {
                    return Ok((ctx.builder.ins().fdiv(left, right), ty));
                }
            }
            BinaryOp::Mod | BinaryOp::ModAssign => {
                if ty.is_int() {
                    return Ok((ctx.builder.ins().srem(left, right), ty));
                } else if ty.is_uint() {
                    return Ok((ctx.builder.ins().urem(left, right), ty));
                }
            }
            BinaryOp::Shl | BinaryOp::ShlAssign => {
                if ty.is_int() || ty.is_uint() {
                    return Ok((ctx.builder.ins().ishl(left, right), ty));
                }
            }
            BinaryOp::Shr | BinaryOp::ShrAssign => {
                if ty.is_int() {
                    return Ok((ctx.builder.ins().sshr(left, right), ty));
                } else if ty.is_uint() {
                    return Ok((ctx.builder.ins().ushr(left, right), ty));
                }
            }
            BinaryOp::BitAnd | BinaryOp::BitAndAssign => {
                return Ok((ctx.builder.ins().band(left, right), ty));
            }
            BinaryOp::BitOr | BinaryOp::BitOrAssign => {
                return Ok((ctx.builder.ins().bor(left, right), ty));
            }
            BinaryOp::BitXor | BinaryOp::BitXorAssign => {
                return Ok((ctx.builder.ins().bxor(left, right), ty));
            }
            BinaryOp::Eq => {
                if ty.is_int() | ty.is_uint() || ty.is_bool() {
                    return Ok((ctx.builder.ins().icmp(IntCC::Equal, left, right), Type::Bool));
                } else if ty.is_float() {
                    return Ok((ctx.builder.ins().fcmp(FloatCC::Equal, left, right), Type::Bool));
                }
            }
            BinaryOp::Ne => {
                if ty.is_int() | ty.is_uint() || ty.is_bool() {
                    return Ok((ctx.builder.ins().icmp(IntCC::NotEqual, left, right), Type::Bool));
                } else if ty.is_float() {
                    return Ok((ctx.builder.ins().fcmp(FloatCC::NotEqual, left, right), Type::Bool));
                }
            }
            BinaryOp::Lt => {
                if ty.is_int() {
                    return Ok((ctx.builder.ins().icmp(IntCC::SignedLessThan, left, right), Type::Bool));
                } else if ty.is_uint() {
                    return Ok((ctx.builder.ins().icmp(IntCC::UnsignedLessThan, left, right), Type::Bool));
                } else if ty.is_float() {
                    return Ok((ctx.builder.ins().fcmp(FloatCC::LessThan, left, right), Type::Bool));
                }
            }
            BinaryOp::Le => {
                if ty.is_int() {
                    return Ok((ctx.builder.ins().icmp(IntCC::SignedLessThanOrEqual, left, right), Type::Bool));
                } else if ty.is_uint() {
                    return Ok((ctx.builder.ins().icmp(IntCC::UnsignedLessThanOrEqual, left, right), Type::Bool));
                } else if ty.is_float() {
                    return Ok((ctx.builder.ins().fcmp(FloatCC::LessThanOrEqual, left, right), Type::Bool));
                }
            }
            BinaryOp::Gt => {
                if ty.is_int() {
                    return Ok((ctx.builder.ins().icmp(IntCC::SignedGreaterThan, left, right), Type::Bool));
                } else if ty.is_uint() {
                    return Ok((ctx.builder.ins().icmp(IntCC::UnsignedGreaterThan, left, right), Type::Bool));
                } else if ty.is_float() {
                    return Ok((ctx.builder.ins().fcmp(FloatCC::GreaterThan, left, right), Type::Bool));
                }
            }
            BinaryOp::Ge => {
                if ty.is_int() {
                    return Ok((ctx.builder.ins().icmp(IntCC::SignedGreaterThanOrEqual, left, right), Type::Bool));
                } else if ty.is_uint() {
                    return Ok((ctx.builder.ins().icmp(IntCC::UnsignedGreaterThanOrEqual, left, right), Type::Bool));
                } else if ty.is_float() {
                    return Ok((ctx.builder.ins().fcmp(FloatCC::GreaterThanOrEqual, left, right), Type::Bool));
                }
            }
            _ => {}
        }
        panic!("未实现 {:?} {:?} {:?} {:?}", left, op, right, ty)
    }

    pub(crate) fn binary_imm<'a>(&mut self, ctx: &'a mut BuildContext, left: (Value, Type), op: BinaryOp, right: Dynamic) -> Result<(Value, Type)> {
        let ty = left.1.clone() + right.get_type();
        let bool_imm = || right.as_bool().map(|value| if value { 1 } else { 0 });
        if ty.is_str() && op.is_add() {
            let left = self.convert(ctx, left, Type::Any)?;
            let right_vt = ctx.get_const(&right).or_else(|_| {
                let idx = self.compiler.get_const(right.clone());
                self.get_const_value(ctx, idx)
            })?;
            let right = self.convert(ctx, right_vt, Type::Any)?;
            let result = self.any_binary(ctx, left, op, right)?.0;
            return Ok((result, ty));
        }
        let left = self.convert(ctx, left, ty.clone())?;
        if ty.is_str() && op.is_logic() {
            let right_vt = ctx.get_const(&right).or_else(|_| {
                let idx = self.compiler.get_const(right.clone());
                self.get_const_value(ctx, idx)
            })?;
            let right = self.convert(ctx, right_vt, Type::Str)?;
            return self.any_logic(ctx, left, op, right);
        }
        let right_float = if ty.is_float() {
            let right = right.as_float().ok_or(anyhow!("非数字"))?;
            Some(if ty.is_f32() { ctx.builder.ins().f32const(right as f32) } else { ctx.builder.ins().f64const(right) })
        } else {
            None
        };
        match op {
            BinaryOp::Add | BinaryOp::AddAssign => {
                if ty.is_str() {
                    let right_vt = ctx.get_const(&right).or_else(|_| {
                        let idx = self.compiler.get_const(right.clone());
                        self.get_const_value(ctx, idx)
                    })?;
                    let right = self.convert(ctx, right_vt, Type::Str)?;
                    let result = self.any_binary(ctx, left, op, right)?.0;
                    return Ok((result, ty));
                }
                if ty.is_int() | ty.is_uint() {
                    return Ok((ctx.builder.ins().iadd_imm(left, right.as_int().ok_or(anyhow!("非整数"))?), ty));
                } else if ty.is_float() {
                    return Ok((ctx.builder.ins().fadd(left, right_float.unwrap()), ty));
                }
            }
            BinaryOp::Sub | BinaryOp::SubAssign => {
                if ty.is_int() | ty.is_uint() {
                    return Ok((ctx.builder.ins().iadd_imm(left, -right.as_int().ok_or(anyhow!("非整数"))?), ty));
                } else if ty.is_float() {
                    return Ok((ctx.builder.ins().fsub(left, right_float.unwrap()), ty));
                }
            }
            BinaryOp::Mul | BinaryOp::MulAssign => {
                if ty.is_int() | ty.is_uint() {
                    return Ok((ctx.builder.ins().imul_imm(left, right.as_int().ok_or(anyhow!("非整数"))?), ty));
                } else if ty.is_float() {
                    return Ok((ctx.builder.ins().fmul(left, right_float.unwrap()), ty));
                }
            }
            BinaryOp::Div | BinaryOp::DivAssign => {
                if ty.is_int() {
                    return Ok((ctx.builder.ins().sdiv_imm(left, right.as_int().ok_or(anyhow!("非整数"))?), ty));
                } else if ty.is_uint() {
                    return Ok((ctx.builder.ins().udiv_imm(left, right.as_int().ok_or(anyhow!("非整数"))?), ty));
                } else if ty.is_float() {
                    return Ok((ctx.builder.ins().fdiv(left, right_float.unwrap()), ty));
                }
            }
            BinaryOp::Shl | BinaryOp::ShlAssign => {
                if ty.is_int() || ty.is_uint() {
                    return Ok((ctx.builder.ins().ishl_imm(left, right.as_int().ok_or(anyhow!("非整数"))?), ty));
                }
            }
            BinaryOp::Shr | BinaryOp::ShrAssign => {
                if ty.is_int() {
                    return Ok((ctx.builder.ins().sshr_imm(left, right.as_int().ok_or(anyhow!("非整数"))?), ty));
                } else if ty.is_uint() {
                    return Ok((ctx.builder.ins().ushr_imm(left, right.as_int().ok_or(anyhow!("非整数"))?), ty));
                }
            }
            BinaryOp::BitAnd | BinaryOp::BitAndAssign => {
                return Ok((ctx.builder.ins().band_imm(left, right.as_int().ok_or(anyhow!("非整数"))?), ty));
            }
            BinaryOp::BitOr | BinaryOp::BitOrAssign => {
                return Ok((ctx.builder.ins().bor_imm(left, right.as_int().ok_or(anyhow!("非整数"))?), ty));
            }
            BinaryOp::BitXor | BinaryOp::BitXorAssign => {
                return Ok((ctx.builder.ins().bxor_imm(left, right.as_int().ok_or(anyhow!("非整数"))?), ty));
            }
            BinaryOp::Eq => {
                if ty.is_int() | ty.is_uint() {
                    return Ok((ctx.builder.ins().icmp_imm(IntCC::Equal, left, right.as_int().unwrap()), Type::Bool));
                } else if ty.is_bool() {
                    return Ok((ctx.builder.ins().icmp_imm(IntCC::Equal, left, bool_imm().unwrap()), Type::Bool));
                } else if ty.is_float() {
                    return Ok((ctx.builder.ins().fcmp(FloatCC::Equal, left, right_float.unwrap()), Type::Bool));
                }
            }
            BinaryOp::Ne => {
                if ty.is_int() | ty.is_uint() {
                    return Ok((ctx.builder.ins().icmp_imm(IntCC::NotEqual, left, right.as_int().unwrap()), Type::Bool));
                } else if ty.is_bool() {
                    return Ok((ctx.builder.ins().icmp_imm(IntCC::NotEqual, left, bool_imm().unwrap()), Type::Bool));
                } else if ty.is_float() {
                    return Ok((ctx.builder.ins().fcmp(FloatCC::NotEqual, left, right_float.unwrap()), Type::Bool));
                }
            }
            BinaryOp::Le => {
                if ty.is_int() {
                    return Ok((ctx.builder.ins().icmp_imm(IntCC::SignedLessThanOrEqual, left, right.as_int().unwrap()), Type::Bool));
                } else if ty.is_uint() {
                    return Ok((ctx.builder.ins().icmp_imm(IntCC::UnsignedLessThanOrEqual, left, right.as_int().unwrap()), Type::Bool));
                } else if ty.is_float() {
                    return Ok((ctx.builder.ins().fcmp(FloatCC::LessThanOrEqual, left, right_float.unwrap()), Type::Bool));
                }
            }
            BinaryOp::Lt => {
                if ty.is_int() {
                    return Ok((ctx.builder.ins().icmp_imm(IntCC::SignedLessThan, left, right.as_int().unwrap()), Type::Bool));
                } else if ty.is_uint() {
                    return Ok((ctx.builder.ins().icmp_imm(IntCC::UnsignedLessThan, left, right.as_int().unwrap()), Type::Bool));
                } else if ty.is_float() {
                    return Ok((ctx.builder.ins().fcmp(FloatCC::LessThan, left, right_float.unwrap()), Type::Bool));
                }
            }
            BinaryOp::Ge => {
                if ty.is_int() {
                    return Ok((ctx.builder.ins().icmp_imm(IntCC::SignedGreaterThanOrEqual, left, right.as_int().unwrap()), Type::Bool));
                } else if ty.is_uint() {
                    return Ok((ctx.builder.ins().icmp_imm(IntCC::UnsignedGreaterThanOrEqual, left, right.as_int().unwrap()), Type::Bool));
                } else if ty.is_float() {
                    return Ok((ctx.builder.ins().fcmp(FloatCC::GreaterThanOrEqual, left, right_float.unwrap()), Type::Bool));
                }
            }
            BinaryOp::Gt => {
                if ty.is_int() {
                    return Ok((ctx.builder.ins().icmp_imm(IntCC::SignedGreaterThan, left, right.as_int().unwrap()), Type::Bool));
                } else if ty.is_uint() {
                    return Ok((ctx.builder.ins().icmp_imm(IntCC::UnsignedGreaterThan, left, right.as_int().unwrap()), Type::Bool));
                } else if ty.is_float() {
                    return Ok((ctx.builder.ins().fcmp(FloatCC::GreaterThan, left, right_float.unwrap()), Type::Bool));
                }
            }
            BinaryOp::Mod | BinaryOp::ModAssign => {
                if ty.is_int() {
                    return Ok((ctx.builder.ins().srem_imm(left, right.as_int().unwrap()), ty));
                } else if ty.is_uint() {
                    return Ok((ctx.builder.ins().urem_imm(left, right.as_int().unwrap()), ty));
                }
            }
            exp => {
                panic!("不支持的操作 {:?}", exp)
            }
        }
        panic!("未实现 {:?} {:?} {:?}", ty, op, right.get_type())
    }
}
