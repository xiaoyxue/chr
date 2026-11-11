use std::{sync::mpsc, time::Duration};

use chr_renderer::Gpu;

use super::Result;

const TIMEOUT: Duration = Duration::from_secs(30);

pub async fn request() -> Result<(Gpu, Option<Timer>)> {
    let instance = Gpu::instance();
    let adapter = instance
        .request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::HighPerformance,
            force_fallback_adapter: false,
            ..Default::default()
        })
        .await?;
    let features =
        wgpu::Features::TIMESTAMP_QUERY | wgpu::Features::TIMESTAMP_QUERY_INSIDE_ENCODERS;
    let supported = adapter.features().contains(features);
    let (device, queue) = adapter
        .request_device(&wgpu::DeviceDescriptor {
            label: Some("optional benchmark timestamp device"),
            required_features: if supported {
                features
            } else {
                wgpu::Features::empty()
            },
            required_limits: wgpu::Limits::default().using_resolution(adapter.limits()),
            ..Default::default()
        })
        .await?;
    let timer = supported.then(|| Timer::new(&device));
    Ok((
        Gpu {
            adapter,
            device,
            queue,
        },
        timer,
    ))
}

pub struct Timer {
    queries: wgpu::QuerySet,
    resolve: wgpu::Buffer,
    readback: wgpu::Buffer,
}

impl Timer {
    fn new(device: &wgpu::Device) -> Self {
        Self {
            queries: device.create_query_set(&wgpu::QuerySetDescriptor {
                label: Some("benchmark timestamps"),
                ty: wgpu::QueryType::Timestamp,
                count: 2,
            }),
            resolve: device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("benchmark timestamp resolve"),
                size: 16,
                usage: wgpu::BufferUsages::QUERY_RESOLVE | wgpu::BufferUsages::COPY_SRC,
                mapped_at_creation: false,
            }),
            readback: device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("benchmark timestamp readback"),
                size: 16,
                usage: wgpu::BufferUsages::COPY_DST | wgpu::BufferUsages::MAP_READ,
                mapped_at_creation: false,
            }),
        }
    }

    pub fn begin(&self, encoder: &mut wgpu::CommandEncoder) {
        encoder.write_timestamp(&self.queries, 0);
    }

    pub fn end(&self, encoder: &mut wgpu::CommandEncoder) {
        encoder.write_timestamp(&self.queries, 1);
        encoder.resolve_query_set(&self.queries, 0..2, &self.resolve, 0);
        encoder.copy_buffer_to_buffer(&self.resolve, 0, &self.readback, 0, 16);
    }

    pub fn read(&self, gpu: &Gpu) -> Result<f64> {
        let (sender, receiver) = mpsc::channel();
        self.readback
            .slice(..)
            .map_async(wgpu::MapMode::Read, move |result| {
                let _ = sender.send(result);
            });
        gpu.device.poll(wgpu::PollType::Wait {
            submission_index: None,
            timeout: Some(TIMEOUT),
        })?;
        receiver.recv_timeout(TIMEOUT)??;
        let result = {
            let bytes = self.readback.slice(..).get_mapped_range()?;
            let start = u64::from_le_bytes(bytes[0..8].try_into()?);
            let end = u64::from_le_bytes(bytes[8..16].try_into()?);
            milliseconds(start, end, gpu.queue.get_timestamp_period())
        };
        self.readback.unmap();
        result
    }
}

fn milliseconds(start: u64, end: u64, period_ns: f32) -> Result<f64> {
    let ticks = end
        .checked_sub(start)
        .ok_or("non-monotonic GPU timestamps")?;
    if ticks == 0 || !period_ns.is_finite() || period_ns <= 0.0 {
        return Err("invalid/zero GPU timestamp interval or period".into());
    }
    Ok(ticks as f64 * f64::from(period_ns) / 1_000_000.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn timestamp_units_and_invalid_intervals() {
        assert_eq!(milliseconds(100, 500_100, 2.0).unwrap(), 1.0);
        assert!(milliseconds(200, 100, 1.0).is_err());
        assert!(milliseconds(100, 100, 1.0).is_err());
        assert!(milliseconds(0, 100, 0.0).is_err());
        assert!(milliseconds(0, 100, f32::NAN).is_err());
    }
}
