use anyhow::{Result, anyhow, bail};
use dynamic::Type;
use parser::{BinaryOp, UnaryOp};
use rspirv::dr::Operand;
use spirv::StorageClass;

use crate::context::{SpirvCompiler, SpirvTy, Value};

impl SpirvCompiler {
    pub(crate) fn unary(&mut self, op: &UnaryOp, value: Value) -> Result<Value> {
        let ty_id = self.get_type(SpirvTy::Value(value.ty.clone()));
        match op {
            UnaryOp::Neg if value.ty.is_float() => Ok(Value { id: self.builder.f_negate(ty_id, None, value.id)?, ty: value.ty }),
            UnaryOp::Neg if value.ty.is_int() || value.ty.is_uint() => Ok(Value { id: self.builder.s_negate(ty_id, None, value.id)?, ty: value.ty }),
            UnaryOp::Not if value.ty.is_int() || value.ty.is_uint() => Ok(Value { id: self.builder.not(ty_id, None, value.id)?, ty: value.ty }),
            UnaryOp::Not => {
                let bool_id = self.get_type(SpirvTy::Value(Type::Bool));
                let value = self.bool_value(value)?;
                Ok(Value { id: self.builder.logical_not(bool_id, None, value.id)?, ty: Type::Bool })
            }
            _ => bail!("unsupported unary op {op:?} for {:?}", value.ty),
        }
    }

    pub(crate) fn index(&mut self, obj: Value, idx: Value) -> Result<Value> {
        match obj.ty.clone() {
            Type::Vec(elem_ty, 0) => {
                let ptr = self.index_runtime_array_ptr(obj.id, (*elem_ty).clone(), idx)?;
                let ty = ptr.ty.clone();
                let ty_id = self.get_type(SpirvTy::Value(ty.clone()));
                let id = self.builder.load(ty_id, None, ptr.id, None, None)?;
                Ok(Value { id, ty })
            }
            Type::Struct { params: _, fields } => {
                let idx_const = self.get_const_u32(&idx).ok_or_else(|| anyhow!("SPIR-V struct indexes must be compile-time u32 constants"))?;
                let ty = fields.get(idx_const as usize).map(|(_, ty)| ty.clone()).ok_or_else(|| anyhow!("SPIR-V struct index {idx_const} out of bounds"))?;
                let ty_id = self.get_type(SpirvTy::Value(ty.clone()));
                let id = self.builder.composite_extract(ty_id, None, obj.id, [idx_const])?;
                Ok(Value { id, ty })
            }
            Type::Vec(elem_ty, len) | Type::Array(elem_ty, len) => {
                let idx_const = self.get_const_u32(&idx).ok_or_else(|| anyhow!("SPIR-V vector indexes must be compile-time u32 constants for now"))?;
                if idx_const >= len {
                    bail!("SPIR-V index {idx_const} out of bounds for length {len}");
                }
                let ty = (*elem_ty).clone();
                let ty_id = self.get_type(SpirvTy::Value(ty.clone()));
                let id = self.builder.composite_extract(ty_id, None, obj.id, [idx_const])?;
                Ok(Value { id, ty })
            }
            ty => bail!("unsupported SPIR-V index on {ty:?}"),
        }
    }

    pub(crate) fn index_ptr(&mut self, obj: Value, idx: Value) -> Result<Value> {
        match obj.ty.clone() {
            Type::Vec(elem_ty, 0) => self.index_runtime_array_ptr(obj.id, (*elem_ty).clone(), idx),
            ty => bail!("unsupported SPIR-V indexed assignment on {ty:?}"),
        }
    }

    pub(crate) fn index_local_ptr(&mut self, ptr: Value, idx: Value) -> Result<Value> {
        match ptr.ty.clone() {
            Type::Array(elem_ty, len) => {
                let idx = self.convert(idx, Type::U32)?;
                if let Some(idx_const) = self.get_const_u32(&idx)
                    && idx_const >= len
                {
                    bail!("SPIR-V index {idx_const} out of bounds for length {len}");
                }
                let elem_ty = (*elem_ty).clone();
                let ptr_ty = self.get_type(SpirvTy::Pointer(elem_ty.clone(), StorageClass::Function));
                let id = self.builder.access_chain(ptr_ty, None, ptr.id, [idx.id])?;
                Ok(Value { id, ty: elem_ty })
            }
            Type::Struct { fields, .. } => {
                let idx_const = self.get_const_u32(&idx).ok_or_else(|| anyhow!("SPIR-V struct indexes must be compile-time u32 constants"))?;
                let field_ty = fields.get(idx_const as usize).map(|(_, ty)| ty.clone()).ok_or_else(|| anyhow!("SPIR-V struct index {idx_const} out of bounds"))?;
                let ptr_ty = self.get_type(SpirvTy::Pointer(field_ty.clone(), StorageClass::Function));
                let id = self.builder.access_chain(ptr_ty, None, ptr.id, [idx.id])?;
                Ok(Value { id, ty: field_ty })
            }
            ty => bail!("unsupported SPIR-V local pointer index on {ty:?}"),
        }
    }

    pub(crate) fn index_runtime_array_ptr(&mut self, buffer: u32, elem_ty: Type, idx: Value) -> Result<Value> {
        let zero = self.const_u32(0);
        let idx = self.convert(idx, Type::U32)?;
        let ptr_ty = self.get_type(SpirvTy::Pointer(elem_ty.clone(), StorageClass::StorageBuffer));
        let id = self.builder.access_chain(ptr_ty, None, buffer, [zero, idx.id])?;
        Ok(Value { id, ty: elem_ty })
    }

    pub(crate) fn binary(&mut self, left: Value, op: &BinaryOp, right: Value) -> Result<Value> {
        let ty = if op.is_logic() { if matches!(op, BinaryOp::And | BinaryOp::Or) { Type::Bool } else { left.ty.clone() + right.ty.clone() } } else { left.ty.clone() + right.ty.clone() };
        let left = self.convert(left, ty.clone())?;
        let right = self.convert(right, ty.clone())?;
        let ty_id = self.get_type(SpirvTy::Value(ty.clone()));
        let bool_id = self.get_type(SpirvTy::Value(Type::Bool));
        let id = match op {
            BinaryOp::Add | BinaryOp::AddAssign if ty.is_float() => self.builder.f_add(ty_id, None, left.id, right.id)?,
            BinaryOp::Add | BinaryOp::AddAssign => self.builder.i_add(ty_id, None, left.id, right.id)?,
            BinaryOp::Sub | BinaryOp::SubAssign if ty.is_float() => self.builder.f_sub(ty_id, None, left.id, right.id)?,
            BinaryOp::Sub | BinaryOp::SubAssign => self.builder.i_sub(ty_id, None, left.id, right.id)?,
            BinaryOp::Mul | BinaryOp::MulAssign if ty.is_float() => self.builder.f_mul(ty_id, None, left.id, right.id)?,
            BinaryOp::Mul | BinaryOp::MulAssign => self.builder.i_mul(ty_id, None, left.id, right.id)?,
            BinaryOp::Div | BinaryOp::DivAssign if ty.is_float() => self.builder.f_div(ty_id, None, left.id, right.id)?,
            BinaryOp::Div | BinaryOp::DivAssign if ty.is_int() => self.builder.s_div(ty_id, None, left.id, right.id)?,
            BinaryOp::Div | BinaryOp::DivAssign => self.builder.u_div(ty_id, None, left.id, right.id)?,
            BinaryOp::Mod | BinaryOp::ModAssign if ty.is_int() => self.builder.s_mod(ty_id, None, left.id, right.id)?,
            BinaryOp::Mod | BinaryOp::ModAssign => self.builder.u_mod(ty_id, None, left.id, right.id)?,
            BinaryOp::Eq if ty.is_float() => self.builder.f_ord_equal(bool_id, None, left.id, right.id)?,
            BinaryOp::Eq if ty.is_bool() => self.builder.logical_equal(bool_id, None, left.id, right.id)?,
            BinaryOp::Eq => self.builder.i_equal(bool_id, None, left.id, right.id)?,
            BinaryOp::Ne if ty.is_float() => self.builder.f_ord_not_equal(bool_id, None, left.id, right.id)?,
            BinaryOp::Ne if ty.is_bool() => self.builder.logical_not_equal(bool_id, None, left.id, right.id)?,
            BinaryOp::Ne => self.builder.i_not_equal(bool_id, None, left.id, right.id)?,
            BinaryOp::Lt if ty.is_float() => self.builder.f_ord_less_than(bool_id, None, left.id, right.id)?,
            BinaryOp::Lt if ty.is_int() => self.builder.s_less_than(bool_id, None, left.id, right.id)?,
            BinaryOp::Lt => self.builder.u_less_than(bool_id, None, left.id, right.id)?,
            BinaryOp::Le if ty.is_float() => self.builder.f_ord_less_than_equal(bool_id, None, left.id, right.id)?,
            BinaryOp::Le if ty.is_int() => self.builder.s_less_than_equal(bool_id, None, left.id, right.id)?,
            BinaryOp::Le => self.builder.u_less_than_equal(bool_id, None, left.id, right.id)?,
            BinaryOp::Gt if ty.is_float() => self.builder.f_ord_greater_than(bool_id, None, left.id, right.id)?,
            BinaryOp::Gt if ty.is_int() => self.builder.s_greater_than(bool_id, None, left.id, right.id)?,
            BinaryOp::Gt => self.builder.u_greater_than(bool_id, None, left.id, right.id)?,
            BinaryOp::Ge if ty.is_float() => self.builder.f_ord_greater_than_equal(bool_id, None, left.id, right.id)?,
            BinaryOp::Ge if ty.is_int() => self.builder.s_greater_than_equal(bool_id, None, left.id, right.id)?,
            BinaryOp::Ge => self.builder.u_greater_than_equal(bool_id, None, left.id, right.id)?,
            BinaryOp::And => self.builder.logical_and(bool_id, None, left.id, right.id)?,
            BinaryOp::Or => self.builder.logical_or(bool_id, None, left.id, right.id)?,
            BinaryOp::BitAnd | BinaryOp::BitAndAssign => self.builder.bitwise_and(ty_id, None, left.id, right.id)?,
            BinaryOp::BitOr | BinaryOp::BitOrAssign => self.builder.bitwise_or(ty_id, None, left.id, right.id)?,
            BinaryOp::BitXor | BinaryOp::BitXorAssign => self.builder.bitwise_xor(ty_id, None, left.id, right.id)?,
            BinaryOp::Shl | BinaryOp::ShlAssign => self.builder.shift_left_logical(ty_id, None, left.id, right.id)?,
            BinaryOp::Shr | BinaryOp::ShrAssign if ty.is_int() => self.builder.shift_right_arithmetic(ty_id, None, left.id, right.id)?,
            BinaryOp::Shr | BinaryOp::ShrAssign => self.builder.shift_right_logical(ty_id, None, left.id, right.id)?,
            _ => bail!("unsupported binary op {op:?} for {ty:?}"),
        };
        let out_ty = if op.is_logic() { Type::Bool } else { ty };
        Ok(Value { id, ty: out_ty })
    }

    pub(crate) fn glsl1(&mut self, value: Value, float_op: spirv::GlslStd450Op, int_op: spirv::GlslStd450Op) -> Result<Value> {
        let op = if value.ty.is_float() { float_op } else { int_op };
        self.glsl1_raw(value, op)
    }

    pub(crate) fn glsl1_raw(&mut self, value: Value, op: spirv::GlslStd450Op) -> Result<Value> {
        let glsl = self.glsl_import();
        let ty_id = self.get_type(SpirvTy::Value(value.ty.clone()));
        let id = self.builder.ext_inst(ty_id, None, glsl, op as u32, [Operand::IdRef(value.id)])?;
        Ok(Value { id, ty: value.ty })
    }

    pub(crate) fn glsl2(&mut self, left: Value, right: Value, float_op: spirv::GlslStd450Op, int_op: spirv::GlslStd450Op, uint_op: spirv::GlslStd450Op) -> Result<Value> {
        let ty = left.ty.clone() + right.ty.clone();
        let left = self.convert(left, ty.clone())?;
        let right = self.convert(right, ty.clone())?;
        let op = if ty.is_float() {
            float_op
        } else if ty.is_int() {
            int_op
        } else {
            uint_op
        };
        let glsl = self.glsl_import();
        let ty_id = self.get_type(SpirvTy::Value(ty.clone()));
        let id = self.builder.ext_inst(ty_id, None, glsl, op as u32, [Operand::IdRef(left.id), Operand::IdRef(right.id)])?;
        Ok(Value { id, ty })
    }

    pub(crate) fn glsl_float2(&mut self, left: Value, right: Value, op: spirv::GlslStd450Op) -> Result<Value> {
        let ty = self.resolve_type(&(left.ty.clone() + right.ty.clone()));
        if !ty.is_float() {
            bail!("GLSL operation {op:?} expects floating-point operands, got {ty:?}");
        }
        let left = self.convert(left, ty.clone())?;
        let right = self.convert(right, ty.clone())?;
        let glsl = self.glsl_import();
        let ty_id = self.get_type(SpirvTy::Value(ty.clone()));
        let id = self.builder.ext_inst(ty_id, None, glsl, op as u32, [Operand::IdRef(left.id), Operand::IdRef(right.id)])?;
        Ok(Value { id, ty })
    }

    pub(crate) fn glsl_float3(&mut self, first: Value, second: Value, third: Value, op: spirv::GlslStd450Op) -> Result<Value> {
        let ty = self.resolve_type(&(first.ty.clone() + second.ty.clone() + third.ty.clone()));
        if !ty.is_float() {
            bail!("GLSL operation {op:?} expects floating-point operands, got {ty:?}");
        }
        let first = self.convert(first, ty.clone())?;
        let second = self.convert(second, ty.clone())?;
        let third = self.convert(third, ty.clone())?;
        let glsl = self.glsl_import();
        let ty_id = self.get_type(SpirvTy::Value(ty.clone()));
        let id = self.builder.ext_inst(ty_id, None, glsl, op as u32, [Operand::IdRef(first.id), Operand::IdRef(second.id), Operand::IdRef(third.id)])?;
        Ok(Value { id, ty })
    }

    pub(crate) fn bool_value(&mut self, value: Value) -> Result<Value> {
        if value.ty.is_bool() {
            Ok(value)
        } else if value.ty.is_int() || value.ty.is_uint() {
            let zero = self.const_zero(value.ty.clone())?;
            let bool_id = self.get_type(SpirvTy::Value(Type::Bool));
            Ok(Value { id: self.builder.i_not_equal(bool_id, None, value.id, zero.id)?, ty: Type::Bool })
        } else if value.ty.is_float() {
            let zero = self.const_zero(value.ty.clone())?;
            let bool_id = self.get_type(SpirvTy::Value(Type::Bool));
            Ok(Value { id: self.builder.f_ord_not_equal(bool_id, None, value.id, zero.id)?, ty: Type::Bool })
        } else {
            bail!("cannot convert {:?} to bool in SPIR-V", value.ty)
        }
    }

    pub(crate) fn convert(&mut self, value: Value, ty: Type) -> Result<Value> {
        let value = Value { id: value.id, ty: self.resolve_type(&value.ty) };
        let ty = self.resolve_type(&ty);
        if value.ty == ty || ty.is_any() {
            return Ok(value);
        }
        if ty.is_native()
            && let Some(const_value) = self.const_value(value.id)
            && let Ok(value) = ty.force(const_value)
        {
            return self.const_dynamic(value);
        }
        let ty_id = self.get_type(SpirvTy::Value(ty.clone()));
        let id = if ty.is_float() && value.ty.is_float() {
            self.builder.f_convert(ty_id, None, value.id)?
        } else if ty.is_float() && value.ty.is_int() {
            self.builder.convert_s_to_f(ty_id, None, value.id)?
        } else if ty.is_float() && value.ty.is_uint() {
            self.builder.convert_u_to_f(ty_id, None, value.id)?
        } else if ty.is_int() && value.ty.is_float() {
            self.builder.convert_f_to_s(ty_id, None, value.id)?
        } else if ty.is_uint() && value.ty.is_float() {
            self.builder.convert_f_to_u(ty_id, None, value.id)?
        } else if ty.is_int() && value.ty.is_uint() {
            if ty.width() == value.ty.width() { self.builder.bitcast(ty_id, None, value.id)? } else { self.builder.u_convert(ty_id, None, value.id)? }
        } else if ty.is_uint() && value.ty.is_int() {
            if ty.width() == value.ty.width() { self.builder.bitcast(ty_id, None, value.id)? } else { self.builder.s_convert(ty_id, None, value.id)? }
        } else if (ty.is_int() && value.ty.is_int()) || (ty.is_uint() && value.ty.is_uint()) {
            if value.ty.is_int() { self.builder.s_convert(ty_id, None, value.id)? } else { self.builder.u_convert(ty_id, None, value.id)? }
        } else {
            bail!("unsupported SPIR-V conversion {:?} -> {:?}", value.ty, ty);
        };
        Ok(Value { id, ty })
    }
}
