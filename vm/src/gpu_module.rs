use anyhow::{Context, Result, anyhow, bail};
use dynamic::{Dynamic, Type, map};
use std::path::Path;
use vulkano::buffer::Subbuffer;

extern "C" fn spirv_compile(input: *const Dynamic) -> *const Dynamic {
    dynamic_result(compile_spirv_response(read_input(input), true))
}

extern "C" fn spirv_check(input: *const Dynamic) -> *const Dynamic {
    dynamic_result(compile_spirv_response(read_input(input), false))
}

extern "C" fn metal_compile(input: *const Dynamic) -> *const Dynamic {
    dynamic_result(compile_metal_response(read_input(input), true))
}

extern "C" fn metal_check(input: *const Dynamic) -> *const Dynamic {
    dynamic_result(compile_metal_response(read_input(input), false))
}

extern "C" fn vulkan_run(input: *const Dynamic) -> *const Dynamic {
    dynamic_result(run_vulkan(read_input(input)))
}

extern "C" fn metal_run(input: *const Dynamic) -> *const Dynamic {
    dynamic_result(run_metal(read_input(input)))
}

pub const GPU_NATIVE: &[(&str, &[Type], Type, *const u8)] = &[
    ("spirv_compile", &[Type::Any], Type::Any, spirv_compile as *const u8),
    ("spirv_check", &[Type::Any], Type::Any, spirv_check as *const u8),
    ("metal_compile", &[Type::Any], Type::Any, metal_compile as *const u8),
    ("metal_check", &[Type::Any], Type::Any, metal_check as *const u8),
    ("vulkan_run", &[Type::Any], Type::Any, vulkan_run as *const u8),
    ("metal_run", &[Type::Any], Type::Any, metal_run as *const u8),
];

fn read_input(input: *const Dynamic) -> Dynamic {
    if input.is_null() { Dynamic::Null } else { unsafe { (*input).clone() } }
}

fn dynamic_result(result: Result<Dynamic>) -> *const Dynamic {
    let value = match result {
        Ok(value) => value,
        Err(err) => map!("ok"=> false, "error"=> format!("{err:#}")),
    };
    Box::into_raw(Box::new(value))
}

fn compile_spirv_response(input: Dynamic, include_module: bool) -> Result<Dynamic> {
    let kernel = compile_spirv_kernel(&input)?;
    let response = map!(
        "ok"=> true,
        "backend"=> "spirv",
        "entry"=> kernel.entry.to_string(),
        "word_count"=> kernel.spirv.words().len() as i64,
        "arg_tys"=> type_list(&kernel.arg_tys),
        "ret_ty"=> format!("{:?}", kernel.ret_ty)
    );
    if include_module {
        let bytes = kernel.spirv.words().iter().flat_map(|word| word.to_le_bytes()).collect::<Vec<_>>();
        response.insert("words", Dynamic::from(kernel.spirv.words()));
        response.insert("bytes", Dynamic::from(bytes));
        response.insert("disassembly", kernel.spirv.disassemble());
    }
    Ok(response)
}

fn compile_spirv_kernel(input: &Dynamic) -> Result<vm_spirv::Kernel> {
    let source = source_bytes(input)?;
    let module_name = module_name(input)?;
    let entry = entry_name(input);
    let workgroup_size = workgroup_size(input)?;
    let generic_args = generic_args(input)?;
    vm_spirv::compile_source_with_externs_generic_args_and_workgroup_size(source, &module_name, &entry, vm_spirv::spirv_builtins(), &generic_args, workgroup_size)
        .with_context(|| format!("compile Zust {module_name}::{entry} to SPIR-V"))
}

#[cfg(target_os = "macos")]
fn compile_metal_response(input: Dynamic, include_module: bool) -> Result<Dynamic> {
    let kernel = compile_metal_kernel(&input)?;
    let response = map!(
        "ok"=> true,
        "backend"=> "metal",
        "entry"=> kernel.entry.to_string(),
        "workgroup_size"=> Dynamic::from(kernel.workgroup_size),
        "arg_tys"=> type_list(&kernel.arg_tys),
        "ret_ty"=> format!("{:?}", kernel.ret_ty)
    );
    if include_module {
        response.insert("source", kernel.metal.source().to_string());
    }
    Ok(response)
}

#[cfg(not(target_os = "macos"))]
fn compile_metal_response(_input: Dynamic, _include_module: bool) -> Result<Dynamic> {
    bail!("Metal backend is only available on macOS")
}

#[cfg(target_os = "macos")]
fn compile_metal_kernel(input: &Dynamic) -> Result<vm_metal::Kernel> {
    let source = source_bytes(input)?;
    let module_name = module_name(input)?;
    let entry = entry_name(input);
    let workgroup_size = workgroup_size(input)?;
    let generic_args = generic_args(input)?;
    vm_metal::compile_source_with_externs_generic_args_and_workgroup_size(source, &module_name, &entry, vm_metal::metal_builtins(), &generic_args, workgroup_size)
        .with_context(|| format!("compile Zust {module_name}::{entry} to Metal"))
}

fn run_vulkan(input: Dynamic) -> Result<Dynamic> {
    let words = spirv_words(&input)?;
    let groups = groups(&input)?;
    let mut runtime = vulkan::Runtime::new()?;
    let mut args = runtime.args();
    let readbacks = add_vulkan_args(&mut args, input.get_dynamic("args").unwrap_or_else(|| Dynamic::list(Vec::new())))?;
    runtime.prepare(&words, args)?;
    runtime.run(groups)?;
    let outputs = readbacks.into_iter().map(VulkanReadback::read).collect::<Result<Vec<_>>>()?;
    Ok(map!("ok"=> true, "backend"=> "vulkan", "groups"=> Dynamic::from(groups), "outputs"=> Dynamic::list(outputs)))
}

#[cfg(target_os = "macos")]
fn run_metal(input: Dynamic) -> Result<Dynamic> {
    let (source, workgroup_size) = if let Some(source) = input.get_dynamic("metal").or_else(|| input.get_dynamic("shader")) {
        (source.as_str().to_string(), workgroup_size(&input)?)
    } else {
        let kernel = compile_metal_kernel(&input)?;
        (kernel.metal.into_source(), kernel.workgroup_size)
    };
    let groups = groups(&input)?;
    let mut runtime = vm_metal::Runtime::new()?;
    let mut args = runtime.args();
    let readbacks = add_metal_args(&mut args, input.get_dynamic("args").unwrap_or_else(|| Dynamic::list(Vec::new())))?;
    runtime.prepare_with_workgroup_size(&source, args, workgroup_size)?;
    runtime.run(groups)?;
    let outputs = readbacks.into_iter().map(MetalReadback::read).collect::<Result<Vec<_>>>()?;
    Ok(map!("ok"=> true, "backend"=> "metal", "groups"=> Dynamic::from(groups), "outputs"=> Dynamic::list(outputs)))
}

#[cfg(not(target_os = "macos"))]
fn run_metal(_input: Dynamic) -> Result<Dynamic> {
    bail!("Metal runtime is only available on macOS")
}

fn source_bytes(input: &Dynamic) -> Result<Vec<u8>> {
    if input.is_str() {
        return Ok(input.as_str().as_bytes().to_vec());
    }
    if let Some(source) = input.get_dynamic("source").or_else(|| input.get_dynamic("code")) {
        if let Some(bytes) = source.as_bytes() {
            return Ok(bytes.to_vec());
        }
        return Ok(source.as_str().as_bytes().to_vec());
    }
    if let Some(path) = input.get_dynamic("path").or_else(|| input.get_dynamic("file")) {
        return std::fs::read(path.as_str()).with_context(|| format!("read Zust source {}", path.as_str()));
    }
    bail!("gpu compile input needs `source`, `code`, `path`, or a source string")
}

fn module_name(input: &Dynamic) -> Result<String> {
    if let Some(module) = input.get_dynamic("module").or_else(|| input.get_dynamic("module_name")) {
        let module = module.as_str();
        if !module.is_empty() {
            return Ok(module.to_string());
        }
    }
    if let Some(path) = input.get_dynamic("path").or_else(|| input.get_dynamic("file")) {
        let stem = Path::new(path.as_str()).file_stem().and_then(|stem| stem.to_str()).ok_or_else(|| anyhow!("cannot infer module name from path {}", path.as_str()))?;
        return Ok(stem.to_string());
    }
    Ok("main".to_string())
}

fn entry_name(input: &Dynamic) -> String {
    input
        .get_dynamic("fn")
        .or_else(|| input.get_dynamic("entry"))
        .or_else(|| input.get_dynamic("function"))
        .map(|entry| entry.as_str().to_string())
        .filter(|entry| !entry.is_empty())
        .unwrap_or_else(|| "main".to_string())
}

fn workgroup_size(input: &Dynamic) -> Result<[u32; 3]> {
    vec3(input.get_dynamic("workgroup_size").or_else(|| input.get_dynamic("workgroup")).unwrap_or_else(|| Dynamic::from([1u32, 1, 1])), "workgroup_size")
}

fn groups(input: &Dynamic) -> Result<[u32; 3]> {
    vec3(input.get_dynamic("groups").or_else(|| input.get_dynamic("dispatch")).unwrap_or_else(|| Dynamic::from([1u32, 1, 1])), "groups")
}

fn vec3(value: Dynamic, name: &str) -> Result<[u32; 3]> {
    let values = dynamic_to_vec::<u32>(&value)?;
    match values.as_slice() {
        [x, y] => Ok([*x, *y, 1]),
        [x, y, z] => Ok([*x, *y, *z]),
        _ => bail!("{name} must contain two or three u32 values"),
    }
}

fn generic_args(input: &Dynamic) -> Result<Vec<Type>> {
    let Some(args) = input.get_dynamic("generic_args").or_else(|| input.get_dynamic("generics")) else {
        return Ok(Vec::new());
    };
    (0..args.len())
        .map(|idx| {
            let value = args.get_idx(idx).ok_or_else(|| anyhow!("missing generic arg {idx}"))?;
            value.as_int().map(Type::ConstInt).ok_or_else(|| anyhow!("generic arg {idx} must be an integer"))
        })
        .collect()
}

fn type_list(types: &[Type]) -> Dynamic {
    Dynamic::list(types.iter().map(|ty| Dynamic::from(format!("{ty:?}"))).collect())
}

fn spirv_words(input: &Dynamic) -> Result<Vec<u32>> {
    if let Some(words) = input.get_dynamic("words").or_else(|| input.get_dynamic("spirv")) {
        if let Some(words) = dynamic_words(&words)? {
            return Ok(words);
        }
    }
    if let Some(bytes) = input.get_dynamic("bytes") {
        let bytes = bytes.as_bytes().ok_or_else(|| anyhow!("SPIR-V `bytes` must be a byte vector"))?;
        return words_from_bytes(bytes);
    }
    if let Some(path) = input.get_dynamic("spirv_path") {
        let bytes = std::fs::read(path.as_str()).with_context(|| format!("read SPIR-V {}", path.as_str()))?;
        return words_from_bytes(&bytes);
    }
    Ok(compile_spirv_kernel(input)?.spirv.into_words())
}

fn dynamic_words(value: &Dynamic) -> Result<Option<Vec<u32>>> {
    if let Some(bytes) = value.as_bytes() {
        return words_from_bytes(bytes).map(Some);
    }
    if value.is_list() || value.is_vec() {
        return dynamic_to_vec::<u32>(value).map(Some);
    }
    Ok(None)
}

fn words_from_bytes(bytes: &[u8]) -> Result<Vec<u32>> {
    if !bytes.len().is_multiple_of(4) {
        bail!("SPIR-V byte length must be divisible by 4");
    }
    Ok(bytes.chunks_exact(4).map(|chunk| u32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]])).collect())
}

fn add_vulkan_args(args: &mut vulkan::Args, specs: Dynamic) -> Result<Vec<VulkanReadback>> {
    let mut readbacks = Vec::new();
    for idx in 0..specs.len() {
        let spec = specs.get_idx(idx).ok_or_else(|| anyhow!("missing Vulkan arg {idx}"))?;
        let ty = arg_type(&spec)?;
        let kind = arg_kind(&spec);
        let read = spec.get_dynamic("read").and_then(|value| value.as_bool()).unwrap_or(kind != "input");
        match (kind.as_str(), ty.as_str()) {
            ("input", "u32") => {
                let _ = args.add_input(scalar::<u32>(&spec)?)?;
            }
            ("input", "i32") => {
                let _ = args.add_input(scalar::<i32>(&spec)?)?;
            }
            ("input", "f32") => {
                let _ = args.add_input(scalar::<f32>(&spec)?)?;
            }
            ("input", "u64") => {
                let _ = args.add_input(scalar::<u64>(&spec)?)?;
            }
            ("input", "i64") => {
                let _ = args.add_input(scalar::<i64>(&spec)?)?;
            }
            ("input", "f64") => {
                let _ = args.add_input(scalar::<f64>(&spec)?)?;
            }
            (_, "u32") => {
                let buf = add_vulkan_vec::<u32>(args, &spec)?;
                if read {
                    readbacks.push(VulkanReadback::U32(idx, buf));
                }
            }
            (_, "i32") => {
                let buf = add_vulkan_vec::<i32>(args, &spec)?;
                if read {
                    readbacks.push(VulkanReadback::I32(idx, buf));
                }
            }
            (_, "f32") => {
                let buf = add_vulkan_vec::<f32>(args, &spec)?;
                if read {
                    readbacks.push(VulkanReadback::F32(idx, buf));
                }
            }
            (_, "u64") => {
                let buf = add_vulkan_vec::<u64>(args, &spec)?;
                if read {
                    readbacks.push(VulkanReadback::U64(idx, buf));
                }
            }
            (_, "i64") => {
                let buf = add_vulkan_vec::<i64>(args, &spec)?;
                if read {
                    readbacks.push(VulkanReadback::I64(idx, buf));
                }
            }
            (_, "f64") => {
                let buf = add_vulkan_vec::<f64>(args, &spec)?;
                if read {
                    readbacks.push(VulkanReadback::F64(idx, buf));
                }
            }
            (_, "bytes") => {
                let buf = if spec.get_dynamic("values").is_some() || spec.get_dynamic("value").is_some() {
                    args.add_bytes(bytes_or_zeros(&spec)?)?
                } else {
                    args.add_bytes_output(spec.get_dynamic("len").and_then(|len| len.as_uint()).unwrap_or(1))?
                };
                if read {
                    readbacks.push(VulkanReadback::Bytes(idx, buf));
                }
            }
            _ => bail!("unsupported Vulkan arg {idx}: kind={kind:?}, type={ty:?}"),
        }
    }
    Ok(readbacks)
}

fn add_vulkan_vec<T>(args: &mut vulkan::Args, spec: &Dynamic) -> Result<Subbuffer<[T]>>
where
    T: vulkano::buffer::BufferContents + Default + Clone + TryFrom<Dynamic>,
    <T as TryFrom<Dynamic>>::Error: std::fmt::Debug,
{
    let values = values_or_zeros::<T>(spec)?;
    args.add_vec(values.len() as u64, |dst| dst.clone_from_slice(&values))
}

enum VulkanReadback {
    U32(usize, Subbuffer<[u32]>),
    I32(usize, Subbuffer<[i32]>),
    F32(usize, Subbuffer<[f32]>),
    U64(usize, Subbuffer<[u64]>),
    I64(usize, Subbuffer<[i64]>),
    F64(usize, Subbuffer<[f64]>),
    Bytes(usize, Subbuffer<[u8]>),
}

impl VulkanReadback {
    fn read(self) -> Result<Dynamic> {
        match self {
            Self::U32(index, buf) => Ok(readback(index, "u32", Dynamic::from(buf.read()?.as_ref()))),
            Self::I32(index, buf) => Ok(readback(index, "i32", Dynamic::from(buf.read()?.as_ref()))),
            Self::F32(index, buf) => Ok(readback(index, "f32", Dynamic::from(buf.read()?.as_ref()))),
            Self::U64(index, buf) => Ok(readback(index, "u64", Dynamic::from(buf.read()?.as_ref()))),
            Self::I64(index, buf) => Ok(readback(index, "i64", Dynamic::from(buf.read()?.as_ref()))),
            Self::F64(index, buf) => Ok(readback(index, "f64", Dynamic::from(buf.read()?.as_ref()))),
            Self::Bytes(index, buf) => Ok(readback(index, "bytes", Dynamic::from(buf.read()?.as_ref()))),
        }
    }
}

#[cfg(target_os = "macos")]
fn add_metal_args(args: &mut vm_metal::Args, specs: Dynamic) -> Result<Vec<MetalReadback>> {
    let mut readbacks = Vec::new();
    for idx in 0..specs.len() {
        let spec = specs.get_idx(idx).ok_or_else(|| anyhow!("missing Metal arg {idx}"))?;
        let ty = arg_type(&spec)?;
        let kind = arg_kind(&spec);
        let read = spec.get_dynamic("read").and_then(|value| value.as_bool()).unwrap_or(kind != "input");
        match (kind.as_str(), ty.as_str()) {
            ("input", "u32") => {
                let _ = args.add_input(scalar::<u32>(&spec)?)?;
            }
            ("input", "i32") => {
                let _ = args.add_input(scalar::<i32>(&spec)?)?;
            }
            ("input", "f32") => {
                let _ = args.add_input(scalar::<f32>(&spec)?)?;
            }
            ("input", "u64") => {
                let _ = args.add_input(scalar::<u64>(&spec)?)?;
            }
            ("input", "i64") => {
                let _ = args.add_input(scalar::<i64>(&spec)?)?;
            }
            ("input", "f64") => {
                let _ = args.add_input(scalar::<f64>(&spec)?)?;
            }
            (_, "u32") => {
                let buf = add_metal_vec::<u32>(args, &spec)?;
                if read {
                    readbacks.push(MetalReadback::U32(idx, buf));
                }
            }
            (_, "i32") => {
                let buf = add_metal_vec::<i32>(args, &spec)?;
                if read {
                    readbacks.push(MetalReadback::I32(idx, buf));
                }
            }
            (_, "f32") => {
                let buf = add_metal_vec::<f32>(args, &spec)?;
                if read {
                    readbacks.push(MetalReadback::F32(idx, buf));
                }
            }
            (_, "u64") => {
                let buf = add_metal_vec::<u64>(args, &spec)?;
                if read {
                    readbacks.push(MetalReadback::U64(idx, buf));
                }
            }
            (_, "i64") => {
                let buf = add_metal_vec::<i64>(args, &spec)?;
                if read {
                    readbacks.push(MetalReadback::I64(idx, buf));
                }
            }
            (_, "f64") => {
                let buf = add_metal_vec::<f64>(args, &spec)?;
                if read {
                    readbacks.push(MetalReadback::F64(idx, buf));
                }
            }
            (_, "bytes") => {
                let buf = if spec.get_dynamic("values").is_some() || spec.get_dynamic("value").is_some() {
                    args.add_bytes(bytes_or_zeros(&spec)?)?
                } else {
                    args.add_bytes_output(spec.get_dynamic("len").and_then(|len| len.as_uint()).unwrap_or(1))?
                };
                if read {
                    readbacks.push(MetalReadback::Bytes(idx, buf));
                }
            }
            _ => bail!("unsupported Metal arg {idx}: kind={kind:?}, type={ty:?}"),
        }
    }
    Ok(readbacks)
}

#[cfg(target_os = "macos")]
fn add_metal_vec<T>(args: &mut vm_metal::Args, spec: &Dynamic) -> Result<vm_metal::MetalBuffer<T>>
where
    T: bytemuck::NoUninit + bytemuck::AnyBitPattern + Default + Clone + TryFrom<Dynamic>,
    <T as TryFrom<Dynamic>>::Error: std::fmt::Debug,
{
    let values = values_or_zeros::<T>(spec)?;
    args.add_vec(values.len() as u64, |dst| dst.clone_from_slice(&values))
}

#[cfg(target_os = "macos")]
enum MetalReadback {
    U32(usize, vm_metal::MetalBuffer<u32>),
    I32(usize, vm_metal::MetalBuffer<i32>),
    F32(usize, vm_metal::MetalBuffer<f32>),
    U64(usize, vm_metal::MetalBuffer<u64>),
    I64(usize, vm_metal::MetalBuffer<i64>),
    F64(usize, vm_metal::MetalBuffer<f64>),
    Bytes(usize, vm_metal::MetalBuffer<u8>),
}

#[cfg(target_os = "macos")]
impl MetalReadback {
    fn read(self) -> Result<Dynamic> {
        match self {
            Self::U32(index, buf) => Ok(readback(index, "u32", Dynamic::from(buf.read()?.as_slice()))),
            Self::I32(index, buf) => Ok(readback(index, "i32", Dynamic::from(buf.read()?.as_slice()))),
            Self::F32(index, buf) => Ok(readback(index, "f32", Dynamic::from(buf.read()?.as_slice()))),
            Self::U64(index, buf) => Ok(readback(index, "u64", Dynamic::from(buf.read()?.as_slice()))),
            Self::I64(index, buf) => Ok(readback(index, "i64", Dynamic::from(buf.read()?.as_slice()))),
            Self::F64(index, buf) => Ok(readback(index, "f64", Dynamic::from(buf.read()?.as_slice()))),
            Self::Bytes(index, buf) => Ok(readback(index, "bytes", Dynamic::from(buf.read()?))),
        }
    }
}

fn arg_type(spec: &Dynamic) -> Result<String> {
    let ty = spec.get_dynamic("type").or_else(|| spec.get_dynamic("ty")).ok_or_else(|| anyhow!("GPU arg missing `type`"))?;
    let ty = ty.as_str().to_ascii_lowercase();
    if ty.is_empty() {
        bail!("GPU arg `type` must be a string");
    }
    Ok(ty)
}

fn arg_kind(spec: &Dynamic) -> String {
    spec.get_dynamic("kind")
        .map(|kind| kind.as_str().to_ascii_lowercase())
        .filter(|kind| !kind.is_empty())
        .unwrap_or_else(|| if spec.get_dynamic("value").is_some() && spec.get_dynamic("values").is_none() { "input".to_string() } else { "vec".to_string() })
}

fn scalar<T>(spec: &Dynamic) -> Result<T>
where
    T: TryFrom<Dynamic>,
    <T as TryFrom<Dynamic>>::Error: std::fmt::Debug,
{
    let value = spec.get_dynamic("value").ok_or_else(|| anyhow!("input GPU arg missing `value`"))?;
    T::try_from(value).map_err(|err| anyhow!("invalid scalar GPU arg: {err:?}"))
}

fn values_or_zeros<T>(spec: &Dynamic) -> Result<Vec<T>>
where
    T: Default + Clone + TryFrom<Dynamic>,
    <T as TryFrom<Dynamic>>::Error: std::fmt::Debug,
{
    if let Some(values) = spec.get_dynamic("values").or_else(|| spec.get_dynamic("value")) {
        let values = dynamic_to_vec::<T>(&values)?;
        if values.is_empty() {
            bail!("GPU vector arg cannot be empty");
        }
        return Ok(values);
    }
    let len = spec.get_dynamic("len").and_then(|len| len.as_uint()).unwrap_or(1);
    if len == 0 {
        bail!("GPU vector arg `len` must be greater than zero");
    }
    Ok(vec![T::default(); len as usize])
}

fn bytes_or_zeros(spec: &Dynamic) -> Result<Vec<u8>> {
    if let Some(values) = spec.get_dynamic("values").or_else(|| spec.get_dynamic("value")) {
        if let Some(bytes) = values.as_bytes() {
            return Ok(bytes.to_vec());
        }
        return dynamic_to_vec::<u8>(&values);
    }
    let len = spec.get_dynamic("len").and_then(|len| len.as_uint()).unwrap_or(1);
    if len == 0 {
        bail!("GPU bytes arg `len` must be greater than zero");
    }
    Ok(vec![0; len as usize])
}

fn dynamic_to_vec<T>(value: &Dynamic) -> Result<Vec<T>>
where
    T: TryFrom<Dynamic>,
    <T as TryFrom<Dynamic>>::Error: std::fmt::Debug,
{
    (0..value.len())
        .map(|idx| {
            let item = value.get_idx(idx).ok_or_else(|| anyhow!("missing vector item {idx}"))?;
            T::try_from(item).map_err(|err| anyhow!("invalid vector item {idx}: {err:?}"))
        })
        .collect()
}

fn readback(index: usize, ty: &str, values: Dynamic) -> Dynamic {
    map!("index"=> index as i64, "type"=> ty, "values"=> values)
}
