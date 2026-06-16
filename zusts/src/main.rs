use anyhow::{Context, Result};
use dynamic::{Dynamic, Type};
use image::{ImageBuffer, Rgb};
use std::path::PathBuf;
use std::time::Instant;
use vulkano::buffer::BufferContents;

const SYNTAX_BOOL_TESTS: &[&str] = &[
    "syntax_suite::test_literals_types_and_comments",
    "syntax_suite::test_unary_binary_and_assigns",
    "syntax_suite::test_control_flow",
    "syntax_suite::test_patterns_lists_dicts_and_fields",
    "syntax_suite::test_structs_impls_generics_and_assoc",
    "syntax_suite::test_closures_arrays_ranges_and_calls",
    "syntax_suite::test_sqrt_builtin",
    "syntax_suite::test_string_numeric_casts",
    "syntax_suite::test_nested_closure_captures",
    "syntax_suite::test_dict_shorthand",
];

const SYNTAX_EDGE_BOOL_TESTS: &[&str] = &[
    "syntax_edge::test_int_extremes",
    "syntax_edge::test_empty_containers",
    "syntax_edge::test_nested_patterns",
    "syntax_edge::test_nested_loops",
    "syntax_edge::test_nested_if_chain",
    "syntax_edge::test_dynamic_list_operations",
    "syntax_edge::test_dynamic_map_operations",
    "syntax_edge::test_string_split",
    "syntax_edge::test_range_expressions",
    "syntax_edge::test_bitwise_operations",
    "syntax_edge::test_negation_on_types",
    "syntax_edge::test_compound_assign_all_ops",
    "syntax_edge::test_array_index_assign",
    "syntax_edge::test_string_concat_all_types",
    "syntax_edge::test_chain_reassign",
    "syntax_edge::test_nested_struct_field_access",
    "syntax_edge::test_mixed_type_list",
    "syntax_edge::test_map_iteration",
    "syntax_edge::test_void_null_in_bool_context",
];

fn main() -> Result<()> {
    let vm = vm::Vm::with_all()?;

    println!("==============================================");
    println!("       Zust 示例运行器");
    println!("==============================================\n");

    // 导入保留在开源仓库中的示例文件
    vm_import_file(&vm, "test", "test.zs")?;
    vm_import_file(&vm, "qsort", "qsort.zs")?;
    vm_import_file(&vm, "syntax_suite", "syntax_suite.zs")?;
    vm_import_file(&vm, "syntax_edge", "syntax_edge.zs")?;
    vm_import_file(&vm, "test_recursive_bug", "bug_tests/test_recursive_bug.zs")?;
    vm_import_file(&vm, "test_is_list_minimal", "bug_tests/test_is_list_minimal.zs")?;

    println!("示例模块加载完成\n");

    // 运行保留下来的回归示例
    for name in SYNTAX_BOOL_TESTS {
        run_bool_test(&vm, name)?;
    }
    for name in SYNTAX_EDGE_BOOL_TESTS {
        run_bool_test(&vm, name)?;
    }
    run_test(&vm, "test_recursive_bug::run_all_tests", &[])?;
    run_test(&vm, "test_is_list_minimal::run_all_tests", &[])?;

    println!("\n==============================================");
    println!("       示例执行完成!");
    println!("==============================================\n");

    // SPIR-V pathfind shader 编译与执行。没有可用 Vulkan 运行时时不影响语言测试结果。
    if let Err(err) = run_pathfind_shader() {
        println!("SPIR-V Pathfind Shader 跳过/失败: {err:#}");
    }

    Ok(())
}

fn zusts_path(path: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(path)
}

fn vm_import_file(vm: &vm::Vm, name: &str, path: &str) -> Result<()> {
    vm.jit.write().compiler.import_file(name, zusts_path(path).to_str().expect("zust test path is valid utf-8")).map(|_| ()).with_context(|| format!("import {path} as {name}"))
}

fn run_test(vm: &vm::Vm, fn_name: &str, tys: &[Type]) -> Result<()> {
    match vm.jit.write().get_fn_ptr(fn_name, tys) {
        Ok((ptr, _ret)) => {
            let test_fn: extern "C" fn() -> *mut Dynamic = unsafe { std::mem::transmute(ptr) };
            let result = unsafe { Box::from_raw(test_fn()) };
            println!("[{}] 结果: {:?}", fn_name, result);
            Ok(())
        }
        Err(e) => {
            println!("[{}] 错误: {:?}", fn_name, e);
            Err(e)
        }
    }
}

fn run_bool_test(vm: &vm::Vm, fn_name: &str) -> Result<()> {
    let (ptr, ret) = vm.jit.write().get_fn_ptr(fn_name, &[])?;
    anyhow::ensure!(ret == Type::Bool, "{fn_name} should return bool, got {:?}", ret);
    let test_fn: extern "C" fn() -> bool = unsafe { std::mem::transmute(ptr) };
    let result = test_fn();
    anyhow::ensure!(result, "{fn_name} returned false");
    println!("[{}] 结果: true", fn_name);
    Ok(())
}

// ---- SPIR-V Pathfind Shader ----

const PATH_WIDTH: usize = 512;
const PATH_HEIGHT: usize = 512;
const PATH_WORKGROUPS: [u32; 3] = [16, 16, 1];
const PATH_OUTPUT: &str = "pathfind.png";

const MAZE_COLS: usize = 32;
const MAZE_ROWS: usize = 32;
const CELL_SIZE: usize = PATH_WIDTH / MAZE_COLS;
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

fn run_pathfind_shader() -> Result<()> {
    println!("==============================================");
    println!("  SPIR-V Pathfind Shader 编译与执行");
    println!("==============================================");

    // 编译 pathfind.zs -> SPIR-V
    let t0 = Instant::now();
    let source = std::fs::read_to_string(zusts_path("gpu/pathfind.zs"))?;
    let kernel = vm_spirv::compile_source_with_workgroup_size(&source, "pathfind", "main", [32, 32, 1])?;
    let words = kernel.spirv.words();
    println!("编译 pathfind.zs -> SPIR-V ({} words) 耗时 {:?}", words.len(), t0.elapsed());

    // 起点终点 (放在迷宫单元格内部中心区域)
    let start_x = CELL_SIZE / 2;
    let start_y = CELL_SIZE / 2;
    let end_x = (MAZE_COLS - 1) * CELL_SIZE + CELL_SIZE / 2;
    let end_y = (MAZE_ROWS - 1) * CELL_SIZE + CELL_SIZE / 2;

    // 生成迷宫网格
    let mut grid = vec![-2.0f32; PATH_WIDTH * PATH_HEIGHT]; // -2 = unreached free
    let maze = generate_maze(MAZE_COLS, MAZE_ROWS);
    carve_maze_walls(&mut grid, &maze, MAZE_COLS, MAZE_ROWS, CELL_SIZE, WALL_THICK);

    // 添加圆形障碍
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

    // 添加多边形障碍
    let polygons = vec![
        Polygon { cx: 300.0, cy: 280.0, r: 30.0, sides: 3, angle_offset: 0.0 },
        Polygon { cx: 150.0, cy: 250.0, r: 25.0, sides: 5, angle_offset: 0.5 },
        Polygon { cx: 420.0, cy: 250.0, r: 28.0, sides: 6, angle_offset: 0.3 },
        Polygon { cx: 250.0, cy: 120.0, r: 22.0, sides: 4, angle_offset: 0.78 },
    ];
    for p in &polygons {
        stamp_polygon(&mut grid, p.cx, p.cy, p.r, p.sides, p.angle_offset);
    }

    grid[start_y * PATH_WIDTH + start_x] = -2.0;
    grid[end_y * PATH_WIDTH + end_x] = -2.0;

    // Vulkan 执行
    let t1 = Instant::now();
    let mut runtime = vulkan::Runtime::new()?;
    let mut args = runtime.args();
    let params = args.add_input(PathParams { start_x: start_x as u32, start_y: start_y as u32, end_x: end_x as u32, end_y: end_y as u32, width: PATH_WIDTH as u32, height: PATH_HEIGHT as u32, iteration: 0 })?;
    let grid_buf = args.add_vec::<f32>((PATH_WIDTH * PATH_HEIGHT) as u64, |buf| {
        buf.copy_from_slice(&grid);
    })?;

    runtime.prepare(words, args)?;

    let max_iter = PATH_WIDTH * PATH_HEIGHT;
    let mut found_iter = None;
    for iter in 0..=max_iter as u32 {
        params.write()?.iteration = iter;
        runtime.run(PATH_WORKGROUPS)?;

        if iter % 50 == 0 || iter <= 5 {
            let read = grid_buf.read()?;
            let end_val = read[end_y * PATH_WIDTH + end_x];
            let reached_count = read.iter().filter(|&&v| v >= 0.0).count();
            let wall_count = read.iter().filter(|&&v| v == -1.0).count();
            if end_val >= 0.0 {
                found_iter = Some(iter);
                break;
            }
            if reached_count > 0 && iter > 0 && reached_count == (PATH_WIDTH * PATH_HEIGHT) - wall_count {
                println!("所有可达单元格已探索但未到达终点");
                break;
            }
        }
    }

    let elapsed = t1.elapsed();
    let final_grid = grid_buf.read()?;
    let grid_slice = &*final_grid;

    let reached = grid_slice.iter().filter(|&&v| v >= 0.0).count();
    let walls = grid_slice.iter().filter(|&&v| v == -1.0).count();
    let unreached = grid_slice.iter().filter(|&&v| v == -2.0).count();

    println!("迷宫: {}x{} cells, 图像: {}x{}", MAZE_COLS, MAZE_ROWS, PATH_WIDTH, PATH_HEIGHT);
    println!("起点: ({}, {}), 终点: ({}, {}), 终点值: {}", start_x, start_y, end_x, end_y, grid_slice[end_y * PATH_WIDTH + end_x]);
    println!("障碍: {} 圆形, {} 多边形", circles.len(), polygons.len());
    println!("reached={}, walls={}, unreached={}", reached, walls, unreached);
    println!("Vulkan 执行耗时: {:.2?}", elapsed);

    if let Some(iter) = found_iter {
        println!("路径在迭代 {} 找到 (距离 {})", iter, iter);
    } else {
        println!("{} 次迭代后未找到路径", max_iter);
    }

    let path = if found_iter.is_some() { trace_path(grid_slice, start_x, start_y, end_x, end_y) } else { Vec::new() };
    println!("路径长度: {} 像素", path.len());

    save_pathfind_png(grid_slice, &path, start_x, start_y, end_x, end_y, PATH_OUTPUT)?;
    println!("已写入 {}", PATH_OUTPUT);

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn syntax_suite_regression() -> Result<()> {
        let vm = vm::Vm::with_all()?;
        vm_import_file(&vm, "syntax_suite", "syntax_suite.zs")?;
        for name in SYNTAX_BOOL_TESTS {
            run_bool_test(&vm, name)?;
        }
        Ok(())
    }

    #[test]
    fn syntax_edge_regression() -> Result<()> {
        let vm = vm::Vm::with_all()?;
        vm_import_file(&vm, "syntax_edge", "syntax_edge.zs")?;
        for name in SYNTAX_EDGE_BOOL_TESTS {
            run_bool_test(&vm, name)?;
        }
        Ok(())
    }
}

fn trace_path(grid: &[f32], sx: usize, sy: usize, ex: usize, ey: usize) -> Vec<(usize, usize)> {
    let mut path = vec![(ex, ey)];
    let mut cx = ex;
    let mut cy = ey;
    let directions: [(i32, i32); 4] = [(0, -1), (0, 1), (-1, 0), (1, 0)];

    for _ in 0..PATH_WIDTH * PATH_HEIGHT {
        if cx == sx && cy == sy {
            break;
        }
        let cur_dist = grid[cy * PATH_WIDTH + cx];
        if cur_dist <= 0.0 {
            break;
        }

        // Gradient descent: move to neighbor with smallest distance
        let mut best: Option<(usize, usize, f32)> = None;
        for (dx, dy) in &directions {
            let nx = cx as i32 + dx;
            let ny = cy as i32 + dy;
            if nx < 0 || ny < 0 || nx >= PATH_WIDTH as i32 || ny >= PATH_HEIGHT as i32 {
                continue;
            }
            let nx = nx as usize;
            let ny = ny as usize;
            let nd = grid[ny * PATH_WIDTH + nx];
            if nd >= 0.0 && nd < cur_dist {
                if best.map_or(true, |b| nd < b.2) {
                    best = Some((nx, ny, nd));
                }
            }
        }

        if let Some((nx, ny, _)) = best {
            path.push((nx, ny));
            cx = nx;
            cy = ny;
        } else {
            break;
        }
    }
    path.reverse();
    path
}

fn generate_maze(cols: usize, rows: usize) -> Vec<Vec<u8>> {
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
    let x = seed.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
    (x >> 33) as usize
}

fn carve_maze_walls(grid: &mut [f32], walls: &[Vec<u8>], cols: usize, rows: usize, cell_size: usize, wall_thick: usize) {
    for cy in 0..rows {
        for cx in 0..cols {
            let cell_x = cx * cell_size;
            let cell_y = cy * cell_size;

            for y in cell_y..cell_y + cell_size {
                for x in cell_x..cell_x + cell_size {
                    grid[y * PATH_WIDTH + x] = -1.0;
                }
            }

            let inner_start = wall_thick;
            let inner_end = cell_size - wall_thick;
            for y in cell_y + inner_start..cell_y + inner_end {
                for x in cell_x + inner_start..cell_x + inner_end {
                    grid[y * PATH_WIDTH + x] = -2.0;
                }
            }

            let cell_walls = walls[cy][cx];

            if cell_walls & 1 == 0 && cy > 0 {
                for y in cell_y..cell_y + wall_thick {
                    for x in cell_x + wall_thick..cell_x + cell_size - wall_thick {
                        grid[y * PATH_WIDTH + x] = -2.0;
                    }
                }
            }
            if cell_walls & 2 == 0 && cx < cols - 1 {
                for y in cell_y + wall_thick..cell_y + cell_size - wall_thick {
                    for x in cell_x + cell_size - wall_thick..cell_x + cell_size {
                        grid[y * PATH_WIDTH + x] = -2.0;
                    }
                }
            }
            if cell_walls & 4 == 0 && cy < rows - 1 {
                for y in cell_y + cell_size - wall_thick..cell_y + cell_size {
                    for x in cell_x + wall_thick..cell_x + cell_size - wall_thick {
                        grid[y * PATH_WIDTH + x] = -2.0;
                    }
                }
            }
            if cell_walls & 8 == 0 && cx > 0 {
                for y in cell_y + wall_thick..cell_y + cell_size - wall_thick {
                    for x in cell_x..cell_x + wall_thick {
                        grid[y * PATH_WIDTH + x] = -2.0;
                    }
                }
            }
        }
    }
}

fn stamp_circle(grid: &mut [f32], cx: f32, cy: f32, r: f32) {
    let min_x = (cx - r).max(0.0) as usize;
    let max_x = ((cx + r) as usize).min(PATH_WIDTH - 1);
    let min_y = (cy - r).max(0.0) as usize;
    let max_y = ((cy + r) as usize).min(PATH_HEIGHT - 1);
    let r2 = r * r;
    for y in min_y..=max_y {
        for x in min_x..=max_x {
            let dx = x as f32 - cx;
            let dy = y as f32 - cy;
            if dx * dx + dy * dy <= r2 {
                grid[y * PATH_WIDTH + x] = -1.0;
            }
        }
    }
}

fn stamp_polygon(grid: &mut [f32], cx: f32, cy: f32, r: f32, sides: usize, angle_offset: f32) {
    let vertices: Vec<(f32, f32)> = (0..sides)
        .map(|i| {
            let angle = angle_offset + (i as f32) * (2.0 * std::f32::consts::PI / sides as f32);
            (cx + r * angle.cos(), cy + r * angle.sin())
        })
        .collect();

    let min_x = vertices.iter().map(|v| v.0).fold(f32::MAX, f32::min).max(0.0) as usize;
    let max_x = vertices.iter().map(|v| v.0).fold(f32::MIN, f32::max).min(PATH_WIDTH as f32 - 1.0) as usize;
    let min_y = vertices.iter().map(|v| v.1).fold(f32::MAX, f32::min).max(0.0) as usize;
    let max_y = vertices.iter().map(|v| v.1).fold(f32::MIN, f32::max).min(PATH_HEIGHT as f32 - 1.0) as usize;

    for y in min_y..=max_y {
        for x in min_x..=max_x {
            if point_in_polygon(x as f32, y as f32, &vertices) {
                grid[y * PATH_WIDTH + x] = -1.0;
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

    let mut image = ImageBuffer::<Rgb<u8>, Vec<u8>>::new(PATH_WIDTH as u32, PATH_HEIGHT as u32);
    for y in 0..PATH_HEIGHT {
        for x in 0..PATH_WIDTH {
            let val = grid[y * PATH_WIDTH + x];
            let pixel = if val == -1.0 {
                Rgb([40, 40, 50])
            } else if val == -2.0 {
                Rgb([220, 220, 230])
            } else {
                let t = if max_dist > 0.0 { val / max_dist } else { 0.0 };
                heat_map(t)
            };
            image.put_pixel(x as u32, y as u32, pixel);
        }
    }

    for &(px, py) in path {
        if px < PATH_WIDTH && py < PATH_HEIGHT {
            image.put_pixel(px as u32, py as u32, Rgb([255, 230, 50]));
        }
    }

    for dy in 0..8 {
        for dx in 0..8 {
            let px = sx + dx;
            let py = sy + dy;
            if px < PATH_WIDTH && py < PATH_HEIGHT {
                image.put_pixel(px as u32, py as u32, Rgb([0, 220, 80]));
            }
        }
    }

    for dy in 0..8 {
        for dx in 0..8 {
            let px = ex + dx;
            let py = ey + dy;
            if px < PATH_WIDTH && py < PATH_HEIGHT {
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
