//! Explicit synthetic program bridge. Rendering and colour math stay in chr-renderer.
//! One GPU worker executes at a time; the scene owner and SDI clock never wait on it.

use crate::calibration::replay::VideoLens;
use crate::frame_submissions::{Execution, FrameSubmissions};
use crate::sdi::SdiInput;
use chr_common::scene_frame as w;
use chr_renderer::{
    Gpu, UyvyConverter,
    composition::{AlphaMode, FrameCompositor, ImageLayer, ImageView, Transfer},
};
use std::sync::Arc;

pub mod pitch;
pub mod synthetic;
pub mod video;
type Error = Box<dyn std::error::Error + Send + Sync>;

/// Reusable worker-local GPU storage, separate from immutable registered inputs.
/// Completed UYVY allocations move into the bounded scheduler/SDI/monitor chain.
pub struct ProgramRenderer {
    configuration_id: w::Id,
    runtime_id: w::Id,
    target: wgpu::Texture,
    converter: UyvyConverter,
    compositor: Option<(w::ShadowMode, FrameCompositor)>,
    timestamps: Option<wgpu::QuerySet>,
    video_lens: Option<Arc<VideoLens>>,
}

impl ProgramRenderer {
    pub fn new(gpu: &Gpu, config: &w::Configuration) -> Result<Self, Error> {
        validate_output(config)?;
        let e = &config.output_extent;
        if e.width.max(e.height) > gpu.device.limits().max_texture_dimension_2d {
            return Err("program extent exceeds GPU limits".into());
        }
        let target = gpu.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("BT.709 program RGB"),
            size: wgpu::Extent3d {
                width: e.width,
                height: e.height,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8Unorm,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::TEXTURE_BINDING,
            view_formats: &[],
        });
        let converter = UyvyConverter::from_bt709_texture(gpu, &target)?;
        Ok(Self {
            configuration_id: config.configuration_id.clone(),
            runtime_id: config.runtime_id.clone(),
            target,
            converter,
            compositor: None,
            timestamps: None,
            video_lens: None,
        })
    }

    pub fn set_video_lens(&mut self, lens: Option<Arc<VideoLens>>) {
        self.video_lens = lens;
    }

    /// Optional native diagnostics, disabled by default (no query allocation).
    /// Supply two timestamp queries from this device with both TIMESTAMP_QUERY
    /// and TIMESTAMP_QUERY_INSIDE_ENCODERS enabled, or None for the normal path.
    /// Each executed frame overwrites slots 0/1 around composition (including
    /// shadows) and UYVY conversion, before the pixel copy or CPU readback.
    /// Resolve/read externally before the next execution; there is no wire metric.
    pub fn set_timestamp_queries(
        &mut self,
        gpu: &Gpu,
        queries: Option<wgpu::QuerySet>,
    ) -> Result<(), Error> {
        if let Some(queries) = &queries
            && (!gpu.device.features().contains(
                wgpu::Features::TIMESTAMP_QUERY | wgpu::Features::TIMESTAMP_QUERY_INSIDE_ENCODERS,
            ) || queries.count() < 2
                || !matches!(queries.ty(), wgpu::QueryType::Timestamp))
        {
            return Err(
                "program timestamps require two timestamp queries and enabled features".into(),
            );
        }
        self.timestamps = queries;
        Ok(())
    }

    /// A successful conversion fences the entire queue, including input upload and CG.
    /// On a GPU wait error the execution is deliberately quarantined, never completed early.
    pub fn execute(&mut self, gpu: &Gpu, execution: Execution) -> Result<(), Error> {
        let result = self.render(gpu, &execution);
        match result {
            Ok(pixels) => execution
                .rendered(Arc::new(pixels))
                .map_err(|e| e.to_string().into()),
            Err(error) => {
                // A bounded failed wait cannot prove that external GPU work has stopped.
                // Dropping Execution is the scheduler's fail-closed physical quarantine.
                drop(execution);
                Err(error)
            }
        }
    }

    fn render(&mut self, gpu: &Gpu, execution: &Execution) -> Result<Vec<u8>, Error> {
        let snapshot = execution.scene.snapshot();
        if snapshot.runtime_id != self.runtime_id
            || snapshot.state.configuration_id != self.configuration_id
        {
            return Err("program worker cannot switch immutable configurations".into());
        }
        let settings = execution.scene.settings();
        // Configuration is immutable for this worker. Camera/light edits only
        // change the scene; MeshRenderer evicts stale resource IDs in prepare.
        // Recompiling the compositor on every effect made camera edits >100 ms.
        let shadow_mode = execution.scene.snapshot().state.shadow_mode;
        if self
            .compositor
            .as_ref()
            .is_none_or(|(old, _)| *old != shadow_mode)
        {
            let mut compositor = FrameCompositor::new(gpu, self.target.format(), settings.output)?;
            compositor.set_sample_count(gpu, settings.raster.sample_count)?;
            self.compositor = Some((shadow_mode, compositor));
        }
        let image = &execution.input.metadata().image;
        let format = match image.format {
            w::ImageFormat::Rgba8Unorm => wgpu::TextureFormat::Rgba8Unorm,
            w::ImageFormat::Bgra8Unorm => wgpu::TextureFormat::Bgra8Unorm,
            w::ImageFormat::Rgba16Float => wgpu::TextureFormat::Rgba16Float,
        };
        let texture = gpu.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("immutable synthetic input upload"),
            size: wgpu::Extent3d {
                width: image.extent.width,
                height: image.extent.height,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format,
            usage: wgpu::TextureUsages::COPY_DST | wgpu::TextureUsages::TEXTURE_BINDING,
            view_formats: &[],
        });
        gpu.queue.write_texture(
            texture.as_image_copy(),
            execution.input.pixels()?,
            wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(
                    image.extent.width
                        * if format == wgpu::TextureFormat::Rgba16Float {
                            8
                        } else {
                            4
                        },
                ),
                rows_per_image: Some(image.extent.height),
            },
            texture.size(),
        );
        let view = ImageView::new(
            &texture,
            match image.transfer {
                w::Transfer::Linear => Transfer::Linear,
                w::Transfer::Srgb => Transfer::Srgb,
                w::Transfer::Bt709 => Transfer::Bt709,
            },
            match image.alpha {
                w::Alpha::Straight => AlphaMode::Straight,
                w::Alpha::Premultiplied => AlphaMode::Premultiplied,
            },
        )?;
        let compositor = &mut self.compositor.as_mut().expect("installed compositor").1;
        let camera_model = self
            .video_lens
            .as_ref()
            .map(|lens| lens.model(&execution.provenance.input.frame))
            .transpose()?;
        compositor.prepare_model(
            gpu,
            execution.scene.scene(),
            &execution.camera,
            camera_model.unwrap_or(&chr_renderer::distortion::CameraModel::Pinhole),
            &[ImageLayer::new(&view, settings.input_stage)],
        )?;
        let mut encoder = gpu.device.create_command_encoder(&Default::default());
        if let Some(queries) = &self.timestamps {
            encoder.write_timestamp(queries, 0);
        }
        compositor.encode(&mut encoder, &self.target)?;
        self.converter.encode(&mut encoder);
        if let Some(queries) = &self.timestamps {
            encoder.write_timestamp(queries, 1);
        }
        let mut pixels = Vec::new();
        self.converter.submit_readback(gpu, encoder, &mut pixels)?;
        Ok(pixels)
    }

    pub fn drive(
        &mut self,
        gpu: &Gpu,
        scheduler: &FrameSubmissions,
        sdi: &mut SdiInput,
    ) -> Result<(), Error> {
        scheduler.handoff(sdi).map_err(|e| e.to_string())?;
        if let Some(execution) = scheduler.next_ready().map_err(|e| e.to_string())? {
            self.execute(gpu, execution)?;
        }
        scheduler.handoff(sdi).map_err(|e| e.to_string())?;
        Ok(())
    }
}

/// The existing output/encoder accepts opaque BT.709 and integral positive FPS only.
pub fn validate_output(config: &w::Configuration) -> Result<(), Error> {
    if config.output_transfer != w::OutputTransfer::Bt709
        || config.output_rate.numerator() != 1
        || !(1..=120).contains(&config.output_rate.denominator())
        || config.output_extent.width == 0
        || config.output_extent.height == 0
        || !config.output_extent.width.is_multiple_of(2)
        || !config.output_extent.height.is_multiple_of(2)
    {
        return Err("program port requires even BT.709 output at integral 1..120 FPS".into());
    }
    Ok(())
}
