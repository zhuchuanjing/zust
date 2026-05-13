use dynamic::Type;
use parser::BinaryOp;
use std::{collections::BTreeMap, rc::Rc};

pub(crate) fn resolve_type_in_defs(ty: &Type, type_defs: &BTreeMap<u32, Type>) -> Type {
    match ty {
        Type::Symbol { id, .. } => type_defs.get(id).cloned().map(|ty| resolve_type_in_defs(&ty, type_defs)).unwrap_or_else(|| ty.clone()),
        Type::Struct { params, fields } => Type::Struct {
            params: params.iter().map(|ty| resolve_type_in_defs(ty, type_defs)).collect(),
            fields: fields.iter().filter_map(|(name, ty)| if matches!(ty, Type::Symbol { id, .. } if !type_defs.contains_key(id)) { None } else { Some((name.clone(), resolve_type_in_defs(ty, type_defs))) }).collect(),
        },
        Type::Vec(elem, len) => Type::Vec(Rc::new(resolve_type_in_defs(elem, type_defs)), *len),
        Type::Array(elem, len) => Type::Array(Rc::new(resolve_type_in_defs(elem, type_defs)), *len),
        Type::Fn { tys, ret } => Type::Fn { tys: tys.iter().map(|ty| resolve_type_in_defs(ty, type_defs)).collect(), ret: Rc::new(resolve_type_in_defs(ret, type_defs)) },
        _ => ty.clone(),
    }
}

pub(crate) fn assignment_base_op(op: &BinaryOp) -> Option<&BinaryOp> {
    match op {
        BinaryOp::AddAssign => Some(&BinaryOp::Add),
        BinaryOp::SubAssign => Some(&BinaryOp::Sub),
        BinaryOp::MulAssign => Some(&BinaryOp::Mul),
        BinaryOp::DivAssign => Some(&BinaryOp::Div),
        BinaryOp::ModAssign => Some(&BinaryOp::Mod),
        BinaryOp::ShlAssign => Some(&BinaryOp::Shl),
        BinaryOp::ShrAssign => Some(&BinaryOp::Shr),
        BinaryOp::BitAndAssign => Some(&BinaryOp::BitAnd),
        BinaryOp::BitOrAssign => Some(&BinaryOp::BitOr),
        BinaryOp::BitXorAssign => Some(&BinaryOp::BitXor),
        _ => None,
    }
}

pub(crate) fn sanitize_ident(name: &str) -> String {
    let mut out = String::with_capacity(name.len());
    for (idx, ch) in name.chars().enumerate() {
        if (idx == 0 && (ch.is_ascii_alphabetic() || ch == '_')) || (idx > 0 && (ch.is_ascii_alphanumeric() || ch == '_')) {
            out.push(ch);
        } else {
            out.push('_');
        }
    }
    if out.is_empty() { "_".to_string() } else { out }
}

pub(crate) fn format_float(value: f64, suffix: &str) -> String {
    if value.is_nan() {
        format!("NAN{suffix}")
    } else if value.is_infinite() && value.is_sign_positive() {
        format!("INFINITY{suffix}")
    } else if value.is_infinite() {
        format!("-INFINITY{suffix}")
    } else {
        let mut text = format!("{value:?}");
        if !text.contains('.') && !text.contains('e') && !text.contains('E') {
            text.push_str(".0");
        }
        text.push_str(suffix);
        text
    }
}
