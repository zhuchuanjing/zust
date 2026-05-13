use anyhow::Result;
use std::fs;
use vm_spirv::compile_source_with_workgroup_size;

fn main() -> Result<()> {
    let source = fs::read_to_string("zusts/gpu/pathfind.zs")?;
    let kernel = compile_source_with_workgroup_size(&source, "pathfind", "main", [32, 32, 1])?;
    let words = kernel.spirv.words();
    let bytes: Vec<u8> = words.iter().flat_map(|w| w.to_le_bytes()).collect();
    fs::write("pathfind.spv", &bytes)?;
    println!("compiled pathfind.zs -> pathfind.spv ({} words, {} bytes)", words.len(), bytes.len());
    Ok(())
}
