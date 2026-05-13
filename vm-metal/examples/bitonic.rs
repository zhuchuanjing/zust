use anyhow::Result;

#[repr(C)]
#[derive(Clone, Copy, Debug)]
struct BitonicParams {
    len: u32,
    k: u32,
    j: u32,
    ascend: u32,
}

unsafe impl bytemuck::Zeroable for BitonicParams {}
unsafe impl bytemuck::Pod for BitonicParams {}

fn main() -> Result<()> {
    let source = std::fs::read("zusts/gpu/bitonic.zs")?;
    let kernel = vm_metal::compile_source_with_workgroup_size(source, "bitonic", "main", [256, 1, 1])?;
    std::fs::write("bitonic.metal", kernel.metal.source())?;

    let mut data = vec![7u32, 3, 5, 1, 6, 2, 4, 0];
    let mut runtime = vm_metal::Runtime::new()?;
    for k in [2, 4, 8] {
        let mut j = k / 2;
        while j > 0 {
            let mut args = runtime.args();
            let _params = args.add_input(BitonicParams { len: data.len() as u32, k, j, ascend: 1 })?;
            let data_buf = args.add_vec::<u32>(data.len() as u64, |buf| buf.copy_from_slice(&data))?;
            runtime.prepare_kernel(&kernel, args)?;
            runtime.run([1, 1, 1])?;
            data = data_buf.read()?;
            j /= 2;
        }
    }

    println!("{data:?}");
    Ok(())
}
