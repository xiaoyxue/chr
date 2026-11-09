fn main() -> anyhow::Result<()> {
    let mut descriptor = wgpu::InstanceDescriptor::new_without_display_handle();
    descriptor.backends = wgpu::Backends::VULKAN;
    let instance = wgpu::Instance::new(descriptor);
    for adapter in pollster::block_on(instance.enumerate_adapters(wgpu::Backends::VULKAN)) {
        println!("{:?}", adapter.get_info());
    }
    Ok(())
}
