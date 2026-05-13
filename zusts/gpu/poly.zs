struct Point {
    x: u32,
    y: u32,
    init: u32,
    max: u32,
}

pub fn main(circles: Vec<[i32; 4]>, p: Point, image: Vec<f32>, image1: Vec<f32>) {
    let group = spirv::group_id();
    let local = spirv::local_id();
    let ux = group[0] * 32u32 + local[0];
    let uy = group[1] * 32u32 + local[1];
    let x = ux as i32;
    let y = uy as i32;
    let pos = uy * 1024u32 + ux;

    if p.init == 0u32 {
        image[pos] = 1048576.0f32;
        image1[pos] = 1048576.0f32;

        let cx = x - 512;
        let cy = 512 - y;
        let blocked = false;

        let circle_idx = 0u32;
        let circle = circles[circle_idx];
        while circle[2] > 0 {
            let dx = cx - circle[0];
            let dy = cy - circle[1];
            let dist = dx * dx + dy * dy;
            if dist < circle[2] * circle[2] {
                blocked = true;
            }
            circle_idx += 1u32;
            circle = circles[circle_idx];
        }

        if blocked {
            image[pos] = -1.0f32;
            image1[pos] = -1.0f32;
        }
        if ux == p.x && uy == p.y {
            image[pos] = 0.0f32;
            image1[pos] = 0.0f32;
        }
    } else {
        let dis = if p.init % 2u32 == 0u32 { image[pos] } else { image1[pos] };
        if dis >= 0.0f32 {
            let best = dis;
            let ny = -1;
            while ny <= 1 {
                let nx = -1;
                while nx <= 1 {
                    if nx != 0 || ny != 0 {
                        let x1 = x + nx;
                        let y1 = y + ny;
                        if x1 >= 0 && x1 < 1024 && y1 >= 0 && y1 < 1024 {
                            let npos = (y1 as u32) * 1024u32 + (x1 as u32);
                            let src = if p.init % 2u32 == 0u32 { image1[npos] } else { image[npos] };
                            if src >= 0.0f32 {
                                let delta = if nx == 0 || ny == 0 { 1.0f32 } else { 1.4142135f32 };
                                let new_val = src + delta;
                                if new_val < best {
                                    best = new_val;
                                }
                            }
                        }
                    }
                    nx += 1;
                }
                ny += 1;
            }
            if best < dis {
                if p.init % 2u32 == 0u32 {
                    image[pos] = best;
                } else {
                    image1[pos] = best;
                }
            }
        }
    }
}
