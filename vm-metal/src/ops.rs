use anyhow::{Result, anyhow, bail};
use dynamic::Type;
use parser::{BinaryOp, Expr, ExprKind};

use crate::{
    context::{MetalCompiler, Value},
    util::sanitize_ident,
};

impl MetalCompiler {
    pub(crate) fn assign(&mut self, target: &Expr, value: Value) -> Result<()> {
        if let ExprKind::Var(idx) = &target.kind {
            let idx = *idx as usize;
            if self.vars.get(idx).and_then(Clone::clone).is_none() {
                let ty = self.resolve_type(&value.ty);
                let name = self.var_name(idx);
                let value = self.convert_code(value, ty.clone())?;
                self.line(format!("{} {name} = {};", self.msl_type(&ty), value.code));
                self.set_var(idx, name, ty);
                return Ok(());
            }
        }
        if let ExprKind::Id(id, None) = &target.kind
            && self.workgroup_static_tys.contains_key(id)
        {
            let ty = self.resolve_type(self.workgroup_static_tys.get(id).unwrap());
            let value = self.convert_code(value, ty.clone())?;
            if self.atomic_type(&ty).is_some() {
                self.line(format!("atomic_store_explicit(&zust_static_{id}, {}, memory_order_relaxed);", value.code));
            } else {
                self.line(format!("zust_static_{id} = {};", value.code));
            }
            return Ok(());
        }
        let target = self.lvalue(target)?;
        let value = self.convert_code(value, target.ty)?;
        self.line(format!("{} = {};", target.code, value.code));
        Ok(())
    }

    pub(crate) fn lvalue(&mut self, expr: &Expr) -> Result<Value> {
        match &expr.kind {
            ExprKind::Var(idx) => self.get_var(*idx as usize),
            ExprKind::Ident(name) => self.get_named_var(name),
            ExprKind::Binary { left, op: BinaryOp::Idx, right } => {
                let idx = self.gen_expr(right)?;
                let target = self.lvalue(left).or_else(|_| self.gen_expr(left))?;
                self.index(target, idx)
            }
            other => bail!("unsupported Metal assignment target: {other:?}"),
        }
    }

    pub(crate) fn index(&mut self, obj: Value, idx: Value) -> Result<Value> {
        match self.resolve_type(&obj.ty) {
            Type::Struct { fields, .. } => {
                let idx_const = self.const_u32(&idx).ok_or_else(|| anyhow!("Metal struct indexes must be compile-time u32 constants"))? as usize;
                let (field, ty) = fields.get(idx_const).ok_or_else(|| anyhow!("Metal struct index {idx_const} out of bounds"))?;
                Ok(Value { code: format!("{}.{}", obj.code, sanitize_ident(field)), ty: ty.clone() })
            }
            Type::Vec(elem_ty, 0) => Ok(Value { code: format!("{}[{}]", obj.code, self.convert_code(idx, Type::U32)?.code), ty: elem_ty.as_ref().clone() }),
            Type::Vec(elem_ty, _) | Type::Array(elem_ty, _) => Ok(Value { code: format!("{}[{}]", obj.code, self.convert_code(idx, Type::U32)?.code), ty: elem_ty.as_ref().clone() }),
            ty => bail!("unsupported Metal index on {ty:?} for {}", obj.code),
        }
    }

    pub(crate) fn binary(&mut self, left: Value, op: &BinaryOp, right: Value) -> Result<Value> {
        let out_ty = if op.is_logic() { Type::Bool } else { self.resolve_type(&(left.ty.clone() + right.ty.clone())) };
        let ty = if matches!(op, BinaryOp::And | BinaryOp::Or) {
            Type::Bool
        } else if op.is_logic() {
            self.resolve_type(&(left.ty.clone() + right.ty.clone()))
        } else {
            out_ty.clone()
        };
        let left = if matches!(op, BinaryOp::And | BinaryOp::Or) { self.bool_expr(left)? } else { self.convert_code(left, ty.clone())? };
        let right = if matches!(op, BinaryOp::And | BinaryOp::Or) { self.bool_expr(right)? } else { self.convert_code(right, ty.clone())? };
        let op_str = match op {
            BinaryOp::Add => "+",
            BinaryOp::Sub => "-",
            BinaryOp::Mul => "*",
            BinaryOp::Div => "/",
            BinaryOp::Mod => "%",
            BinaryOp::Eq => "==",
            BinaryOp::Ne => "!=",
            BinaryOp::Lt => "<",
            BinaryOp::Gt => ">",
            BinaryOp::Le => "<=",
            BinaryOp::Ge => ">=",
            BinaryOp::And => "&&",
            BinaryOp::Or => "||",
            BinaryOp::BitAnd => "&",
            BinaryOp::BitOr => "|",
            BinaryOp::BitXor => "^",
            BinaryOp::Shl => "<<",
            BinaryOp::Shr => ">>",
            _ => bail!("unsupported Metal binary op {op:?}"),
        };
        Ok(Value { code: format!("({} {op_str} {})", left.code, right.code), ty: out_ty })
    }

    pub(crate) fn call_math2(&mut self, name: &str, left: Value, right: Value) -> Result<Value> {
        let ty = self.resolve_type(&(left.ty.clone() + right.ty.clone()));
        let left = self.convert_code(left, ty.clone())?;
        let right = self.convert_code(right, ty.clone())?;
        Ok(Value { code: format!("{name}({}, {})", left.code, right.code), ty })
    }

    pub(crate) fn call_float_math2(&mut self, name: &str, left: Value, right: Value) -> Result<Value> {
        let ty = self.resolve_type(&(left.ty.clone() + right.ty.clone()));
        if !ty.is_float() {
            bail!("Metal math function {name} expects floating-point operands, got {ty:?}");
        }
        let left = self.convert_code(left, ty.clone())?;
        let right = self.convert_code(right, ty.clone())?;
        Ok(Value { code: format!("{name}({}, {})", left.code, right.code), ty })
    }

    pub(crate) fn call_math3(&mut self, name: &str, first: Value, second: Value, third: Value) -> Result<Value> {
        let ty = self.resolve_type(&(first.ty.clone() + second.ty.clone() + third.ty.clone()));
        if !ty.is_float() {
            bail!("Metal math function {name} expects floating-point operands, got {ty:?}");
        }
        let first = self.convert_code(first, ty.clone())?;
        let second = self.convert_code(second, ty.clone())?;
        let third = self.convert_code(third, ty.clone())?;
        Ok(Value { code: format!("{name}({}, {}, {})", first.code, second.code, third.code), ty })
    }

    pub(crate) fn bool_expr(&mut self, value: Value) -> Result<Value> {
        if value.ty.is_bool() {
            Ok(value)
        } else if value.ty.is_int() || value.ty.is_uint() || value.ty.is_float() {
            Ok(Value { code: format!("({} != {})", value.code, self.zero_literal(&value.ty)), ty: Type::Bool })
        } else {
            bail!("cannot convert {:?} to bool in Metal", value.ty)
        }
    }

    pub(crate) fn convert_code(&mut self, value: Value, ty: Type) -> Result<Value> {
        let ty = self.resolve_type(&ty);
        let value_ty = self.resolve_type(&value.ty);
        if ty.is_any() || value_ty == ty {
            return Ok(Value { code: value.code, ty: value_ty });
        }
        if ty.is_native() || ty.is_bool() {
            return Ok(Value { code: format!("{}({})", self.msl_type(&ty), value.code), ty });
        }
        bail!("unsupported Metal conversion {:?} -> {:?}", value_ty, ty)
    }
}
