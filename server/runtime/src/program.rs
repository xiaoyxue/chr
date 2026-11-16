//! Explicit synthetic program bridge. Rendering and colour math stay in chr-renderer.
//! One GPU worker executes at a time; the scene owner and SDI clock never wait on it.

use crate::calibration::{replay::VideoLens, tracking::rows3};
use crate::frame_submissions::{Execution, FrameSubmissions};
use crate::retained::BoundScene;
use crate::sdi::SdiInput;
use chr_common::scene_frame as w;
use chr_renderer::{
    Gpu, TextureReadback, UyvyConverter,
    camera::Camera,
    composition::{AlphaMode, FrameCompositor, ImageLayer, ImageView, Transfer},
    coordinates::ImageExtent,
    distortion::{CameraModel, FrameWarp, LensDistortion, RadialDistortion},
};
use std::sync::Arc;

pub mod assets;
pub mod description;
pub mod pitch;
pub mod spec;
pub mod still;
pub mod synthetic;
pub mod video;
type Error = Box<dyn std::error::Error + Send + Sync>;

/// Largest camera native extent and program output size: 8K UHD.
pub const MAX_EXTENT: [u32; 2] = [7680, 4320];
/// The synthetic and software-video demonstrations' own limit.
pub const DEMO_MAX_EXTENT: [u32; 2] = [1280, 720];

/// True for a nonzero, even extent within `max`.
pub fn fits(width: u32, height: u32, max: [u32; 2]) -> bool {
    (1..=max[0]).contains(&width)
        && (1..=max[1]).contains(&height)
        && width.is_multiple_of(2)
        && height.is_multiple_of(2)
}

/// Composition budget of a program with a lens: the native and output canvases
/// plus a guard-banded ideal canvas of up to twice the native area, all
/// RGBA16Float, and never below the renderer's 256 MiB default.
pub fn composition_budget(native: w::Extent, output: w::Extent) -> u64 {
    let pixels = |e: w::Extent| u64::from(e.width) * u64::from(e.height);
    ((3 * pixels(native) + pixels(output)) * 8).max(256 * 1024 * 1024)
}

/// The parts of one program frame: a bound scene, its camera and the input image.
pub struct Frame<'a> {
    pub scene: &'a BoundScene,
    pub camera: &'a Camera,
    pub image: &'a w::ImageDescription,
    /// Tightly packed pixels as `image` describes them.
    pub pixels: &'a [u8],
    /// Native frame identity; selects the lens model of a calibrated video scene.
    pub key: &'a w::FrameKey,
    /// The frame's own warp of a tracked program (`frame_warp` of its `ti_c` geometry).
    pub frame_warp: Option<[[f64; 3]; 3]>,
    /// Media time of the frame on its timeline; animates flags reproducibly.
    pub seconds: f64,
}

impl<'a> Frame<'a> {
    pub fn of(execution: &'a Execution) -> Result<Self, Error> {
        Ok(Self {
            scene: &execution.scene,
            camera: &execution.camera,
            image: &execution.input.metadata().image,
            pixels: execution.input.pixels()?,
            key: &execution.provenance.input.frame,
            frame_warp: frame_warp(&execution.provenance.input.geometry),
            seconds: media_seconds(
                execution.input.metadata().timeline.time_base,
                execution.input.metadata().position.ticks.get(),
            ),
        })
    }
}

/// Seconds of `ticks` on a timeline with `time_base` seconds per tick.
pub fn media_seconds(time_base: w::TimeBase, ticks: u64) -> f64 {
    ticks as f64 * f64::from(time_base.numerator()) / f64::from(time_base.denominator())
}

/// The warp a frame's geometry carries, if any.
pub fn frame_warp(geometry: &w::FrameGeometry) -> Option<[[f64; 3]; 3]> {
    match geometry {
        w::FrameGeometry::TiC {
            frame_warp: Some(rows),
            ..
        } => Some(rows3(rows)),
        _ => None,
    }
}

/// Reusable worker-local GPU storage, separate from immutable registered inputs.
/// Completed UYVY allocations move into the bounded scheduler/SDI/program-tap chain.
pub struct ProgramRenderer {
    configuration_id: w::Id,
    runtime_id: w::Id,
    target: wgpu::Texture,
    converter: UyvyConverter,
    compositor: Option<(w::ShadowMode, FrameCompositor)>,
    timestamps: Option<wgpu::QuerySet>,
    video_lens: Option<Arc<VideoLens>>,
    tracked_lens: Option<RadialDistortion>,
    rgba_readback: Option<TextureReadback>,
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
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT
                | wgpu::TextureUsages::TEXTURE_BINDING
                | wgpu::TextureUsages::COPY_SRC,
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
            tracked_lens: None,
            rgba_readback: None,
        })
    }

    pub fn set_video_lens(&mut self, lens: Option<Arc<VideoLens>>) {
        self.video_lens = lens;
    }

    /// The startup lens of a tracked program; each frame supplies its own warp.
    pub fn set_tracked_lens(&mut self, lens: Option<RadialDistortion>) {
        self.tracked_lens = lens;
    }

    /// Both lens sources of a program, as its installation provides them.
    pub fn set_lenses(&mut self, program: &synthetic::Synthetic) {
        self.set_video_lens(program.video_lens.clone());
        self.set_tracked_lens(program.tracking.as_ref().map(|tracked| tracked.lens));
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
        self.render_frame(gpu, &Frame::of(execution)?)
    }

    /// Composes one frame from explicit parts and returns its UYVY output, as
    /// [`Self::execute`] does for an admitted execution. A render worker rebuilds
    /// these parts from a request.
    pub fn render_frame(&mut self, gpu: &Gpu, frame: &Frame) -> Result<Vec<u8>, Error> {
        let _input = self.prepare(gpu, frame)?;
        let compositor = &mut self.compositor.as_mut().expect("installed compositor").1;
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

    /// Composes `execution` like [`Self::execute`] but returns the BT.709 RGBA8
    /// program image (tightly packed, top-down) instead of handing UYVY to the port.
    /// The execution is reported failed: nothing of it is transmitted.
    pub fn render_rgba(&mut self, gpu: &Gpu, execution: Execution) -> Result<Vec<u8>, Error> {
        let rendered = self.compose_rgba(gpu, &execution);
        if rendered.is_ok() {
            execution.failed().map_err(|e| e.to_string())?;
        }
        rendered
    }

    fn compose_rgba(&mut self, gpu: &Gpu, execution: &Execution) -> Result<Vec<u8>, Error> {
        let _input = self.prepare(gpu, &Frame::of(execution)?)?;
        if self.rgba_readback.is_none() {
            let size = self.target.size();
            let extent = ImageExtent::new(size.width, size.height)?;
            self.rgba_readback = Some(TextureReadback::new(gpu, extent)?);
        }
        let readback = self.rgba_readback.as_ref().expect("created above");
        let compositor = &mut self.compositor.as_mut().expect("installed compositor").1;
        let mut encoder = gpu.device.create_command_encoder(&Default::default());
        compositor.encode(&mut encoder, &self.target)?;
        let mut pixels = Vec::new();
        readback.submit(gpu, encoder, &self.target, &mut pixels)?;
        Ok(pixels)
    }

    /// Uploads the input and prepares the compositor; returns the input texture,
    /// which must outlive the submission.
    fn prepare(&mut self, gpu: &Gpu, frame: &Frame) -> Result<wgpu::Texture, Error> {
        let snapshot = frame.scene.snapshot();
        if snapshot.runtime_id != self.runtime_id
            || snapshot.state.configuration_id != self.configuration_id
        {
            return Err("program worker cannot switch immutable configurations".into());
        }
        let settings = frame.scene.settings();
        // Configuration is immutable for this worker. Camera/light edits only
        // change the scene; MeshRenderer evicts stale resource IDs in prepare.
        // Recompiling the compositor on every effect made camera edits >100 ms.
        let shadow_mode = snapshot.state.shadow_mode;
        if self
            .compositor
            .as_ref()
            .is_none_or(|(old, _)| *old != shadow_mode)
        {
            let mut compositor = FrameCompositor::new(gpu, self.target.format(), settings.output)?;
            compositor.set_sample_count(gpu, settings.raster.sample_count)?;
            self.compositor = Some((shadow_mode, compositor));
        }
        let image = frame.image;
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
            frame.pixels,
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
        let tracked;
        let camera_model = match (&self.video_lens, self.tracked_lens) {
            (Some(lens), _) => lens.model(frame.key)?,
            (None, Some(radial)) => {
                tracked = CameraModel::Distorted(Box::new(LensDistortion::new(
                    ImageExtent::new(image.extent.width, image.extent.height)?,
                    radial,
                    frame.frame_warp.map(FrameWarp::new).transpose()?,
                )));
                &tracked
            }
            (None, None) if frame.frame_warp.is_some() => {
                return Err("a frame warp needs a tracked program's lens".into());
            }
            (None, None) => &CameraModel::Pinhole,
        };
        compositor.set_animation_time(frame.seconds);
        compositor.prepare_model(
            gpu,
            frame.scene.scene(),
            frame.camera,
            camera_model,
            &[ImageLayer::new(&view, settings.input_stage)],
        )?;
        Ok(texture)
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
        || !fits(
            config.output_extent.width,
            config.output_extent.height,
            MAX_EXTENT,
        )
    {
        return Err(
            "program port requires even BT.709 output up to 7680x4320 at integral 1..120 FPS"
                .into(),
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extents_up_to_8k_fit_and_odd_or_larger_ones_do_not() {
        for (width, height) in [
            (7680, 4320),
            (3840, 2160),
            (4096, 2160),
            (1920, 1080),
            (2, 2),
        ] {
            assert!(fits(width, height, MAX_EXTENT), "{width}x{height}");
        }
        for (width, height) in [
            (7682, 4320),
            (7680, 4322),
            (1921, 1080),
            (1920, 1081),
            (0, 2),
        ] {
            assert!(!fits(width, height, MAX_EXTENT), "{width}x{height}");
        }
        assert!(fits(1280, 720, DEMO_MAX_EXTENT));
        assert!(!fits(1920, 1080, DEMO_MAX_EXTENT));
    }

    #[test]
    fn composition_budget_covers_canvases_and_guard_band() {
        let extent = |width, height| w::Extent { width, height };
        assert_eq!(
            composition_budget(extent(1920, 1080), extent(1280, 720)),
            256 * 1024 * 1024
        );
        // 8K native and output: 4 x 33.2 MP x 8 bytes, about 1.06 GB.
        let k8 = extent(7680, 4320);
        assert_eq!(composition_budget(k8, k8), 4 * 7680 * 4320 * 8);
    }
}
