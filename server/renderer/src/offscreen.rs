use std::{error::Error, sync::mpsc, time::Duration};

use crate::{Gpu, TriangleRenderer, coordinates::ImageExtent};

pub(crate) type BoxError = Box<dyn Error + Send + Sync>;

/// The bounded wait for one frame's GPU work and readback.
const READBACK_TIMEOUT: Duration = Duration::from_secs(30);

/// A buffer that receives one texture copy and returns it as tightly packed rows.
pub(crate) struct Readback {
    buffer: wgpu::Buffer,
    row_bytes: u32,
    padded_row_bytes: u32,
    height: u32,
}

impl Readback {
    pub(crate) fn new(gpu: &Gpu, row_bytes: u32, height: u32) -> Result<Self, BoxError> {
        let padded_row_bytes = row_bytes
            .div_ceil(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT)
            .checked_mul(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT)
            .ok_or("padded row size overflow")?;
        let size = u64::from(padded_row_bytes) * u64::from(height);
        if size > gpu.device.limits().max_buffer_size {
            return Err("readback exceeds device buffer limit".into());
        }
        Ok(Self {
            buffer: gpu.device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("readback"),
                size,
                usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
                mapped_at_creation: false,
            }),
            row_bytes,
            padded_row_bytes,
            height,
        })
    }

    pub(crate) fn record_copy(&self, encoder: &mut wgpu::CommandEncoder, texture: &wgpu::Texture) {
        encoder.copy_texture_to_buffer(
            texture.as_image_copy(),
            wgpu::TexelCopyBufferInfo {
                buffer: &self.buffer,
                layout: wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(self.padded_row_bytes),
                    rows_per_image: Some(self.height),
                },
            },
            texture.size(),
        );
    }

    /// Waits for `submission`, which must contain [`Self::record_copy`], and copies the rows.
    pub(crate) fn finish(
        &self,
        gpu: &Gpu,
        submission: wgpu::SubmissionIndex,
        out: &mut Vec<u8>,
    ) -> Result<(), BoxError> {
        let (sender, receiver) = mpsc::channel();
        self.buffer
            .slice(..)
            .map_async(wgpu::MapMode::Read, move |result| {
                let _ = sender.send(result);
            });
        gpu.device.poll(wgpu::PollType::Wait {
            submission_index: Some(submission),
            timeout: Some(READBACK_TIMEOUT),
        })?;
        receiver.recv_timeout(READBACK_TIMEOUT)??;
        let row_bytes = self.row_bytes as usize;
        out.clear();
        out.reserve(row_bytes * self.height as usize);
        {
            let data = self.buffer.slice(..).get_mapped_range()?;
            for row in data.chunks_exact(self.padded_row_bytes as usize) {
                out.extend_from_slice(&row[..row_bytes]);
            }
        }
        self.buffer.unmap();
        Ok(())
    }
}

/// A reusable sRGB RGBA8 render target with a CPU readback buffer.
///
/// Readback blocks until the GPU finishes. It suits screenshots, tests, and the mock
/// preview encoder; it is not the intended continuous streaming path, which should hand
/// GPU frames to a hardware encoder without a raw-pixel CPU copy.
pub struct OffscreenTarget {
    extent: ImageExtent,
    texture: wgpu::Texture,
    view: wgpu::TextureView,
    encoded_view: wgpu::TextureView,
    readback: Readback,
}

impl OffscreenTarget {
    pub const FORMAT: wgpu::TextureFormat = wgpu::TextureFormat::Rgba8UnormSrgb;

    pub fn new(gpu: &Gpu, extent: ImageExtent) -> Result<Self, BoxError> {
        let limits = gpu.device.limits();
        if extent.width() > limits.max_texture_dimension_2d
            || extent.height() > limits.max_texture_dimension_2d
        {
            return Err("offscreen dimensions exceed device limits".into());
        }
        let row_bytes = extent.width().checked_mul(4).ok_or("row size overflow")?;
        let readback = Readback::new(gpu, row_bytes, extent.height())?;
        let texture = gpu.device.create_texture(&wgpu::TextureDescriptor {
            label: Some("offscreen target"),
            size: wgpu::Extent3d {
                width: extent.width(),
                height: extent.height(),
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: Self::FORMAT,
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT
                | wgpu::TextureUsages::COPY_SRC
                | wgpu::TextureUsages::TEXTURE_BINDING,
            view_formats: &[wgpu::TextureFormat::Rgba8Unorm],
        });
        Ok(Self {
            extent,
            view: texture.create_view(&Default::default()),
            encoded_view: texture.create_view(&wgpu::TextureViewDescriptor {
                label: Some("offscreen target, sRGB-encoded bytes"),
                format: Some(wgpu::TextureFormat::Rgba8Unorm),
                ..Default::default()
            }),
            texture,
            readback,
        })
    }

    pub fn extent(&self) -> ImageExtent {
        self.extent
    }

    /// The same texels viewed as `Rgba8Unorm`: shaders read the stored sRGB-encoded
    /// (gamma) values rather than decoded linear light, as video conversion requires.
    pub(crate) fn encoded_view(&self) -> &wgpu::TextureView {
        &self.encoded_view
    }

    /// Records draws with `record`, submits them, and only waits for completion.
    pub fn render(
        &self,
        gpu: &Gpu,
        record: impl FnOnce(&mut wgpu::CommandEncoder, &wgpu::TextureView),
    ) -> Result<(), BoxError> {
        let mut encoder = gpu.device.create_command_encoder(&Default::default());
        record(&mut encoder, &self.view);
        let submission = gpu.queue.submit([encoder.finish()]);
        gpu.device.poll(wgpu::PollType::Wait {
            submission_index: Some(submission),
            timeout: Some(READBACK_TIMEOUT),
        })?;
        Ok(())
    }

    /// Records draws with `record`, then copies the result into `pixels` as tightly
    /// packed, top-down sRGB RGBA8. `pixels` is resized to `width * height * 4`.
    pub fn render_rgba(
        &self,
        gpu: &Gpu,
        record: impl FnOnce(&mut wgpu::CommandEncoder, &wgpu::TextureView),
        pixels: &mut Vec<u8>,
    ) -> Result<(), BoxError> {
        let mut encoder = gpu.device.create_command_encoder(&Default::default());
        record(&mut encoder, &self.view);
        self.readback.record_copy(&mut encoder, &self.texture);
        let submission = gpu.queue.submit([encoder.finish()]);
        self.readback.finish(gpu, submission, pixels)
    }
}

/// Render through the shared pipeline and return tightly packed, top-down sRGB RGBA8.
/// This native readback blocks until completion, with a bounded GPU wait.
pub fn render_triangle_rgba(gpu: &Gpu, extent: ImageExtent) -> Result<Vec<u8>, Box<dyn Error>> {
    render_rotated_triangle_rgba(gpu, extent, 0.0)
}

/// Like [`render_triangle_rgba`], with the display rotation of
/// [`TriangleRenderer::set_rotation`].
pub fn render_rotated_triangle_rgba(
    gpu: &Gpu,
    extent: ImageExtent,
    radians: f64,
) -> Result<Vec<u8>, Box<dyn Error>> {
    let target = OffscreenTarget::new(gpu, extent).map_err(|error| error as Box<dyn Error>)?;
    let renderer = TriangleRenderer::new(&gpu.device, OffscreenTarget::FORMAT);
    renderer.resize(&gpu.queue, extent);
    renderer.set_rotation(&gpu.queue, radians);
    let mut pixels = Vec::new();
    target
        .render_rgba(
            gpu,
            |encoder, view| renderer.encode(encoder, view),
            &mut pixels,
        )
        .map_err(|error| error as Box<dyn Error>)?;
    Ok(pixels)
}
