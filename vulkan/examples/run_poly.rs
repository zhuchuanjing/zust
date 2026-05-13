use anyhow::{Result, anyhow};
use image::{ImageBuffer, Rgb};
use vm_spirv::compile_file_with_workgroup_size;
use vulkan::Runtime;
use vulkano::buffer::BufferContents;

const WIDTH: usize = 1024;
const HEIGHT: usize = 1024;
const WORKGROUPS: [u32; 3] = [32, 32, 1];
const ITERATIONS: u32 = 1000;
const OUTPUT_PATH: &str = "output.png";

#[derive(BufferContents, Clone, Copy, Debug)]
#[repr(C)]
struct Point {
    x: u32,
    y: u32,
    init: u32,
    max: u32,
}

fn main() -> Result<()> {
    let shader_path = std::env::var("POLY_ZS").unwrap_or_else(|_| "zusts/gpu/poly.zs".to_string());
    let kernel = compile_file_with_workgroup_size(&shader_path, "poly", "main", [32, 32, 1])?;
    let words = kernel.spirv.words();

    let mut runtime = Runtime::new()?;
    let mut args = runtime.args();
    let _circles = args.add_vec::<[i32; 4]>(8, |buf| {
        buf[0] = [512, 400, 80, 0]; // center circle
        buf[1] = [300, 500, 60, 0]; // left circle
        buf[2] = [700, 300, 100, 0]; // right circle
        buf[3] = [400, 200, 50, 0]; // top-left circle
        buf[4] = [600, 700, 70, 0]; // bottom-right circle
        buf[5] = [200, 200, 40, 0]; // small top-left
        buf[6] = [800, 600, 90, 0]; // large right
        buf[7] = [0, 0, 0, 0]; // sentinel
    })?;
    let point = args.add_input(Point { x: 300, y: 300, init: 0, max: ITERATIONS })?;
    let image = args.add_vec::<f32>((WIDTH * HEIGHT) as u64, |buf| buf.fill(0.0))?;
    let image1 = args.add_vec::<f32>((WIDTH * HEIGHT) as u64, |buf| buf.fill(0.0))?;

    runtime.prepare(words, args)?;
    for init in 0..ITERATIONS {
        point.write()?.init = init;
        runtime.run(WORKGROUPS)?;
    }

    point.write()?.init = ITERATIONS;
    runtime.run(WORKGROUPS)?;

    let final_image = if ITERATIONS % 2 == 0 { image.read()? } else { image1.read()? };
    save_path_field_png(&final_image, OUTPUT_PATH)?;

    let reached = final_image.iter().filter(|&&value| value >= 0.0 && value < 1048576.0).count();
    let blocked = final_image.iter().filter(|&&value| value < 0.0).count();
    println!("compiled {shader_path} ({} words)", words.len());
    println!("executed poly with Vulkan");
    println!("reached: {reached}");
    println!("blocked: {blocked}");
    println!("wrote {OUTPUT_PATH}");
    Ok(())
}

fn save_path_field_png(field: &[f32], path: &str) -> Result<()> {
    if field.len() != WIDTH * HEIGHT {
        return Err(anyhow!("path field length does not match image dimensions"));
    }
    let mut image = ImageBuffer::<Rgb<u8>, Vec<u8>>::new(WIDTH as u32, HEIGHT as u32);
    for y in 0..HEIGHT {
        for x in 0..WIDTH {
            let value = field[y * WIDTH + x];
            let pixel = if value < 0.0 {
                Rgb([255, 255, 255])
            } else if value >= 1048576.0 {
                Rgb([0, 0, 0])
            } else {
                let t = (value / (WIDTH as f32 * 1.414)).clamp(0.0, 1.0);
                let shade = (255.0 * (1.0 - t)).round() as u8;
                Rgb([shade / 3, shade, 255u8.saturating_sub(shade / 2)])
            };
            image.put_pixel(x as u32, y as u32, pixel);
        }
    }
    image.save(path)?;
    Ok(())
}
