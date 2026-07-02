use anyhow::{Context, Result};
use compiler::{Compiler, Symbol};
use vm_spirv::spirv_builtins;

fn main() -> Result<()> {
    let source_path = std::env::var("MANDEL_ZS").unwrap_or_else(|_| "zusts/gpu/mandelbrot_bigfloat2.zs".to_string());
    let module_name = std::env::var("MANDEL_MODULE").unwrap_or_else(|_| "mandelbrot_bigfloat2".to_string());
    let fn_name = std::env::var("MANDEL_FN").unwrap_or_else(|_| "main".to_string());

    let mut compiler = Compiler::new();
    for ext in spirv_builtins() {
        let native = Symbol::native(ext.arg_tys, ext.ret_ty);
        if let Some((module, name)) = ext.full_name.split_once("::") {
            compiler.sym_tab.symbols.add_module(module.into());
            let _ = compiler.sym_tab.symbols.add_to_module(module, name.into(), native);
        } else {
            compiler.add_symbol(&ext.full_name, native);
        }
    }
    compiler.import_file(&module_name, &source_path)?;

    let full_name = format!("{module_name}::{fn_name}");
    let id = if let Ok(id) = std::env::var("MANDEL_ID") {
        id.parse().with_context(|| format!("parse MANDEL_ID={id:?}"))?
    } else {
        compiler.sym_tab.symbols.get_id(&full_name).or_else(|_| compiler.sym_tab.symbols.get_id(&fn_name)).with_context(|| format!("function {full_name} not found"))?
    };
    let symbol = compiler.sym_tab.symbols.get_symbol(id)?.1.clone();
    let Symbol::Fn { ty, args, generic_params, cap, body, is_pub } = symbol else {
        anyhow::bail!("{full_name} is not a function");
    };

    println!("function: {full_name}");
    println!("id: {id}");
    println!("is_pub: {is_pub}");
    println!("type: {ty:#?}");
    println!("args: {args:#?}");
    println!("generic_params: {generic_params:#?}");
    println!("capture: {cap:#?}");
    println!("body_display:\n{body}");
    println!("body_debug:\n{body:#?}");
    Ok(())
}
