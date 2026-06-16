use super::{JITRunTime, context::BuildContext};
use cranelift::prelude::*;
use dynamic::{Dynamic, Type};
use parser::{BinaryOp, Expr};

use anyhow::{Result, anyhow};

impl JITRunTime {
    fn strcat(&mut self, ctx: &mut BuildContext, left: Value, right: Value) -> Result<Value> {
        let fn_id = self.strcat_fn.ok_or_else(|| anyhow!("VM strcat runtime is not registered"))?;
        let fn_ref = self.get_fn_ref(ctx, fn_id);
        let call_inst = ctx.builder.ins().call(fn_ref, &[left, right]);
        Ok(ctx.builder.inst_results(call_inst)[0])
    }

    fn strcat_i64(&mut self, ctx: &mut BuildContext, left: Value, right: Value) -> Result<Value> {
        let fn_id = self.strcat_i64_fn.ok_or_else(|| anyhow!("VM strcat i64 runtime is not registered"))?;
        let fn_ref = self.get_fn_ref(ctx, fn_id);
        let call_inst = ctx.builder.ins().call(fn_ref, &[left, right]);
        Ok(ctx.builder.inst_results(call_inst)[0])
    }

    fn strcat_assign(&mut self, ctx: &mut BuildContext, left: Value, right: Value) -> Result<Value> {
        let fn_id = self.strcat_assign_fn.ok_or_else(|| anyhow!("VM strcat assign runtime is not registered"))?;
        let fn_ref = self.get_fn_ref(ctx, fn_id);
        let call_inst = ctx.builder.ins().call(fn_ref, &[left, right]);
        Ok(ctx.builder.inst_results(call_inst)[0])
    }

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
        let ty_ptr = Self::type_ptr_const(ctx, ty);
        let fn_id = self.struct_from_ptr_fn.ok_or_else(|| anyhow!("VM struct Dynamic runtime is not registered"))?;
        let fn_ref = self.get_fn_ref(ctx, fn_id);
        let call_inst = ctx.builder.ins().call(fn_ref, &[base, ty_ptr]);
        Ok(ctx.builder.inst_results(call_inst)[0])
    }

    fn array_to_dynamic(&mut self, ctx: &mut BuildContext, base: Value, ty: &Type) -> Result<Value> {
        let Type::Array(_, _) = ty else {
            return Err(anyhow!("不是数组 {:?}", ty));
        };
        let ty_ptr = Self::type_ptr_const(ctx, ty);
        let fn_id = self.array_from_ptr_fn.ok_or_else(|| anyhow!("VM array Dynamic runtime is not registered"))?;
        let fn_ref = self.get_fn_ref(ctx, fn_id);
        let call_inst = ctx.builder.ins().call(fn_ref, &[base, ty_ptr]);
        Ok(ctx.builder.inst_results(call_inst)[0])
    }

    pub(crate) fn bool_value(&mut self, ctx: &mut BuildContext, vt: (Value, Type)) -> Result<Value> {
        if vt.1.is_bool() {
            Ok(vt.0)
        } else if vt.1.is_void() {
            Ok(ctx.builder.ins().iconst(types::I8, 0))
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
                } else if vt.1.is_array() {
                    return self.array_to_dynamic(ctx, vt.0, &vt.1);
                } else if vt.1.is_struct() {
                    return self.struct_to_dynamic(ctx, vt.0, &vt.1);
                } else if vt.1.is_bool() {
                    return self.call(ctx, self.get_method(&Type::Any, "from_bool")?, vec![vt.0]).map(|(v, _)| v);
                } else if vt.1.is_uint() {
                    if vt.1.width() == 8 {
                        // u64 → Any：必须用 from_u64 保留无符号语义，from_i64 会在 >i64::MAX 时产生负数
                        return self.call(ctx, self.get_method(&Type::Any, "from_u64")?, vec![vt.0]).map(|(v, _)| v);
                    }
                    let v = ctx.builder.ins().uextend(types::I64, vt.0);
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
                } else if matches!(vt.1, Type::Map | Type::List(_) | Type::Iter) {
                    return Ok(vt.0);
                } else if matches!(vt.1, Type::Symbol { .. }) {
                    return Ok(vt.0);
                }
            } else if vt.1.is_any() {
                if ty.is_bool() {
                    return self.call(ctx, self.get_method(&Type::Any, "to_bool")?, vec![vt.0]).map(|(v, _)| v);
                } else if ty.is_array() {
                    return self.any_to_array(ctx, vt.0, &ty);
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
                } else if vt.1.is_str() {
                    let v = self.call(ctx, self.get_method(&Type::Any, "to_f64")?, vec![vt.0]).map(|(v, _)| v)?;
                    return Ok(ctx.builder.ins().fdemote(types::F32, v));
                }
            } else if ty.is_f64() {
                if vt.1.is_int() {
                    return Ok(ctx.builder.ins().fcvt_from_sint(types::F64, vt.0));
                } else if vt.1.is_uint() {
                    return Ok(ctx.builder.ins().fcvt_from_uint(types::F64, vt.0));
                } else if vt.1.is_f32() {
                    return Ok(ctx.builder.ins().fpromote(types::F64, vt.0));
                } else if vt.1.is_str() {
                    return self.call(ctx, self.get_method(&Type::Any, "to_f64")?, vec![vt.0]).map(|(v, _)| v);
                }
            } else if let Type::Symbol { id: _, params: _ } = ty {
                log::debug!("convert {:?} -> {:?}", vt, ty);
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

    /// 整数除法 / 取余的运行期守卫。
    ///
    /// Cranelift 的 `sdiv/udiv/srem/urem` 在除数为 0(有符号还有 `INT_MIN/-1`)时
    /// 发出硬件 trap,会直接杀掉进程且无法被 `catch_unwind` 捕获。这里在除法前
    /// 分支:除数非法时调用 `__vm_arith_fault` 记录运行期错误并返回 0,合法时才
    /// 进入真正的除法块(此时除数可证非 0,trap 永不触发)。
    fn guarded_idiv(&mut self, ctx: &mut BuildContext, left: Value, right: Value, signed: bool, is_rem: bool) -> Result<Value> {
        use cranelift::codegen::ir::BlockArg;
        let int_ty = ctx.builder.func.dfg.value_type(left);
        let is_zero = ctx.builder.ins().icmp_imm(IntCC::Equal, right, 0);
        let is_bad = if signed {
            let min = match int_ty.bits() {
                8 => i8::MIN as i64,
                16 => i16::MIN as i64,
                32 => i32::MIN as i64,
                _ => i64::MIN,
            };
            let is_min = ctx.builder.ins().icmp_imm(IntCC::Equal, left, min);
            let is_neg_one = ctx.builder.ins().icmp_imm(IntCC::Equal, right, -1);
            let is_overflow = ctx.builder.ins().band(is_min, is_neg_one);
            ctx.builder.ins().bor(is_zero, is_overflow)
        } else {
            is_zero
        };

        let ok_block = ctx.builder.create_block();
        let bad_block = ctx.builder.create_block();
        let merge_block = ctx.builder.create_block();
        ctx.builder.append_block_param(merge_block, int_ty);
        ctx.builder.ins().brif(is_bad, bad_block, &[], ok_block, &[]);

        ctx.builder.switch_to_block(ok_block);
        let raw = match (signed, is_rem) {
            (true, false) => ctx.builder.ins().sdiv(left, right),
            (false, false) => ctx.builder.ins().udiv(left, right),
            (true, true) => ctx.builder.ins().srem(left, right),
            (false, true) => ctx.builder.ins().urem(left, right),
        };
        ctx.builder.ins().jump(merge_block, &[BlockArg::Value(raw)]);
        ctx.builder.seal_block(ok_block);

        ctx.builder.switch_to_block(bad_block);
        let fault_fn = self.arith_fault_fn.ok_or_else(|| anyhow!("VM arith fault runtime is not registered"))?;
        let fault_ref = self.get_fn_ref(ctx, fault_fn);
        ctx.builder.ins().call(fault_ref, &[]);
        let zero = ctx.builder.ins().iconst(int_ty, 0);
        ctx.builder.ins().jump(merge_block, &[BlockArg::Value(zero)]);
        ctx.builder.seal_block(bad_block);

        ctx.builder.switch_to_block(merge_block);
        ctx.builder.seal_block(merge_block);
        Ok(ctx.builder.block_params(merge_block)[0])
    }

    /// 立即数除法/取余:除数在编译期已知,据此选最省的代码:
    /// - 非零常量(无符号任意非零;有符号且非 -1):不可能 trap,直接 `*_imm`,无守卫;
    /// - 除数为 0:编译期已知会出错,记 fault 并返回 0,不发 trap;
    /// - 有符号 `/ -1` 或 `% -1`:可能 `INT_MIN/-1` 溢出 trap,回退到运行期守卫。
    ///
    /// 这避免了对 `x / 2`、`x % 1000000007` 这类常量除法附加无谓的判零分支
    /// (除零守卫只在真正可能 trap 时才生成)。
    fn idiv_imm(&mut self, ctx: &mut BuildContext, left: Value, divisor: i64, signed: bool, is_rem: bool) -> Result<Value> {
        let int_ty = ctx.builder.func.dfg.value_type(left);
        if divisor == 0 {
            let fault_fn = self.arith_fault_fn.ok_or_else(|| anyhow!("VM arith fault runtime is not registered"))?;
            let fault_ref = self.get_fn_ref(ctx, fault_fn);
            ctx.builder.ins().call(fault_ref, &[]);
            return Ok(ctx.builder.ins().iconst(int_ty, 0));
        }
        if signed && divisor == -1 {
            let rv = ctx.builder.ins().iconst(int_ty, -1);
            return self.guarded_idiv(ctx, left, rv, true, is_rem);
        }
        Ok(match (signed, is_rem) {
            (true, false) => ctx.builder.ins().sdiv_imm(left, divisor),
            (false, false) => ctx.builder.ins().udiv_imm(left, divisor),
            (true, true) => ctx.builder.ins().srem_imm(left, divisor),
            (false, true) => ctx.builder.ins().urem_imm(left, divisor),
        })
    }

    pub(crate) fn binary_with_expected(&mut self, ctx: &mut BuildContext, left: (Value, Type), op: BinaryOp, right: &Expr, expected: Option<&Type>) -> Result<(Value, Type)> {
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
        let numeric_expected = expected.filter(|ty| (ty.is_int() || ty.is_uint() || ty.is_float()) && (left.1.is_any() || right.1.is_any()));
        let ty = if (op.is_add() || op.is_logic()) && (left.1.is_str() || right.1.is_str() || right_ty.is_str()) {
            Type::Str
        } else if !op.is_logic()
            && let Some(expected) = numeric_expected
        {
            expected.clone()
        } else if (op.is_add() || op.is_logic()) && (left.1.is_any() || right.1.is_any()) {
            Type::Any
        } else {
            left.1.clone() + right.1.clone()
        }; //为了支持字符串的加法需要单独处理
        if ty.is_str() && op.is_add() {
            if op == BinaryOp::AddAssign {
                let left = self.convert(ctx, left, Type::Any)?;
                let right = self.convert(ctx, right, Type::Any)?;
                return Ok((self.strcat_assign(ctx, left, right)?, ty));
            }
            if left.1.is_str() && right.1.is_str() {
                return Ok((self.strcat(ctx, left.0, right.0)?, Type::Str));
            }
            if left.1.is_str() && right.1.is_int() {
                let right = self.convert(ctx, right, Type::I64)?;
                return Ok((self.strcat_i64(ctx, left.0, right)?, Type::Str));
            }
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
                    return Ok((self.guarded_idiv(ctx, left, right, true, false)?, ty));
                } else if ty.is_uint() {
                    return Ok((self.guarded_idiv(ctx, left, right, false, false)?, ty));
                } else if ty.is_float() {
                    return Ok((ctx.builder.ins().fdiv(left, right), ty));
                }
            }
            BinaryOp::Mod | BinaryOp::ModAssign => {
                if ty.is_int() {
                    return Ok((self.guarded_idiv(ctx, left, right, true, true)?, ty));
                } else if ty.is_uint() {
                    return Ok((self.guarded_idiv(ctx, left, right, false, true)?, ty));
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
        // 回退到动态分发，避免因未知类型组合导致进程崩溃
        log::debug!("binary_with_expected fallback to dynamic: {:?} {:?} {:?}", ty, op, right);
        let left_any = self.convert(ctx, (left, ty.clone()), Type::Any)?;
        let right_any = self.convert(ctx, (right, ty.clone()), Type::Any)?;
        if op.is_logic() { self.any_logic(ctx, left_any, op, right_any) } else { self.any_binary(ctx, left_any, op, right_any) }
    }

    pub(crate) fn binary_imm<'a>(&mut self, ctx: &'a mut BuildContext, left: (Value, Type), op: BinaryOp, right: Dynamic) -> Result<(Value, Type)> {
        let ty = left.1.clone() + right.get_type();
        let bool_imm = || right.as_bool().map(|value| if value { 1 } else { 0 });
        if ty.is_str() && op.is_add() {
            if op == BinaryOp::AddAssign {
                let left = self.convert(ctx, left, Type::Any)?;
                let right_vt = ctx.get_const(&right).or_else(|_| {
                    let idx = self.compiler.get_const(right.clone());
                    self.get_const_value(ctx, idx)
                })?;
                let right = self.convert(ctx, right_vt, Type::Any)?;
                return Ok((self.strcat_assign(ctx, left, right)?, ty));
            }
            if left.1.is_str() && right.is_str() {
                let right_vt = ctx.get_const(&right).or_else(|_| {
                    let idx = self.compiler.get_const(right.clone());
                    self.get_const_value(ctx, idx)
                })?;
                let right = self.convert(ctx, right_vt, Type::Str)?;
                return Ok((self.strcat(ctx, left.0, right)?, Type::Str));
            }
            if left.1.is_str() && right.is_int() {
                let right = ctx.get_const(&right)?;
                let right = self.convert(ctx, right, Type::I64)?;
                return Ok((self.strcat_i64(ctx, left.0, right)?, Type::Str));
            }
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
                if ty.is_int() || ty.is_uint() {
                    let divisor = right.as_int().ok_or(anyhow!("非整数"))?;
                    return Ok((self.idiv_imm(ctx, left, divisor, ty.is_int(), false)?, ty));
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
                if ty.is_int() || ty.is_uint() {
                    let divisor = right.as_int().ok_or(anyhow!("非整数"))?;
                    return Ok((self.idiv_imm(ctx, left, divisor, ty.is_int(), true)?, ty));
                }
            }
            exp => {
                // 回退到动态分发，避免因未知操作导致进程崩溃
                log::debug!("binary_imm fallback to dynamic (unsupported op): {:?} {:?}", ty, exp);
                let left_any = self.convert(ctx, (left, ty.clone()), Type::Any)?;
                let right_vt = ctx.get_const(&right).or_else(|_| {
                    let idx = self.compiler.get_const(right.clone());
                    self.get_const_value(ctx, idx)
                })?;
                let right_any = self.convert(ctx, right_vt, Type::Any)?;
                if exp.is_logic() {
                    return self.any_logic(ctx, left_any, exp, right_any);
                }
                return self.any_binary(ctx, left_any, exp, right_any);
            }
        }
        // 回退到动态分发，避免因未知类型组合导致进程崩溃
        log::debug!("binary_imm fallback to dynamic (unsupported type combo): {:?} {:?} {:?}", ty, op, right.get_type());
        let left_any = self.convert(ctx, (left, ty.clone()), Type::Any)?;
        let right_vt = ctx.get_const(&right).or_else(|_| {
            let idx = self.compiler.get_const(right.clone());
            self.get_const_value(ctx, idx)
        })?;
        let right_any = self.convert(ctx, right_vt, Type::Any)?;
        if op.is_logic() { self.any_logic(ctx, left_any, op, right_any) } else { self.any_binary(ctx, left_any, op, right_any) }
    }
}
