//! Upload-path probe for #144, not a renderer path: how fast the CPU writes an
//! RGBA8 camera frame (1080p, 4K, 8K) into wgpu's Vulkan host-visible memory, the
//! way `queue.write_texture` stages every program input.
//!
//! `queue.write_texture` allocates a fresh `MAP_WRITE | COPY_SRC` staging buffer
//! per call; wgpu-hal places it with gpu-allocator's `CpuToGpu`, which prefers
//! `DEVICE_LOCAL | HOST_VISIBLE` memory: the 256 MiB BAR1 window when Resizable BAR
//! is off, shared by every process on the GPU. The probe times a memcpy into fresh
//! buffers of that kind and of the `GpuToCpu` kind (`HOST_CACHED` system memory),
//! into one reused mapped buffer, and whole `write_texture` calls. Run it alone and
//! next to another Vulkan process (such as a live render worker) to compare:
//!
//! ```text
//! cargo run --release -p chr-renderer --example upload_probe
//! ```

use std::time::Instant;

use chr_renderer::Gpu;

const REPEATS: usize = 5;

fn milliseconds(start: Instant) -> f64 {
    (start.elapsed().as_secs_f64() * 10_000.0).round() / 10.0
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let gpu = pollster::block_on(Gpu::request(&Gpu::instance(), None))?;
    let wait = || gpu.device.poll(wgpu::PollType::wait_indefinitely());
    for (label, width, height) in [
        ("1080p", 1920, 1080),
        ("4K", 3840, 2160),
        ("8K", 7680, 4320),
    ] {
        let data = vec![7_u8; width as usize * height as usize * 4];
        let buffer = |usage| {
            gpu.device.create_buffer(&wgpu::BufferDescriptor {
                label: None,
                size: data.len() as u64,
                usage,
                mapped_at_creation: true,
            })
        };
        for (kind, usage) in [
            (
                "fresh MAP_WRITE|COPY_SRC (CpuToGpu, BAR1 first)",
                wgpu::BufferUsages::MAP_WRITE | wgpu::BufferUsages::COPY_SRC,
            ),
            (
                "fresh MAP_READ|COPY_DST (GpuToCpu, host cached)",
                wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
            ),
        ] {
            let mut times = Vec::new();
            for _ in 0..REPEATS {
                let buffer = buffer(usage);
                let start = Instant::now();
                buffer
                    .slice(..)
                    .get_mapped_range_mut()?
                    .copy_from_slice(&data);
                times.push(milliseconds(start));
                buffer.unmap();
                drop(buffer);
                wait()?;
            }
            println!("{label} {kind}: memcpy {times:?} ms");
        }

        let reused = buffer(wgpu::BufferUsages::MAP_WRITE | wgpu::BufferUsages::COPY_SRC);
        let mut times = Vec::new();
        for _ in 0..REPEATS {
            let start = Instant::now();
            reused
                .slice(..)
                .get_mapped_range_mut()?
                .copy_from_slice(&data);
            times.push(milliseconds(start));
            reused.unmap();
            reused.slice(..).map_async(wgpu::MapMode::Write, |_| {});
            wait()?;
        }
        println!("{label} reused MAP_WRITE|COPY_SRC: memcpy {times:?} ms");

        let texture = gpu.device.create_texture(&wgpu::TextureDescriptor {
            label: None,
            size: wgpu::Extent3d {
                width,
                height,
                depth_or_array_layers: 1,
            },
            mip_level_count: 1,
            sample_count: 1,
            dimension: wgpu::TextureDimension::D2,
            format: wgpu::TextureFormat::Rgba8Unorm,
            usage: wgpu::TextureUsages::COPY_DST,
            view_formats: &[],
        });
        let mut times = Vec::new();
        for _ in 0..REPEATS {
            let start = Instant::now();
            gpu.queue.write_texture(
                texture.as_image_copy(),
                &data,
                wgpu::TexelCopyBufferLayout {
                    offset: 0,
                    bytes_per_row: Some(width * 4),
                    rows_per_image: None,
                },
                texture.size(),
            );
            times.push(milliseconds(start));
            gpu.queue.submit([]);
            wait()?;
        }
        println!("{label} queue.write_texture: {times:?} ms");
    }
    Ok(())
}
