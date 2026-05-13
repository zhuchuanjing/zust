use anyhow::{Result, bail};
use dynamic::{Dynamic, Type};

use crate::context::{SpirvCompiler, SpirvTy, Value};

impl SpirvCompiler {
    pub(crate) fn const_zero(&mut self, ty: Type) -> Result<Value> {
        let value = if ty.is_float() {
            if ty.is_f64() { Dynamic::F64(0.0) } else { Dynamic::F32(0.0) }
        } else if ty.is_uint() {
            Dynamic::U32(0)
        } else {
            Dynamic::I32(0)
        };
        self.const_dynamic(value).and_then(|v| self.convert(v, ty))
    }

    pub(crate) fn const_u32(&mut self, value: u32) -> u32 {
        self.const_dynamic(Dynamic::U32(value)).expect("u32 constants are supported").id
    }

    pub(crate) fn get_const_u32(&self, value: &Value) -> Option<u32> {
        self.consts.iter().find_map(|(c, id)| if *id == value.id { c.as_uint().map(|v| v as u32).or_else(|| c.as_int().and_then(|v| if v >= 0 { Some(v as u32) } else { None })) } else { None })
    }

    pub(crate) fn const_value(&self, id: u32) -> Option<Dynamic> {
        self.consts.iter().find_map(|(value, const_id)| (*const_id == id).then(|| value.clone()))
    }

    pub(crate) fn const_dynamic(&mut self, value: Dynamic) -> Result<Value> {
        let ty = value.get_type();
        if let Some((_, id)) = self.consts.iter().find(|(v, _)| v == &value && v.get_type() == ty) {
            return Ok(Value { id: *id, ty: value.get_type() });
        }
        let ty_id = self.get_type(SpirvTy::Value(ty.clone()));
        let id = match value.clone() {
            Dynamic::Bool(true) => self.builder.constant_true(ty_id),
            Dynamic::Bool(false) => self.builder.constant_false(ty_id),
            Dynamic::F32(v) => self.builder.constant_bit32(ty_id, v.to_bits()),
            Dynamic::F64(v) => self.builder.constant_bit64(ty_id, v.to_bits()),
            Dynamic::I8(v) => self.builder.constant_bit32(ty_id, v as i32 as u32),
            Dynamic::I16(v) => self.builder.constant_bit32(ty_id, v as i32 as u32),
            Dynamic::I32(v) => self.builder.constant_bit32(ty_id, v as u32),
            Dynamic::I64(v) => self.builder.constant_bit64(ty_id, v as u64),
            Dynamic::U8(v) => self.builder.constant_bit32(ty_id, v as u32),
            Dynamic::U16(v) => self.builder.constant_bit32(ty_id, v as u32),
            Dynamic::U32(v) => self.builder.constant_bit32(ty_id, v),
            Dynamic::U64(v) => self.builder.constant_bit64(ty_id, v),
            other => bail!("unsupported SPIR-V constant: {other:?}"),
        };
        self.consts.push((value, id));
        Ok(Value { id, ty })
    }
}
