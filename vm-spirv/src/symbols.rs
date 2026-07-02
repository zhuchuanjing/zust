use anyhow::{Result, bail};
use compiler::{Compiler, Symbol};
use dynamic::Type;
use std::collections::BTreeMap;

use crate::context::UserFn;

pub(crate) fn collect_user_fns(compiler: &Compiler) -> Result<BTreeMap<u32, UserFn>> {
    let mut fns = BTreeMap::new();
    for (idx, (_, symbol)) in compiler.sym_tab.symbols.symbols.iter().enumerate() {
        if let Symbol::Fn { ty: Type::Fn { tys, .. }, args, generic_params, body, .. } = symbol {
            let arg_tys = if generic_params.is_empty() {
                let Ok(arg_tys) = tys.iter().map(|ty| compiler.sym_tab.symbols.get_type(ty)).collect::<Result<Vec<_>>>() else {
                    continue;
                };
                arg_tys
            } else {
                tys.clone()
            };
            fns.insert(idx as u32, UserFn { arg_names: args.clone(), arg_tys, generic_params: generic_params.clone(), body: body.clone() });
        }
    }
    Ok(fns)
}

pub(crate) fn collect_type_defs(compiler: &Compiler) -> BTreeMap<u32, Type> {
    compiler.sym_tab.symbols.symbols.iter().enumerate().filter_map(|(idx, (_, symbol))| if let Symbol::Struct(ty, _) = symbol { Some((idx as u32, ty.clone())) } else { None }).collect()
}

pub(crate) fn collect_workgroup_statics(compiler: &Compiler) -> Result<BTreeMap<u32, Type>> {
    let mut statics = BTreeMap::new();
    for (idx, (_, symbol)) in compiler.sym_tab.symbols.symbols.iter().enumerate() {
        if let Symbol::Static { ty, value, .. } = symbol {
            if value.is_some() {
                bail!("SPIR-V workgroup static initializers are not supported yet; initialize them in the kernel before spirv::barrier()");
            }
            statics.insert(idx as u32, compiler.sym_tab.symbols.get_type(ty)?);
        }
    }
    Ok(statics)
}
