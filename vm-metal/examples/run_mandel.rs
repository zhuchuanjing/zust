use anyhow::{Result, anyhow};
use dynamic::{Dynamic, Type};
use image::{ImageBuffer, Rgba};
use std::time::Instant;

const WIDTH: usize = 1024;
const HEIGHT: usize = 1024;
const SAMPLE_WIDTH: usize = WIDTH * 2 + 1;
const SAMPLE_HEIGHT: usize = HEIGHT * 2 + 1;
const WORKGROUPS: [u32; 3] = [129, 129, 1];
const WORKGROUP_SIZE: [u32; 3] = [16, 16, 1];
const BIGFLOAT_LIMBS: usize = 2;
const MAX_ITER: u32 = 1000;
const OUTPUT_PATH: &str = "mand-metal.png";

fn bigfloat_from_f32<const N: usize>(value: f32) -> Dynamic {
    fn zero<const N: usize>() -> Dynamic {
        dynamic::map!("sign"=> false, "exp"=> 0i32, "data"=> Dynamic::from(&[0u32; N][..]))
    }

    let mut out = [0u32; N];
    let sign = value < 0.0;
    let mut mag = if sign { -value } else { value };
    if mag == 0.0 || mag.is_nan() {
        return zero::<N>();
    }

    let base = 4_294_967_296.0f32;
    let mut exp = 0i32;
    let mut norm_steps = 0;
    while mag >= base && norm_steps < 16 {
        mag /= base;
        exp += 1;
        norm_steps += 1;
    }

    let mut tiny_steps = 0;
    while mag < 1.0 && tiny_steps < 16 {
        mag *= base;
        exp -= 1;
        tiny_steps += 1;
    }

    if mag >= base {
        out.fill(u32::MAX);
        return dynamic::map!("sign"=> sign, "exp"=> exp, "data"=> Dynamic::from(&out[..]));
    }
    if mag < 1.0 {
        return zero::<N>();
    }

    let mut idx = N;
    while idx > 0 {
        idx -= 1;
        let limb = mag as u32;
        out[idx] = limb;
        mag = (mag - limb as f32) * base;
        if idx > 0 {
            exp -= 1;
        }
    }

    dynamic::map!("sign"=> sign, "exp"=> exp, "data"=> Dynamic::from(&out[..]))
}

fn main() -> Result<()> {
    let total_start = Instant::now();
    let source_path = std::env::var("MANDEL_ZS").unwrap_or_else(|_| "zusts/gpu/mandelbrot.zs".to_string());
    let module_name = std::env::var("MANDEL_MODULE").unwrap_or_else(|_| "mandelbrot".to_string());
    let output_path = std::env::var("MANDEL_OUTPUT").unwrap_or_else(|_| OUTPUT_PATH.to_string());
    let kernel = vm_metal::compile_file_with_generic_args_and_workgroup_size(&source_path, &module_name, "main", &[Type::ConstInt(BIGFLOAT_LIMBS as i64)], WORKGROUP_SIZE)?;
    let vm = vm::Vm::new();
    vm.import_file(&module_name, &source_path)?;
    let params_layout = vm.gpu_struct_layout(&format!("{module_name}::Params"), &[Type::ConstInt(BIGFLOAT_LIMBS as i64)])?;
    let params_bytes = params_layout.pack_map(&dynamic::map!(
        "x"=> bigfloat_from_f32::<BIGFLOAT_LIMBS>(-0.7454),
        "y"=> bigfloat_from_f32::<BIGFLOAT_LIMBS>(0.1103),
        "step"=> bigfloat_from_f32::<BIGFLOAT_LIMBS>(0.000014),
        "max_iter"=> MAX_ITER
    ))?;

    let mut runtime = vm_metal::Runtime::new()?;
    let mut args = runtime.args();
    let _params = args.add_bytes(params_bytes)?;
    let output = args.add_vec::<f32>((SAMPLE_WIDTH * SAMPLE_HEIGHT) as u64, |pixels| pixels.fill(0.0))?;

    runtime.prepare_kernel(&kernel, args)?;
    let gpu_start = Instant::now();
    runtime.run(WORKGROUPS)?;
    let gpu_elapsed = gpu_start.elapsed();

    let samples = output.read()?;
    let post_start = Instant::now();
    let pixels = average_mandelbrot_samples(&samples, WIDTH as u32, HEIGHT as u32, SAMPLE_WIDTH as u32, MAX_ITER)?;
    let post_elapsed = post_start.elapsed();
    let escaped = pixels.iter().filter(|&&value| value >= 0.0).count();
    let finite = pixels.iter().filter(|value| value.is_finite()).count();
    let max = pixels.iter().copied().fold(0.0f32, f32::max);
    let checksum = pixels.iter().fold(0.0f64, |acc, &value| acc + value as f64);
    let center = pixels[(HEIGHT / 2) * WIDTH + (WIDTH / 2)];

    println!("executed {source_path} with Metal");
    println!("bigfloat_limbs: {BIGFLOAT_LIMBS}");
    println!("pixels: {}x{}", WIDTH, HEIGHT);
    println!("samples: {}x{}", SAMPLE_WIDTH, SAMPLE_HEIGHT);
    println!("escaped: {escaped}");
    println!("finite: {finite}");
    println!("max_escape_seen: {max:.3}");
    println!("center: {center:.3}");
    println!("checksum: {checksum:.3}");
    println!("gpu_elapsed_ms: {:.3}", gpu_elapsed.as_secs_f64() * 1000.0);
    println!("post_elapsed_ms: {:.3}", post_elapsed.as_secs_f64() * 1000.0);
    println!("total_elapsed_ms: {:.3}", total_start.elapsed().as_secs_f64() * 1000.0);

    if escaped == 0 {
        return Err(anyhow!("Metal Mandelbrot kernel ran but produced an all-zero output buffer"));
    }

    save_mandelbrot_png(&pixels, WIDTH as u32, HEIGHT as u32, MAX_ITER, &output_path)?;
    println!("wrote {output_path}");

    Ok(())
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
