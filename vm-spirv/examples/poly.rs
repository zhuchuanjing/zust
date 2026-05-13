use anyhow::Result;

fn main() -> Result<()> {
    let source = std::fs::read("zusts/gpu/poly.zs")?;
    let kernel = vm_spirv::compile_source_with_workgroup_size(source, "poly", "main", [32, 32, 1])?;
    let bytes = kernel.spirv.words().iter().flat_map(|word| word.to_le_bytes()).collect::<Vec<_>>();
    std::fs::write("poly.spv", bytes)?;
    std::fs::write("poly.spvasm", kernel.spirv.disassemble())?;
    println!("wrote poly.spv ({} words)", kernel.spirv.words().len());
    Ok(())
}
