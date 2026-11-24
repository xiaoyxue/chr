/// A device and queue shared by all render targets.
pub struct Gpu {
    pub adapter: wgpu::Adapter,
    pub device: wgpu::Device,
    pub queue: wgpu::Queue,
}

impl Gpu {
    /// Honors wgpu environment overrides, including `WGPU_BACKEND`.
    pub fn instance() -> wgpu::Instance {
        wgpu::Instance::new(wgpu::InstanceDescriptor::new_without_display_handle_from_env())
    }

    pub async fn request(
        instance: &wgpu::Instance,
        surface: Option<&wgpu::Surface<'_>>,
    ) -> Result<Self, Box<dyn std::error::Error>> {
        let adapter = instance
            .request_adapter(&wgpu::RequestAdapterOptions {
                power_preference: wgpu::PowerPreference::HighPerformance,
                compatible_surface: surface,
                force_fallback_adapter: false,
                ..Default::default()
            })
            .await?;
        let info = adapter.get_info();
        log::info!(
            "using {:?} adapter {:?} ({:?})",
            info.backend,
            info.name,
            info.device_type
        );
        let (device, queue) = adapter
            .request_device(&wgpu::DeviceDescriptor {
                label: Some("chr device"),
                required_limits: Self::required_limits(&adapter),
                ..Default::default()
            })
            .await?;
        Ok(Self {
            adapter,
            device,
            queue,
        })
    }

    /// Limits every chr device requests: WebGPU defaults raised to the adapter's
    /// resolution limits, plus room for the 15 material maps (`material_maps.rs`) beside
    /// the lighting textures in one fragment stage. Desktop Vulkan and D3D12 adapters
    /// offer far more than 32.
    pub fn required_limits(adapter: &wgpu::Adapter) -> wgpu::Limits {
        let supported = adapter.limits();
        wgpu::Limits {
            max_sampled_textures_per_shader_stage: 32,
            max_samplers_per_shader_stage: 32,
            ..wgpu::Limits::default().using_resolution(supported)
        }
    }
}
