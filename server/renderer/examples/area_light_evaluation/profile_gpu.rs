use super::*;
use chr_renderer::{Gpu, scene::PbrMaterial};

pub(super) async fn run(table: &Table) -> Result<(), Box<dyn Error>> {
    let instance = Gpu::instance();
    let gpu = Gpu::request(&instance, None).await?;
    let info = gpu.adapter.get_info();
    println!("native BRDF oracle adapter={info:?}");
    if !matches!(
        info.device_type,
        wgpu::DeviceType::DiscreteGpu | wgpu::DeviceType::IntegratedGpu
    ) {
        return Err("software/unknown adapter is not hardware evidence".into());
    }
    let device = &gpu.device;
    let queue = &gpu.queue;
    // Compile the actual renderer BRDF without copying/extracting its equations.
    // Unused raster entry points and bindings are not part of this compute pipeline.
    let source = format!(
        "{}\n{}\n{}",
        include_str!("../../src/environment_sampling.wgsl"),
        include_str!("../../src/mesh.wgsl"),
        include_str!("profile.wgsl")
    );
    let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
        label: Some("native BRDF counterexample oracle"),
        source: wgpu::ShaderSource::Wgsl(source.into()),
    });
    let pipeline = device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
        label: Some("high-sample oracle, not real-time shading"),
        layout: None,
        module: &shader,
        entry_point: Some("profile_reference"),
        compilation_options: Default::default(),
        cache: None,
    });
    let params = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("native oracle parameters"),
        size: 32,
        usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });
    fs::create_dir_all("output/area-light-native")?;
    let mut csv = fs::File::create("output/area-light-native/native.csv")?;
    writeln!(
        csv,
        "shape,roughness,n,native_reference,cpu_reference,native_cpu_delta,candidate64,absolute_error,relative_error"
    )?;
    for (shape_id, shape) in [Shape::Rectangle, Shape::Disk, Shape::Sphere]
        .into_iter()
        .enumerate()
    {
        for roughness in [0.15, 0.5] {
            let (light, material) = profile::counterexample(shape, roughness);
            let native = PbrMaterial {
                base_color: [0.7, 0.7, 0.7, 1.0],
                metallic: material.metal as f32,
                roughness: material.roughness as f32,
                ..PbrMaterial::default()
            };
            for n in [512u32, 1024, 2048] {
                let size = u64::from(n) * u64::from(n) * 4;
                let samples = device.create_buffer(&wgpu::BufferDescriptor {
                    label: Some("native oracle sample contributions"),
                    size,
                    usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC,
                    mapped_at_creation: false,
                });
                let readback = device.create_buffer(&wgpu::BufferDescriptor {
                    label: Some("native oracle readback"),
                    size,
                    usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
                    mapped_at_creation: false,
                });
                let bind = device.create_bind_group(&wgpu::BindGroupDescriptor {
                    label: Some("only oracle bindings"),
                    layout: &pipeline.get_bind_group_layout(0),
                    entries: &[
                        wgpu::BindGroupEntry {
                            binding: 1,
                            resource: params.as_entire_binding(),
                        },
                        wgpu::BindGroupEntry {
                            binding: 2,
                            resource: samples.as_entire_binding(),
                        },
                    ],
                });
                queue.write_buffer(
                    &params,
                    0,
                    bytemuck::cast_slice(&[shape_id as u32, n, 0, 0]),
                );
                queue.write_buffer(
                    &params,
                    16,
                    bytemuck::cast_slice(&[
                        native.base_color[0],
                        native.metallic,
                        native.roughness,
                        material.nv as f32,
                    ]),
                );
                let mut encoder = device.create_command_encoder(&Default::default());
                {
                    let mut pass = encoder.begin_compute_pass(&Default::default());
                    pass.set_pipeline(&pipeline);
                    pass.set_bind_group(0, &bind, &[]);
                    pass.dispatch_workgroups(n / 8, n / 8, 1);
                }
                encoder.copy_buffer_to_buffer(&samples, 0, &readback, 0, size);
                queue.submit([encoder.finish()]);
                let bytes = gpu::read(device, &readback)?;
                let mut actual = 0.0;
                for bytes in bytes.as_chunks::<4>().0 {
                    let value = f32::from_le_bytes(*bytes);
                    if !value.is_finite() || value < 0.0 {
                        return Err("invalid native BRDF oracle sample".into());
                    }
                    actual += f64::from(value);
                }
                let cpu = reference(light, material, table.lookup(material), n as usize);
                let cpu = cpu[2] + cpu[3];
                let delta = (actual - cpu).abs();
                if delta > 1e-7 {
                    return Err(
                        format!("native/scalar mismatch: {shape:?} n={n} delta={delta}").into(),
                    );
                }
                let c = candidate(light, material, table.lookup(material), 64);
                let c = c[2] + c[3];
                let error = (c - actual).abs();
                writeln!(
                    csv,
                    "{shape:?},{roughness},{n},{actual:.12e},{cpu:.12e},{delta:.12e},{c:.12e},{error:.12e},{:.12e}",
                    error / actual
                )?;
                println!(
                    "{shape:?} roughness={roughness} n={n} native={actual:.12e} scalar_delta={delta:.3e}"
                );
            }
        }
    }
    println!(
        "The high-sample native BRDF check is an oracle, not a candidate or GPU budget result."
    );
    Ok(())
}
