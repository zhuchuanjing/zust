use anyhow::{Result, anyhow};
use std::time::Instant;
use vm_spirv::compile_file_with_workgroup_size;
use vulkan::Runtime;
use vulkano::buffer::BufferContents;

const DEFAULT_LEN: usize = 4096;
const LOCAL_SIZE: u32 = 256;

#[derive(BufferContents, Clone, Copy, Debug)]
#[repr(C)]
struct BitonicParams {
    len: u32,
    k: u32,
    j: u32,
    ascend: u32,
}

fn main() -> Result<()> {
    let len = std::env::var("BITONIC_LEN").ok().map(|value| value.parse::<usize>()).transpose()?.unwrap_or(DEFAULT_LEN);
    if !len.is_power_of_two() {
        return Err(anyhow!("BITONIC_LEN must be a power of two"));
    }
    if len > u32::MAX as usize {
        return Err(anyhow!("BITONIC_LEN must fit in u32"));
    }

    let shader_path = std::env::var("BITONIC_ZS").unwrap_or_else(|_| "zusts/gpu/bitonic.zs".to_string());
    let kernel = compile_file_with_workgroup_size(&shader_path, "bitonic", "main", [LOCAL_SIZE, 1, 1])?;
    let words = kernel.spirv.words();

    let input = shuffled_input(len);
    let expected_checksum = checksum(&input);

    let mut runtime = Runtime::new()?;
    let mut args = runtime.args();
    let params = args.add_input(BitonicParams { len: len as u32, k: 2, j: 1, ascend: 1 })?;
    let data = args.add_vec::<u32>(len as u64, |buf| buf.copy_from_slice(&input))?;

    runtime.prepare(words, args)?;
    let groups = [ceil_div(len as u32, LOCAL_SIZE), 1, 1];

    let sort_start = Instant::now();
    let mut dispatches = 0u32;
    let mut k = 2u32;
    while k <= len as u32 {
        let mut j = k >> 1;
        while j > 0 {
            {
                let mut p = params.write()?;
                p.k = k;
                p.j = j;
                p.ascend = 1;
            }
            runtime.run(groups)?;
            dispatches += 1;
            j >>= 1;
        }
        k <<= 1;
    }
    let sort_elapsed = sort_start.elapsed();

    let sorted = data.read()?;
    let actual_checksum = checksum(&sorted);
    let inversions = sorted.windows(2).filter(|pair| pair[0] > pair[1]).count();

    println!("compiled {shader_path} ({} words)", words.len());
    println!("executed bitonic with Vulkan");
    println!("items: {len}");
    println!("dispatches: {dispatches}");
    println!("sort_time_ms: {:.3}", sort_elapsed.as_secs_f64() * 1000.0);
    println!("first: {}", sorted[0]);
    println!("last: {}", sorted[len - 1]);
    println!("checksum: {actual_checksum}");

    if expected_checksum != actual_checksum {
        return Err(anyhow!("bitonic output checksum changed: expected {expected_checksum}, got {actual_checksum}"));
    }
    if inversions != 0 {
        return Err(anyhow!("bitonic output is not sorted; found {inversions} adjacent inversions"));
    }

    println!("sorted: ok");
    Ok(())
}

fn shuffled_input(len: usize) -> Vec<u32> {
    let mut values = (0..len as u32).collect::<Vec<_>>();
    let mut seed = 0x1234_5678u32;
    for i in (1..values.len()).rev() {
        seed = seed.wrapping_mul(1664525).wrapping_add(1013904223);
        let j = (seed as usize) % (i + 1);
        values.swap(i, j);
    }
    values
}

fn checksum(values: &[u32]) -> u64 {
    values.iter().map(|&value| value as u64).sum()
}

fn ceil_div(value: u32, divisor: u32) -> u32 {
    (value + divisor - 1) / divisor
}
