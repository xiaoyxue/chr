use super::*;
use chr_renderer::Gpu;
use wgpu::util::DeviceExt;

const SIZE: u32 = 256;
const EXPOSURE: f64 = 4.0;
const ERROR_SCALE: f64 = 0.03;

fn surface(x: u32, y: u32) -> V {
    V(
        ((y as f64 + 0.5) / SIZE as f64 - 0.5) * 6.8 / 0.3,
        -((x as f64 + 0.5) / SIZE as f64 - 0.5) * 6.8,
        0.0,
    )
}

fn display(value: f64) -> u8 {
    let exposed = (value.max(0.0) * EXPOSURE).min(65504.0);
    let mapped = exposed / (1.0 + exposed);
    let srgb = if mapped <= 0.0031308 {
        12.92 * mapped
    } else {
        1.055 * mapped.powf(1.0 / 2.4) - 0.055
    };
    (255.0 * srgb).round() as u8
}

fn pixels(device: &wgpu::Device, buffer: &wgpu::Buffer) -> Result<Vec<[f64; 4]>, Box<dyn Error>> {
    let bytes = gpu::read(device, buffer)?;
    bytes
        .as_chunks::<16>()
        .0
        .iter()
        .map(|pixel| {
            let result = std::array::from_fn(|i| {
                f32::from_le_bytes(pixel[i * 4..i * 4 + 4].try_into().expect("four bytes")) as f64
            });
            if result.iter().any(|v| !v.is_finite() || *v < 0.0) {
                Err("invalid rendered radiance".into())
            } else {
                Ok(result)
            }
        })
        .collect()
}

pub(super) async fn run(table: &Table) -> Result<(), Box<dyn Error>> {
    let gpu = Gpu::request(&Gpu::instance(), None).await?;
    let info = gpu.adapter.get_info();
    println!("image comparison adapter={info:?}");
    if !matches!(
        info.device_type,
        wgpu::DeviceType::DiscreteGpu | wgpu::DeviceType::IntegratedGpu
    ) {
        return Err("software/unknown adapter is not hardware evidence".into());
    }
    let device = &gpu.device;
    let queue = &gpu.queue;
    let reference_source = format!(
        "{}\n{}\n{}\n{}",
        include_str!("../../src/environment_sampling.wgsl"),
        include_str!("../../src/mesh.wgsl"),
        include_str!("comparison_scene.wgsl"),
        include_str!("comparison_reference.wgsl"),
    );
    let candidate_source = format!(
        "{}\n{}\n{}",
        include_str!("candidate.wgsl"),
        include_str!("comparison_scene.wgsl"),
        include_str!("comparison_candidate.wgsl"),
    );
    let pipeline = |source: String, entry| {
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some(entry),
            source: wgpu::ShaderSource::Wgsl(source.into()),
        });
        device.create_compute_pipeline(&wgpu::ComputePipelineDescriptor {
            label: Some(entry),
            layout: None,
            module: &shader,
            entry_point: Some(entry),
            compilation_options: Default::default(),
            cache: None,
        })
    };
    let reference_pipeline = pipeline(reference_source, "comparison_reference");
    let candidate_pipeline = pipeline(candidate_source, "comparison_candidate");
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
        label: Some("unchanged LTC data"),
        contents: bytemuck::cast_slice(&entries),
        usage: wgpu::BufferUsages::STORAGE,
    });
    let params = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("comparison parameters"),
        size: 16,
        usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });
    let bytes = u64::from(SIZE * SIZE * 16);
    let output = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("scene-linear comparison"),
        size: bytes,
        usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_SRC,
        mapped_at_creation: false,
    });
    let readback = device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("comparison image readback"),
        size: bytes,
        usage: wgpu::BufferUsages::MAP_READ | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    });
    let reference_bind = device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("native reference bindings"),
        layout: &reference_pipeline.get_bind_group_layout(0),
        entries: &[
            wgpu::BindGroupEntry {
                binding: 1,
                resource: params.as_entire_binding(),
            },
            wgpu::BindGroupEntry {
                binding: 2,
                resource: output.as_entire_binding(),
            },
        ],
    });
    let candidate_bind = device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("candidate bindings"),
        layout: &candidate_pipeline.get_bind_group_layout(0),
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
    let render =
        |shape: u32, samples: u32, reference: bool| -> Result<Vec<[f64; 4]>, Box<dyn Error>> {
            queue.write_buffer(
                &params,
                0,
                bytemuck::cast_slice(&[shape, samples, SIZE, 64]),
            );
            let mut encoder = device.create_command_encoder(&Default::default());
            {
                let mut pass = encoder.begin_compute_pass(&Default::default());
                if reference {
                    pass.set_pipeline(&reference_pipeline);
                    pass.set_bind_group(0, &reference_bind, &[]);
                    pass.dispatch_workgroups(SIZE, SIZE, 1);
                } else {
                    pass.set_pipeline(&candidate_pipeline);
                    pass.set_bind_group(0, &candidate_bind, &[]);
                    pass.dispatch_workgroups(SIZE / 8, SIZE / 8, 1);
                }
            }
            encoder.copy_buffer_to_buffer(&output, 0, &readback, 0, bytes);
            queue.submit([encoder.finish()]);
            pixels(device, &readback)
        };
    let root = "output/area-light-native/comparison";
    fs::create_dir_all(root)?;
    let mut metrics = fs::File::create(format!("{root}/metrics.csv"))?;
    writeln!(
        metrics,
        "shape,max_reference_delta,max_candidate_scalar_delta,max_reference_cpu_delta,max_absolute_error,mean_absolute_error"
    )?;
    for (shape_id, shape) in [Shape::Rectangle, Shape::Disk, Shape::Sphere]
        .into_iter()
        .enumerate()
    {
        let name = format!("{shape:?}").to_lowercase();
        let coarse = render(shape_id as u32, 128, true)?;
        let reference = render(shape_id as u32, 256, true)?;
        let candidate_image = render(shape_id as u32, 0, false)?;
        let mut reference_delta: f64 = 0.0;
        let mut scalar_delta: f64 = 0.0;
        let mut cpu_delta: f64 = 0.0;
        let mut max_error: f64 = 0.0;
        let mut error_sum = 0.0;
        let mut count = 0;
        let mut images: [Vec<u8>; 3] = std::array::from_fn(|_| Vec::new());
        let material = Material {
            roughness: 0.5,
            metal: 1.0,
            nv: 0.3,
        };
        for (index, ((r, c), low)) in reference
            .iter()
            .zip(&candidate_image)
            .zip(&coarse)
            .enumerate()
        {
            let error = (r[0] - c[0]).abs();
            if r[3] > 0.5 {
                reference_delta = reference_delta.max((r[0] - low[0]).abs());
                max_error = max_error.max(error);
                error_sum += error;
                count += 1;
                let x = index as u32 % SIZE;
                let y = index as u32 / SIZE;
                if x.is_multiple_of(63) && y.is_multiple_of(63) {
                    let p = surface(x, y);
                    let light = Light {
                        center: V(-p.0, -p.1, 2.0),
                        ..default_light(shape)
                    };
                    let cpu_candidate = candidate(light, material, table.lookup(material), 64)[2];
                    let cpu_reference = profile::angular_reference(light, material, 512);
                    scalar_delta = scalar_delta.max((c[0] - cpu_candidate).abs());
                    cpu_delta = cpu_delta.max((r[0] - cpu_reference).abs());
                }
            }
            images[0].extend_from_slice(&[display(r[0]); 3]);
            images[1].extend_from_slice(&[display(c[0]); 3]);
            // A fixed, explicitly labelled linear error scale; not a beauty image.
            let e = (error / ERROR_SCALE).clamp(0.0, 1.0);
            images[2].extend_from_slice(&[
                (255.0 * e) as u8,
                (255.0 * (e * 2.0 - 1.0).max(0.0)) as u8,
                0,
            ]);
        }
        println!(
            "{shape:?}: reference128_256={reference_delta:.3e} candidate_scalar={scalar_delta:.3e} reference_cpu={cpu_delta:.3e} max_error={max_error:.6}"
        );
        if reference_delta > 5e-5 || scalar_delta > 2e-6 || cpu_delta > 1e-4 {
            return Err(format!("comparison accuracy check failed for {shape:?}").into());
        }
        writeln!(
            metrics,
            "{shape:?},{reference_delta:.12e},{scalar_delta:.12e},{cpu_delta:.12e},{max_error:.12e},{:.12e}",
            error_sum / count as f64
        )?;
        for (kind, image) in ["reference", "ltc", "error"].into_iter().zip(images) {
            image::save_buffer(
                format!("{root}/{name}-{kind}.png"),
                &image,
                SIZE,
                SIZE,
                image::ColorType::Rgb8,
            )?;
        }
        let linear: Vec<f32> = reference
            .iter()
            .zip(&candidate_image)
            .flat_map(|(r, c)| [r[0] as f32, c[0] as f32, r[3] as f32])
            .collect();
        fs::write(
            format!("{root}/{name}-linear.bin"),
            bytemuck::cast_slice(&linear),
        )?;
    }
    fs::write(
        format!("{root}/index.html"),
        include_str!("comparison.html"),
    )?;
    Ok(())
}
