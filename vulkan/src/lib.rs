use anyhow::{Context, Result, anyhow};
use std::{error::Error, sync::Arc};
use vulkano::{
    buffer::{Buffer, BufferContents, BufferCreateInfo, BufferUsage, Subbuffer},
    command_buffer::{AutoCommandBufferBuilder, CommandBufferUsage, allocator::StandardCommandBufferAllocator},
    descriptor_set::{DescriptorSet, WriteDescriptorSet, allocator::StandardDescriptorSetAllocator},
    device::{Device, DeviceCreateInfo, Queue, QueueCreateInfo, QueueFlags},
    instance::{Instance, InstanceCreateFlags, InstanceCreateInfo},
    memory::allocator::{AllocationCreateInfo, FreeListAllocator, GenericMemoryAllocator, MemoryTypeFilter, StandardMemoryAllocator},
    pipeline::{ComputePipeline, Pipeline, PipelineBindPoint, PipelineLayout, PipelineShaderStageCreateInfo, compute::ComputePipelineCreateInfo, layout::PipelineDescriptorSetLayoutCreateInfo},
    shader::{ShaderModule, ShaderModuleCreateInfo},
    sync::{self, GpuFuture},
};

pub struct Runtime {
    device: Arc<Device>,
    queue: Arc<Queue>,
    pipeline: Option<Arc<ComputePipeline>>,
    set: Option<Arc<DescriptorSet>>,
    command_buffer_allocator: Arc<StandardCommandBufferAllocator>,
}

impl Runtime {
    pub fn new() -> Result<Self> {
        let library = vulkano::VulkanLibrary::new().context("no local Vulkan library/DLL")?;
        let instance = Instance::new(library, InstanceCreateInfo { flags: InstanceCreateFlags::ENUMERATE_PORTABILITY, max_api_version: Some(vulkano::Version::V1_3), ..Default::default() })
            .context("failed to create Vulkan instance")?;

        let physical_device = instance.enumerate_physical_devices().context("failed to enumerate Vulkan physical devices")?.next().ok_or_else(|| anyhow!("no Vulkan physical device available"))?;

        let queue_family_index = physical_device
            .queue_family_properties()
            .iter()
            .enumerate()
            .position(|(_, props)| props.queue_flags.contains(QueueFlags::COMPUTE) || props.queue_flags.contains(QueueFlags::GRAPHICS))
            .ok_or_else(|| anyhow!("no compute-capable Vulkan queue family found"))? as u32;

        let features = physical_device.supported_features().clone();

        let (device, mut queues) =
            Device::new(physical_device, DeviceCreateInfo { enabled_features: features, queue_create_infos: vec![QueueCreateInfo { queue_family_index, ..Default::default() }], ..Default::default() })
                .context("failed to create Vulkan device")?;
        let queue = queues.next().ok_or_else(|| anyhow!("Vulkan queue creation returned no queue"))?;
        let command_buffer_allocator = Arc::new(StandardCommandBufferAllocator::new(device.clone(), Default::default()));

        Ok(Self { device, queue, pipeline: None, set: None, command_buffer_allocator })
    }

    pub fn args(&self) -> Args {
        Args::new(self.device.clone())
    }

    pub fn prepare(&mut self, shader_words: &[u32], args: Args) -> Result<()> {
        let shader = unsafe { ShaderModule::new(self.device.clone(), ShaderModuleCreateInfo::new(shader_words))? };
        let entry_point = shader.entry_point("main").ok_or_else(|| anyhow!("SPIR-V shader must expose a `main` entry point"))?;
        let stage = PipelineShaderStageCreateInfo::new(entry_point);
        let layout = PipelineLayout::new(self.device.clone(), PipelineDescriptorSetLayoutCreateInfo::from_stages([&stage]).into_pipeline_layout_create_info(self.device.clone())?)?;
        let pipeline = ComputePipeline::new(self.device.clone(), None, ComputePipelineCreateInfo::stage_layout(stage, layout)).map_err(|err| {
            let mut message = format!("failed to create Vulkan compute pipeline: {err}");
            if let Some(source) = err.source() {
                message.push_str(&format!("; caused by: {source}"));
            }
            anyhow!(message)
        })?;

        let descriptor_set_allocator = Arc::new(StandardDescriptorSetAllocator::new(self.device.clone(), Default::default()));
        let set_layout = pipeline.layout().set_layouts().first().ok_or_else(|| anyhow!("compute pipeline has no descriptor set layout"))?.clone();
        self.set = Some(DescriptorSet::new(descriptor_set_allocator, set_layout, args.set_writers, [])?);
        self.pipeline = Some(pipeline);
        Ok(())
    }

    pub fn run(&self, groups: [u32; 3]) -> Result<()> {
        let pipeline = self.pipeline.as_ref().ok_or_else(|| anyhow!("runtime has not been prepared with a pipeline"))?;
        let set = self.set.as_ref().ok_or_else(|| anyhow!("runtime has not been prepared with descriptor args"))?;

        let mut builder = AutoCommandBufferBuilder::primary(self.command_buffer_allocator.clone(), self.queue.queue_family_index(), CommandBufferUsage::OneTimeSubmit)?;
        builder.bind_pipeline_compute(pipeline.clone())?;
        builder.bind_descriptor_sets(PipelineBindPoint::Compute, pipeline.layout().clone(), 0, set.clone())?;
        unsafe {
            builder.dispatch(groups)?;
        }
        let command_buffer = builder.build()?;
        let future = sync::now(self.device.clone()).then_execute(self.queue.clone(), command_buffer)?.then_signal_fence_and_flush()?;
        future.wait(None)?;
        Ok(())
    }

    pub fn cleanup(&mut self) {
        self.pipeline = None;
        self.set = None;
    }
}

impl Default for Runtime {
    fn default() -> Self {
        Self::new().expect("failed to initialize Vulkan runtime")
    }
}

impl Drop for Runtime {
    fn drop(&mut self) {
        self.cleanup();
    }
}

pub struct Args {
    allocator: Arc<GenericMemoryAllocator<FreeListAllocator>>,
    set_writers: Vec<WriteDescriptorSet>,
}

impl Args {
    pub fn new(device: Arc<Device>) -> Self {
        Self { allocator: Arc::new(StandardMemoryAllocator::new_default(device)), set_writers: Vec::new() }
    }

    pub fn add_input<T: BufferContents>(&mut self, value: T) -> Result<Subbuffer<T>> {
        let idx = self.set_writers.len() as u32;
        let buf = Buffer::from_data(
            self.allocator.clone(),
            BufferCreateInfo { usage: BufferUsage::STORAGE_BUFFER | BufferUsage::TRANSFER_SRC, ..Default::default() },
            AllocationCreateInfo { memory_type_filter: MemoryTypeFilter::PREFER_DEVICE | MemoryTypeFilter::HOST_SEQUENTIAL_WRITE, ..Default::default() },
            value,
        )?;
        self.set_writers.push(WriteDescriptorSet::buffer(idx, buf.clone()));
        Ok(buf)
    }

    pub fn add_output<T: BufferContents>(&mut self) -> Result<Subbuffer<T>> {
        let idx = self.set_writers.len() as u32;
        let buf = Buffer::new_sized::<T>(
            self.allocator.clone(),
            BufferCreateInfo { usage: BufferUsage::STORAGE_BUFFER | BufferUsage::TRANSFER_DST, ..Default::default() },
            AllocationCreateInfo { memory_type_filter: MemoryTypeFilter::PREFER_DEVICE | MemoryTypeFilter::HOST_RANDOM_ACCESS, ..Default::default() },
        )?;
        self.set_writers.push(WriteDescriptorSet::buffer(idx, buf.clone()));
        Ok(buf)
    }

    pub fn add_vec<T: BufferContents>(&mut self, len: u64, init: impl FnOnce(&mut [T])) -> Result<Subbuffer<[T]>> {
        let idx = self.set_writers.len() as u32;
        let buf = Buffer::new_slice::<T>(
            self.allocator.clone(),
            BufferCreateInfo { usage: BufferUsage::STORAGE_BUFFER | BufferUsage::TRANSFER_SRC | BufferUsage::TRANSFER_DST, ..Default::default() },
            AllocationCreateInfo { memory_type_filter: MemoryTypeFilter::PREFER_DEVICE | MemoryTypeFilter::HOST_RANDOM_ACCESS, ..Default::default() },
            len,
        )?;
        {
            let mut mapping = buf.write()?;
            init(&mut mapping);
        }
        self.set_writers.push(WriteDescriptorSet::buffer(idx, buf.clone()));
        Ok(buf)
    }
}
