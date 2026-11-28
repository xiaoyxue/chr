use std::{
    collections::{HashMap, HashSet, hash_map::Entry},
    num::NonZeroU64,
    ops::Range,
};
use wgpu::util::DeviceExt;

use crate::{
    Gpu,
    camera::CameraView,
    coordinates::ImageExtent,
    diagnostics::{DiagnosticView, Overlays, RenderSettings, line_indices},
    environment::{self, EnvironmentGpu, EnvironmentSettings},
    flag::{self, FlagDraw, FlagGpu},
    material_maps::{MaterialMapCache, fullscreen_pipeline, texture_entry},
    offscreen::BoxError,
    scene::{
        AreaShape, FlagShading, Light, MapSlot, MaterialId, MaterialMaps, MeshData, SceneView,
    },
    shadow::{self, CascadeSettings, ShadowAtlas, ShadowMode, ShadowProjection, ShadowSettings},
};

#[derive(Clone, Copy, Debug, Default)]
pub enum ToneMap {
    Reinhard,
    #[default]
    Aces,
}

#[derive(Clone, Copy, Debug)]
pub struct PbrSettings {
    pub exposure: f32,
    pub tone_map: ToneMap,
    pub shadows: ShadowSettings,
    pub environment: EnvironmentSettings,
}

impl Default for PbrSettings {
    fn default() -> Self {
        Self {
            exposure: 1.0,
            tone_map: ToneMap::Aces,
            shadows: ShadowSettings::default(),
            environment: EnvironmentSettings::default(),
        }
    }
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct Vertex {
    position: [f32; 3],
    normal: [f32; 3],
    uv: [f32; 2],
    tangent: [f32; 4],
    color: [f32; 4],
}

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct CameraUniform {
    clip: [[f32; 4]; 4],
    eye: [f32; 4],
    options: [u32; 4],
    optical_depth: [f32; 4],
}

const CAMERA_UNIFORM_SIZE: u64 = size_of::<CameraUniform>() as u64;

#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct LightUniform {
    position_kind: [f32; 4],
    direction_inner: [f32; 4],
    emission_outer: [f32; 4],
    shadow_matrix: [[[f32; 4]; 4]; 4],
    shadow_params: [f32; 4],
    shadow_depth: [[f32; 4]; 4],
    // Source size, search cap, filter cap, effective shadow mode.
    pcss: [f32; 4],
    pcss_samples: [f32; 4],
    cascade_splits: [f32; 4],
    cascade_params: [f32; 4],
    // Area lights: unit tangent + half width/radius, then half height.
    area: [[f32; 4]; 2],
}

/// Curved-emitter polygon sides qualified by `ltc-plausible-v1`.
pub const AREA_CURVE_SEGMENTS: u32 = 64;

/// Pinned official LTC GGX table (see `ltc/LICENSE.ltc`), padded to two vec4s per texel.
fn ltc_table(device: &wgpu::Device) -> wgpu::Buffer {
    let values: Vec<f32> = include_bytes!("ltc/ltc.bin")
        .chunks_exact(24)
        .flat_map(|texel| {
            let v: [f32; 6] = std::array::from_fn(|i| {
                f32::from_le_bytes(texel[4 * i..4 * i + 4].try_into().expect("4-byte f32"))
            });
            [v[0], v[1], v[2], v[3], v[4], v[5], 0.0, 0.0]
        })
        .collect();
    device.create_buffer_init(&wgpu::util::BufferInitDescriptor {
        label: Some("LTC GGX table"),
        contents: bytemuck::cast_slice(&values),
        usage: wgpu::BufferUsages::STORAGE,
    })
}

/// What one draw's instances share: mesh.wgsl's `Object`, a dynamic-offset uniform,
/// so fragment stages read it in uniform control flow.
#[repr(C)]
#[derive(Clone, Copy, PartialEq, bytemuck::Pod, bytemuck::Zeroable)]
struct DrawUniform {
    color: [f32; 4],
    emissive_cutoff: [f32; 4],
    factors: [f32; 4],
    flags: [u32; 4],
    /// Specular colour RGB and strength (`KHR_materials_specular`).
    specular: [f32; 4],
    /// IOR, clearcoat intensity, clearcoat roughness and clearcoat normal scale.
    coat: [f32; 4],
    /// Sheen colour RGB and roughness.
    sheen: [f32; 4],
    /// Iridescence intensity, film IOR and film thickness in nm at texture values 0 and 1.
    iridescence: [f32; 4],
    /// Anisotropy strength and the cosine and sine of its rotation; w unused.
    anisotropy: [f32; 4],
    /// x: bit `MapSlot as u32` set when that slot has a texture; yzw unused.
    maps: [u32; 4],
    /// Two rows per [`MapSlot`] of its texture-coordinate transform (`UvTransform::rows`).
    uv: [[f32; 4]; 2 * MapSlot::COUNT],
}

/// One instance's transform: mesh.wgsl's `Instance`, read by vertex stages at
/// `instance_index` (which counts from the draw's first instance).
#[repr(C)]
#[derive(Clone, Copy, bytemuck::Pod, bytemuck::Zeroable)]
struct InstanceUniform {
    model: [[f32; 4]; 4],
    normal: [[f32; 4]; 3],
}

pub(crate) const OBJECT_BYTES: u64 = size_of::<DrawUniform>() as u64;
const INSTANCE_BYTES: u64 = size_of::<InstanceUniform>() as u64;

struct MeshBuffers {
    /// Keeps the keyed data alive, so its addresses cannot be reused by other data.
    _source: MeshData,
    vertices: wgpu::Buffer,
    indices: wgpu::Buffer,
    lines: Option<wgpu::Buffer>,
}

/// GPU mesh buffers are keyed by shared data, not by scene handle: a scene rebuilt
/// for a new revision (for example a moved instance) reuses the uploaded mesh.
type MeshKey = [usize; 3];

/// One `draw_indexed`: every instance in `instances` shares the mesh part, the
/// material and `offset`'s uniform.
struct Draw {
    mesh: MeshKey,
    indices: Range<u32>,
    offset: u32,
    instances: Range<u32>,
    material: MaterialId,
    /// Faces the shaded and shadow pipelines cull: the back of single-sided parts.
    cull: Option<wgpu::Face>,
    shadow_catcher: bool,
    decal: bool,
    holdout: bool,
    /// Flag slot and look; the surface comes from the slot, not the mesh vertices.
    flag: Option<(usize, FlagShading)>,
}

/// One instance's mesh part, before batching.
struct Part {
    mesh: MeshKey,
    indices: Range<u32>,
    material: MaterialId,
    mirrored: bool,
    shadow_catcher: bool,
    decal: bool,
    holdout: bool,
    flag: Option<(usize, FlagShading)>,
    uniform: DrawUniform,
}

impl Part {
    /// Parts with equal keys share their uniform: it follows from the material,
    /// handedness and kind.
    fn key(&self) -> (MeshKey, u32, u32, MaterialId, bool, bool, bool) {
        (
            self.mesh,
            self.indices.start,
            self.indices.end,
            self.material,
            self.mirrored,
            self.shadow_catcher,
            self.decal,
        )
    }
}

struct Batch {
    part: Part,
    instances: Vec<InstanceUniform>,
}

/// Orders the parts for drawing and merges those one instanced draw can cover.
/// Opaque parts and holdouts group by key in order of first appearance (depth
/// testing makes their order irrelevant); flags keep one draw each (each has its
/// own surface); decals, then catchers, keep scene order, since they blend, and
/// merge only with a neighbour: instances of one draw rasterize in order.
fn batch(parts: Vec<(Part, InstanceUniform)>) -> Vec<Batch> {
    let mut opaque: Vec<Batch> = Vec::new();
    let mut groups = HashMap::<_, usize>::new();
    let mut flags = Vec::new();
    let mut decals: Vec<Batch> = Vec::new();
    let mut catchers: Vec<Batch> = Vec::new();
    for (part, instance) in parts {
        let single = |part: Part| Batch {
            part,
            instances: vec![instance],
        };
        if part.flag.is_some() {
            flags.push(single(part));
            continue;
        }
        if !part.shadow_catcher && !part.decal {
            match groups.entry(part.key()) {
                Entry::Occupied(entry) => opaque[*entry.get()].instances.push(instance),
                Entry::Vacant(entry) => {
                    entry.insert(opaque.len());
                    opaque.push(single(part));
                }
            }
            continue;
        }
        let ordered = if part.shadow_catcher {
            &mut catchers
        } else {
            &mut decals
        };
        match ordered.last_mut() {
            Some(last) if last.part.key() == part.key() => last.instances.push(instance),
            _ => ordered.push(single(part)),
        }
    }
    opaque
        .into_iter()
        .chain(flags)
        .chain(decals)
        .chain(catchers)
        .collect()
}

/// What a pass has bound, so consecutive draws rebind only what changes.
#[derive(Default)]
struct Bindings {
    object: Option<(Option<usize>, u32)>,
    material: Option<MaterialId>,
    vertices: Option<MeshKey>,
    indices: Option<MeshKey>,
}

impl Bindings {
    /// Binds `draw`'s object uniform, material and buffers; flags read their
    /// surface through group 1 instead of mesh vertices.
    fn bind(
        &mut self,
        pass: &mut wgpu::RenderPass<'_>,
        renderer: &MeshRenderer,
        draw: &Draw,
        mesh: &MeshBuffers,
        wire: bool,
    ) {
        let object = (draw.flag.map(|(slot, _)| slot), draw.offset);
        if self.object != Some(object) {
            let group = match draw.flag {
                Some((slot, _)) => renderer.flags.group(slot),
                None => &renderer.object_group,
            };
            pass.set_bind_group(1, group, &[draw.offset]);
            self.object = Some(object);
        }
        if self.material != Some(draw.material) {
            pass.set_bind_group(2, renderer.maps.group(draw.material), &[]);
            self.material = Some(draw.material);
        }
        if draw.flag.is_none() && self.vertices != Some(draw.mesh) {
            pass.set_vertex_buffer(0, mesh.vertices.slice(..));
            self.vertices = Some(draw.mesh);
        }
        if self.indices != Some(draw.mesh) {
            let indices = if wire {
                mesh.lines.as_ref().expect("wireframe prepared")
            } else {
                &mesh.indices
            };
            pass.set_index_buffer(indices.slice(..), wgpu::IndexFormat::Uint32);
            self.indices = Some(draw.mesh);
        }
    }
}

#[derive(PartialEq)]
struct CascadeKey {
    view: [[f32; 4]; 4],
    settings: CascadeSettings,
    padding: f32,
}

struct ShadowCache {
    scene: SceneView,
    resolution: u32,
    filter_radius: f32,
    max_bytes: u64,
    active: Vec<usize>,
    cascades: Option<CascadeKey>,
}

#[derive(Clone, Copy)]
enum PipelineKind {
    Mesh,
    Wire,
    Sky,
    ShadowCatcher,
    /// A catcher's depth alone, which hides what lies below the ground.
    Holdout,
    Decal,
    FlagSimple,
}

/// The holdout's depth bias (Depth32Float units of about 2^-24, and per unit of
/// depth slope): the ground's depth moves slightly away, so geometry on it, such
/// as a model's feet or a decal, and the catcher itself still pass the depth test.
const HOLDOUT_BIAS: wgpu::DepthBiasState = wgpu::DepthBiasState {
    constant: 16,
    slope_scale: 1.0,
    clamp: 0.0,
};

/// One pipeline per hardware face culling. Single-sided parts cull their back,
/// which is the rasterizer's front face when their model mirrors; double-sided
/// parts cull nothing. The shaders discard the same fragments (`front_facing`
/// follows the same winding); culling skips them before rasterization.
struct Culled([wgpu::RenderPipeline; 3]);

impl Culled {
    fn new(create: impl FnMut(Option<wgpu::Face>) -> wgpu::RenderPipeline) -> Self {
        Self([None, Some(wgpu::Face::Back), Some(wgpu::Face::Front)].map(create))
    }

    fn get(&self, cull: Option<wgpu::Face>) -> &wgpu::RenderPipeline {
        &self.0[match cull {
            None => 0,
            Some(wgpu::Face::Back) => 1,
            Some(wgpu::Face::Front) => 2,
        }]
    }
}

fn scene_pipeline(
    device: &wgpu::Device,
    layout: &wgpu::PipelineLayout,
    shader: &wgpu::ShaderModule,
    format: wgpu::TextureFormat,
    samples: u32,
    kind: PipelineKind,
    cull: Option<wgpu::Face>,
) -> wgpu::RenderPipeline {
    raster_pipeline(
        device,
        layout,
        shader,
        format,
        samples,
        kind,
        Vertices::Mesh(cull),
    )
}

/// Where a pipeline's vertices come from: mesh buffers, with the faces to cull, or
/// the flag surface by vertex index (flags are double-sided).
#[derive(Clone, Copy)]
enum Vertices {
    Mesh(Option<wgpu::Face>),
    Flag,
}

/// Physically shaded HDR pipelines, which cull the faces their shaders discard.
fn shaded_pipeline(
    device: &wgpu::Device,
    layout: &wgpu::PipelineLayout,
    shader: &wgpu::ShaderModule,
    samples: u32,
    kind: PipelineKind,
) -> Culled {
    Culled::new(|cull| {
        let format = wgpu::TextureFormat::Rgba16Float;
        scene_pipeline(device, layout, shader, format, samples, kind, cull)
    })
}

fn raster_pipeline(
    device: &wgpu::Device,
    layout: &wgpu::PipelineLayout,
    shader: &wgpu::ShaderModule,
    format: wgpu::TextureFormat,
    samples: u32,
    kind: PipelineKind,
    vertices: Vertices,
) -> wgpu::RenderPipeline {
    let (flag, cull) = match vertices {
        Vertices::Mesh(cull) => (false, cull),
        Vertices::Flag => (true, None),
    };
    let sky = matches!(kind, PipelineKind::Sky);
    let wire = matches!(kind, PipelineKind::Wire);
    let catcher = matches!(kind, PipelineKind::ShadowCatcher);
    let decal = matches!(kind, PipelineKind::Decal);
    let holdout = matches!(kind, PipelineKind::Holdout);
    let buffers = [Some(vertex_layout())];
    device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some("shared scene raster"),
        layout: Some(layout),
        vertex: wgpu::VertexState {
            module: shader,
            entry_point: Some(if sky {
                "vs_sky"
            } else if flag {
                "vs_flag"
            } else {
                "vs_main"
            }),
            compilation_options: Default::default(),
            buffers: if sky || flag { &[] } else { &buffers },
        },
        fragment: Some(wgpu::FragmentState {
            module: shader,
            entry_point: Some(if sky {
                "fs_sky"
            } else if wire {
                "fs_wire"
            } else if catcher {
                "fs_shadow_catcher"
            } else if holdout {
                "fs_holdout"
            } else if decal {
                "fs_decal"
            } else if matches!(kind, PipelineKind::FlagSimple) {
                "fs_flag_simple"
            } else {
                "fs_main"
            }),
            compilation_options: Default::default(),
            targets: &[Some(wgpu::ColorTargetState {
                format,
                // Premultiplied colour over the destination: a catcher's black
                // multiplies it by 1 - alpha; a decal adds alpha * colour.
                blend: (catcher || decal).then_some(wgpu::BlendState::PREMULTIPLIED_ALPHA_BLENDING),
                write_mask: if holdout {
                    wgpu::ColorWrites::empty()
                } else {
                    wgpu::ColorWrites::ALL
                },
            })],
        }),
        primitive: wgpu::PrimitiveState {
            topology: if wire {
                wgpu::PrimitiveTopology::LineList
            } else {
                wgpu::PrimitiveTopology::TriangleList
            },
            cull_mode: cull,
            ..Default::default()
        },
        depth_stencil: Some(wgpu::DepthStencilState {
            format: wgpu::TextureFormat::Depth32Float,
            depth_write_enabled: Some(!sky && !decal),
            depth_compare: Some(if sky {
                wgpu::CompareFunction::Always
            } else {
                wgpu::CompareFunction::Less
            }),
            stencil: Default::default(),
            bias: if holdout {
                HOLDOUT_BIAS
            } else {
                Default::default()
            },
        }),
        multisample: wgpu::MultisampleState {
            count: samples,
            ..Default::default()
        },
        multiview_mask: None,
        cache: None,
    })
}

/// Shared mesh/depth renderer with diagnostic unlit and textured PBR paths.
/// Prepare and submit a frame before preparing another frame with these uniforms.
pub struct MeshRenderer {
    settings: RenderSettings,
    format: wgpu::TextureFormat,
    shader: wgpu::ShaderModule,
    pipeline_layout: wgpu::PipelineLayout,
    sky_layout: wgpu::PipelineLayout,
    wire_pipeline: wgpu::RenderPipeline,
    overlays: Overlays,
    msaa: Option<(ImageExtent, wgpu::TextureFormat, wgpu::TextureView)>,
    pipeline: wgpu::RenderPipeline,
    pbr_pipeline: Culled,
    catcher_pipeline: Culled,
    holdout_pipeline: Culled,
    decal_pipeline: Culled,
    sky_pipeline: wgpu::RenderPipeline,
    environment: EnvironmentGpu,
    camera: wgpu::Buffer,
    camera_group: wgpu::BindGroup,
    object_layout: wgpu::BindGroupLayout,
    objects: wgpu::Buffer,
    instances: wgpu::Buffer,
    object_group: wgpu::BindGroup,
    stride: u64,
    capacity: u64,
    instance_capacity: u64,
    depth: Option<(ImageExtent, wgpu::TextureView)>,
    meshes: HashMap<MeshKey, MeshBuffers>,
    draws: Vec<Draw>,
    maps: MaterialMapCache,
    lights: wgpu::Buffer,
    ltc: wgpu::Buffer,
    light_group: wgpu::BindGroup,
    light_layout: wgpu::BindGroupLayout,
    light_capacity: u64,
    tone_pipeline: wgpu::RenderPipeline,
    tone_layout: wgpu::BindGroupLayout,
    tone_settings: wgpu::Buffer,
    hdr: Option<(ImageExtent, wgpu::TextureView, wgpu::BindGroup)>,
    pbr_active: bool,
    camera_layout: wgpu::BindGroupLayout,
    shadow_pipeline: Culled,
    point_shadow_pipeline: Culled,
    shadow_atlas: ShadowAtlas,
    shadow_cameras: Vec<(usize, bool, wgpu::BindGroup)>,
    shadow_projections: Vec<Option<ShadowProjection>>,
    shadow_layers: Vec<Option<usize>>,
    shadow_cache: Option<ShadowCache>,
    shadow_dirty: bool,
    shadow_passes_last_frame: u32,
    flags: FlagGpu,
    flag_shader: wgpu::ShaderModule,
    /// Raster and shadow layouts of flag draws.
    flag_layouts: [wgpu::PipelineLayout; 2],
    flag_pipelines: FlagPipelines,
    animation_seconds: f64,
}

/// Flag variants of the mesh pipelines; their vertex stages read the flag surface.
struct FlagPipelines {
    mesh: wgpu::RenderPipeline,
    pbr: wgpu::RenderPipeline,
    simple: wgpu::RenderPipeline,
    wire: wgpu::RenderPipeline,
    shadow: wgpu::RenderPipeline,
    point_shadow: wgpu::RenderPipeline,
}

impl FlagPipelines {
    fn new(
        device: &wgpu::Device,
        [layout, shadow_layout]: [&wgpu::PipelineLayout; 2],
        shader: &wgpu::ShaderModule,
        format: wgpu::TextureFormat,
        samples: u32,
    ) -> Self {
        let raster = |format, kind| {
            raster_pipeline(
                device,
                layout,
                shader,
                format,
                samples,
                kind,
                Vertices::Flag,
            )
        };
        let hdr = wgpu::TextureFormat::Rgba16Float;
        Self {
            mesh: raster(format, PipelineKind::Mesh),
            pbr: raster(hdr, PipelineKind::Mesh),
            simple: raster(hdr, PipelineKind::FlagSimple),
            wire: raster(format, PipelineKind::Wire),
            shadow: depth_pipeline(device, shadow_layout, shader, true, "fs_shadow", None),
            point_shadow: depth_pipeline(
                device,
                shadow_layout,
                shader,
                true,
                "fs_shadow_point",
                None,
            ),
        }
    }
}

impl MeshRenderer {
    pub fn new(device: &wgpu::Device, format: wgpu::TextureFormat) -> Self {
        let light_size = std::mem::size_of::<LightUniform>() as u64;
        let camera_layout = uniform_layout(device, false, CAMERA_UNIFORM_SIZE);
        let object_layout = object_layout(device);
        let camera = uniform_buffer(device, CAMERA_UNIFORM_SIZE);
        let camera_group = uniform_group(device, &camera_layout, &camera, CAMERA_UNIFORM_SIZE);
        let maps = MaterialMapCache::new(device);
        let environment = EnvironmentGpu::new(device);
        let mut light_entries = vec![
            wgpu::BindGroupLayoutEntry {
                binding: 0,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Buffer {
                    ty: wgpu::BufferBindingType::Storage { read_only: true },
                    has_dynamic_offset: false,
                    min_binding_size: NonZeroU64::new(light_size),
                },
                count: None,
            },
            wgpu::BindGroupLayoutEntry {
                binding: 1,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Texture {
                    sample_type: wgpu::TextureSampleType::Depth,
                    view_dimension: wgpu::TextureViewDimension::D2Array,
                    multisampled: false,
                },
                count: None,
            },
            wgpu::BindGroupLayoutEntry {
                binding: 2,
                visibility: wgpu::ShaderStages::FRAGMENT,
                ty: wgpu::BindingType::Sampler(wgpu::SamplerBindingType::Comparison),
                count: None,
            },
        ];
        light_entries.extend(environment::layout_entries());
        light_entries.push(wgpu::BindGroupLayoutEntry {
            binding: 7,
            visibility: wgpu::ShaderStages::FRAGMENT,
            ty: wgpu::BindingType::Buffer {
                ty: wgpu::BufferBindingType::Storage { read_only: true },
                has_dynamic_offset: false,
                min_binding_size: NonZeroU64::new(64 * 64 * 32),
            },
            count: None,
        });
        let light_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("scene lights and environment"),
            entries: &light_entries,
        });
        let lights = device.create_buffer(&wgpu::BufferDescriptor {
            label: Some("scene lights"),
            size: light_size,
            usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
            mapped_at_creation: false,
        });
        let shadow_atlas = ShadowAtlas::new(device, 1, 1);
        let ltc = ltc_table(device);
        let light_group = light_group(
            device,
            &light_layout,
            &lights,
            light_size,
            &shadow_atlas,
            &environment,
            &ltc,
        );
        let tone_settings = uniform_buffer(device, 16);
        let tone_layout = device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
            label: Some("tone map"),
            entries: &[
                texture_entry(0),
                wgpu::BindGroupLayoutEntry {
                    binding: 1,
                    visibility: wgpu::ShaderStages::FRAGMENT,
                    ty: wgpu::BindingType::Buffer {
                        ty: wgpu::BufferBindingType::Uniform,
                        has_dynamic_offset: false,
                        min_binding_size: NonZeroU64::new(16),
                    },
                    count: None,
                },
            ],
        });
        let tone_pipeline_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("tone map"),
            bind_group_layouts: &[Some(&tone_layout)],
            immediate_size: 0,
        });
        let tone_shader = device.create_shader_module(wgpu::include_wgsl!("tone_map.wgsl"));
        let tone_pipeline =
            fullscreen_pipeline(device, &tone_pipeline_layout, &tone_shader, format);
        let stride = OBJECT_BYTES.div_ceil(u64::from(
            device.limits().min_uniform_buffer_offset_alignment,
        )) * u64::from(device.limits().min_uniform_buffer_offset_alignment);
        let objects = uniform_buffer(device, stride);
        let instances = instance_buffer(device, INSTANCE_BYTES);
        let object_group = object_group(device, &object_layout, &objects, &instances);
        let layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("mesh layout"),
            bind_group_layouts: &[
                Some(&camera_layout),
                Some(&object_layout),
                Some(&maps.layout),
                Some(&light_layout),
            ],
            immediate_size: 0,
        });
        let shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("mesh and environment shading"),
            source: environment::shader_source(include_str!("mesh.wgsl")),
        });
        let sky_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("sky background"),
            bind_group_layouts: &[None, None, None, Some(&light_layout)],
            immediate_size: 0,
        });
        let sky_pipeline = scene_pipeline(
            device,
            &sky_layout,
            &shader,
            wgpu::TextureFormat::Rgba16Float,
            1,
            PipelineKind::Sky,
            None,
        );
        let shadow_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("shadow layout"),
            bind_group_layouts: &[
                Some(&camera_layout),
                Some(&object_layout),
                Some(&maps.layout),
            ],
            immediate_size: 0,
        });
        let shadow_pipeline = Culled::new(|cull| {
            depth_pipeline(device, &shadow_layout, &shader, false, "fs_shadow", cull)
        });
        let point_shadow_pipeline = Culled::new(|cull| {
            depth_pipeline(
                device,
                &shadow_layout,
                &shader,
                false,
                "fs_shadow_point",
                cull,
            )
        });
        // Diagnostic and unlit views show back faces: only shaded pipelines cull.
        let pipeline = scene_pipeline(
            device,
            &layout,
            &shader,
            format,
            1,
            PipelineKind::Mesh,
            None,
        );
        let pbr_pipeline = shaded_pipeline(device, &layout, &shader, 1, PipelineKind::Mesh);
        let catcher_pipeline =
            shaded_pipeline(device, &layout, &shader, 1, PipelineKind::ShadowCatcher);
        let holdout_pipeline = shaded_pipeline(device, &layout, &shader, 1, PipelineKind::Holdout);
        let decal_pipeline = shaded_pipeline(device, &layout, &shader, 1, PipelineKind::Decal);
        let wire_pipeline = scene_pipeline(
            device,
            &layout,
            &shader,
            format,
            1,
            PipelineKind::Wire,
            None,
        );
        let flags = FlagGpu::new(device);
        let flag_shader = device.create_shader_module(wgpu::ShaderModuleDescriptor {
            label: Some("flag shading"),
            source: environment::shader_source(&format!(
                "{}{}\n{}",
                flag::prelude(),
                include_str!("mesh.wgsl"),
                include_str!("flag_surface.wgsl")
            )),
        });
        let flag_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("flag layout"),
            bind_group_layouts: &[
                Some(&camera_layout),
                Some(&flags.draw_layout),
                Some(&maps.layout),
                Some(&light_layout),
            ],
            immediate_size: 0,
        });
        let flag_shadow_layout = device.create_pipeline_layout(&wgpu::PipelineLayoutDescriptor {
            label: Some("flag shadow layout"),
            bind_group_layouts: &[
                Some(&camera_layout),
                Some(&flags.draw_layout),
                Some(&maps.layout),
            ],
            immediate_size: 0,
        });
        let flag_pipelines = FlagPipelines::new(
            device,
            [&flag_layout, &flag_shadow_layout],
            &flag_shader,
            format,
            1,
        );
        let overlays = Overlays::new(device, format, &camera_layout);
        Self {
            settings: RenderSettings::default(),
            format,
            shader,
            pipeline_layout: layout,
            sky_layout,
            wire_pipeline,
            overlays,
            msaa: None,
            pipeline,
            camera,
            camera_group,
            object_layout,
            objects,
            instances,
            object_group,
            stride,
            capacity: stride,
            instance_capacity: INSTANCE_BYTES,
            depth: None,
            meshes: HashMap::new(),
            draws: Vec::new(),
            pbr_pipeline,
            catcher_pipeline,
            holdout_pipeline,
            decal_pipeline,
            sky_pipeline,
            environment,
            maps,
            lights,
            ltc,
            light_group,
            light_layout,
            light_capacity: light_size,
            tone_pipeline,
            tone_layout,
            tone_settings,
            hdr: None,
            pbr_active: false,
            camera_layout,
            shadow_pipeline,
            point_shadow_pipeline,
            shadow_atlas,
            shadow_cameras: Vec::new(),
            shadow_projections: Vec::new(),
            shadow_layers: Vec::new(),
            shadow_cache: None,
            shadow_dirty: false,
            shadow_passes_last_frame: 0,
            flags,
            flag_shader,
            flag_layouts: [flag_layout, flag_shadow_layout],
            flag_pipelines,
            animation_seconds: 0.0,
        }
    }

    /// Time of the next prepared frame in seconds, for animated content such as
    /// SimpleWave flags. Use the frame's own media time so renders are reproducible.
    /// Native flags run on their own clock (`scene::Cloth::seconds`).
    pub fn set_animation_time(&mut self, seconds: f64) {
        self.animation_seconds = seconds;
    }

    /// Validate before changing state. Prepare again before encoding after any change.
    /// Only main raster pipelines/attachments change; scene, IBL and shadow caches survive.
    pub fn set_render_settings(
        &mut self,
        gpu: &Gpu,
        settings: RenderSettings,
    ) -> Result<(), BoxError> {
        settings.validate(gpu, self.format)?;
        if self.settings.sample_count != settings.sample_count {
            let samples = settings.sample_count;
            let create = |kind| {
                scene_pipeline(
                    &gpu.device,
                    &self.pipeline_layout,
                    &self.shader,
                    self.format,
                    samples,
                    kind,
                    None,
                )
            };
            let shaded = |kind| {
                shaded_pipeline(
                    &gpu.device,
                    &self.pipeline_layout,
                    &self.shader,
                    samples,
                    kind,
                )
            };
            self.pipeline = create(PipelineKind::Mesh);
            self.pbr_pipeline = shaded(PipelineKind::Mesh);
            self.catcher_pipeline = shaded(PipelineKind::ShadowCatcher);
            self.holdout_pipeline = shaded(PipelineKind::Holdout);
            self.decal_pipeline = shaded(PipelineKind::Decal);
            self.wire_pipeline = create(PipelineKind::Wire);
            self.sky_pipeline = scene_pipeline(
                &gpu.device,
                &self.sky_layout,
                &self.shader,
                wgpu::TextureFormat::Rgba16Float,
                samples,
                PipelineKind::Sky,
                None,
            );
            self.flag_pipelines = FlagPipelines::new(
                &gpu.device,
                [&self.flag_layouts[0], &self.flag_layouts[1]],
                &self.flag_shader,
                self.format,
                settings.sample_count,
            );
            self.depth = None;
            self.msaa = None;
        }
        self.settings = settings;
        Ok(())
    }

    pub fn render_settings(&self) -> RenderSettings {
        self.settings
    }

    /// The destination must have the camera's extent, the constructor's format and one sample.
    pub fn prepare(
        &mut self,
        gpu: &Gpu,
        scene: &SceneView,
        camera: &dyn CameraView,
    ) -> Result<(), BoxError> {
        self.prepare_mode(gpu, scene, camera, None)
    }

    /// Scene-linear HDR shading followed by one output exposure/tone-map pass.
    pub fn prepare_pbr(
        &mut self,
        gpu: &Gpu,
        scene: &SceneView,
        camera: &dyn CameraView,
        settings: PbrSettings,
    ) -> Result<(), BoxError> {
        if !settings.exposure.is_finite() || settings.exposure < 0.0 {
            return Err("exposure must be finite and nonnegative".into());
        }
        settings.shadows.validate()?;
        let quality = settings.shadows.pcss;
        let mut counts = [0_u64; 4];
        for light in scene.lighting().lights() {
            let blended = matches!(light, Light::Directional { .. })
                && settings.shadows.cascades.count > 1
                && settings.shadows.cascades.blend_fraction > 0.0;
            counts[light.effective_shadow_mode(settings.shadows.mode) as usize] +=
                if blended { 2 } else { 1 };
        }
        if counts[ShadowMode::Pcf as usize] > 0
            && ![1, 4, 9, 16, 25, 36, 49, 64].contains(&settings.shadows.pcf_samples)
        {
            return Err("PCF requires a square kernel of 1..=64 samples".into());
        }
        if counts[ShadowMode::Pcss as usize] > 0 {
            quality.validate()?;
        }
        let samples = counts[ShadowMode::Hard as usize]
            + counts[ShadowMode::Pcf as usize] * u64::from(settings.shadows.pcf_samples)
            + counts[ShadowMode::Pcss as usize]
                * (u64::from(quality.blocker_samples) + u64::from(quality.filter_samples));
        if samples > settings.shadows.max_samples_per_fragment {
            return Err(
                "shadow filtering exceeds the configured per-fragment sample budget".into(),
            );
        }
        self.prepare_mode(gpu, scene, camera, Some(settings))
    }

    fn prepare_mode(
        &mut self,
        gpu: &Gpu,
        scene: &SceneView,
        camera: &dyn CameraView,
        settings: Option<PbrSettings>,
    ) -> Result<(), BoxError> {
        self.settings.validate(gpu, self.format)?;
        let matrix = camera.clip_from_world()?;
        let extent = camera.extent();
        let limits = gpu.device.limits();
        if extent.width() > limits.max_texture_dimension_2d
            || extent.height() > limits.max_texture_dimension_2d
        {
            return Err("mesh target exceeds texture limits".into());
        }
        let mut parts = Vec::new();
        let mut used = HashSet::new();
        let mut used_materials = HashSet::new();
        let unlit_maps = MaterialMaps::default();
        let mut flags = Vec::new();
        for (instance_id, instance) in scene.instances().filter(|(_, i)| i.visible()) {
            let mesh = scene.mesh(instance.mesh())?;
            let id = mesh.content_key();
            // A flag grid has one part, so each flag instance is one draw and one slot.
            let flag = instance.flag().map(|flag| {
                flags.push(FlagDraw::new(
                    instance_id,
                    flag,
                    instance.model(),
                    self.animation_seconds,
                ));
                (flags.len() - 1, flag.shading)
            });
            let rows = instance.model().rows();
            let model: [[f32; 4]; 4] =
                std::array::from_fn(|column| std::array::from_fn(|row| rows[row][column] as f32));
            let n = instance.model().normal_matrix();
            let normal =
                std::array::from_fn(|c| [n[0][c] as f32, n[1][c] as f32, n[2][c] as f32, 0.0]);
            if !model
                .iter()
                .flatten()
                .chain(normal.iter().flatten())
                .all(|v| v.is_finite())
            {
                return Err("model exceeds GPU float range".into());
            }
            let mirrored = instance.model().is_mirrored();
            for (part, material) in mesh.parts().iter().zip(instance.materials()) {
                let factors = scene.material(*material)?;
                let maps = scene.material_maps(*material)?;
                let bound = if settings.is_some() {
                    maps
                } else {
                    &unlit_maps
                };
                self.maps.prepare(gpu, *material, bound)?;
                let layers = factors.layers;
                let (sin, cos) = layers.anisotropy_rotation.sin_cos();
                let mut uv = [[0.0; 4]; 2 * MapSlot::COUNT];
                for slot in MapSlot::ALL {
                    let rows = bound.transform(slot).rows();
                    for (r, row) in rows.into_iter().enumerate() {
                        uv[2 * slot as usize + r] = [row[0], row[1], row[2], 0.0];
                    }
                }
                used_materials.insert(*material);
                let part = Part {
                    mesh: id,
                    indices: part.clone(),
                    material: *material,
                    mirrored,
                    shadow_catcher: factors.shadow_catcher && flag.is_none(),
                    decal: factors.decal && flag.is_none(),
                    holdout: factors.holdout && flag.is_none(),
                    flag,
                    uniform: DrawUniform {
                        color: factors.base_color,
                        emissive_cutoff: [
                            factors.emissive[0],
                            factors.emissive[1],
                            factors.emissive[2],
                            factors.alpha_cutoff.unwrap_or(-1.0),
                        ],
                        factors: [
                            factors.metallic,
                            factors.roughness,
                            factors.normal_scale,
                            factors.occlusion_strength,
                        ],
                        flags: [
                            u32::from(maps.normal.is_some()),
                            u32::from(factors.unlit),
                            u32::from(factors.double_sided || flag.is_some()),
                            u32::from(mirrored),
                        ],
                        specular: [
                            layers.specular_color[0],
                            layers.specular_color[1],
                            layers.specular_color[2],
                            layers.specular,
                        ],
                        coat: [
                            layers.ior,
                            layers.clearcoat,
                            layers.clearcoat_roughness,
                            layers.clearcoat_normal_scale,
                        ],
                        sheen: [
                            layers.sheen_color[0],
                            layers.sheen_color[1],
                            layers.sheen_color[2],
                            layers.sheen_roughness,
                        ],
                        iridescence: [
                            layers.iridescence,
                            layers.iridescence_ior,
                            layers.iridescence_thickness[0],
                            layers.iridescence_thickness[1],
                        ],
                        anisotropy: [layers.anisotropy_strength, cos, sin, 0.0],
                        maps: [
                            MapSlot::ALL
                                .into_iter()
                                .filter(|&slot| bound.get(slot).is_some())
                                .fold(0, |bits, slot| bits | 1 << slot as u32),
                            0,
                            0,
                            0,
                        ],
                        uv,
                    },
                };
                parts.push((part, InstanceUniform { model, normal }));
            }
            let indices = bytemuck::cast_slice(mesh.indices());
            if indices.len() as u64 > limits.max_buffer_size {
                return Err("mesh exceeds device buffer limits".into());
            }
            if mesh.positions().len() as u64 * 64 > limits.max_buffer_size {
                return Err("mesh vertex attributes exceed GPU limits".into());
            }
            self.meshes.entry(id).or_insert_with(|| {
                let attrs = mesh.attributes();
                let vertices: Vec<_> = mesh
                    .positions()
                    .iter()
                    .enumerate()
                    .map(|(i, &position)| Vertex {
                        position,
                        normal: attrs.normals.get(i).copied().unwrap_or([0.0; 3]),
                        uv: attrs.tex_coords.get(i).copied().unwrap_or([0.0; 2]),
                        tangent: attrs
                            .tangents
                            .get(i)
                            .copied()
                            .unwrap_or([0.0, 0.0, 0.0, 1.0]),
                        color: attrs.colors.get(i).copied().unwrap_or([1.0; 4]),
                    })
                    .collect();
                MeshBuffers {
                    _source: mesh.clone(),
                    lines: None,
                    vertices: gpu
                        .device
                        .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                            label: Some("mesh surface vertices"),
                            contents: bytemuck::cast_slice(&vertices),
                            usage: wgpu::BufferUsages::VERTEX,
                        }),
                    indices: gpu
                        .device
                        .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                            label: Some("mesh indices"),
                            contents: indices,
                            usage: wgpu::BufferUsages::INDEX,
                        }),
                }
            });
            if self.settings.view == DiagnosticView::Wireframe {
                let count = mesh.indices().len() as u64 * 2;
                if count > u64::from(u32::MAX) || count * 4 > limits.max_buffer_size {
                    return Err("wireframe indices exceed GPU limits".into());
                }
                let buffers = self.meshes.get_mut(&id).expect("mesh inserted above");
                buffers.lines.get_or_insert_with(|| {
                    gpu.device
                        .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                            label: Some("derived mesh line indices"),
                            contents: bytemuck::cast_slice(&line_indices(mesh.indices())),
                            usage: wgpu::BufferUsages::INDEX,
                        })
                });
            }
            used.insert(id);
        }
        let batches = batch(parts);
        let mut objects = vec![0_u8; (self.stride as usize) * batches.len().max(1)];
        let mut instances = Vec::with_capacity(batches.iter().map(|b| b.instances.len()).sum());
        let mut draws = Vec::with_capacity(batches.len());
        for (
            index,
            Batch {
                part,
                instances: batch,
            },
        ) in batches.into_iter().enumerate()
        {
            let offset = index * self.stride as usize;
            objects[offset..offset + OBJECT_BYTES as usize]
                .copy_from_slice(bytemuck::bytes_of(&part.uniform));
            let first = u32::try_from(instances.len())?;
            instances.extend(batch);
            draws.push(Draw {
                mesh: part.mesh,
                indices: part.indices,
                offset: u32::try_from(offset)?,
                instances: first..u32::try_from(instances.len())?,
                material: part.material,
                cull: (part.uniform.flags[2] == 0).then_some(if part.mirrored {
                    wgpu::Face::Front
                } else {
                    wgpu::Face::Back
                }),
                shadow_catcher: part.shadow_catcher,
                decal: part.decal,
                holdout: part.holdout,
                flag: part.flag,
            });
        }
        let bytes = objects.len() as u64;
        let instance_bytes = INSTANCE_BYTES * instances.len().max(1) as u64;
        if bytes > limits.max_buffer_size || instance_bytes > limits.max_storage_buffer_binding_size
        {
            return Err("mesh draw uniforms exceed device buffer limit".into());
        }
        if bytes > self.capacity || instance_bytes > self.instance_capacity {
            if bytes > self.capacity {
                self.objects = uniform_buffer(&gpu.device, bytes);
                self.capacity = bytes;
            }
            if instance_bytes > self.instance_capacity {
                self.instances = instance_buffer(&gpu.device, instance_bytes);
                self.instance_capacity = instance_bytes;
            }
            self.object_group = object_group(
                &gpu.device,
                &self.object_layout,
                &self.objects,
                &self.instances,
            );
        }
        // Cached shadow maps stay valid for the scene, but not once a flag moves.
        self.shadow_dirty |= self
            .flags
            .prepare(gpu, &flags, &self.objects, &self.instances);
        if self.depth.as_ref().is_none_or(|(size, _)| *size != extent) {
            let texture = gpu.device.create_texture(&wgpu::TextureDescriptor {
                label: Some("mesh depth"),
                size: wgpu::Extent3d {
                    width: extent.width(),
                    height: extent.height(),
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: self.settings.sample_count,
                dimension: wgpu::TextureDimension::D2,
                format: wgpu::TextureFormat::Depth32Float,
                usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
                view_formats: &[],
            });
            self.depth = Some((extent, texture.create_view(&Default::default())));
        }
        let physical = settings.is_some() && self.settings.view == DiagnosticView::Shaded;
        let color_format = if physical {
            wgpu::TextureFormat::Rgba16Float
        } else {
            self.format
        };
        if self.settings.sample_count > 1
            && self
                .msaa
                .as_ref()
                .is_none_or(|(size, format, _)| *size != extent || *format != color_format)
        {
            let texture = gpu.device.create_texture(&wgpu::TextureDescriptor {
                label: Some("multisampled scene colour"),
                size: wgpu::Extent3d {
                    width: extent.width(),
                    height: extent.height(),
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: self.settings.sample_count,
                dimension: wgpu::TextureDimension::D2,
                format: color_format,
                usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
                view_formats: &[],
            });
            self.msaa = Some((
                extent,
                color_format,
                texture.create_view(&Default::default()),
            ));
        }
        if settings.is_some() && self.hdr.as_ref().is_none_or(|(size, _, _)| *size != extent) {
            let texture = gpu.device.create_texture(&wgpu::TextureDescriptor {
                label: Some("scene linear HDR"),
                size: wgpu::Extent3d {
                    width: extent.width(),
                    height: extent.height(),
                    depth_or_array_layers: 1,
                },
                mip_level_count: 1,
                sample_count: 1,
                dimension: wgpu::TextureDimension::D2,
                format: wgpu::TextureFormat::Rgba16Float,
                usage: wgpu::TextureUsages::RENDER_ATTACHMENT
                    | wgpu::TextureUsages::TEXTURE_BINDING,
                view_formats: &[],
            });
            let view = texture.create_view(&Default::default());
            let group = gpu.device.create_bind_group(&wgpu::BindGroupDescriptor {
                label: Some("HDR output"),
                layout: &self.tone_layout,
                entries: &[
                    wgpu::BindGroupEntry {
                        binding: 0,
                        resource: wgpu::BindingResource::TextureView(&view),
                    },
                    wgpu::BindGroupEntry {
                        binding: 1,
                        resource: self.tone_settings.as_entire_binding(),
                    },
                ],
            });
            self.hdr = Some((extent, view, group));
        }
        let mut lights: Vec<_> = if settings.is_some() {
            scene
                .lighting()
                .lights()
                .iter()
                .copied()
                .map(Self::pack_light)
                .collect::<Result<_, _>>()?
        } else {
            Vec::new()
        };
        let shadows = settings.map_or(ShadowSettings::default(), |s| s.shadows);
        let environment_changed = self.environment.prepare(
            gpu,
            if settings.is_some() {
                scene.lighting().sky()
            } else {
                None
            },
            if settings.is_some() {
                scene.lighting().constant()
            } else {
                [0.0; 3]
            },
            camera,
            settings.map_or(EnvironmentSettings::default(), |s| s.environment),
        )?;
        let atlas_changed =
            self.prepare_shadows(gpu, scene, camera, settings.map(|s| s.shadows))?;
        for (index, light) in lights.iter_mut().enumerate() {
            light.pcss[3] =
                scene.lighting().lights()[index].effective_shadow_mode(shadows.mode) as u32 as f32;
            if let Some(layer) = self.shadow_layers.get(index).copied().flatten()
                && let Some(projection) = self.shadow_projections.get(layer).copied().flatten()
            {
                let count = if matches!(scene.lighting().lights()[index], Light::Directional { .. })
                {
                    shadows.cascades.count as usize
                } else {
                    1
                };
                for cascade in 0..count {
                    if let Some(map) = self.shadow_projections[layer + cascade] {
                        light.shadow_matrix[cascade] = map.matrix;
                        light.shadow_depth[cascade] = map.depth_scale;
                        light.cascade_splits[cascade] = map.split[1];
                    }
                }
                light.cascade_params = [
                    projection.split[0],
                    shadows.cascades.blend_fraction as f32,
                    0.0,
                    0.0,
                ];
                light.pcss[1] = shadows.pcss.max_search_radius;
                light.pcss[2] = shadows.pcss.max_filter_radius;
                light.pcss_samples = [
                    shadows.pcss.blocker_samples as f32,
                    shadows.pcss.filter_samples as f32,
                    shadows.pcf_samples as f32,
                    count as f32,
                ];
                light.shadow_params = [
                    layer as f32,
                    shadows.depth_bias,
                    shadows.normal_bias,
                    shadows.filter_radius,
                ];
            }
        }
        let bytes = std::mem::size_of::<LightUniform>() as u64 * lights.len().max(1) as u64;
        if bytes > limits.max_storage_buffer_binding_size {
            return Err("scene lights exceed storage binding limit".into());
        }
        let buffer_changed = bytes > self.light_capacity;
        if buffer_changed {
            self.lights = gpu.device.create_buffer(&wgpu::BufferDescriptor {
                label: Some("scene lights"),
                size: bytes,
                usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
                mapped_at_creation: false,
            });
            self.light_capacity = bytes;
        }
        if buffer_changed || atlas_changed || environment_changed {
            self.light_group = light_group(
                &gpu.device,
                &self.light_layout,
                &self.lights,
                self.light_capacity,
                &self.shadow_atlas,
                &self.environment,
                &self.ltc,
            );
        }
        if !lights.is_empty() {
            gpu.queue
                .write_buffer(&self.lights, 0, bytemuck::cast_slice(&lights));
        }
        let eye = camera.center().map(|v| v as f32);
        let forward = camera.forward();
        let optical_depth = [
            forward[0] as f32,
            forward[1] as f32,
            forward[2] as f32,
            -forward
                .iter()
                .zip(camera.center())
                .map(|(a, b)| a * b)
                .sum::<f64>() as f32,
        ];
        if !eye.into_iter().chain(optical_depth).all(f32::is_finite) {
            return Err("camera exceeds GPU float range".into());
        }
        let camera_uniform = CameraUniform {
            clip: matrix,
            eye: [eye[0], eye[1], eye[2], 0.0],
            options: [
                u32::from(settings.is_some()),
                lights.len() as u32,
                u32::from(camera.is_orthographic()),
                self.settings.view as u32,
            ],
            optical_depth,
        };
        gpu.queue
            .write_buffer(&self.camera, 0, bytemuck::bytes_of(&camera_uniform));
        if let Some(settings) = settings {
            gpu.queue.write_buffer(
                &self.tone_settings,
                0,
                bytemuck::cast_slice(&[
                    settings.exposure,
                    match settings.tone_map {
                        ToneMap::Reinhard => 0.0,
                        ToneMap::Aces => 1.0,
                    },
                    0.0,
                    0.0,
                ]),
            );
        }
        gpu.queue.write_buffer(&self.objects, 0, &objects);
        if !instances.is_empty() {
            gpu.queue
                .write_buffer(&self.instances, 0, bytemuck::cast_slice(&instances));
        }
        self.meshes.retain(|id, _| used.contains(id));
        self.maps.retain(&used_materials);
        self.draws = draws;
        self.pbr_active = settings.is_some();
        self.overlays.prepare(gpu, scene, self.settings.overlays)?;
        Ok(())
    }

    /// Record only after successful prepare; GPU buffers stay owned by the renderer.
    pub fn encode(&mut self, encoder: &mut wgpu::CommandEncoder, view: &wgpu::TextureView) {
        self.encode_passes(encoder, view, true);
    }

    pub(crate) fn linear_view(&self) -> Option<&wgpu::TextureView> {
        self.hdr
            .as_ref()
            .filter(|_| self.pbr_active && self.settings.view == DiagnosticView::Shaded)
            .map(|(_, view, _)| view)
    }

    pub(crate) fn encode_linear(&mut self, encoder: &mut wgpu::CommandEncoder) {
        if let Some(view) = self.linear_view().cloned() {
            self.encode_passes(encoder, &view, false);
        }
    }

    fn encode_passes(
        &mut self,
        encoder: &mut wgpu::CommandEncoder,
        view: &wgpu::TextureView,
        output_transform: bool,
    ) {
        self.shadow_passes_last_frame = 0;
        let Some((_, depth)) = &self.depth else {
            return;
        };
        self.flags.encode(encoder);
        if self.shadow_dirty {
            for (layer, point, camera_group) in &self.shadow_cameras {
                self.shadow_passes_last_frame += 1;
                let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                    label: Some("light shadow map"),
                    color_attachments: &[],
                    depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                        view: &self.shadow_atlas.layers[*layer],
                        depth_ops: Some(wgpu::Operations {
                            load: wgpu::LoadOp::Clear(1.0),
                            store: wgpu::StoreOp::Store,
                        }),
                        stencil_ops: None,
                    }),
                    ..Default::default()
                });
                pass.set_bind_group(0, camera_group, &[]);
                let mut bound = None;
                let mut bindings = Bindings::default();
                // Decals lie on the ground they decorate and cast no shadow; nor do holdouts.
                for draw in self
                    .draws
                    .iter()
                    .filter(|draw| !draw.decal && !draw.holdout)
                {
                    let pipeline = match (draw.flag, *point) {
                        (None, false) => self.shadow_pipeline.get(draw.cull),
                        (None, true) => self.point_shadow_pipeline.get(draw.cull),
                        (Some(_), false) => &self.flag_pipelines.shadow,
                        (Some(_), true) => &self.flag_pipelines.point_shadow,
                    };
                    if !bound.is_some_and(|bound| std::ptr::eq(bound, pipeline)) {
                        pass.set_pipeline(pipeline);
                        bound = Some(pipeline);
                    }
                    let mesh = &self.meshes[&draw.mesh];
                    bindings.bind(&mut pass, self, draw, mesh, false);
                    pass.draw_indexed(draw.indices.clone(), 0, draw.instances.clone());
                }
            }
            self.shadow_dirty = false;
        }
        let physical = self.pbr_active && self.settings.view == DiagnosticView::Shaded;
        let hdr = self.hdr.as_ref().filter(|_| physical);
        let destination = hdr.map_or(view, |(_, texture, _)| texture);
        let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: Some("mesh scene"),
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: self.msaa.as_ref().map_or(destination, |(_, _, view)| view),
                depth_slice: None,
                resolve_target: self.msaa.as_ref().map(|_| destination),
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Clear(if output_transform {
                        wgpu::Color {
                            r: 0.018,
                            g: 0.025,
                            b: 0.04,
                            a: 1.0,
                        }
                    } else {
                        wgpu::Color::TRANSPARENT
                    }),
                    store: wgpu::StoreOp::Store,
                },
            })],
            depth_stencil_attachment: Some(wgpu::RenderPassDepthStencilAttachment {
                view: depth,
                depth_ops: Some(wgpu::Operations {
                    load: wgpu::LoadOp::Clear(1.0),
                    store: wgpu::StoreOp::Discard,
                }),
                stencil_ops: None,
            }),
            ..Default::default()
        });
        if physical && self.environment.visible {
            pass.set_pipeline(&self.sky_pipeline);
            pass.set_bind_group(3, &self.light_group, &[]);
            pass.draw(0..3, 0..1);
        }
        let wire = self.settings.view == DiagnosticView::Wireframe;
        let flags = &self.flag_pipelines;
        let flag_pipeline = if wire {
            &flags.wire
        } else if physical {
            &flags.pbr
        } else {
            &flags.mesh
        };
        pass.set_bind_group(0, &self.camera_group, &[]);
        pass.set_bind_group(3, &self.light_group, &[]);
        // Decals and catchers blend over whatever lies behind them, so they follow
        // opaque geometry; catchers come last so their shadows also darken decals.
        // Depth testing hides both behind real objects.
        let deferred = |draw: &&Draw| physical && (draw.shadow_catcher || draw.decal);
        // Holdouts draw only their depth, below.
        let opaque = self
            .draws
            .iter()
            .filter(|draw| !deferred(draw) && !draw.holdout);
        let decals = self.draws.iter().filter(|draw| physical && draw.decal);
        let catchers = self
            .draws
            .iter()
            .filter(|draw| physical && draw.shadow_catcher);
        let mut bindings = Bindings::default();
        if physical {
            // Catchers and holdouts stand for the real ground: their depth comes first,
            // so what lies below it is not drawn and the camera's picture shows there.
            for draw in self
                .draws
                .iter()
                .filter(|draw| draw.shadow_catcher || draw.holdout)
            {
                pass.set_pipeline(self.holdout_pipeline.get(draw.cull));
                bindings.bind(&mut pass, self, draw, &self.meshes[&draw.mesh], false);
                pass.draw_indexed(draw.indices.clone(), 0, draw.instances.clone());
            }
        }
        let mut bound = None;
        for draw in opaque.chain(decals).chain(catchers) {
            let pipeline = match draw.flag {
                Some((_, FlagShading::Simple)) if physical => &flags.simple,
                Some(_) => flag_pipeline,
                None if wire => &self.wire_pipeline,
                None if physical && draw.shadow_catcher => self.catcher_pipeline.get(draw.cull),
                None if physical && draw.decal => self.decal_pipeline.get(draw.cull),
                None if physical => self.pbr_pipeline.get(draw.cull),
                None => &self.pipeline,
            };
            if !bound.is_some_and(|bound| std::ptr::eq(bound, pipeline)) {
                pass.set_pipeline(pipeline);
                bound = Some(pipeline);
            }
            let mesh = &self.meshes[&draw.mesh];
            bindings.bind(&mut pass, self, draw, mesh, wire);
            let indices = if wire {
                draw.indices.start * 2..draw.indices.end * 2
            } else {
                draw.indices.clone()
            };
            pass.draw_indexed(indices, 0, draw.instances.clone());
        }
        drop(pass);
        if let Some((_, _, group)) = hdr.filter(|_| output_transform) {
            let mut pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("single output transform"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                ..Default::default()
            });
            pass.set_pipeline(&self.tone_pipeline);
            pass.set_bind_group(0, group, &[]);
            pass.draw(0..3, 0..1);
        }
        if output_transform {
            self.overlays.encode(encoder, view, &self.camera_group);
        }
    }

    /// Number of draws in each scene pass of the last prepared frame: instances
    /// that share a mesh part, material and handedness are drawn together.
    pub fn draws_per_pass(&self) -> usize {
        self.draws.len()
    }

    fn pack_light(light: Light) -> Result<LightUniform, BoxError> {
        let normalize = |v: [f64; 3]| {
            let scale = v.into_iter().map(f64::abs).fold(0.0, f64::max);
            let v = v.map(|x| x / scale);
            let len = v.into_iter().map(|x| x * x).sum::<f64>().sqrt();
            v.map(|x| (x / len) as f32)
        };
        let mut area = [[0.0; 4]; 2];
        let (position, kind, direction, emission, inner, outer) = match light {
            Light::Directional {
                direction,
                irradiance,
                ..
            } => ([0.0; 3], 0.0, normalize(direction), irradiance, 0.0, 0.0),
            Light::Point {
                position,
                intensity,
                ..
            } => (
                position.map(|v| v as f32),
                1.0,
                [0.0; 3],
                intensity,
                0.0,
                0.0,
            ),
            Light::Spot {
                position,
                direction,
                intensity,
                inner_angle,
                outer_angle,
                ..
            } => (
                position.map(|v| v as f32),
                2.0,
                normalize(direction),
                intensity,
                inner_angle.cos() as f32,
                outer_angle.cos() as f32,
            ),
            Light::Area {
                position,
                normal,
                tangent,
                shape,
                radiance,
                two_sided,
                ..
            } => {
                let n = normalize(normal).map(f64::from);
                let t = normalize(tangent).map(f64::from);
                let along = n.iter().zip(t).map(|(a, b)| a * b).sum::<f64>();
                let tangent = normalize(std::array::from_fn(|i| t[i] - along * n[i]));
                let (code, extent, height) = match shape {
                    AreaShape::Rectangle {
                        half_width,
                        half_height,
                    } => (0.0, half_width, half_height),
                    AreaShape::Disk { radius } => (1.0, radius, 0.0),
                    AreaShape::Sphere { radius } => (2.0, radius, 0.0),
                };
                area = [
                    [tangent[0], tangent[1], tangent[2], extent],
                    [height, 0.0, 0.0, 0.0],
                ];
                (
                    position.map(|v| v as f32),
                    3.0,
                    n.map(|v| v as f32),
                    radiance,
                    f32::from(u8::from(two_sided)),
                    code,
                )
            }
        };
        let uniform = LightUniform {
            position_kind: [position[0], position[1], position[2], kind],
            direction_inner: [direction[0], direction[1], direction[2], inner],
            emission_outer: [emission[0], emission[1], emission[2], outer],
            shadow_matrix: [[[0.0; 4]; 4]; 4],
            shadow_params: [-1.0, 0.0, 0.0, 0.0],
            shadow_depth: [[0.0; 4]; 4],
            pcss: [
                match light {
                    Light::Directional { angular_radius, .. } => angular_radius.tan(),
                    Light::Point { source_radius, .. } | Light::Spot { source_radius, .. } => {
                        source_radius
                    }
                    Light::Area { shape, .. } => shape.equivalent_radius(),
                },
                0.0,
                0.0,
                0.0,
            ],
            pcss_samples: [0.0; 4],
            cascade_splits: [0.0; 4],
            cascade_params: [0.0; 4],
            area,
        };
        if !bytemuck::cast_slice::<_, f32>(&[uniform])
            .iter()
            .all(|v| v.is_finite())
        {
            return Err("light exceeds GPU float range".into());
        }

        Ok(uniform)
    }

    /// Depth passes encoded by the last frame; zero when cached maps were reused.
    pub fn shadow_passes_last_frame(&self) -> u32 {
        self.shadow_passes_last_frame
    }

    /// Whether the last prepare rebuilt source-dependent IBL data.
    pub fn environment_preprocessed_last_prepare(&self) -> bool {
        self.environment.preprocessed
    }

    fn prepare_shadows(
        &mut self,
        gpu: &Gpu,
        scene: &SceneView,
        camera: &dyn CameraView,
        settings: Option<ShadowSettings>,
    ) -> Result<bool, BoxError> {
        let pbr = settings.is_some();
        let settings = settings.unwrap_or_default();
        let lights = scene.lighting().lights();
        let active: Vec<_> = lights
            .iter()
            .enumerate()
            .filter(|(_, light)| {
                pbr && light.effective_shadow_mode(settings.mode) != ShadowMode::Off
            })
            .map(|(index, _)| index)
            .collect();
        if active.is_empty() {
            self.shadow_cache = None;
            self.shadow_cameras.clear();
            self.shadow_projections.clear();
            self.shadow_layers.clear();
            self.shadow_dirty = false;
            return self.shadow_atlas.ensure(&gpu.device, 1, 1, u64::MAX);
        }
        let cascades = if settings.cascades.count > 1
            && active
                .iter()
                .any(|&index| matches!(lights[index], Light::Directional { .. }))
        {
            Some(CascadeKey {
                view: camera.clip_from_world()?,
                settings: settings.cascades,
                padding: shadow::cascade_padding(settings),
            })
        } else {
            None
        };
        if self.shadow_cache.as_ref().is_some_and(|cache| {
            cache.scene.same_snapshot(scene)
                && cache.resolution == settings.resolution
                && cache.filter_radius == settings.filter_radius
                && cache.max_bytes == settings.max_bytes
                && cache.active == active
                && cache.cascades == cascades
        }) {
            return Ok(false);
        }
        let bounds = shadow::scene_bounds(scene);
        let layers = u32::try_from(
            active
                .iter()
                .map(|&index| shadow::face_count(lights[index], settings.cascades))
                .sum::<usize>()
                .max(1),
        )?;
        let resolution = if bounds.is_some() && !lights.is_empty() {
            settings.resolution
        } else {
            1
        };
        let changed =
            self.shadow_atlas
                .ensure(&gpu.device, resolution, layers, settings.max_bytes)?;
        let mut projections = Vec::new();
        let mut light_layers = vec![None; lights.len()];
        for &index in &active {
            light_layers[index] = Some(projections.len());
            projections.extend(match bounds {
                Some(bounds) => shadow::projections(lights[index], bounds, settings, camera)?,
                None => vec![None; shadow::face_count(lights[index], settings.cascades)],
            });
        }
        let dirty = changed
            || self
                .shadow_cache
                .as_ref()
                .is_none_or(|cache| !cache.scene.same_snapshot(scene))
            || projections != self.shadow_projections
            || light_layers != self.shadow_layers;
        let mut cameras = Vec::new();
        for (index, matrix) in projections.iter().enumerate().filter(|_| dirty) {
            if let Some(matrix) = matrix {
                let data = CameraUniform {
                    clip: matrix.matrix,
                    eye: matrix
                        .point_position
                        .map_or([0.0; 4], |p| [p[0], p[1], p[2], matrix.depth_scale[1]]),
                    // Radial near distance is bit-packed in the shared camera options.
                    options: [0, matrix.depth_scale[0].to_bits(), 0, 0],
                    optical_depth: [0.0; 4],
                };
                let buffer = gpu
                    .device
                    .create_buffer_init(&wgpu::util::BufferInitDescriptor {
                        label: Some("shadow camera"),
                        contents: bytemuck::bytes_of(&data),
                        usage: wgpu::BufferUsages::UNIFORM,
                    });
                cameras.push((
                    index,
                    matrix.point_position.is_some(),
                    uniform_group(
                        &gpu.device,
                        &self.camera_layout,
                        &buffer,
                        CAMERA_UNIFORM_SIZE,
                    ),
                ));
            }
        }
        if dirty {
            self.shadow_cameras = cameras;
        }
        self.shadow_projections = projections;
        self.shadow_layers = light_layers;
        self.shadow_cache = Some(ShadowCache {
            scene: scene.clone(),
            resolution: settings.resolution,
            filter_radius: settings.filter_radius,
            max_bytes: settings.max_bytes,
            active,
            cascades,
        });
        self.shadow_dirty |= dirty;
        Ok(changed)
    }
}

/// Alpha-masked shadow depth; `flag` reads the flag surface by vertex index.
fn depth_pipeline(
    device: &wgpu::Device,
    layout: &wgpu::PipelineLayout,
    shader: &wgpu::ShaderModule,
    flag: bool,
    fragment: &str,
    cull: Option<wgpu::Face>,
) -> wgpu::RenderPipeline {
    let buffers = [Some(vertex_layout())];
    device.create_render_pipeline(&wgpu::RenderPipelineDescriptor {
        label: Some("alpha-masked shadow depth"),
        layout: Some(layout),
        vertex: wgpu::VertexState {
            module: shader,
            entry_point: Some(if flag { "vs_flag_shadow" } else { "vs_shadow" }),
            compilation_options: Default::default(),
            buffers: if flag { &[] } else { &buffers },
        },
        fragment: Some(wgpu::FragmentState {
            module: shader,
            entry_point: Some(fragment),
            compilation_options: Default::default(),
            targets: &[],
        }),
        primitive: wgpu::PrimitiveState {
            cull_mode: cull,
            ..Default::default()
        },
        depth_stencil: Some(wgpu::DepthStencilState {
            format: wgpu::TextureFormat::Depth32Float,
            depth_write_enabled: Some(true),
            depth_compare: Some(wgpu::CompareFunction::Less),
            stencil: Default::default(),
            bias: Default::default(),
        }),
        multisample: Default::default(),
        multiview_mask: None,
        cache: None,
    })
}

fn vertex_layout() -> wgpu::VertexBufferLayout<'static> {
    const ATTRIBUTES: [wgpu::VertexAttribute; 5] =
        wgpu::vertex_attr_array![0=>Float32x3,1=>Float32x3,2=>Float32x2,3=>Float32x4,4=>Float32x4];
    wgpu::VertexBufferLayout {
        array_stride: 64,
        step_mode: wgpu::VertexStepMode::Vertex,
        attributes: &ATTRIBUTES,
    }
}

fn light_group(
    device: &wgpu::Device,
    layout: &wgpu::BindGroupLayout,
    buffer: &wgpu::Buffer,
    size: u64,
    atlas: &ShadowAtlas,
    environment: &EnvironmentGpu,
    ltc: &wgpu::Buffer,
) -> wgpu::BindGroup {
    let mut entries = vec![
        wgpu::BindGroupEntry {
            binding: 0,
            resource: wgpu::BindingResource::Buffer(wgpu::BufferBinding {
                buffer,
                offset: 0,
                size: NonZeroU64::new(size),
            }),
        },
        wgpu::BindGroupEntry {
            binding: 1,
            resource: wgpu::BindingResource::TextureView(&atlas.view),
        },
        wgpu::BindGroupEntry {
            binding: 2,
            resource: wgpu::BindingResource::Sampler(&atlas.sampler),
        },
    ];
    entries.extend(environment.entries());
    entries.push(wgpu::BindGroupEntry {
        binding: 7,
        resource: ltc.as_entire_binding(),
    });
    device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("lights, visibility and environment"),
        layout,
        entries: &entries,
    })
}

/// Group 1 of mesh draws: the draw's `Object` (dynamic offset) and every
/// instance's transform at binding 2, after flags' surface at binding 1.
fn object_layout(device: &wgpu::Device) -> wgpu::BindGroupLayout {
    device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("mesh objects and instances"),
        entries: &[object_entry(), instance_entry(2)],
    })
}

pub(crate) fn object_entry() -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding: 0,
        visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
        ty: wgpu::BindingType::Buffer {
            ty: wgpu::BufferBindingType::Uniform,
            has_dynamic_offset: true,
            min_binding_size: NonZeroU64::new(OBJECT_BYTES),
        },
        count: None,
    }
}

pub(crate) fn instance_entry(binding: u32) -> wgpu::BindGroupLayoutEntry {
    wgpu::BindGroupLayoutEntry {
        binding,
        visibility: wgpu::ShaderStages::VERTEX,
        ty: wgpu::BindingType::Buffer {
            ty: wgpu::BufferBindingType::Storage { read_only: true },
            has_dynamic_offset: false,
            min_binding_size: NonZeroU64::new(INSTANCE_BYTES),
        },
        count: None,
    }
}

/// The dynamic-offset view of `objects` (one `Object`) for group 1's binding 0.
pub(crate) fn object_binding(objects: &wgpu::Buffer) -> wgpu::BindingResource<'_> {
    wgpu::BindingResource::Buffer(wgpu::BufferBinding {
        buffer: objects,
        offset: 0,
        size: NonZeroU64::new(OBJECT_BYTES),
    })
}

fn object_group(
    device: &wgpu::Device,
    layout: &wgpu::BindGroupLayout,
    objects: &wgpu::Buffer,
    instances: &wgpu::Buffer,
) -> wgpu::BindGroup {
    device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("mesh objects and instances"),
        layout,
        entries: &[
            wgpu::BindGroupEntry {
                binding: 0,
                resource: object_binding(objects),
            },
            wgpu::BindGroupEntry {
                binding: 2,
                resource: instances.as_entire_binding(),
            },
        ],
    })
}

fn instance_buffer(device: &wgpu::Device, size: u64) -> wgpu::Buffer {
    device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("mesh instances"),
        size,
        usage: wgpu::BufferUsages::STORAGE | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    })
}

fn uniform_layout(device: &wgpu::Device, dynamic: bool, size: u64) -> wgpu::BindGroupLayout {
    device.create_bind_group_layout(&wgpu::BindGroupLayoutDescriptor {
        label: Some("mesh uniform layout"),
        entries: &[wgpu::BindGroupLayoutEntry {
            binding: 0,
            visibility: wgpu::ShaderStages::VERTEX_FRAGMENT,
            ty: wgpu::BindingType::Buffer {
                ty: wgpu::BufferBindingType::Uniform,
                has_dynamic_offset: dynamic,
                min_binding_size: NonZeroU64::new(size),
            },
            count: None,
        }],
    })
}
fn uniform_buffer(device: &wgpu::Device, size: u64) -> wgpu::Buffer {
    device.create_buffer(&wgpu::BufferDescriptor {
        label: Some("mesh uniforms"),
        size,
        usage: wgpu::BufferUsages::UNIFORM | wgpu::BufferUsages::COPY_DST,
        mapped_at_creation: false,
    })
}
fn uniform_group(
    device: &wgpu::Device,
    layout: &wgpu::BindGroupLayout,
    buffer: &wgpu::Buffer,
    size: u64,
) -> wgpu::BindGroup {
    device.create_bind_group(&wgpu::BindGroupDescriptor {
        label: Some("mesh uniforms"),
        layout,
        entries: &[wgpu::BindGroupEntry {
            binding: 0,
            resource: wgpu::BindingResource::Buffer(wgpu::BufferBinding {
                buffer,
                offset: 0,
                size: NonZeroU64::new(size),
            }),
        }],
    })
}
