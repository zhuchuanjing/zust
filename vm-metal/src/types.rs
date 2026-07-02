use dynamic::Type;

use crate::{context::MetalCompiler, util::resolve_type_in_defs};

impl MetalCompiler {
    pub(crate) fn msl_type(&self, ty: &Type) -> String {
        let ty = self.resolve_type(ty);
        match ty {
            Type::Void => "void".to_string(),
            Type::Bool => "bool".to_string(),
            Type::U8 => "uchar".to_string(),
            Type::I8 => "char".to_string(),
            Type::U16 => "ushort".to_string(),
            Type::I16 => "short".to_string(),
            Type::U32 => "uint".to_string(),
            Type::I32 => "int".to_string(),
            Type::U64 => "ulong".to_string(),
            Type::I64 => "long".to_string(),
            Type::F16 => "half".to_string(),
            Type::F32 => "float".to_string(),
            Type::F64 => "double".to_string(),
            Type::Vec(elem, len @ 2..=4) => format!("{}{}", self.msl_type(&elem), len),
            Type::Array(elem, len) => format!("array<{}, {}>", self.msl_type(&elem), len),
            Type::Struct { .. } => self.struct_name(&ty),
            Type::Vec(elem, 0) => self.msl_type(&elem),
            other => format!("/* unsupported {:?} */ uint", other),
        }
    }

    pub(crate) fn struct_name(&self, ty: &Type) -> String {
        if let Type::Symbol { id, .. } = ty
            && let Some(name) = self.type_names.get(id)
        {
            return name.clone();
        }
        let resolved = self.resolve_type(ty);
        if let Some(name) = self.struct_names_by_layout.iter().find_map(|(candidate, name)| (candidate == &resolved).then(|| name.clone())) {
            return name;
        }
        if let Some(name) = self.concrete_structs.borrow().iter().find_map(|(candidate, name)| (candidate == &resolved).then(|| name.clone())) {
            return name;
        }
        let mut concrete_structs = self.concrete_structs.borrow_mut();
        let name = format!("ZustConcreteStruct{}", concrete_structs.len());
        concrete_structs.push((resolved, name.clone()));
        name
    }

    pub(crate) fn resolve_type(&self, ty: &Type) -> Type {
        let ty = self.compiler.sym_tab.symbols.get_type(ty).unwrap_or_else(|_| ty.clone());
        resolve_type_in_defs(&ty, &self.type_defs)
    }

    pub(crate) fn is_runtime_array(ty: &Type) -> bool {
        matches!(ty, Type::Vec(_, 0))
    }

    pub(crate) fn atomic_type(&self, ty: &Type) -> Option<String> {
        match ty {
            Type::U32 => Some("atomic_uint".to_string()),
            Type::I32 => Some("atomic_int".to_string()),
            _ => None,
        }
    }
}
