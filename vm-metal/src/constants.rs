use anyhow::{Result, bail};
use dynamic::{Dynamic, Type};

use crate::{
    context::{MetalCompiler, Value},
    util::format_float,
};

impl MetalCompiler {
    pub(crate) fn const_dynamic(&self, value: Dynamic) -> Result<Value> {
        let ty = value.get_type();
        let code = match value {
            Dynamic::Bool(true) => "true".to_string(),
            Dynamic::Bool(false) => "false".to_string(),
            Dynamic::F32(v) => format_float(v as f64, "f"),
            Dynamic::F64(v) => format_float(v, ""),
            Dynamic::I8(v) => v.to_string(),
            Dynamic::I16(v) => v.to_string(),
            Dynamic::I32(v) => v.to_string(),
            Dynamic::I64(v) => format!("{v}L"),
            Dynamic::U8(v) => format!("{v}u"),
            Dynamic::U16(v) => format!("{v}u"),
            Dynamic::U32(v) => format!("{v}u"),
            Dynamic::U64(v) => format!("{v}ul"),
            other => bail!("unsupported Metal constant: {other:?}"),
        };
        Ok(Value { code, ty })
    }

    pub(crate) fn const_u32(&self, value: &Value) -> Option<u32> {
        value.code.strip_suffix('u').or(Some(value.code.as_str())).and_then(|s| s.parse::<u32>().ok())
    }

    pub(crate) fn zero_literal(&self, ty: &Type) -> String {
        if ty.is_float() { "0.0".to_string() } else { "0".to_string() }
    }

    pub(crate) fn one_literal(&self, ty: &Type) -> String {
        if ty.is_float() {
            "1.0".to_string()
        } else if ty.is_uint() {
            "1u".to_string()
        } else {
            "1".to_string()
        }
    }
}
