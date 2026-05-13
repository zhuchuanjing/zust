mod api;
mod constants;
mod context;
mod expr;
mod externs;
mod memory;
mod ops;
mod stmt;
mod symbols;
mod types;

pub use api::{BuiltinFn, ExternalFn, ExternalFnKind, Kernel, SpirvModule, spirv_builtins};

use anyhow::{Context, Result, bail};
use compiler::{Compiler, Symbol, substitute_stmt, substitute_type};
use dynamic::Type;
use parser::{Span, Stmt, StmtKind};
use std::{collections::BTreeMap, path::Path};

use crate::{
    context::SpirvCompiler,
    externs::register_externs,
    symbols::{collect_type_defs, collect_user_fns, collect_workgroup_statics},
};

pub fn compile_source(source: impl AsRef<[u8]>, module_name: &str, fn_name: &str) -> Result<Kernel> {
    compile_source_with_externs(source, module_name, fn_name, spirv_builtins())
}

pub fn compile_source_with_workgroup_size(source: impl AsRef<[u8]>, module_name: &str, fn_name: &str, workgroup_size: [u32; 3]) -> Result<Kernel> {
    compile_source_with_externs_and_workgroup_size(source, module_name, fn_name, spirv_builtins(), workgroup_size)
}

pub fn compile_file_with_workgroup_size(path: impl AsRef<Path>, module_name: &str, fn_name: &str, workgroup_size: [u32; 3]) -> Result<Kernel> {
    compile_file_with_generic_args_and_workgroup_size(path, module_name, fn_name, &[], workgroup_size)
}

pub fn compile_file_with_generic_args_and_workgroup_size(path: impl AsRef<Path>, module_name: &str, fn_name: &str, generic_args: &[Type], workgroup_size: [u32; 3]) -> Result<Kernel> {
    let mut compiler = Compiler::new();
    let externs = register_externs(&mut compiler, spirv_builtins())?;
    compiler.import_file(module_name, path)?;
    compile_function_with_externs_generic_args_and_workgroup_size(&mut compiler, module_name, fn_name, externs, generic_args, workgroup_size)
}

pub fn compile_source_with_externs(source: impl AsRef<[u8]>, module_name: &str, fn_name: &str, externs: impl IntoIterator<Item = ExternalFn>) -> Result<Kernel> {
    compile_source_with_externs_and_workgroup_size(source, module_name, fn_name, externs, [1, 1, 1])
}

pub fn compile_source_with_externs_and_workgroup_size(source: impl AsRef<[u8]>, module_name: &str, fn_name: &str, externs: impl IntoIterator<Item = ExternalFn>, workgroup_size: [u32; 3]) -> Result<Kernel> {
    compile_source_with_externs_generic_args_and_workgroup_size(source, module_name, fn_name, externs, &[], workgroup_size)
}

pub fn compile_source_with_externs_generic_args_and_workgroup_size(
    source: impl AsRef<[u8]>,
    module_name: &str,
    fn_name: &str,
    externs: impl IntoIterator<Item = ExternalFn>,
    generic_args: &[Type],
    workgroup_size: [u32; 3],
) -> Result<Kernel> {
    let mut compiler = Compiler::new();
    let externs = register_externs(&mut compiler, externs)?;
    compiler.import_code(module_name, source.as_ref().to_vec())?;
    compile_function_with_externs_generic_args_and_workgroup_size(&mut compiler, module_name, fn_name, externs, generic_args, workgroup_size)
}

pub fn compile_function(compiler: &mut Compiler, module_name: &str, fn_name: &str) -> Result<Kernel> {
    let externs = register_externs(compiler, spirv_builtins())?;
    compile_function_with_externs(compiler, module_name, fn_name, externs)
}

pub fn compile_function_with_externs(compiler: &mut Compiler, module_name: &str, fn_name: &str, externs: BTreeMap<u32, ExternalFnKind>) -> Result<Kernel> {
    compile_function_with_externs_and_workgroup_size(compiler, module_name, fn_name, externs, [1, 1, 1])
}

pub fn compile_function_with_externs_and_workgroup_size(compiler: &mut Compiler, module_name: &str, fn_name: &str, externs: BTreeMap<u32, ExternalFnKind>, workgroup_size: [u32; 3]) -> Result<Kernel> {
    compile_function_with_externs_generic_args_and_workgroup_size(compiler, module_name, fn_name, externs, &[], workgroup_size)
}

pub fn compile_function_with_externs_generic_args_and_workgroup_size(
    compiler: &mut Compiler,
    module_name: &str,
    fn_name: &str,
    externs: BTreeMap<u32, ExternalFnKind>,
    generic_args: &[Type],
    workgroup_size: [u32; 3],
) -> Result<Kernel> {
    let full_name = format!("{module_name}::{fn_name}");
    let id = compiler.symbols.get_id(&full_name).or_else(|_| compiler.symbols.get_id(fn_name)).with_context(|| format!("function {full_name} not found"))?;
    let symbol = compiler.symbols.get_symbol(id)?.1.clone();
    let Symbol::Fn { ty, args, generic_params, cap, body, is_pub: _ } = symbol else {
        bail!("{full_name} is not a zust function");
    };
    let Type::Fn { tys: decl_arg_tys, ret: _ } = ty else {
        bail!("{full_name} has non-function type {ty:?}");
    };
    let (arg_tys, body) = specialize_entry_function(compiler, module_name, &args, &generic_params, generic_args, &decl_arg_tys, body.as_ref(), cap)?;
    let ret_ty = compiler.infer_fn_with_params(id, &arg_tys, generic_args)?;
    let ret_ty = compiler.symbols.get_type(&ret_ty)?;
    let type_defs = collect_type_defs(compiler);
    let user_fns = collect_user_fns(compiler)?;
    let workgroup_static_tys = collect_workgroup_statics(compiler)?;
    let builder = SpirvCompiler::new(externs, user_fns, type_defs, workgroup_static_tys, compiler.clone(), workgroup_size);
    let spirv = builder.compile_kernel(&arg_tys, ret_ty.clone(), &body)?;
    Ok(Kernel { spirv, entry: "main".into(), arg_tys: arg_tys.clone(), ret_ty })
}

fn specialize_entry_function(
    compiler: &mut Compiler,
    module_name: &str,
    args: &[smol_str::SmolStr],
    generic_params: &[Type],
    generic_args: &[Type],
    decl_arg_tys: &[Type],
    body: &Stmt,
    cap: compiler::Capture,
) -> Result<(Vec<Type>, Stmt)> {
    if generic_params.is_empty() {
        let arg_tys = decl_arg_tys.iter().map(|ty| resolve_entry_type(compiler, module_name, ty)).collect::<Result<Vec<_>>>()?;
        return Ok((arg_tys, body.clone()));
    }
    if generic_params.len() != generic_args.len() {
        bail!("entry function expects {} generic args, got {}", generic_params.len(), generic_args.len());
    }
    let body = substitute_stmt(body, generic_params, generic_args);
    let mut arg_tys = decl_arg_tys.iter().map(|ty| resolve_entry_type(compiler, module_name, &substitute_type(ty, generic_params, generic_args))).collect::<Result<Vec<_>>>()?;
    let mut cap = cap;
    let compiled = compiler.compile_fn(args, &mut arg_tys, body, &mut cap)?;
    Ok((arg_tys, Stmt::new(StmtKind::Block(compiled), Span::default())))
}

fn resolve_entry_type(compiler: &Compiler, module_name: &str, ty: &Type) -> Result<Type> {
    compiler.symbols.get_type(ty).or_else(|_| compiler.symbols.get_type(&qualify_entry_type(module_name, ty)))
}

fn qualify_entry_type(module_name: &str, ty: &Type) -> Type {
    match ty {
        Type::Ident { name, params } if !name.contains("::") && name.as_str() != "Vec" => {
            Type::Ident { name: format!("{module_name}::{name}").into(), params: params.iter().map(|param| qualify_entry_type(module_name, param)).collect() }
        }
        Type::Ident { name, params } => Type::Ident { name: name.clone(), params: params.iter().map(|param| qualify_entry_type(module_name, param)).collect() },
        Type::Array(elem, len) => Type::Array(std::rc::Rc::new(qualify_entry_type(module_name, elem)), *len),
        Type::ArrayParam(elem, len) => Type::ArrayParam(std::rc::Rc::new(qualify_entry_type(module_name, elem)), std::rc::Rc::new(qualify_entry_type(module_name, len))),
        Type::Vec(elem, len) => Type::Vec(std::rc::Rc::new(qualify_entry_type(module_name, elem)), *len),
        Type::Tuple(items) => Type::Tuple(items.iter().map(|item| qualify_entry_type(module_name, item)).collect()),
        Type::Struct { params, fields } => {
            Type::Struct { params: params.iter().map(|param| qualify_entry_type(module_name, param)).collect(), fields: fields.iter().map(|(name, ty)| (name.clone(), qualify_entry_type(module_name, ty))).collect() }
        }
        Type::Fn { tys, ret } => Type::Fn { tys: tys.iter().map(|ty| qualify_entry_type(module_name, ty)).collect(), ret: std::rc::Rc::new(qualify_entry_type(module_name, ret)) },
        Type::Symbol { id, params } => Type::Symbol { id: *id, params: params.iter().map(|param| qualify_entry_type(module_name, param)).collect() },
        other => other.clone(),
    }
}

#[cfg(test)]
mod tests;
