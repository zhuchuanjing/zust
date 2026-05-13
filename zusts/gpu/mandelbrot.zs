import("bigfloat", "../bigfloat.zs");

pub struct Params<N> {
    x: bigfloat::BigFloat<N>,
    y: bigfloat::BigFloat<N>,
    step: bigfloat::BigFloat<N>,
    max_iter: u32,
}

pub fn main<N>(params: Params<N>, buf: Vec<f32>) {
    let group = spirv::group_id();
    let local = spirv::local_id();
    let sample_width = 2049u32;
    let sample_height = 2049u32;

    let px = group[0] * 16u32 + local[0];
    let py = group[1] * 16u32 + local[1];

    if px < sample_width && py < sample_height {
        let x_offset = bigfloat::BigFloat<N>::from_f32(((px as f32) * 0.5f32) - 512.5f32);
        let y_offset = bigfloat::BigFloat<N>::from_f32(512.5f32 - ((py as f32) * 0.5f32));
        let x_bf = params.x.add(x_offset.mul(params.step));
        let y_bf = params.y.add(y_offset.mul(params.step));
        let pos = py * sample_width + px;
        let iter = 0u32;
        let zx = bigfloat::BigFloat<N>::zero();
        let zy = bigfloat::BigFloat<N>::zero();
        let two_bf = bigfloat::BigFloat<N>::from_u32(2u32);
        let escape_radius2 = bigfloat::BigFloat<N>::from_u32(4u32);

        while iter < params.max_iter {
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

        let value = -1.0f32;
        if iter < params.max_iter {
            let radius2 = zx.mul(zx).add(zy.mul(zy)).to_f32();
            value = (iter as f32) + 1.0f32 - log(log(radius2) * 0.5f32) / 0.69314718056f32;
        }
        buf[pos] = value;
    }
}
