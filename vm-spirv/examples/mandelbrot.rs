use anyhow::Result;
use dynamic::Type;

const BIGFLOAT_LIMBS: usize = 2;

fn main() -> Result<()> {
    let source_path = std::env::var("MANDEL_ZS").unwrap_or_else(|_| "zusts/gpu/mandelbrot.zs".to_string());
    let module_name = std::env::var("MANDEL_MODULE").unwrap_or_else(|_| "mandelbrot".to_string());
    let spv_path = std::env::var("MANDEL_SPV").unwrap_or_else(|_| "mandel.spv".to_string());
    let asm_path = std::env::var("MANDEL_SPVASM").unwrap_or_else(|_| "mandel.spvasm".to_string());
    let local_size = std::env::var("MANDEL_LOCAL_SIZE").ok().map(|value| parse_workgroups(&value)).transpose()?.unwrap_or([16, 16, 1]);

    let kernel = vm_spirv::compile_file_with_generic_args_and_workgroup_size(&source_path, &module_name, "main", &[Type::ConstInt(BIGFLOAT_LIMBS as i64)], local_size)?;
    let bytes = kernel.spirv.words().iter().flat_map(|word| word.to_le_bytes()).collect::<Vec<_>>();
    std::fs::write(&spv_path, bytes)?;
    std::fs::write(&asm_path, kernel.spirv.disassemble())?;
    println!("compiled {source_path}");
    println!("bigfloat_limbs: {BIGFLOAT_LIMBS}");
    println!("wrote {spv_path} ({} words)", kernel.spirv.words().len());
    Ok(())
}

fn parse_workgroups(value: &str) -> Result<[u32; 3]> {
    let parts = value.split(['x', ',', ' ']).filter(|part| !part.is_empty()).map(str::parse::<u32>).collect::<Result<Vec<_>, _>>()?;
    match parts.as_slice() {
        [x, y] => Ok([*x, *y, 1]),
        [x, y, z] => Ok([*x, *y, *z]),
        _ => anyhow::bail!("MANDEL_LOCAL_SIZE must be `x,y` or `x,y,z`, got {value:?}"),
    }
}
