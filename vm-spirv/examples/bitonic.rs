use anyhow::Result;

fn main() -> Result<()> {
    let source = std::fs::read("zusts/gpu/bitonic.zs")?;
    let kernel = vm_spirv::compile_source_with_workgroup_size(source, "bitonic", "main", [256, 1, 1])?;
    let bytes = kernel.spirv.words().iter().flat_map(|word| word.to_le_bytes()).collect::<Vec<_>>();
    std::fs::write("bitonic.spv", bytes)?;
    std::fs::write("bitonic.spvasm", kernel.spirv.disassemble())?;
    println!("wrote bitonic.spv ({} words)", kernel.spirv.words().len());
    Ok(())
}
