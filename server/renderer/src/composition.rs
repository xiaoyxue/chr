//! Renderer-only image composition. Runtime owns frame identity, acquisition and
//! producer synchronization; all images and targets must belong to the same GPU.
//! Colour primaries are linear sRGB/Rec.709 D65; transfer tags do not convert gamuts.
use std::num::NonZeroU64;
use wgpu::util::DeviceExt;

use crate::{
    Gpu, MeshRenderer, PbrSettings, RenderSettings, ToneMap, camera::CameraView,
    coordinates::ImageExtent, offscreen::BoxError, scene::SceneView,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u32)]
pub enum Transfer {
    Linear = 0,
    Srgb = 1,
    Bt709 = 2,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u32)]
pub enum AlphaMode {
    Straight = 0,
    /// RGB is premultiplied in linear light, before applying the transfer function.
    Premultiplied = 1,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ImageStage {
    SceneBackground,
    SceneForeground,
    DisplayBackground,
    DisplayForeground,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ImageFit {
    Contain,
    Cover,
    Stretch,
}

/// Destination rectangle in native camera pixels, except display foregrounds,
/// whose rectangle is in output pixels. Negative origins allow explicit cropping.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PixelRect {
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
}

impl PixelRect {
    pub fn full(extent: ImageExtent) -> Self {
        Self {
            x: 0.0,
            y: 0.0,
            width: extent.width() as f32,
            height: extent.height() as f32,
        }
    }

    fn validate(self) -> Result<(), BoxError> {
        if ![self.x, self.y, self.width, self.height]
            .into_iter()
            .all(f32::is_finite)
            || self.width <= 0.0
            || self.height <= 0.0
        {
            return Err("image rectangle must be finite with positive dimensions".into());
        }
        Ok(())
    }
}

/// Descriptor-validated view of a caller-owned colour texture. Pixel contents
/// must be finite, nonnegative and obey the declared alpha/transfer convention.
/// The caller must retain external resource leases until submitted GPU work ends.
#[derive(Clone)]
pub struct ImageView {
    view: wgpu::TextureView,
    extent: ImageExtent,
    decode: Transfer,
    alpha: AlphaMode,
}

impl ImageView {
    pub fn new(
        texture: &wgpu::Texture,
        transfer: Transfer,
        alpha: AlphaMode,
    ) -> Result<Self, BoxError> {
        let size = texture.size();
        if texture.dimension() != wgpu::TextureDimension::D2
            || size.depth_or_array_layers != 1
            || texture.sample_count() != 1
            || !texture
                .usage()
                .contains(wgpu::TextureUsages::TEXTURE_BINDING)
        {
            return Err("image input must be a single-sample 2D sampleable texture".into());
        }
        let decode = validated_transfer(texture.format(), transfer)?;
        Ok(Self {
            view: texture.create_view(&Default::default()),
            extent: ImageExtent::new(size.width, size.height)?,
            decode,
            alpha,
        })
    }

    pub fn extent(&self) -> ImageExtent {
        self.extent
    }
}

#[derive(Clone, Copy)]
pub struct ImageLayer<'a> {
    pub image: &'a ImageView,
    pub stage: ImageStage,
    pub rect: PixelRect,
    pub opacity: f32,
}

impl<'a> ImageLayer<'a> {
    pub fn new(image: &'a ImageView, stage: ImageStage) -> Self {
        Self {
            image,
            stage,
            rect: PixelRect::full(image.extent),
            opacity: 1.0,
        }
    }
}

/// Fixed display-output configuration. Scene layers and CG share one exposure/
/// tone-map; display-referred layers bypass it. Clear colour is straight linear.
#[derive(Clone, Copy, Debug)]
pub struct CompositionOutput {
    pub extent: ImageExtent,
    pub fit: ImageFit,
    pub transfer: Transfer,
    pub alpha: AlphaMode,
    pub clear_color: [f32; 4],
    pub pbr: PbrSettings,
    /// Additional scene/display colour canvases; excludes caller inputs and mesh resources.
    pub max_intermediate_bytes: u64,
}

impl CompositionOutput {
    pub fn new(extent: ImageExtent) -> Self {
        Self {
            extent,
            fit: ImageFit::Contain,
            transfer: Transfer::Srgb,
            alpha: AlphaMode::Straight,
            clear_color: [0.0, 0.0, 0.0, 1.0],
            pbr: PbrSettings::default(),
            max_intermediate_bytes: 256 * 1024 * 1024,
        }
    }
}

fn validated_transfer(
    format: wgpu::TextureFormat,
    transfer: Transfer,
) -> Result<Transfer, BoxError> {
    use wgpu::TextureFormat as F;
    match format {
        F::Rgba8UnormSrgb | F::Bgra8UnormSrgb if transfer == Transfer::Srgb => Ok(Transfer::Linear),
        F::Rgba8Unorm | F::Bgra8Unorm => Ok(transfer),
        F::Rgba16Float | F::Rgba32Float if transfer == Transfer::Linear => Ok(Transfer::Linear),
        _ => Err("unsupported image/output colour format or transfer pairing".into()),
    }
}

fn camera_rect(native: ImageExtent, output: ImageExtent, fit: ImageFit) -> PixelRect {
    let w = output.width() as f32;
    let h = output.height() as f32;
    if fit == ImageFit::Stretch {
        return PixelRect::full(output);
    }
    let sx = w / native.width() as f32;
    let sy = h / native.height() as f32;
    let scale = if fit == ImageFit::Contain {
        sx.min(sy)
    } else {
        sx.max(sy)
    };
    let width = native.width() as f32 * scale;
    let height = native.height() as f32 * scale;
    PixelRect {
        x: (w - width) * 0.5,
        y: (h - height) * 0.5,
        width,
        height,
    }
}

fn fit_rect(rect: PixelRect, native: ImageExtent, viewport: PixelRect) -> PixelRect {
    let sx = viewport.width / native.width() as f32;
    let sy = viewport.height / native.height() as f32;
    PixelRect {
        x: viewport.x + rect.x * sx,
        y: viewport.y + rect.y * sy,
        width: rect.width * sx,
        height: rect.height * sy,
    }
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct LayerUniform {
    rect: [f32; 4],
    canvas: [f32; 4],
    options: [u32; 4],
    output: [u32; 4],
}

struct ImagePass {
    stage: ImageStage,
    group: wgpu::BindGroup,
}

struct ImagePipeline {
    layout: wgpu::BindGroupLayout,
    blend: wgpu::RenderPipeline,
    output: wgpu::RenderPipeline,
}

impl ImagePipeline {
    fn new(device: &wgpu::Device, format: wgpu::TextureFormat) -> Self {
        let layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("typed image layer"),
            entries: &[
                wgpu::BindGroupLayoutEntry {
                    binding: 0,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Texture {
                        sample_type: wgpu::TextureSampleType::Float { filterable: false },
                        view_dimension: wgpu::TextureViewDimension::D2,
                        multisampled: false,
                    },
                    count: None,
                },
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: NonZeroU64::new(64),
                    },
                    count: None,
                },
            ],
        });
        let pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("typed image layer"),
            bind_group_layouts: &[Some(&layout)],
            immediate_size: 0,
        });
        let shader = device.create_shader_module(wgpu::include_wgsl!("composition.wgsl"));
        let pipeline = |format, blend| {
            device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
                label: Some("linear image composition"),
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
                    targets: &[Some(wgpu::ColorTargetState {
                        format,
                        blend,
                        write_mask: wgpu::ColorWrites::ALL,
                    })],
                }),
                primitive: Default::default(),
                depth_stencil: None,
                multisample: Default::default(),
                multiview_mask: None,
                cache: None,
            })
        };
        let blend = pipeline(
            wgpu::TextureFormat::Rgba16Float,
            Some(wgpu::BlendState::PREMULTIPLIED_ALPHA_BLENDING),
        );
        let output = pipeline(format, None);
        Self {
            layout,
            blend,
            output,
        }
    }

    fn group(
        &self,
        device: &wgpu::Device,
        image: &ImageView,
        data: LayerUniform,
    ) -> wgpu::BindGroup {
        let buffer = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
            label: Some("image layer parameters"),
            contents: bytemuck::bytes_of(&data),
            usage: wgpu::BufferUsages::UNIFORM,
        });
        device.create_bind_group(&wgpu::BindGroupDescriptor {
            label: Some("image layer"),
            layout: &self.layout,
            entries: &[
                wgpu::BindGroupEntry {
                    binding: 0,
                    resource: wgpu::BindingResource::TextureView(&image.view),
                },
                wgpu::BindGroupEntry {
                    binding: 1,
                    resource: buffer.as_entire_binding(),
                },
            ],
        })
    }
}

fn layer_uniform(
    image: &ImageView,
    rect: PixelRect,
    canvas: ImageExtent,
    opacity: f32,
    kind: u32,
    output: CompositionOutput,
    effective_transfer: Transfer,
) -> LayerUniform {
    LayerUniform {
        rect: [rect.x, rect.y, rect.width, rect.height],
        canvas: [
            canvas.width() as f32,
            canvas.height() as f32,
            output.pbr.exposure,
            opacity,
        ],
        options: [
            kind,
            image.decode as u32,
            image.alpha as u32,
            match output.pbr.tone_map {
                ToneMap::Reinhard => 0,
                ToneMap::Aces => 1,
            },
        ],
        output: [effective_transfer as u32, output.alpha as u32, 0, 0],
    }
}

fn canvas(device: &wgpu::Device, extent: ImageExtent) -> ImageView {
    let texture = device.create_texture(&wgpu::TextureDescriptor {
        label: Some("composition canvas"),
        size: wgpu::Extent3d {
            width: extent.width(),
            height: extent.height(),
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Rgba16Float,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING,
        view_formats: &[],
    });
    ImageView {
        view: texture.create_view(&Default::default()),
        extent,
        decode: Transfer::Linear,
        alpha: AlphaMode::Premultiplied,
    }
}

fn clear(encoder: &mut wgpu::CommandEncoder, view: &wgpu::TextureView, color: [f32; 4]) {
    let _pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
        label: Some("composition clear"),
        color_attachments: &[Some(wgpu::RenderPassColorAttachment {
            view,
            depth_slice: None,
            resolve_target: None,
            ops: wgpu::Operations {
                load: wgpu::LoadOp::Clear(wgpu::Color {
                    r: f64::from(color[0] * color[3]),
                    g: f64::from(color[1] * color[3]),
                    b: f64::from(color[2] * color[3]),
                    a: f64::from(color[3]),
                }),
                store: wgpu::StoreOp::Store,
            },
        })],
        ..Default::default()
    });
}

fn draw(
    encoder: &mut wgpu::CommandEncoder,
    view: &wgpu::TextureView,
    pipeline: &wgpu::RenderPipeline,
    group: &wgpu::BindGroup,
) {
    let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
        label: Some("image composition"),
        color_attachments: &[Some(wgpu::RenderPassColorAttachment {
            view,
            depth_slice: None,
            resolve_target: None,
            ops: wgpu::Operations {
                load: wgpu::LoadOp::Load,
                store: wgpu::StoreOp::Store,
            },
        })],
        ..Default::default()
    });
    pass.set_pipeline(pipeline);
    pass.set_bind_group(0, group, &[]);
    pass.draw(0..6, 0..1);
}

/// Composes resolved native-camera CG with typed image layers into a fixed target.
/// Prepare, encode and submit before preparing the next frame. Producer writes
/// and external resource leases remain the caller's responsibility until GPU completion.
pub struct FrameCompositor {
    renderer: MeshRenderer,
    pipeline: ImagePipeline,
    config: CompositionOutput,
    format: wgpu::TextureFormat,
    transfer: Transfer,
    scene_canvas: Option<ImageView>,
    display_canvas: ImageView,
    layers: Vec<ImagePass>,
    cg: Option<wgpu::BindGroup>,
    tone: Option<wgpu::BindGroup>,
    output: wgpu::BindGroup,
}

impl FrameCompositor {
    pub fn new(
        gpu: &Gpu,
        format: wgpu::TextureFormat,
        config: CompositionOutput,
    ) -> Result<Self, BoxError> {
        let transfer = validated_transfer(format, config.transfer)?;
        if !gpu
            .adapter
            .get_texture_format_features(format)
            .allowed_usages
            .contains(wgpu::TextureUsages::RENDER_ATTACHMENT)
            || config.extent.width().max(config.extent.height())
                > gpu.device.limits().max_texture_dimension_2d
            || !config
                .clear_color
                .into_iter()
                .all(|v| (0.0..=1.0).contains(&v))
            || !config.pbr.exposure.is_finite()
            || config.pbr.exposure < 0.0
        {
            return Err("invalid composition output configuration".into());
        }
        let display_bytes =
            u64::from(config.extent.width()) * u64::from(config.extent.height()) * 8;
        if display_bytes > config.max_intermediate_bytes {
            return Err("composition canvases exceed the configured memory budget".into());
        }
        let pipeline = ImagePipeline::new(&gpu.device, format);
        let display_canvas = canvas(&gpu.device, config.extent);
        let output = pipeline.group(
            &gpu.device,
            &display_canvas,
            layer_uniform(
                &display_canvas,
                PixelRect::full(config.extent),
                config.extent,
                1.0,
                2,
                config,
                transfer,
            ),
        );
        Ok(Self {
            renderer: MeshRenderer::new(&gpu.device, wgpu::TextureFormat::Rgba16Float),
            pipeline,
            config,
            format,
            transfer,
            scene_canvas: None,
            display_canvas,
            layers: Vec::new(),
            cg: None,
            tone: None,
            output,
        })
    }

    /// Composition uses shaded CG; display-space diagnostics remain on MeshRenderer.
    pub fn set_sample_count(&mut self, gpu: &Gpu, sample_count: u32) -> Result<(), BoxError> {
        self.renderer.set_render_settings(
            gpu,
            RenderSettings {
                sample_count,
                ..Default::default()
            },
        )?;
        self.cg = None;
        Ok(())
    }

    pub fn prepare(
        &mut self,
        gpu: &Gpu,
        scene: &SceneView,
        camera: &dyn CameraView,
        layers: &[ImageLayer<'_>],
    ) -> Result<(), BoxError> {
        self.cg = None;
        let native = camera.extent();
        let bytes = (u64::from(native.width()) * u64::from(native.height())
            + u64::from(self.config.extent.width()) * u64::from(self.config.extent.height()))
            * 8;
        if bytes > self.config.max_intermediate_bytes {
            return Err("composition canvases exceed the configured memory budget".into());
        }
        let viewport = camera_rect(native, self.config.extent, self.config.fit);
        for layer in layers {
            layer.rect.validate()?;
            if !(0.0..=1.0).contains(&layer.opacity) {
                return Err("image opacity must be in [0,1]".into());
            }
            if layer.stage == ImageStage::DisplayBackground {
                fit_rect(layer.rect, native, viewport).validate()?;
            }
        }
        self.renderer
            .prepare_pbr(gpu, scene, camera, self.config.pbr)?;
        if self
            .scene_canvas
            .as_ref()
            .is_none_or(|image| image.extent != native)
        {
            self.scene_canvas = Some(canvas(&gpu.device, native));
        }
        let scene_canvas = self.scene_canvas.as_ref().expect("prepared native canvas");
        let cg = ImageView {
            view: self
                .renderer
                .linear_view()
                .ok_or("no prepared shaded HDR view")?
                .clone(),
            extent: native,
            decode: Transfer::Linear,
            alpha: AlphaMode::Premultiplied,
        };
        self.cg = Some(self.pipeline.group(
            &gpu.device,
            &cg,
            layer_uniform(
                &cg,
                PixelRect::full(native),
                native,
                1.0,
                0,
                self.config,
                self.transfer,
            ),
        ));
        self.tone = Some(self.pipeline.group(
            &gpu.device,
            scene_canvas,
            layer_uniform(
                scene_canvas,
                viewport,
                self.config.extent,
                1.0,
                1,
                self.config,
                self.transfer,
            ),
        ));
        self.layers = layers
            .iter()
            .map(|layer| {
                let (rect, extent) = match layer.stage {
                    ImageStage::SceneBackground | ImageStage::SceneForeground => {
                        (layer.rect, native)
                    }
                    ImageStage::DisplayBackground => {
                        (fit_rect(layer.rect, native, viewport), self.config.extent)
                    }
                    ImageStage::DisplayForeground => (layer.rect, self.config.extent),
                };
                ImagePass {
                    stage: layer.stage,
                    group: self.pipeline.group(
                        &gpu.device,
                        layer.image,
                        layer_uniform(
                            layer.image,
                            rect,
                            extent,
                            layer.opacity,
                            0,
                            self.config,
                            self.transfer,
                        ),
                    ),
                }
            })
            .collect();
        Ok(())
    }

    /// The output is the whole single-mip, single-sample caller texture, not an
    /// implicit crop or alternate-format view. Queue submission/completion is external.
    pub fn encode(
        &mut self,
        encoder: &mut wgpu::CommandEncoder,
        target: &wgpu::Texture,
    ) -> Result<(), BoxError> {
        if target.format() != self.format
            || target.size().width != self.config.extent.width()
            || target.size().height != self.config.extent.height()
            || target.size().depth_or_array_layers != 1
            || target.dimension() != wgpu::TextureDimension::D2
            || target.sample_count() != 1
            || target.mip_level_count() != 1
            || !target
                .usage()
                .contains(wgpu::TextureUsages::RENDER_ATTACHMENT)
        {
            return Err("composition target does not match its fixed output configuration".into());
        }
        let cg = self
            .cg
            .as_ref()
            .ok_or("composition must be prepared before encoding")?;
        let scene = self.scene_canvas.as_ref().expect("prepared native canvas");
        let tone = self.tone.as_ref().expect("prepared scene output transform");
        self.renderer.encode_linear(encoder);
        clear(encoder, &scene.view, [0.0; 4]);
        for layer in self
            .layers
            .iter()
            .filter(|layer| layer.stage == ImageStage::SceneBackground)
        {
            draw(encoder, &scene.view, &self.pipeline.blend, &layer.group);
        }
        draw(encoder, &scene.view, &self.pipeline.blend, cg);
        for layer in self
            .layers
            .iter()
            .filter(|layer| layer.stage == ImageStage::SceneForeground)
        {
            draw(encoder, &scene.view, &self.pipeline.blend, &layer.group);
        }
        clear(encoder, &self.display_canvas.view, self.config.clear_color);
        for layer in self
            .layers
            .iter()
            .filter(|layer| layer.stage == ImageStage::DisplayBackground)
        {
            draw(
                encoder,
                &self.display_canvas.view,
                &self.pipeline.blend,
                &layer.group,
            );
        }
        draw(
            encoder,
            &self.display_canvas.view,
            &self.pipeline.blend,
            tone,
        );
        for layer in self
            .layers
            .iter()
            .filter(|layer| layer.stage == ImageStage::DisplayForeground)
        {
            draw(
                encoder,
                &self.display_canvas.view,
                &self.pipeline.blend,
                &layer.group,
            );
        }
        let view = target.create_view(&Default::default());
        draw(encoder, &view, &self.pipeline.output, &self.output);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn output_fitting_preserves_native_pixel_coordinates() {
        let native = ImageExtent::new(4, 2).unwrap();
        let output = ImageExtent::new(8, 8).unwrap();
        let contain = camera_rect(native, output, ImageFit::Contain);
        assert_eq!(
            contain,
            PixelRect {
                x: 0.0,
                y: 2.0,
                width: 8.0,
                height: 4.0
            }
        );
        assert_eq!(
            camera_rect(native, output, ImageFit::Cover),
            PixelRect {
                x: -4.0,
                y: 0.0,
                width: 16.0,
                height: 8.0
            }
        );
        assert_eq!(
            camera_rect(native, output, ImageFit::Stretch),
            PixelRect::full(output)
        );
        assert_eq!(
            fit_rect(
                PixelRect {
                    x: 1.0,
                    y: 0.0,
                    width: 1.0,
                    height: 1.0
                },
                native,
                contain
            ),
            PixelRect {
                x: 2.0,
                y: 2.0,
                width: 2.0,
                height: 2.0
            }
        );
    }
}
