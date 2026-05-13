use anyhow::Result;
use image::{ImageBuffer, Rgb};
use std::time::Instant;
use vm_spirv::compile_file_with_workgroup_size;
use vulkan::Runtime;
use vulkano::buffer::BufferContents;

const WIDTH: usize = 512;
const HEIGHT: usize = 512;
const WORKGROUPS: [u32; 3] = [16, 16, 1];
const OUTPUT_PATH: &str = "pathfind.png";

// Maze parameters
const MAZE_COLS: usize = 32;
const MAZE_ROWS: usize = 32;
const CELL_SIZE: usize = WIDTH / MAZE_COLS; // 16
const WALL_THICK: usize = 2;

#[derive(BufferContents, Clone, Copy, Debug)]
#[repr(C)]
struct PathParams {
    start_x: u32,
    start_y: u32,
    end_x: u32,
    end_y: u32,
    width: u32,
    height: u32,
    iteration: u32,
}

// Obstacle shapes
struct Circle {
    cx: f32,
    cy: f32,
    r: f32,
}

struct Polygon {
    cx: f32,
    cy: f32,
    r: f32,
    sides: usize,
    angle_offset: f32,
}

fn main() -> Result<()> {
    let shader_path = std::env::var("PATHFIND_ZS").unwrap_or_else(|_| "zusts/gpu/pathfind.zs".to_string());
    let kernel = compile_file_with_workgroup_size(&shader_path, "pathfind", "main", [32, 32, 1])?;
    let words = kernel.spirv.words();

    // Start and end points
    let start_x = CELL_SIZE / 2;
    let start_y = CELL_SIZE / 2;
    let end_x = WIDTH - CELL_SIZE / 2;
    let end_y = HEIGHT - CELL_SIZE / 2;

    // Generate maze grid
    let mut grid = vec![-2.0f32; WIDTH * HEIGHT]; // -2 = unreached free

    // Carve maze walls
    let maze = generate_maze(MAZE_COLS, MAZE_ROWS);
    carve_maze_walls(&mut grid, &maze, MAZE_COLS, MAZE_ROWS, CELL_SIZE, WALL_THICK);

    // Add circle obstacles
    let circles = vec![
        Circle { cx: 200.0, cy: 200.0, r: 30.0 },
        Circle { cx: 350.0, cy: 150.0, r: 25.0 },
        Circle { cx: 400.0, cy: 350.0, r: 35.0 },
        Circle { cx: 130.0, cy: 380.0, r: 20.0 },
        Circle { cx: 280.0, cy: 420.0, r: 28.0 },
    ];
    for c in &circles {
        stamp_circle(&mut grid, c.cx, c.cy, c.r);
    }

    // Add polygon obstacles
    let polygons = vec![
        Polygon { cx: 300.0, cy: 280.0, r: 30.0, sides: 3, angle_offset: 0.0 },  // triangle
        Polygon { cx: 150.0, cy: 250.0, r: 25.0, sides: 5, angle_offset: 0.5 },  // pentagon
        Polygon { cx: 420.0, cy: 250.0, r: 28.0, sides: 6, angle_offset: 0.3 },  // hexagon
        Polygon { cx: 250.0, cy: 120.0, r: 22.0, sides: 4, angle_offset: 0.78 }, // diamond
    ];
    for p in &polygons {
        stamp_polygon(&mut grid, p.cx, p.cy, p.r, p.sides, p.angle_offset);
    }

    // Make sure start and end cells are free
    grid[start_y * WIDTH + start_x] = -2.0;
    grid[end_y * WIDTH + end_x] = -2.0;

    let t0 = Instant::now();

    let mut runtime = Runtime::new()?;
    let mut args = runtime.args();
    let params = args.add_input(PathParams { start_x: start_x as u32, start_y: start_y as u32, end_x: end_x as u32, end_y: end_y as u32, width: WIDTH as u32, height: HEIGHT as u32, iteration: 0 })?;
    let grid_buf = args.add_vec::<f32>((WIDTH * HEIGHT) as u64, |buf| {
        buf.copy_from_slice(&grid);
    })?;

    runtime.prepare(words, args)?;

    // Run BFS iterations
    let max_iter = WIDTH * HEIGHT;
    let mut found_iter = None;
    for iter in 0..=max_iter as u32 {
        params.write()?.iteration = iter;
        runtime.run(WORKGROUPS)?;

        // Check every few iterations if destination reached
        if iter % 50 == 0 || iter <= 5 {
            let read = grid_buf.read()?;
            let end_val = read[end_y * WIDTH + end_x];
            let start_val = read[start_y * WIDTH + start_x];
            let reached_count = read.iter().filter(|&&v| v >= 0.0).count();
            let wall_count = read.iter().filter(|&&v| v == -1.0).count();
            eprintln!("iter={iter} start_val={start_val} end_val={end_val} reached={reached_count} walls={wall_count}");
            if end_val >= 0.0 {
                found_iter = Some(iter);
                break;
            }
            if reached_count > 0 && iter > 0 && reached_count == (WIDTH * HEIGHT) - wall_count {
                eprintln!("all reachable cells explored but end not reached");
                break;
            }
        }
    }

    let elapsed = t0.elapsed();
    let final_grid = grid_buf.read()?;
    let grid_slice = &*final_grid;

    let reached = grid_slice.iter().filter(|&&v| v >= 0.0).count();
    let walls = grid_slice.iter().filter(|&&v| v == -1.0).count();
    let unreached = grid_slice.iter().filter(|&&v| v == -2.0).count();

    println!("compiled {shader_path} ({} words)", words.len());
    println!("executed pathfind with Vulkan");
    println!("maze: {MAZE_COLS}x{MAZE_ROWS} cells, image: {WIDTH}x{HEIGHT}");
    println!("obstacles: {} circles, {} polygons", circles.len(), polygons.len());
    println!("reached: {reached}, walls: {walls}, unreached: {unreached}");
    println!("time: {:.2?}", elapsed);

    if let Some(iter) = found_iter {
        println!("path found at iteration {iter} (distance {})", iter);
    } else {
        println!("path NOT found after {max_iter} iterations");
    }

    // Trace path on CPU (backtrack from end to start)
    let path = if found_iter.is_some() { trace_path(grid_slice, start_x, start_y, end_x, end_y) } else { Vec::new() };
    println!("path length: {} pixels", path.len());

    save_pathfind_png(grid_slice, &path, start_x, start_y, end_x, end_y, OUTPUT_PATH)?;
    println!("wrote {OUTPUT_PATH}");

    Ok(())
}

fn trace_path(grid: &[f32], sx: usize, sy: usize, ex: usize, ey: usize) -> Vec<(usize, usize)> {
    let mut path = vec![(ex, ey)];
    let mut cx = ex;
    let mut cy = ey;
    let directions: [(i32, i32); 4] = [(0, -1), (0, 1), (-1, 0), (1, 0)];

    loop {
        if cx == sx && cy == sy {
            break;
        }
        let cur_dist = grid[cy * WIDTH + cx] as i32;
        if cur_dist <= 0 {
            break;
        }
        let mut moved = false;
        for (dx, dy) in &directions {
            let nx = cx as i32 + dx;
            let ny = cy as i32 + dy;
            if nx < 0 || ny < 0 || nx >= WIDTH as i32 || ny >= HEIGHT as i32 {
                continue;
            }
            let nx = nx as usize;
            let ny = ny as usize;
            let nd = grid[ny * WIDTH + nx];
            if nd >= 0.0 && (nd as i32) == cur_dist - 1 {
                path.push((nx, ny));
                cx = nx;
                cy = ny;
                moved = true;
                break;
            }
        }
        if !moved {
            break;
        }
    }
    path.reverse();
    path
}

// ---- Maze generation (recursive backtracking) ----

fn generate_maze(cols: usize, rows: usize) -> Vec<Vec<u8>> {
    // Each cell has walls: 0=up, 1=right, 2=down, 3=left
    // 1 = wall present, 0 = wall removed
    let mut walls = vec![vec![0b1111u8; cols]; rows];
    let mut visited = vec![vec![false; cols]; rows];
    let mut stack = vec![(0usize, 0usize)];
    visited[0][0] = true;
    let dirs: [(i32, i32, u8, u8); 4] = [(0, -1, 0, 2), (1, 0, 1, 3), (0, 1, 2, 0), (-1, 0, 3, 1)];

    while let Some(&(cx, cy)) = stack.last() {
        let neighbors: Vec<(usize, usize, u8, u8)> = dirs
            .iter()
            .filter_map(|&(dx, dy, wall, opp)| {
                let nx = cx as i32 + dx;
                let ny = cy as i32 + dy;
                if nx >= 0 && ny >= 0 && nx < cols as i32 && ny < rows as i32 && !visited[ny as usize][nx as usize] { Some((nx as usize, ny as usize, wall, opp)) } else { None }
            })
            .collect();

        if neighbors.is_empty() {
            stack.pop();
            continue;
        }

        // Pick random neighbor
        let idx = simple_rand(stack.len()) % neighbors.len();
        let (nx, ny, wall, opp) = neighbors[idx];
        walls[cy][cx] &= !(1 << wall);
        walls[ny][nx] &= !(1 << opp);
        visited[ny][nx] = true;
        stack.push((nx, ny));
    }
    walls
}

fn simple_rand(seed: usize) -> usize {
    // Simple LCG for deterministic maze
    let x = seed.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
    (x >> 33) as usize
}

fn carve_maze_walls(grid: &mut [f32], walls: &[Vec<u8>], cols: usize, rows: usize, cell_size: usize, wall_thick: usize) {
    for cy in 0..rows {
        for cx in 0..cols {
            let cell_x = cx * cell_size;
            let cell_y = cy * cell_size;

            // Fill entire cell area with wall first
            for y in cell_y..cell_y + cell_size {
                for x in cell_x..cell_x + cell_size {
                    grid[y * WIDTH + x] = -1.0;
                }
            }

            // Carve interior (leave border walls)
            let inner_start = wall_thick;
            let inner_end = cell_size - wall_thick;
            for y in cell_y + inner_start..cell_y + inner_end {
                for x in cell_x + inner_start..cell_x + inner_end {
                    grid[y * WIDTH + x] = -2.0;
                }
            }

            // Remove walls where walls bit is 0 (passage open)
            let cell_walls = walls[cy][cx];

            // Up passage (wall bit 0)
            if cell_walls & 1 == 0 && cy > 0 {
                for y in cell_y..cell_y + wall_thick {
                    for x in cell_x + wall_thick..cell_x + cell_size - wall_thick {
                        grid[y * WIDTH + x] = -2.0;
                    }
                }
            }
            // Right passage (wall bit 1)
            if cell_walls & 2 == 0 && cx < cols - 1 {
                for y in cell_y + wall_thick..cell_y + cell_size - wall_thick {
                    for x in cell_x + cell_size - wall_thick..cell_x + cell_size {
                        grid[y * WIDTH + x] = -2.0;
                    }
                }
            }
            // Down passage (wall bit 2)
            if cell_walls & 4 == 0 && cy < rows - 1 {
                for y in cell_y + cell_size - wall_thick..cell_y + cell_size {
                    for x in cell_x + wall_thick..cell_x + cell_size - wall_thick {
                        grid[y * WIDTH + x] = -2.0;
                    }
                }
            }
            // Left passage (wall bit 3)
            if cell_walls & 8 == 0 && cx > 0 {
                for y in cell_y + wall_thick..cell_y + cell_size - wall_thick {
                    for x in cell_x..cell_x + wall_thick {
                        grid[y * WIDTH + x] = -2.0;
                    }
                }
            }
        }
    }
}

fn stamp_circle(grid: &mut [f32], cx: f32, cy: f32, r: f32) {
    let min_x = (cx - r).max(0.0) as usize;
    let max_x = ((cx + r) as usize).min(WIDTH - 1);
    let min_y = (cy - r).max(0.0) as usize;
    let max_y = ((cy + r) as usize).min(HEIGHT - 1);
    let r2 = r * r;
    for y in min_y..=max_y {
        for x in min_x..=max_x {
            let dx = x as f32 - cx;
            let dy = y as f32 - cy;
            if dx * dx + dy * dy <= r2 {
                grid[y * WIDTH + x] = -1.0;
            }
        }
    }
}

fn stamp_polygon(grid: &mut [f32], cx: f32, cy: f32, r: f32, sides: usize, angle_offset: f32) {
    // Compute vertices
    let vertices: Vec<(f32, f32)> = (0..sides)
        .map(|i| {
            let angle = angle_offset + (i as f32) * (2.0 * std::f32::consts::PI / sides as f32);
            (cx + r * angle.cos(), cy + r * angle.sin())
        })
        .collect();

    let min_x = vertices.iter().map(|v| v.0).fold(f32::MAX, f32::min).max(0.0) as usize;
    let max_x = vertices.iter().map(|v| v.0).fold(f32::MIN, f32::max).min(WIDTH as f32 - 1.0) as usize;
    let min_y = vertices.iter().map(|v| v.1).fold(f32::MAX, f32::min).max(0.0) as usize;
    let max_y = vertices.iter().map(|v| v.1).fold(f32::MIN, f32::max).min(HEIGHT as f32 - 1.0) as usize;

    for y in min_y..=max_y {
        for x in min_x..=max_x {
            if point_in_polygon(x as f32, y as f32, &vertices) {
                grid[y * WIDTH + x] = -1.0;
            }
        }
    }
}

fn point_in_polygon(px: f32, py: f32, vertices: &[(f32, f32)]) -> bool {
    let n = vertices.len();
    let mut inside = false;
    let mut j = n - 1;
    for i in 0..n {
        let (xi, yi) = vertices[i];
        let (xj, yj) = vertices[j];
        if ((yi > py) != (yj > py)) && (px < (xj - xi) * (py - yi) / (yj - yi) + xi) {
            inside = !inside;
        }
        j = i;
    }
    inside
}

fn save_pathfind_png(grid: &[f32], path: &[(usize, usize)], sx: usize, sy: usize, ex: usize, ey: usize, path_str: &str) -> Result<()> {
    let max_dist = grid.iter().copied().filter(|&v| v >= 0.0).fold(0.0f32, f32::max);

    let mut image = ImageBuffer::<Rgb<u8>, Vec<u8>>::new(WIDTH as u32, HEIGHT as u32);
    for y in 0..HEIGHT {
        for x in 0..WIDTH {
            let val = grid[y * WIDTH + x];
            let pixel = if val == -1.0 {
                // Wall / obstacle
                Rgb([40, 40, 50])
            } else if val == -2.0 {
                // Unreached free space
                Rgb([220, 220, 230])
            } else {
                // BFS distance: blue -> cyan -> yellow -> red
                let t = if max_dist > 0.0 { val / max_dist } else { 0.0 };
                heat_map(t)
            };
            image.put_pixel(x as u32, y as u32, pixel);
        }
    }

    // Draw path
    for &(px, py) in path {
        if px < WIDTH && py < HEIGHT {
            image.put_pixel(px as u32, py as u32, Rgb([255, 230, 50]));
        }
    }

    // Draw start marker (8x8 green)
    for dy in 0..8 {
        for dx in 0..8 {
            let px = sx + dx;
            let py = sy + dy;
            if px < WIDTH && py < HEIGHT {
                image.put_pixel(px as u32, py as u32, Rgb([0, 220, 80]));
            }
        }
    }

    // Draw end marker (8x8 red)
    for dy in 0..8 {
        for dx in 0..8 {
            let px = ex + dx;
            let py = ey + dy;
            if px < WIDTH && py < HEIGHT {
                image.put_pixel(px as u32, py as u32, Rgb([220, 40, 40]));
            }
        }
    }

    image.save(path_str)?;
    Ok(())
}

fn heat_map(t: f32) -> Rgb<u8> {
    let t = t.clamp(0.0, 1.0);
    let (r, g, b) = if t < 0.25 {
        let s = t / 0.25;
        (0.0, s, 1.0)
    } else if t < 0.5 {
        let s = (t - 0.25) / 0.25;
        (0.0, 1.0, 1.0 - s)
    } else if t < 0.75 {
        let s = (t - 0.5) / 0.25;
        (s, 1.0, 0.0)
    } else {
        let s = (t - 0.75) / 0.25;
        (1.0, 1.0 - s, 0.0)
    };
    Rgb([(r * 255.0) as u8, (g * 255.0) as u8, (b * 255.0) as u8])
}
