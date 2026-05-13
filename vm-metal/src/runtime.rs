use anyhow::{Result, anyhow};
use bytemuck::{AnyBitPattern, NoUninit, cast_slice, cast_slice_mut};
use metal::{Buffer, CompileOptions, ComputePipelineState, Device, MTLResourceOptions, MTLSize};
use objc::rc::autoreleasepool;
use std::ffi::c_void;

use crate::Kernel;

pub struct Runtime {
    device: Device,
    queue: metal::CommandQueue,
    pipeline: Option<ComputePipelineState>,
    buffers: Vec<Buffer>,
    workgroup_size: [u32; 3],
}

impl Runtime {
    pub fn new() -> Result<Self> {
        let device = Device::system_default().ok_or_else(|| anyhow!("no Metal device available"))?;
        let queue = device.new_command_queue();
        Ok(Self { device, queue, pipeline: None, buffers: Vec::new(), workgroup_size: [1, 1, 1] })
    }

    pub fn args(&self) -> Args {
        Args::new(self.device.clone())
    }

    pub fn prepare(&mut self, source: &str, args: Args) -> Result<()> {
        self.prepare_with_workgroup_size(source, args, [1, 1, 1])
    }

    pub fn prepare_kernel(&mut self, kernel: &Kernel, args: Args) -> Result<()> {
        self.prepare_with_workgroup_size(kernel.metal.source(), args, kernel.workgroup_size)
    }

    pub fn prepare_with_workgroup_size(&mut self, source: &str, args: Args, workgroup_size: [u32; 3]) -> Result<()> {
        let options = CompileOptions::new();
        let library = self.device.new_library_with_source(source, &options).map_err(|err| anyhow!("failed to compile Metal shader: {err}"))?;
        let function = library.get_function("zust_main", None).map_err(|err| anyhow!("Metal shader must expose a `zust_main` kernel: {err}"))?;
        let pipeline = self.device.new_compute_pipeline_state_with_function(&function).map_err(|err| anyhow!("failed to create Metal compute pipeline: {err}"))?;
        self.pipeline = Some(pipeline);
        self.buffers = args.buffers;
        self.workgroup_size = workgroup_size;
        Ok(())
    }

    pub fn run(&self, groups: [u32; 3]) -> Result<()> {
        let pipeline = self.pipeline.as_ref().ok_or_else(|| anyhow!("runtime has not been prepared with a pipeline"))?;
        autoreleasepool(|| {
            let command_buffer = self.queue.new_command_buffer();
            let encoder = command_buffer.new_compute_command_encoder();
            encoder.set_compute_pipeline_state(pipeline);
            for (idx, buffer) in self.buffers.iter().enumerate() {
                encoder.set_buffer(idx as u64, Some(buffer), 0);
            }
            encoder.dispatch_thread_groups(
                MTLSize { width: groups[0] as u64, height: groups[1] as u64, depth: groups[2] as u64 },
                MTLSize { width: self.workgroup_size[0] as u64, height: self.workgroup_size[1] as u64, depth: self.workgroup_size[2] as u64 },
            );
            encoder.end_encoding();
            command_buffer.commit();
            command_buffer.wait_until_completed();
        });
        Ok(())
    }

    pub fn cleanup(&mut self) {
        self.pipeline = None;
        self.buffers.clear();
    }
}

impl Default for Runtime {
    fn default() -> Self {
        Self::new().expect("failed to initialize Metal runtime")
    }
}

pub struct Args {
    device: Device,
    buffers: Vec<Buffer>,
}

impl Args {
    fn new(device: Device) -> Self {
        Self { device, buffers: Vec::new() }
    }

    pub fn add_input<T: NoUninit>(&mut self, value: T) -> Result<MetalBuffer<T>> {
        let bytes = bytemuck::bytes_of(&value);
        let buffer = self.device.new_buffer_with_data(bytes.as_ptr().cast::<c_void>(), bytes.len() as u64, MTLResourceOptions::StorageModeShared);
        self.buffers.push(buffer.clone());
        Ok(MetalBuffer { buffer, len: 1, marker: std::marker::PhantomData })
    }

    pub fn add_output<T: NoUninit + AnyBitPattern>(&mut self) -> Result<MetalBuffer<T>> {
        let len = std::mem::size_of::<T>() as u64;
        let buffer = self.device.new_buffer(len, MTLResourceOptions::StorageModeShared);
        self.buffers.push(buffer.clone());
        Ok(MetalBuffer { buffer, len: 1, marker: std::marker::PhantomData })
    }

    pub fn add_vec<T: NoUninit + AnyBitPattern>(&mut self, len: u64, init: impl FnOnce(&mut [T])) -> Result<MetalBuffer<T>> {
        let byte_len = len.checked_mul(std::mem::size_of::<T>() as u64).ok_or_else(|| anyhow!("Metal buffer length overflow"))?;
        let buffer = self.device.new_buffer(byte_len, MTLResourceOptions::StorageModeShared);
        {
            let slice = unsafe { std::slice::from_raw_parts_mut(buffer.contents().cast::<T>(), len as usize) };
            init(slice);
            buffer.did_modify_range(metal::NSRange { location: 0, length: byte_len });
        }
        self.buffers.push(buffer.clone());
        Ok(MetalBuffer { buffer, len, marker: std::marker::PhantomData })
    }
}

pub struct MetalBuffer<T> {
    buffer: Buffer,
    len: u64,
    marker: std::marker::PhantomData<T>,
}

impl<T: NoUninit + AnyBitPattern> MetalBuffer<T> {
    pub fn read(&self) -> Result<Vec<T>> {
        let slice = unsafe { std::slice::from_raw_parts(self.buffer.contents().cast::<u8>(), self.len as usize * std::mem::size_of::<T>()) };
        Ok(cast_slice(slice).to_vec())
    }

    pub fn write(&self) -> Result<MetalBufferWrite<'_, T>> {
        let slice = unsafe { std::slice::from_raw_parts_mut(self.buffer.contents().cast::<u8>(), self.len as usize * std::mem::size_of::<T>()) };
        Ok(MetalBufferWrite { buffer: &self.buffer, bytes: slice, marker: std::marker::PhantomData })
    }
}

pub struct MetalBufferWrite<'a, T> {
    buffer: &'a Buffer,
    bytes: &'a mut [u8],
    marker: std::marker::PhantomData<T>,
}

impl<T: NoUninit + AnyBitPattern> std::ops::Deref for MetalBufferWrite<'_, T> {
    type Target = [T];

    fn deref(&self) -> &Self::Target {
        cast_slice(self.bytes)
    }
}

impl<T: NoUninit + AnyBitPattern> std::ops::DerefMut for MetalBufferWrite<'_, T> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        cast_slice_mut(self.bytes)
    }
}

impl<T> Drop for MetalBufferWrite<'_, T> {
    fn drop(&mut self) {
        self.buffer.did_modify_range(metal::NSRange { location: 0, length: self.bytes.len() as u64 });
    }
}
