struct PathParams {
    start_x: u32,
    start_y: u32,
    end_x: u32,
    end_y: u32,
    width: u32,
    height: u32,
    iteration: u32,
}

pub fn main(params: PathParams, grid: Vec<f32>) {
    let group = spirv::group_id();
    let local = spirv::local_id();
    let px = group[0] * 32u32 + local[0];
    let py = group[1] * 32u32 + local[1];
    if px >= params.width || py >= params.height {
        return;
    }
    let w = params.width;
    let row = py * w;
    let pos = row + px;

    if params.iteration == 0u32 {
        if px == params.start_x && py == params.start_y {
            grid[pos] = 0.0f32;
        }
        return;
    }

    let cell = grid[pos];
    if cell >= 0.0f32 {
        return;
    }
    if cell == -1.0f32 {
        return;
    }

    let frontier = (params.iteration - 1u32) as f32;

    if py > 0u32 {
        let up = pos - w;
        let n = grid[up];
        if n == frontier {
            grid[pos] = params.iteration as f32;
            return;
        }
    }
    if py + 1u32 < params.height {
        let down = pos + w;
        let n = grid[down];
        if n == frontier {
            grid[pos] = params.iteration as f32;
            return;
        }
    }
    if px > 0u32 {
        let left = pos - 1u32;
        let n = grid[left];
        if n == frontier {
            grid[pos] = params.iteration as f32;
            return;
        }
    }
    if px + 1u32 < params.width {
        let right = pos + 1u32;
        let n = grid[right];
        if n == frontier {
            grid[pos] = params.iteration as f32;
            return;
        }
    }
}
