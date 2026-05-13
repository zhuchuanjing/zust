use anyhow::Result;
use compiler::Compiler;
use dynamic::Type;
use parser::Stmt;
use smol_str::SmolStr;
use std::{
    cell::RefCell,
    collections::{BTreeMap, BTreeSet},
    sync::Arc,
};

use crate::{
    api::{ExternalFnKind, MetalModule},
    util::{resolve_type_in_defs, sanitize_ident},
};

#[derive(Debug, Clone)]
pub(crate) struct Value {
    pub(crate) code: String,
    pub(crate) ty: Type,
}

#[derive(Debug, Clone)]
pub(crate) struct Var {
    pub(crate) name: String,
    pub(crate) ty: Type,
}

#[derive(Debug, Clone)]
pub(crate) struct UserFn {
    pub(crate) arg_names: Vec<SmolStr>,
    pub(crate) arg_tys: Vec<Type>,
    pub(crate) generic_params: Vec<Type>,
    pub(crate) body: Arc<Stmt>,
}

pub(crate) struct MetalCompiler {
    pub(crate) externs: BTreeMap<u32, ExternalFnKind>,
    pub(crate) user_fns: BTreeMap<u32, UserFn>,
    pub(crate) type_defs: BTreeMap<u32, Type>,
    pub(crate) type_names: BTreeMap<u32, String>,
    pub(crate) struct_names_by_layout: Vec<(Type, String)>,
    pub(crate) concrete_structs: RefCell<Vec<(Type, String)>>,
    pub(crate) workgroup_static_tys: BTreeMap<u32, Type>,
    pub(crate) compiler: Compiler,
    pub(crate) vars: Vec<Option<Var>>,
    pub(crate) names: Vec<Option<SmolStr>>,
    pub(crate) out: String,
    pub(crate) indent: usize,
    pub(crate) tmp: u32,
    pub(crate) inline_stack: Vec<u32>,
    pub(crate) inline_return: Option<String>,
    pub(crate) ret_buffer: Option<String>,
}

impl MetalCompiler {
    pub(crate) fn new(
        externs: BTreeMap<u32, ExternalFnKind>,
        user_fns: BTreeMap<u32, UserFn>,
        type_defs: BTreeMap<u32, Type>,
        type_names: BTreeMap<u32, String>,
        workgroup_static_tys: BTreeMap<u32, Type>,
        compiler: Compiler,
        _workgroup_size: [u32; 3],
    ) -> Self {
        let struct_names_by_layout = type_defs
            .iter()
            .filter_map(|(id, ty)| {
                let resolved = resolve_type_in_defs(ty, &type_defs);
                type_names.get(id).cloned().map(|name| (resolved, name))
            })
            .collect();
        Self {
            externs,
            user_fns,
            type_defs,
            type_names,
            struct_names_by_layout,
            concrete_structs: RefCell::new(Vec::new()),
            workgroup_static_tys,
            compiler,
            vars: Vec::new(),
            names: Vec::new(),
            out: String::new(),
            indent: 0,
            tmp: 0,
            inline_stack: Vec::new(),
            inline_return: None,
            ret_buffer: None,
        }
    }

    pub(crate) fn compile_kernel(mut self, arg_tys: &[Type], ret_ty: Type, body: &Stmt) -> Result<MetalModule> {
        let args = self.kernel_args(arg_tys, &ret_ty)?;
        self.out.clear();
        self.indent = 1;
        self.vars.clear();
        self.names.clear();
        self.ret_buffer = (!ret_ty.is_void()).then_some("zust_ret".to_string());

        for (id, ty) in self.workgroup_static_tys.clone() {
            let ty = self.resolve_type(&ty);
            let decl_ty = self.atomic_type(&ty).unwrap_or_else(|| self.msl_type(&ty));
            self.line(format!("threadgroup {decl_ty} zust_static_{id};"));
        }

        for (idx, ty) in arg_tys.iter().enumerate() {
            let ty = self.resolve_type(ty);
            if Self::is_runtime_array(&ty) {
                self.set_var(idx, format!("zust_arg{idx}"), ty);
            } else {
                let local = format!("zust_arg{idx}_value");
                self.line(format!("{} {local} = zust_arg{idx}[0];", self.msl_type(&ty)));
                self.set_var(idx, local, ty);
            }
        }

        self.gen_stmt(body)?;
        let mut source = String::new();
        source.push_str("#include <metal_stdlib>\nusing namespace metal;\n\n");
        source.push_str(&self.emit_struct_defs()?);
        source.push('\n');
        source.push_str(&format!("kernel void zust_main({}) {{\n", args.join(", ")));
        source.push_str(&self.out);
        source.push_str("}\n");
        Ok(MetalModule { source })
    }

    pub(crate) fn emit_struct_defs(&self) -> Result<String> {
        let mut out = String::new();
        let mut named_structs = Vec::new();
        for (id, ty) in &self.type_defs {
            if matches!(ty, Type::Struct { params, .. } if !params.is_empty()) {
                continue;
            }
            let name = self.type_names.get(id).cloned().unwrap_or_else(|| format!("ZustStruct{id}"));
            named_structs.push((self.resolve_type(ty), name));
        }
        for (ty, name) in self.concrete_structs.borrow().iter() {
            named_structs.push((self.resolve_type(ty), name.clone()));
        }

        let mut emitted = BTreeSet::new();
        let mut visiting = BTreeSet::new();
        for (ty, name) in &named_structs {
            self.emit_struct_def(ty, name, &named_structs, &mut emitted, &mut visiting, &mut out)?;
        }
        Ok(out)
    }

    fn emit_struct_def(&self, ty: &Type, name: &str, named_structs: &[(Type, String)], emitted: &mut BTreeSet<String>, visiting: &mut BTreeSet<String>, out: &mut String) -> Result<()> {
        if emitted.contains(name) || visiting.contains(name) {
            return Ok(());
        }
        let Type::Struct { fields, .. } = self.resolve_type(ty) else {
            return Ok(());
        };
        if fields.is_empty() {
            return Ok(());
        }

        visiting.insert(name.to_string());
        for (_, field_ty) in &fields {
            let field_ty = self.resolve_type(field_ty);
            if !matches!(field_ty, Type::Struct { .. }) {
                continue;
            }
            if let Some((dep_ty, dep_name)) = named_structs.iter().find(|(candidate, _)| self.resolve_type(candidate) == field_ty) {
                self.emit_struct_def(dep_ty, dep_name, named_structs, emitted, visiting, out)?;
            }
        }
        visiting.remove(name);

        if emitted.insert(name.to_string()) {
            out.push_str(&format!("struct {name} {{\n"));
            for (field, ty) in fields {
                out.push_str(&format!("    {} {};\n", self.msl_type(&ty), sanitize_ident(&field)));
            }
            out.push_str("};\n\n");
        }
        Ok(())
    }

    pub(crate) fn kernel_args(&self, arg_tys: &[Type], ret_ty: &Type) -> Result<Vec<String>> {
        let mut args = Vec::new();
        for (idx, ty) in arg_tys.iter().enumerate() {
            let ty = self.resolve_type(ty);
            if let Type::Vec(elem, 0) = ty {
                args.push(format!("device {}* zust_arg{idx} [[buffer({idx})]]", self.msl_type(&elem)));
            } else {
                args.push(format!("device {}* zust_arg{idx} [[buffer({idx})]]", self.msl_type(&ty)));
            }
        }
        if !ret_ty.is_void() {
            args.push(format!("device {}* zust_ret [[buffer({})]]", self.msl_type(&self.resolve_type(ret_ty)), arg_tys.len()));
        }
        args.push("uint3 zust_group_id [[threadgroup_position_in_grid]]".to_string());
        args.push("uint3 zust_local_id [[thread_position_in_threadgroup]]".to_string());
        Ok(args)
    }
}
