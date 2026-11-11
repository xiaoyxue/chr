use std::{error::Error, sync::mpsc, time::Duration};

use crate::{Gpu, TriangleRenderer, coordinates::ImageExtent};

/// Render through the shared pipeline and return tightly packed, top-down sRGB RGBA8.
/// This native readback blocks until completion, with a bounded GPU wait.
pub fn render_triangle_rgba(gpu: &Gpu, extent: ImageExtent) -> Result<Vec<u8>, Box<dyn Error>> {
    let limits = gpu.device.limits();
    if extent.width() > limits.max_texture_dimension_2d
        || extent.height() > limits.max_texture_dimension_2d
    {
        return Err("offscreen dimensions exceed device limits".into());
    }
    let row_bytes = extent.width().checked_mul(4).ok_or("row size overflow")?;
    let padded_row_bytes = row_bytes
        .div_ceil(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT)
        .checked_mul(wgpu::COPY_BYTES_PER_ROW_ALIGNMENT)
        .ok_or("padded row size overflow")?;
    let buffer_size = u64::from(padded_row_bytes) * u64::from(extent.height());
    if buffer_size > limits.max_buffer_size {
        return Err("offscreen readback exceeds device buffer limit".into());
    }
    let texture = gpu.device.create_texture(&wgpu::TextureDescriptor {
        label: Some("offscreen triangle"),
        size: wgpu::Extent3d {
            width: extent.width(),
            height: extent.height(),
            depth_or_array_layers: 1,
        },
        mip_level_count: 1,
        sample_count: 1,
        dimension: wgpu::TextureDimension::D2,
        format: wgpu::TextureFormat::Rgba8UnormSrgb,
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT | wgpu::TextureUsages::COPY_SRC,
        view_formats: &[],
    });
    let buffer = gpu.device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("triangle readback"),
        size: buffer_size,
        usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
        mapped_at_creation: false,
    });
    let renderer = TriangleRenderer::new(&gpu.device, texture.format());
    renderer.resize(&gpu.queue, extent);
    let mut encoder = gpu.device.create_command_encoder(&Default::default());
    renderer.encode(&mut encoder, &texture.create_view(&Default::default()));
    encoder.copy_texture_to_buffer(
        texture.as_image_copy(),
        wgpu::TexelCopyBufferInfo {
            buffer: &buffer,
            layout: wgpu::TexelCopyBufferLayout {
                offset: 0,
                bytes_per_row: Some(padded_row_bytes),
                rows_per_image: Some(extent.height()),
            },
        },
        texture.size(),
    );
    let submission = gpu.queue.submit([encoder.finish()]);
    let (sender, receiver) = mpsc::channel();
    buffer
        .slice(..)
        .map_async(wgpu::MapMode::Read, move |result| {
            let _ = sender.send(result);
        });
    gpu.device.poll(wgpu::PollType::Wait {
        submission_index: Some(submission),
        timeout: Some(Duration::from_secs(30)),
    })?;
    receiver.recv_timeout(Duration::from_secs(30))??;
    let data = buffer.slice(..).get_mapped_range()?;
    let mut pixels = Vec::with_capacity(row_bytes as usize * extent.height() as usize);
    for row in data.chunks_exact(padded_row_bytes as usize) {
        pixels.extend_from_slice(&row[..row_bytes as usize]);
    }
    drop(data);
    buffer.unmap();
    Ok(pixels)
}
