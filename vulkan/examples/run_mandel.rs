use anyhow::{Result, anyhow};
use image::{ImageBuffer, Rgba};
use std::time::Instant;
use vm_spirv::compile_file_with_workgroup_size;
use vulkan::Runtime;
use vulkano::buffer::BufferContents;

const WIDTH: usize = 1024;
const HEIGHT: usize = 1024;
const SAMPLE_WIDTH: usize = WIDTH * 2 + 1;
const SAMPLE_HEIGHT: usize = HEIGHT * 2 + 1;
const LOCAL_SIZE_X: u32 = 16;
const WORKGROUPS: [u32; 3] = [16, 16, 1];
const MAX_ITER: u32 = 1000;
const OUTPUT_PATH: &str = "mand.png";

#[derive(BufferContents, Clone, Copy)]
#[repr(C)]
struct MandelParams {
    origin_x: f32,
    origin_y: f32,
    scale: f32,
    max_iter: u32,
    group_offset_x: u32,
    group_offset_y: u32,
}

fn main() -> Result<()> {
    let total_start = Instant::now();
    let shader_path = std::env::var("MANDEL_ZS").unwrap_or_else(|_| "zusts/gpu/mandelbrot_bigfloat4.zs".to_string());
    let module_name = std::env::var("MANDEL_MODULE").unwrap_or_else(|_| "mandelbrot_bigfloat4".to_string());
    let output_path = std::env::var("MANDEL_OUTPUT").unwrap_or_else(|_| OUTPUT_PATH.to_string());
    let workgroups = std::env::var("MANDEL_WORKGROUPS").ok().map(|value| parse_workgroups(&value)).transpose()?.unwrap_or(WORKGROUPS);
    let shader_tile_size = std::env::var("MANDEL_TILE_SIZE").ok().map(|value| value.parse::<u32>()).transpose()?.unwrap_or(LOCAL_SIZE_X);
    let max_iter = std::env::var("MANDEL_MAX_ITER").ok().map(|value| value.parse::<u32>()).transpose()?.unwrap_or(MAX_ITER);
    let kernel = compile_file_with_workgroup_size(&shader_path, &module_name, "main", [LOCAL_SIZE_X, LOCAL_SIZE_X, 1])?;
    let words = kernel.spirv.words();

    let mut runtime = Runtime::new()?;
    let mut args = runtime.args();
    let params = args.add_input(MandelParams { origin_x: -0.7454, origin_y: 0.1103, scale: 0.000014, max_iter, group_offset_x: 0, group_offset_y: 0 })?;
    let output = args.add_vec::<f32>((SAMPLE_WIDTH * SAMPLE_HEIGHT) as u64, |pixels| pixels.fill(-2.0))?;

    runtime.prepare(words, args)?;
    let gpu_start = Instant::now();
    let total_workgroups = total_workgroups(shader_tile_size);
    let dispatch_count = run_tiled(&runtime, &params, workgroups, total_workgroups, max_iter)?;
    let gpu_elapsed = gpu_start.elapsed();

    let samples = output.read()?;
    let unwritten_samples = samples.iter().filter(|&&value| value == -2.0).count();
    let started_samples = samples.iter().filter(|&&value| value == -3.0).count();
    let interior_samples = samples.iter().filter(|&&value| value == -1.0).count();
    let post_start = Instant::now();
    let pixels = average_mandelbrot_samples(&samples, WIDTH as u32, HEIGHT as u32, SAMPLE_WIDTH as u32, max_iter)?;
    let post_elapsed = post_start.elapsed();
    let escaped = pixels.iter().filter(|&&value| value >= 0.0).count();
    let finite = pixels.iter().filter(|value| value.is_finite()).count();
    let max = pixels.iter().copied().fold(0.0f32, f32::max);
    let checksum = pixels.iter().fold(0.0f64, |acc, &value| acc + value as f64);
    let center = pixels[(HEIGHT / 2) * WIDTH + (WIDTH / 2)];

    println!("compiled {shader_path}::{module_name} ({} words)", words.len());
    println!("executed mandelbrot with Vulkan");
    println!("tile_workgroups: {}x{}x{}", workgroups[0], workgroups[1], workgroups[2]);
    println!("shader_tile_size: {shader_tile_size}");
    println!("total_workgroups: {}x{}x{}", total_workgroups[0], total_workgroups[1], total_workgroups[2]);
    println!("dispatch_count: {dispatch_count}");
    println!("pixels: {}x{}", WIDTH, HEIGHT);
    println!("samples: {}x{}", SAMPLE_WIDTH, SAMPLE_HEIGHT);
    println!("max_iter: {max_iter}");
    println!("unwritten_samples: {unwritten_samples}");
    println!("started_samples: {started_samples}");
    println!("interior_samples: {interior_samples}");
    println!("escaped: {escaped}");
    println!("finite: {finite}");
    println!("max_escape_seen: {max:.3}");
    println!("center: {center:.3}");
    println!("checksum: {checksum:.3}");
    println!("gpu_elapsed_ms: {:.3}", gpu_elapsed.as_secs_f64() * 1000.0);
    println!("post_elapsed_ms: {:.3}", post_elapsed.as_secs_f64() * 1000.0);
    println!("total_elapsed_ms: {:.3}", total_start.elapsed().as_secs_f64() * 1000.0);

    if unwritten_samples != 0 {
        return Err(anyhow!("mandelbrot shader left {unwritten_samples} samples unwritten"));
    }
    if finite != pixels.len() {
        return Err(anyhow!("mandelbrot output contains non-finite pixels"));
    }

    save_mandelbrot_png(&pixels, WIDTH as u32, HEIGHT as u32, max_iter, &output_path)?;
    println!("wrote {output_path}");

    Ok(())
}

fn parse_workgroups(value: &str) -> Result<[u32; 3]> {
    let parts = value.split(['x', ',', ' ']).filter(|part| !part.is_empty()).map(str::parse::<u32>).collect::<Result<Vec<_>, _>>()?;
    match parts.as_slice() {
        [x, y] => Ok([*x, *y, 1]),
        [x, y, z] => Ok([*x, *y, *z]),
        _ => Err(anyhow!("MANDEL_WORKGROUPS must be `x,y` or `x,y,z`, got {value:?}")),
    }
}

fn total_workgroups(shader_tile_size: u32) -> [u32; 3] {
    let tile = shader_tile_size.max(1) as usize;
    [SAMPLE_WIDTH.div_ceil(tile) as u32, SAMPLE_HEIGHT.div_ceil(tile) as u32, 1]
}

fn run_tiled(runtime: &Runtime, params: &vulkano::buffer::Subbuffer<MandelParams>, tile_workgroups: [u32; 3], total_workgroups: [u32; 3], max_iter: u32) -> Result<u32> {
    if tile_workgroups[0] == 0 || tile_workgroups[1] == 0 || tile_workgroups[2] == 0 {
        return Err(anyhow!("MANDEL_WORKGROUPS must be non-zero in every dimension"));
    }
    if tile_workgroups[2] != 1 || total_workgroups[2] != 1 {
        return Err(anyhow!("mandelbrot tiling currently expects z workgroups to be 1"));
    }

    let mut dispatch_count = 0u32;
    let mut y = 0u32;
    while y < total_workgroups[1] {
        let groups_y = tile_workgroups[1].min(total_workgroups[1] - y);
        let mut x = 0u32;
        while x < total_workgroups[0] {
            let groups_x = tile_workgroups[0].min(total_workgroups[0] - x);
            {
                let mut params = params.write()?;
                *params = MandelParams { origin_x: -0.7454, origin_y: 0.1103, scale: 0.000014, max_iter, group_offset_x: x, group_offset_y: y };
            }
            runtime.run([groups_x, groups_y, 1])?;
            dispatch_count += 1;
            x += groups_x;
        }
        y += groups_y;
    }
    Ok(dispatch_count)
}

fn average_mandelbrot_samples(samples: &[f32], width: u32, height: u32, sample_width: u32, max_iter: u32) -> Result<Vec<f32>> {
    let sample_height = height * 2 + 1;
    if samples.len() != (sample_width * sample_height) as usize {
        return Err(anyhow!("sample buffer length does not match supersampled dimensions"));
    }

    let mut pixels = vec![-1.0f32; (width * height) as usize];
    for y in 0..height {
        for x in 0..width {
            let mut sum = 0.0f32;
            let mut escaped = false;
            for dy in 0..3 {
                for dx in 0..3 {
                    let sample = samples[((y * 2 + dy) * sample_width + (x * 2 + dx)) as usize];
                    if sample.is_finite() && sample >= 0.0 {
                        sum += sample;
                        escaped = true;
                    } else {
                        sum += max_iter as f32;
                    }
                }
            }
            if escaped {
                pixels[(y * width + x) as usize] = sum / 9.0;
            }
        }
    }
    Ok(pixels)
}

fn save_mandelbrot_png(iter_data: &[f32], width: u32, height: u32, max_iter: u32, path: &str) -> Result<()> {
    if iter_data.len() != (width * height) as usize {
        return Err(anyhow!("pixel buffer length does not match image dimensions"));
    }
    let mut image = ImageBuffer::<Rgba<u8>, Vec<u8>>::new(width, height);
    for y in 0..height {
        for x in 0..width {
            let escape = iter_data[(y * width + x) as usize];
            image.put_pixel(x, y, Rgba(escape_to_rgba(escape, max_iter)));
        }
    }
    image.save(path)?;
    Ok(())
}

fn escape_to_rgba(escape: f32, max_iter: u32) -> [u8; 4] {
    if !escape.is_finite() || escape < 0.0 {
        return [0, 0, 0, 255];
    }
    let t = (escape / max_iter as f32).sqrt();
    let bands = (escape * 0.035).fract();
    let glow = 0.68 + 0.32 * (1.0 - (2.0 * bands - 1.0).abs());
    let r = cosine_channel(t, 0.92, 0.28) * glow;
    let g = cosine_channel(t, 0.48, 0.33) * glow;
    let b = cosine_channel(t, 0.18, 0.45) * glow;
    [to_byte(r), to_byte(g), to_byte(b), 255]
}

fn cosine_channel(t: f32, phase: f32, contrast: f32) -> f32 {
    let v = 0.5 + 0.5 * ((t + phase) * std::f32::consts::TAU).cos();
    v.powf(contrast)
}

fn to_byte(value: f32) -> u8 {
    (value.clamp(0.0, 1.0) * 255.0).round() as u8
}
