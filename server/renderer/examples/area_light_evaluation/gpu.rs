use super::*;
use wgpu::util::DeviceExt;

fn read(device: &wgpu::Device, buffer: &wgpu::Buffer) -> Result<Vec<u8>, Box<dyn Error>> {
    let (tx, rx) = std::sync::mpsc::channel();
    buffer.slice(..).map_async(wgpu::MapMode::Read, move |r| {
        let _ = tx.send(r);
    });
    device.poll(wgpu::PollType::wait_indefinitely())?;
    rx.recv()??;
    let bytes = buffer.slice(..).get_mapped_range()?.to_vec();
    buffer.unmap();
    Ok(bytes)
}

pub(super) async fn run(table: &Table) -> Result<(), Box<dyn Error>> {
    let instance = wgpu::Instance::new(wgpu::InstanceDescriptor {
        backends: wgpu::Backends::VULKAN,
        ..wgpu::InstanceDescriptor::new_without_display_handle()
    });
    let adapter = instance
        .request_adapter(&wgpu::RequestAdapterOptions {
            power_preference: wgpu::PowerPreference::HighPerformance,
            force_fallback_adapter: false,
            ..Default::default()
        })
        .await?;
    let info = adapter.get_info();
    println!(
        "adapter={info:?} available_features={:?}",
        adapter.features()
    );
    if !matches!(
        info.device_type,
        wgpu::DeviceType::DiscreteGpu | wgpu::DeviceType::IntegratedGpu
    ) {
        return Err("software/unknown adapter is not hardware evidence".into());
    }
    let features = wgpu::Features::TIMESTAMP_QUERY;
    if !adapter.features().contains(features) {
        return Err("GPU timing gate unavailable: TIMESTAMP_QUERY".into());
    }
    let (device, queue) = adapter
        .request_device(&wgpu::DeviceDescriptor {
            label: Some("area method experiment"),
            required_features: features,
            ..Default::default()
        })
        .await?;
    println!(
        "enabled_features={features:?} timestamp_period_ns={}",
        queue.get_timestamp_period()
    );
    let entries: Vec<[f32; 8]> = table
        .0
        .iter()
        .map(|t| {
            [
                t[0] as f32,
                t[1] as f32,
                t[2] as f32,
                t[3] as f32,
                t[4] as f32,
                t[5] as f32,
                0.0,
                0.0,
            ]
        })
        .collect();
    let lut = device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
        label: Some("LTC 64x64 matrix/amplitude"),
        contents: bytemuck::cast_slice(&entries),
        usage: wgpu::BufferUsages::STORAGE,
    });
    let size = 256u32;
    let bytes = u64::from(size * size * 16);
    let output = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("synthetic illumination"),
        size: bytes,
        usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC,
        mapped_at_creation: false,
    });
    let pixels = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("pixel validation"),
        size: bytes,
        usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });
    let params = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("experiment parameters"),
        size: 16,
        usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });
    let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("LTC candidate"),
        source: wgpu::ShaderSource::Wgsl(include_str!("candidate.wgsl").into()),
    });
    let pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
        label: Some("area candidate"),
        layout: None,
        module: &shader,
        entry_point: Some("main"),
        compilation_options: Default::default(),
        cache: None,
    });
    let bind = device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("experiment bindings"),
        layout: &pipeline.get_bind_group_layout(0),
        entries: &[
            wgpu::BindGroupEntry {
                binding: 0,
                resource: lut.as_entire_binding(),
            },
            wgpu::BindGroupEntry {
                binding: 1,
                resource: output.as_entire_binding(),
            },
            wgpu::BindGroupEntry {
                binding: 2,
                resource: params.as_entire_binding(),
            },
        ],
    });
    let queries = device.create_query_set(&wgpu::QuerySetDescriptor {
        label: Some("isolated compute interval"),
        ty: wgpu::QueryType::Timestamp,
        count: 2,
    });
    let resolved = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("timestamp resolve"),
        size: 16,
        usage: wgpu::BufferUsages::QUERY_RESOLVE | wgpu::BufferUsages::COPY_SRC,
        mapped_at_creation: false,
    });
    let timestamps = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("timestamp readback"),
        size: 16,
        usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });
    fs::create_dir_all("output/area-light-methods")?;
    let mut csv = fs::File::create("output/area-light-methods/gpu.csv")?;
    writeln!(
        csv,
        "shape,lights,sides,resolution,iteration,gpu_ms,max_scalar_delta"
    )?;
    for (shape_id, shape) in [Shape::Rectangle, Shape::Disk, Shape::Sphere]
        .into_iter()
        .enumerate()
    {
        for lights in [1u32, 8] {
            for sides in [16u32, 64] {
                if shape == Shape::Rectangle && sides == 64 {
                    continue;
                }
                queue.write_buffer(
                    &params,
                    0,
                    bytemuck::cast_slice(&[shape_id as u32, lights, size, sides]),
                );
                for iteration in 0..8 {
                    let mut encoder = device.create_command_encoder(&Default::default());
                    {
                        let mut pass = encoder.begin_compute_pass(&wgpu::ComputePassDescriptor {
                            label: Some("only candidate dispatch"),
                            timestamp_writes: Some(wgpu::ComputePassTimestampWrites {
                                query_set: &queries,
                                beginning_of_pass_write_index: Some(0),
                                end_of_pass_write_index: Some(1),
                            }),
                        });
                        pass.set_pipeline(&pipeline);
                        pass.set_bind_group(0, &bind, &[]);
                        pass.dispatch_workgroups(size / 8, size / 8, 1);
                    }
                    encoder.resolve_query_set(&queries, 0..2, &resolved, 0);
                    encoder.copy_buffer_to_buffer(&resolved, 0, &timestamps, 0, 16);
                    encoder.copy_buffer_to_buffer(&output, 0, &pixels, 0, bytes);
                    queue.submit([encoder.finish()]);
                    let times = read(&device, &timestamps)?;
                    let begin = u64::from_le_bytes(times[..8].try_into()?);
                    let end = u64::from_le_bytes(times[8..].try_into()?);
                    let milliseconds = end.checked_sub(begin).ok_or("reversed timestamps")? as f64
                        * queue.get_timestamp_period() as f64
                        * 1e-6;
                    let data = read(&device, &pixels)?;
                    let mut max_delta: f64 = 0.0;
                    for y in (0..size).step_by(31) {
                        for x in (0..size).step_by(31) {
                            let uv = (
                                (x as f64 + 0.5) / size as f64,
                                (y as f64 + 0.5) / size as f64,
                            );
                            let m = Material {
                                roughness: 0.15 + 0.7 * uv.0,
                                metal: 0.0,
                                nv: 0.3 + 0.65 * uv.1,
                            };
                            let ltc = table.lookup(m);
                            let mut expected = [0.0; 4];
                            for l in 0..lights {
                                let light = Light {
                                    center: V(
                                        l as f64 * 0.25 - (uv.0 - 0.5) * 4.0,
                                        -(uv.1 - 0.5) * 4.0,
                                        3.0,
                                    ),
                                    ..default_light(shape)
                                };
                                let c = candidate(light, m, ltc, sides as usize);
                                for (dst, src) in
                                    expected.iter_mut().zip([c[2] + c[3], c[0], c[2], c[3]])
                                {
                                    *dst += src;
                                }
                            }
                            for (channel, expected) in expected.into_iter().enumerate() {
                                let offset = ((x + size * y) * 16) as usize + 4 * channel;
                                let actual =
                                    f32::from_le_bytes(data[offset..offset + 4].try_into()?) as f64;
                                if !actual.is_finite() {
                                    return Err("nonfinite GPU output".into());
                                }
                                if (actual - expected).abs() > 2e-4 {
                                    let start = ((x + size * y) * 16) as usize;
                                    let channels: Vec<_> = data[start..start + 16]
                                        .as_chunks::<4>()
                                        .0
                                        .iter()
                                        .map(|b| f32::from_le_bytes(*b))
                                        .collect();
                                    return Err(format!("GPU/scalar mismatch {shape:?} lights={lights} sides={sides} x={x} y={y} channel={channel}: actual={actual} expected={expected} GPU={channels:?}").into());
                                }
                                max_delta = max_delta.max((actual - expected).abs());
                            }
                        }
                    }
                    if iteration > 0 {
                        writeln!(
                            csv,
                            "{shape:?},{lights},{sides},{size},{iteration},{milliseconds:.6},{max_delta:.9e}"
                        )?;
                    }
                }
                println!("GPU completed {shape:?} lights={lights} sides={sides}");
            }
        }
    }
    Ok(())
}
