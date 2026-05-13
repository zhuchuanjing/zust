import("bigfloat", "../bigfloat.zs");

struct Params {
    x: f32,
    y: f32,
    step: f32,
    max_iter: u32,
    group_offset_x: u32,
    group_offset_y: u32,
}

fn escape_value_bigfloat4(x: f32, y: f32, max_iter: u32) {
    let iter = 0u32;
    let zx = bigfloat::BigFloat<4>::from_f32(0.0f32);
    let zy = bigfloat::BigFloat<4>::from_f32(0.0f32);
    let x_bf = bigfloat::BigFloat<4>::from_f32(x);
    let y_bf = bigfloat::BigFloat<4>::from_f32(y);
    let two_bf = bigfloat::BigFloat<4>::from_f32(2.0f32);
    let escape_radius2 = bigfloat::BigFloat<4>::from_f32(4.0f32);

    while iter < max_iter {
        let zx2 = zx.mul(zx);
        let zy2 = zy.mul(zy);
        let radius2 = zx2.add(zy2);
        if radius2.gt(escape_radius2) {
            break;
        }

        let tmp = zx2.sub(zy2).add(x_bf);
        let next_zy = two_bf.mul(zx).mul(zy).add(y_bf);
        zy = next_zy;
        zx = tmp;
        iter += 1u32;
    }

    if iter < max_iter {
        let radius2 = zx.mul(zx).add(zy.mul(zy)).to_f32();
        (iter as f32) + 1.0f32 - log(log(radius2) * 0.5f32) / 0.69314718056f32
    } else {
        -1.0f32
    }
}

pub fn main(params: Params, buf: Vec<f32>) {
    let group = spirv::group_id();
    let local = spirv::local_id();
    let sample_width = 2049u32;
    let sample_height = 2049u32;

    let px = (params.group_offset_x + group[0]) * 16u32 + local[0];
    let py = (params.group_offset_y + group[1]) * 16u32 + local[1];

    if px < sample_width && py < sample_height {
        let x = params.x + (((px as f32) * 0.5f32) - 512.5f32) * params.step;
        let y = params.y + (512.5f32 - ((py as f32) * 0.5f32)) * params.step;
        let pos = py * sample_width + px;
        buf[pos] = escape_value_bigfloat4(x, y, params.max_iter);
    }
}
