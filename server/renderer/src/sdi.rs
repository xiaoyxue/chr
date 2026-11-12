//! GPU conversion from the rendered frame to an SDI output pixel format.

use crate::{
    Gpu, OffscreenTarget,
    offscreen::{BoxError, Readback},
};

/// Converts an [`OffscreenTarget`] to 8-bit UYVY 4:2:2, the packed layout SDI cards such
/// as DeckLink accept as "8-bit YUV".
///
/// Each 4-byte group is `Cb, Y0, Cr, Y1` for two horizontally adjacent pixels; rows are
/// top-down and `width * 2` bytes long. Colors use BT.709 coefficients and 8-bit limited
/// range (Y 16-235, Cb/Cr 16-240). Chroma is the mean of each pixel pair.
/// `new` preserves the legacy triangle's stored sRGB approximation;
/// `from_bt709_texture` accepts correctly BT.709-encoded program RGB.
pub struct UyvyConverter {
    pipeline: wgpu::RenderPipeline,
    bind_group: wgpu::BindGroup,
    texture: wgpu::Texture,
    view: wgpu::TextureView,
    readback: Readback,
}

impl UyvyConverter {
    /// Packed texels: one `Rgba8Uint` texel holds one UYVY pixel pair.
    const FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8Uint;

    /// Binds `source`; convert only frames rendered into that same target.
    pub fn new(gpu: &Gpu, source: &OffscreenTarget) -> Result<Self, BoxError> {
        Self::from_view(gpu, source.encoded_view(), source.extent())
    }

    /// Converts caller-owned BT.709-encoded RGB, without another transfer operation.
    /// The texture must remain alive and immutable until `convert` completes.
    pub fn from_bt709_texture(gpu: &Gpu, source: &wgpu::Texture) -> Result<Self, BoxError> {
        if source.format() != wgpu::TextureFormat::Rgba8Unorm
            || source.dimension() != wgpu::TextureDimension::D2
            || source.depth_or_array_layers() != 1
            || source.sample_count() != 1
            || !source
                .usage()
                .contains(wgpu::TextureUsages::TEXTURE_BINDING)
        {
            return Err(
                "BT.709 conversion requires a sampleable single-sample RGBA8 texture".into(),
            );
        }
        Self::from_view(
            gpu,
            &source.create_view(&Default::default()),
            crate::coordinates::ImageExtent::new(source.width(), source.height())?,
        )
    }

    fn from_view(
        gpu: &Gpu,
        source: &wgpu::TextureView,
        extent: crate::coordinates::ImageExtent,
    ) -> Result<Self, BoxError> {
        if !extent.width().is_multiple_of(2) {
            return Err("UYVY 4:2:2 needs an even width".into());
        }
        let texture = gpu.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("UYVY output"),
            size: wgpu::Extent3d {
                width: extent.width() / 2,
                height: extent.height(),
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: Self::FORMAT,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
            view_formats: &[],
        });
        let layout = gpu
            .device
            .create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
                label: Some("UYVY source layout"),
                entries: &[wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: false },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                }],
            });
        let bind_group = gpu.device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("UYVY source"),
            layout: &layout,
            entries: &[wgpu::BindGroupEntry {
                binding: 0,
                resource: wgpu::BindingResource::TextureView(source),
            }],
        });
        let pipeline_layout = gpu
            .device
            .create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
                label: Some("UYVY layout"),
                bind_group_layouts: &[Some(&layout)],
                immediate_size: 0,
            });
        let shader = gpu
            .device
            .create_shader_module(wgpu::include_wgsl!("uyvy.wgsl"));
        let pipeline = gpu
            .device
            .create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some("UYVY pipeline"),
                layout: Some(&pipeline_layout),
                vertex: wgpu::VertexState {
                    module: &shader,
                    entry_point: Some("vs_main"),
                    compilation_options: Default::default(),
                    buffers: &[],
                },
                fragment: Some(wgpu::FragmentState {
                    module: &shader,
                    entry_point: Some("fs_main"),
                    compilation_options: Default::default(),
                    targets: &[Some(Self::FORMAT.into())],
                }),
                primitive: wgpu::PrimitiveState::default(),
                depth_stencil: None,
                multisample: wgpu::MultisampleState::default(),
                multiview_mask: None,
                cache: None,
            });
        Ok(Self {
            pipeline,
            bind_group,
            view: texture.create_view(&Default::default()),
            readback: Readback::new(gpu, extent.width() * 2, extent.height())?,
            texture,
        })
    }

    /// Converts the source's current contents and copies the UYVY rows into `out`.
    /// Submit the frame's rendering first; queue order makes this pass see it.
    pub fn convert(&self, gpu: &Gpu, out: &mut Vec<u8>) -> Result<(), BoxError> {
        let mut encoder = gpu.device.create_command_encoder(&Default::default());
        self.encode(&mut encoder);
        self.submit_readback(gpu, encoder, out)
    }

    /// Records conversion only, after source rendering in the same encoder.
    /// Does not submit, copy to staging, or wait. The source must stay alive.
    pub fn encode(&self, encoder: &mut wgpu::CommandEncoder) {
        {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("UYVY conversion"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &self.view,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                ..Default::default()
            });
            pass.set_pipeline(&self.pipeline);
            pass.set_bind_group(0, &self.bind_group, &[]);
            pass.draw(0..3, 0..1);
        }
    }

    /// Appends the pixel copy, submits prior rendering/conversion, then waits for
    /// CPU readback. Call `encode` first; finish before reusing this converter.
    pub fn submit_readback(
        &self,
        gpu: &Gpu,
        mut encoder: wgpu::CommandEncoder,
        out: &mut Vec<u8>,
    ) -> Result<(), BoxError> {
        self.readback.record_copy(&mut encoder, &self.texture);
        let submission = gpu.queue.submit([encoder.finish()]);
        self.readback.finish(gpu, submission, out)
    }
}
