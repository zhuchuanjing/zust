struct Params {
    x: f32,
    y: f32,
    step: f32,
    max_iter: u32,
}

pub fn main(params: Params, buf: Vec<f32>) {
    let group = spirv::group_id();
    let local = spirv::local_id();
    let sample_width = 2049u32;
    let sample_height = 2049u32;
    let px = group[0] * 16u32 + local[0];
    let py = group[1] * 16u32 + local[1];

    if px >= sample_width || py >= sample_height {
        return;
    }

    let x0 = params.x + (((px as f32) * 0.5f32) - 512.5f32) * params.step;
    let y0 = params.y + (512.5f32 - ((py as f32) * 0.5f32)) * params.step;
    let zx = 0.0f32;
    let zy = 0.0f32;
    let iter = 0u32;

    while iter < params.max_iter && (zx * zx + zy * zy) <= 4.0f32 {
        let x_next = zx * zx - zy * zy + x0;
        zy = 2.0f32 * zx * zy + y0;
        zx = x_next;
        iter += 1u32;
    }

    let value = -1.0f32;
    if iter < params.max_iter {
        let radius2 = zx * zx + zy * zy;
        value = (iter as f32) + 1.0f32 - log(log(radius2) * 0.5f32) / 0.69314718056f32;
    }

    let pos = py * sample_width + px;
    buf[pos] = value;
}
