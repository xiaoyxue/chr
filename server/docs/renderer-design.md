# Simplified renderer scope and design

Status: **roadmap implemented on Linux except optional atmosphere/sky visibility; see
[renderer-acceptance.md](renderer-acceptance.md)**. This document compares the local `trd`
reference at `9c7bccc` with the `chr` baseline at `dcebb33`. It proposes a smaller
renderer, not a port of the reference application suite. The paired translation
is [renderer-design.zh-CN.md](renderer-design.zh-CN.md).

[coordinates.md](coordinates.md) remains the coordinate contract.
All capabilities below are targets unless identified as existing.

`chr_renderer::scene` now provides shared indexed CPU meshes, per-part material
bindings, model matrices/visibility, validated PBR factors and direct/environment
light configuration. `Scene::snapshot` owns an immutable `SceneView`; later
updates retain old resources and reject stale or cross-scene handles.
Deleting an asset referenced by a live instance fails; singular model matrices
are rejected, with visibility used for hiding. Geometry and material values are
shared rather than copied on every instance update. This is a library foundation,
not a live scene-control protocol: area/atmosphere lighting
and runtime adaptation remain follow-up work.
The existing triangle path is unchanged.

`glb::import_glb` now decodes static BIN-backed GLB geometry and embedded PNG/JPEG
maps without filesystem access. Default node transforms and the glTF-to-chr
axis rotation are baked once; material parts, normals, UV0, tangents, vertex
colours and sampler metadata are retained. Missing normals use flat triangle
normals. Decoded RGBA8 images are shared across maps/materials.
Skins, morphs, alpha blending, external resources and unsupported extensions are
reported as unsupported; animation clips are ignored with a diagnostic.
Texture data is consumed by the shared PBR path described below.

`chr_renderer::camera` implements validated pinhole cameras from K/pose,
FOV/look-at, or Ti+C. It provides native-pixel projection, pixel-to-world rays
and a column-major WGSL clip matrix with metric near/far depth. Ti+C uses a
derived proper optical frame while retaining the original Ti coefficients.
This does not add lens distortion or change the live input protocol.

`MeshRenderer` supplies the shared indexed, opaque unlit/depth baseline.
Meshes stay GPU-resident while referenced by the prepared view; instance/part
uniforms and depth targets are reused. The M1 example renders a synthetic pitch
and grounded, face-coloured boxes to `output/renderer/milestone-m1.png`.
Run `cargo run --locked -p chr-app --example mesh_milestone` from the repository
root in its development environment. This image demonstrates camera projection,
mesh placement and occlusion, not PBR or shadows; it is not the #40 interactive
Socket/WebSocket acceptance.

`MeshRenderer::prepare_pbr` now evaluates metallic-roughness PBR using GGX,
height-correlated Smith visibility and Burley diffuse. It binds base-colour,
metallic-roughness, normal and emissive maps, authored or derivative tangent
frames, vertex colours, alpha masks and double-sided/mirrored surfaces.
Colour textures and their mipmaps are filtered in linear light; data maps stay
linear. Occlusion maps are retained for indirect lighting and do not darken
punctual direct light. No implicit ambient fill or fixed light rig is added.

Scene directional/point/spot inputs feed unshadowed direct illumination.
An ideal point source contributes nothing at its exact singular centre; otherwise
attenuation is inverse-square. The raster roughness floor is 0.045. Radiance is
bounded by the RGBA16F range before one output exposure and Reinhard/ACES pass.
M2 does not include environment lighting; shadow controls, area lighting and
atmosphere are not claimed by that increment.

M3 adds directional/spot `Hard` and `Pcf` shadow maps through
`PbrSettings::shadows`. A directional map fits the retained scene bounds,
independently of the viewing camera; spot maps use their light cones.
Alpha masks, model transforms and sidedness are shared with main rendering.
Receiver-plane depth correction, explicit depth/normal bias and configurable
PCF texel radius handle sampling artifacts. Texture limits and a configurable
memory budget are checked before map allocation.

Unchanged scene state reuses its depth maps across camera motion and Hard/PCF
mode changes. The M3 overview captures compare Off/Hard/PCF with identical
sun-plus-four-spot illumination and ordinary pinhole framing. They remain
owner-local diagnostics, not full interactive acceptance. Cascades, point-light
cubemaps, PCSS and sky occlusion are not implemented by this increment.

M4 adds directional/spot `Pcss` on the same depth maps. Each directional light
has an angular source radius in radians; each local light has a shadow-source
radius in metres. The shader reconstructs each blocker into linear light depth
before averaging, uses angular separation for sun penumbrae and similar triangles
for spots, then filters comparisons with the resulting projected radius.
Zero size is exactly Hard; no blockers is fully lit. Punctual BRDF evaluation and
emission units do not change.

Shared `PcssSettings` expose 1..=64 samples per stage (32+32 by default), separate
search/filter caps (64 shadow texels each). A shared shadow budget limits
worst-case per-fragment work (1024 logical samples across lights). Invalid quality or budget overflow is an
error. Fixed disk sampling is deterministic without screen-space random noise
or temporal accumulation; receiver-plane correction applies to both stages.
Quality-only changes and camera motion reuse the maps. Large kernels are capped
and thin blockers may be missed; this remains a single-average-blocker approximation.
The `shadow-pcss.png` capture retains M3 lighting; `shadow-pcss-wide.png` increases
source radii fourfold without changing energy.
sky visibility and finite-area BRDF integration remain unimplemented.
PCSS is high priority; area/LTC is now low priority.

Algorithm reference: NVIDIA's
[Soft Shadows sample](https://docs.nvidia.com/gameworks/content/gameworkslibrary/graphicssamples/opengl_samples/softshadowssample.htm)
and its PCSS integration references. The bounded two-stage strategy is retained,
with deterministic disk sampling and explicit world/angular units rather than
an invented sun distance. The local `trd` grounding/blob shader is not reused
as visibility. GPU regressions compare controlled contact/separation/source-size
cases against independent disk-emitter ray/plane visibility integration, plus
zero-size, no-blocker, near-plane, cache and budget cases. These fixtures are not
a general-scene quality benchmark or a realtime performance guarantee.

M5 extends Hard/PCF/PCSS to points with six depth-array faces per light, ordered
`+X,-X,+Y,-Y,+Z,-Z`. Depth is radial distance divided by a common far radius fitted
to visible scene bounds. A 0.01-metre spherical near exclusion applies to casters
and visibility queries; the face near planes include that sphere at cube corners.
This does not alter the light's inverse-square attenuation.

Point kernels are sampled in direction/tangent space, with `2/resolution` as
the face-centre texel angular scale. Bilinear neighbours are individually
remapped across faces rather than clamped. Receiver-plane comparisons reconstruct
the radial distance for the actual fetched texel; empty far samples remain lit.
PCSS averages radial blocker distances, uses the nearest scene-bound radius to
conservatively tighten blocker search, and retains the shared sample/radius limits.
The scalar penumbra remains a finite-source approximation, not shape-aware BRDF
integration. The local reference's grounding blob is not used.

Synthetic GPU fixtures move casters across all six face centres, twelve edges
and eight corners, including partially covered filtered edges, no-blocker/far
boundaries, spherical near clipping, masks, mirroring and mixed planar/cube layers.
Point contact-hardening is also compared with independent emitter visibility.
Memory budgets count six layers per point. M5 `point-*` captures explicitly use
1024 resolution for sun + four points (25 layers, 100 MiB); M3/M4 `shadow-*`
retain sun + four spots at 2048. Hard/PCF/PCSS and camera changes reuse unchanged
maps. Broad performance acceptance, scene sky visibility
and area/LTC remain follow-ups.

M6 adds `Light::shadow_mode: Option<ShadowMode>` for all three light kinds.
`None` inherits the frame's default mode; an explicit Off suppresses only
visibility, not direct illumination. Explicit enabled modes work with an Off
frame default. The enum lives in GPU-independent scene state and keeps its
crate-root re-export.

Map allocation and cache identity include the effective active light set, and
only effective PCSS lights require PCSS quality validation.
Default filter changes reuse maps if the active set stays unchanged; authored
override edits change the scene snapshot and conservatively rebuild maps.
The unlit path ignores overrides. GPU tests isolate coloured light contributions
and verify inherited/explicit modes, disabled-map savings, active-set changes,
budgets and unlit behavior without relying on manual image review.
The `point-mixed.png` diagnostic keeps the same sun/four-point energy and 1024
resolution, assigning sun PCSS and corner points Hard/PCF/PCSS/Off in X/Y order.
It needs 19 layers (76 MiB); the shadow-disabled point still illuminates.
This does not add live scene-control protocol support.

M7 makes PCF quality explicit with square 1/4/9/16/25/36/49/64-sample kernels.
The default 3x3 footprint is preserved; the single-sample mode uses its centre.
Other counts sample the same texel-radius footprint more or less densely, with
the same point-face remapping. Changing sample count does not redraw depth maps.
The common `ShadowSettings::max_samples_per_fragment` replaces the former
PCSS-only budget: effective Off/Hard/PCF/PCSS cost 0/1/PCF count/blocker+filter
logical samples respectively. Active quality and combined budgets are checked
before GPU work; inactive modes do not force irrelevant quality validation.
The private example distinguishes mixed modes with one shadow disabled
(`point-mixed.png`) from all-shadow-enabled mixed modes (`point-mixed-all.png`),
and adds a 64-sample PCF capture. Disabled shadows do not disable illumination.
Automated tests exercise each supported kernel for all three light kinds,
cache reuse, invalid kernels and mixed-mode budget accounting.

M8 adds optional 2..=4 directional cascades; count 1 retains the existing
scene-fitted path. Splits interpolate linear/logarithmic optical depth over the
camera near plane to the lesser of its far plane and an explicit maximum shadow
distance (default 200 m). The default transition band is 10% of each interval;
the last band fades to fully lit, making distance truncation explicit.

Each overlap-aware slice uses a rotation-stable bounding sphere and a
world-texel-snapped centre. Its light-depth range retains full visible scene
bounds, including offscreen casters. PCSS uses common metric blocker distances
and each map's own UV scale. The shared budgets count all cascade layers and
two visibility evaluations for blended directional lights. View-dependent fits
are recomputed as needed; unchanged snapped matrices reuse depth maps.
The method follows the frustum partitioning, stable fitting and transition
principles in Microsoft's
[Cascaded Shadow Maps](https://learn.microsoft.com/en-us/windows/win32/dxtecharts/cascaded-shadow-maps),
implemented in portable wgpu rather than copying its platform-specific renderer.

CPU tests cover overlap and subtexel stability; GPU fixtures exercise Hard,
PCF and PCSS around split boundaries, independent emitter visibility, offscreen
casters, mixed point/cascade layer offsets, final-distance fading and budgets.
`sun-single.png`/`sun-cascades.png` hold sun-only lighting, geometry, camera and
1024 resolution constant while comparing one/four maps. General renderer
performance acceptance remains separate from these functional tests.

M9 implements OpenEXR-backed environment illumination and background through
the retained `SkyLight`. Diffuse uses three-band SH with exact per-pixel
lat-long basis integrals and Lambert convolution. Specular uses six spatial
roughness layers, deterministic GGX importance sampling and a split-sum BRDF
LUT with correlated Smith visibility. Float32 textures are manually bilinear
sampled; no optional float32-filtering feature is required. Source normalization
plus a separate scale preserves HDR values before the existing output bound.

One yaw/gain applies to diffuse, reflections and backdrop; hiding the backdrop
does not disable illumination. Coordinates map +X to UV (0.5,0.5), +Y to
(0.75,0.5), and -Z to V=0. Source/quality changes rebuild cached data; pose/gain,
visibility and exposure changes do not. A missing sky releases source-specific
resources and cannot inherit a prior scene's environment. Explicit prefilter
quality and active-resource budgets bound allocations and compute work.

This remains labelled unoccluded IBL. Material AO affects indirect light only,
and punctual shadow maps do not attenuate the environment contribution.
Lambert diffuse differs from direct Burley; low-order SH, the normal-view
split-sum prefilter and single-scattering GGX have known approximation/energy
limits. The SH and split-sum approach follows the primary
[Filament PBR documentation](https://google.github.io/filament/Filament.md.html),
implemented independently, with full-resolution panorama layers rather than
adopting its engine/cubemap pipeline or claiming its advanced material features.

Automated synthetic tests cover constant HDR energy at tiny and larger inputs,
rough-metal single-scattering energy, shared background/reflection rotation,
roughness response, hidden backgrounds, AO/direct-shadow separation, resource
replacement, exposure-independent cache reuse and budget failures.
The example accepts an optional final `--environment <local.exr>`; without it,
M9 captures explicitly use a synthetic studio panorama. No physical atmosphere,
scene sky occlusion or live-control integration is implied.

The native `composition::FrameCompositor` now accepts descriptor-validated
caller-owned GPU images, a retained scene, `CameraView` and a fixed output
configuration. It does not own frame identity, acquisition, scheduling or
producer synchronization. GPU image contents must satisfy the declared transfer/
alpha contract and use the same device; external leases remain live until GPU
completion.
All colour uses sRGB/Rec.709 D65 primaries; transfer tags are not gamut conversion.

Composition uses linear-premultiplied intermediates: scene backgrounds, CG and
scene foregrounds are combined before one configured exposure/tone-map; display
backgrounds and foregrounds surround that transformed scene without receiving
its exposure again. Straight images are premultiplied before filtering. Resolved
MSAA CG is unpremultiplied before tone mapping and premultiplied again afterward.
Input/output transfers are explicit Linear, sRGB or BT.709 with validated format
pairings. The existing RGBA16F scene/intermediate range remains the bound.

CG and native image rectangles share the native camera extent and one configured
Contain/Cover/Stretch output fit, with no Ti rewrite or extra half-pixel offset.
Display foregrounds use output-pixel rectangles. Output is a validated whole
single-mip, single-sample caller texture; fixed output alpha may be straight or
linear-premultiplied before encoding. Linear output means display-linear after
the scene tone-map, not bypassed HDR radiance. Sky remains CG, so its background
visibility is explicit rather than silently suppressed for a plate.

The shared scene renderer supplies depth, shadows, IBL and 1x/4x shaded CG. Image
layers never become virtual geometry or shadow casters. Source bindings are
replaced per prepared frame and native canvases resize without changing output
configuration. Invalid metadata, targets and additional-canvas budgets fail
explicitly. `composition_milestone` provides a synthetic colour/lifetime example;
CPU/GPU tests cover exact native fitting, premultiplied alpha, colour-domain
separation, BT.709, virtual depth, orthographic/MSAA joins and source replacement.
This closes the renderer-only boundary, not the pending scene/frame wire contract,
runtime resource/scheduling integration or SDI-derived monitor acceptance.

The offline `pbr_milestone` example consumes a locally prepared camera fixture
and owner-provided GLBs, renders the virtual ground plane and grounded models,
and writes sun/point/spot comparison PNGs. Camera input remains a conditional
Ti+C projective estimate, not accepted physical calibration. Private input and
output remain local-only. This example is not a new public wire schema or a
replacement for the required NativeSocket/WebSocket interactive path.

For inspection, the example additionally emits `standard.png` and per-model
`model-N.png` views. They reuse the ordinary FOV/look-at pinhole camera with a
level optical axis, zero skew and bounds-based framing. Models and illumination
are unchanged, and imported Ti/C are preserved for the comparison views.
This separates camera-induced apparent tilt from actual model orientation.

The current workspace is rooted at the repository root: `server/renderer`
(`chr-renderer`), `server/runtime` (`chr-server`), `server/viewer` (`chr-app`),
and `common` (`chr-common`). The current server renders a rotating triangle to
a simulated SDI port and monitors that transmitted output over WebRTC.

The [frame-input contract](../../docs/render-frame-input.md) confirms a persistent
scene and an initial **pinhole-only, no-distortion** input profile. This broader
roadmap retains orthographic and distorted-lens goals as later extensions, not
new optional fields in the initial payload. JSON-RPC control is already decided
by [protocol conventions](../../docs/protocol-conventions.md); business method
schemas, frame resources, and precise scene-binding boundaries remain open.

## 1. Simplified feature list

Simplify ownership, configuration, and delivery surfaces, **not** the requested
camera and lighting capabilities. Keep one Rust/wgpu core and the existing
native/headless shell; do not reproduce the reference's editor and protocol stack.

| Area | Reference / current state | Proposed chr scope |
|---|---|---|
| Rendering core | trd shares rendering across several front-ends; chr shares its triangle pipeline | One renderer for native surfaces and offscreen textures |
| Scene | trd has placed primitives, resource stores, overlays, and scene assembly | Flat mesh instances, shared assets, camera, lights, and background; no ECS or scene graph |
| Geometry | trd loads OBJ and GLB, including material parts | Static indexed meshes and instances, one model matrix per instance; GLB import preserves material parts; OBJ conversion stays outside core |
| Materials | trd exposes the full Disney parameter set | PBR is the primary/default material path, referencing trd's shading; metallic-roughness, base color, normal, emissive, occlusion, opaque/masked alpha, and double-sided surfaces; unlit is secondary |
| Render modes | Filled, wireframe, textured, PBR, and diagnostic views | PBR is the production path; unlit serves bootstrap, explicitly unlit assets, and diagnostics; wireframe, normals, depth, axes, and bounds do not create separate scene models |
| Cameras | trd stores view/projection/viewport with CG and CV constructors; chr implements Ti projection math only | Perspective/FOV and calibrated pinhole as equivalent inputs; orthographic; tracked Ti + C; explicit lens distortion |
| Local/direct lights | trd packs a fixed three-directional-light rig; point-light shader slots exist but default to empty | Authored directional, point, spot, and area lights; no hidden key/fill/rim rig |
| Sky | trd supports HDR environment lighting and an optional visible backdrop | One sky source: OpenEXR `.exr` environment or physical atmosphere; shared lighting/background parameters |
| Area shapes | No authored area-light path in the inspected trd lighting interface | Rectangle, disk, and sphere; explicit size, orientation, emission, and sidedness |
| Shadows | trd implements blob shadows and explicitly rejects `shadow_map`; chr has no shadow path | Geometric shadows with explicit PCF/PCSS support, finite-area/sun shadows, and sky visibility; blobs are not a substitute |
| Output | trd has per-object tone mapping; chr outputs an sRGB triangle | Scene-linear HDR accumulation, one output exposure/tone map, sRGB presentation/readback |
| Input | chr-common already implements JSON-RPC and local IPC; frame-input business schemas remain draft | Adapt scene mutations, fixed render configuration, and FrameKey/image_ref/logical-time/Ti/C inputs; do not resend a complete scene per frame |
| Platforms | trd has native, browser, CLI, GUI, and editor delivery | Linux native and headless only; Windows is out of scope; browser delivery is deferred, not architecturally blocked |
| Editing/media | trd includes picking, transform editing, video decode adapters, sparse tracks, export, and replay | Not part of this renderer increment; no timeline, media decoder, or legacy trd document migration |

### Configured raster and inspection (implemented)

`MeshRenderer::set_render_settings(&Gpu, RenderSettings)` configures the shared
renderer independently of `PbrSettings` and scene ownership. Call `prepare` or
`prepare_pbr` again before encoding after a settings change. Defaults are
single-sample `DiagnosticView::Shaded` with both overlays off; the production PBR
and legacy unlit defaults are unchanged.

`sample_count` accepts **1 or 4 only**. The adapter must support render attachment
usage and the requested sample count for output colour, RGBA16F and Depth32Float;
multisampled colour also requires resolve support. Invalid or unsupported
requests fail without changing the current configuration, never silently
downgrading. Colour, main depth, mesh and sky pipelines share the configured
sample count. Multisampled HDR resolves to single-sample HDR before the one
exposure/tone-map pass; inspection/unlit colour resolves directly to the
single-sample output. Resize or sample-count changes recreate matching
attachments; output-format changes still require a new renderer. Shadow maps
remain independently single-sample.

`DiagnosticView::{Wireframe, Normals, Depth}` inspects the same visible instances,
transforms and GPU vertex buffers, but bypasses lighting, sky and tone mapping.
These views inspect raw geometry, including backfaces and alpha-masked regions,
not material opacity. Wireframe derives line-list indices from each indexed
triangle, retaining material-part ranges; it shows all triangle edges (not a
hidden-line or silhouette-only mode). It needs no polygon-line device feature
and does not mistake shared vertex IDs for per-triangle barycentrics.
Normals encode the normalized world inverse-transpose geometric normal as
`(n + 1) / 2`, with winding-aware derivative normals when attributes are absent;
normal maps and face-forward lighting flips are intentionally excluded.
Depth encodes normalized GPU depth `[0,1]`, near black/far white, **not metric
distance or a calibration export**. These diagnostic values pass through the
output format's normal sRGB encoding, not the physical exposure curve.

`DiagnosticOverlays` independently enables `axes` and `bounds` for any view.
One-metre world axes are +X red, +Y green and -Z/up blue. Yellow boxes show each
visible instance's conservative world-axis-aligned bounds. Overlays are
single-pixel, single-sample, always-on-top lines drawn after the output transform;
they are not antialiased scene surfaces and never enter HDR radiance, lighting,
scene resources or shadow-caster geometry. They may be clipped by the camera.
Switching inspection modes, overlays or main sample counts preserves the
production shadow/IBL caches when the scene and relevant settings are unchanged.

This increment uses the reference attachment/options/gizmo separation recorded
in this design (the local reference checkout was unavailable), simplifying it
into renderer-owned configuration, matching colour/depth attachments and a small
post-output line pass; no reference code/assets were copied, and it does not
port an editor or create another scene model.
Portable derived indexed edges replace optional polygon-line/barycentric
assumptions. Synthetic `diagnostics_gpu` checks cover edge coverage, resize,
sample-count and view switching, nonuniform/mirrored normals, axes/bounds,
unchanged physical output and retained environment/shadow caches.
The orthographic integration fixture repeats MSAA/resize/view/overlay checks
with a mirrored nonuniform instance, verifies normalized optical depth and
switches back through pinhole without losing physical output or IBL/shadow caches.
The shared 112-byte camera uniform keeps the orthographic flag in `options.z`
and the diagnostic selector in `options.w`; overlays use only its clip-matrix prefix.
Run `cargo test --locked -p chr-renderer --test diagnostics_gpu -- --ignored --nocapture`
on a hardware GPU. Native window resize/minimize/HiDPI interaction is not
accepted by offscreen tests and remains unverified over SSH.

Deferred material features include anisotropy, subsurface, transmission,
clearcoat, sheen, and general alpha blending. Do not silently flatten an
unsupported GLB feature into an apparently equivalent material: report it at
import. Preserve original source bytes outside the rendering hot path when an
upstream document needs lossless round-tripping.

Prioritize a correct, shared PBR implementation using trd's PBR/Disney shader
as an algorithm reference. Simplify the public parameter set and ownership,
not the correctness of its lighting/material response. Other display modes are
supporting tools, not equally weighted alternative material systems.

HDR image assets use **OpenEXR `.exr`**, not Radiance `.hdr`. Environment loading
must preserve scene-linear floating-point values and declare supported channels,
precision and colour metadata. Define the supported flat RGB(A) profile and
reject unsupported deep/multipart/channel layouts explicitly rather than
silently converting them to LDR. Existing sRGB PNG captures remain unchanged;
any future HDR image export also uses `.exr`.

The CPU import boundary is `chr_renderer::exr::import_exr(&[u8])`, returning
the existing validated `scene::EnvironmentMap`. The caller loads bytes and
schedules decoding outside rendering; the core performs no filesystem I/O.
Its currently supported profile is:

- One flat part, exactly `R`, `G`, `B`, optionally `A`; every channel is
  full-resolution HALF or FLOAT (mixed precision is allowed). HALF widens exactly
  to `f32`; FLOAT and values above one are retained. RGB must be finite and
  nonnegative. Alpha must be exactly one; transparent associated RGB is rejected
  rather than silently unpremultiplied or treated as complete radiance.
- Scan lines or single-level tiles with lossless NONE, RLE, ZIPS, ZIP or PIZ
  compression. Deep, multipart, mip/rip levels, integer or extra/layered/subsampled
  channels and other compression methods are unsupported.
- Complete zero-origin, matching data/display windows and square pixels.
  `envmap` may be absent (explicit latitude-longitude assumption) or
  latitude-longitude; cube maps and multiview layouts are rejected. Raster order
  is preserved, top row first; no rotation, resampling, exposure or gain is applied.
- Scene-linear sRGB/Rec.709 primaries and D65 white are the working-space contract.
  Untagged input explicitly assumes that space. `chromaticities`, when present,
  must match R `(0.64, 0.33)`, G `(0.30, 0.60)`, B `(0.15, 0.06)` and white
  `(0.3127, 0.3290)` within `1e-5` per coordinate. No transfer function or
  colour-space conversion is performed. `whiteLuminance`, `adoptedNeutral`,
  rendering/look transforms and all custom attributes (including ACES/colour-space
  tags) are rejected, not silently ignored. Standard descriptive/camera metadata
  is not applied or retained in `EnvironmentMap`; callers retain source bytes
  when needed.

Synthetic in-memory EXR tests cover this profile, invalid radiance, malformed
headers/payloads, unsupported formats and retained-scene replacement. This decoder
is independent of trd's GPU environment algorithms; none are ported here. The
decoder itself does not perform IBL convolution/prefiltering or GPU work; M9's
shared environment path supplies lighting, background and cache behavior.

The full target includes **all five light families, physical atmosphere, multiple
area shapes, and shadows including PCF and PCSS**. Staging them is an
implementation order, not a reduction to a shadow-free MVP. Fisheye, panoramic cameras, depth of field,
motion blur, cloud rendering, and full global illumination are outside this scope.
GLB animation playback, skeletal skinning, morph deformation, and animation
blending are also deferred; only whole-model matrix updates are supported.

Linux is the sole delivery and acceptance target for this design. Windows
support and Windows-specific validation are not required. This documentation-only
proposal does not remove existing Windows scaffolding from the current baseline.

## 2. Minimal architecture

```text
JSON-RPC scene/config commands -> chr-common -> runtime adapter -> retained Scene
                                                                      |
FrameKey + image_ref + logical_time + Ti/C -> frame assembly <-----------+
                                                   |
                         validated frame + Camera + bound scene view
                                                   |
                                  one Renderer in chr-renderer
                                      |
                                      +--> runtime -> SDI output -> monitor -> WebRTC
                                      +--> viewer/test target -> presentation/readback
```

| Owner | Responsibility |
|---|---|
| Protocol adapter | Runtime-side mapping of chr-common business messages and native frame inputs; field/coordinate validation, resource resolution, and scene/frame consistency |
| `Scene` | Retained flat mesh instances, authored lights, one sky, and background visibility; the render worker mutates state at defined boundaries |
| `Camera` | One authored projection source, native image extent, lens calibration, and clipping parameters |
| `RenderSettings` | Preconfigured output/colour settings, antialiasing, debug view, and quality/resource budgets; not repeated or changed implicitly per frame |
| `Renderer` | GPU resources, derived camera data, pipelines, shadow/sky caches, pass encoding, and readback |
| Native/headless shell | Linux window lifecycle, input/events, surface acquisition/recovery, presentation, filesystem I/O, and image encoding |

Use ordinary modules inside `chr-renderer`, starting with camera, scene,
material, light, and render responsibilities. Do not create a crate per concept,
a generic render graph, a plugin framework, or a duplicate typed-math library.
Keep strong types for units/conventions where they prevent real mistakes.

The conceptual render entry is `render(scene_view, camera, settings, target)`;
the scene view comes from retained state, not a complete wire scene each frame. Assets
are uploaded once and referenced by stable handles; they are not decoded or
uploaded on every frame. Resource removal must reject stale handles rather than
silently point them at a reused asset. A target is a destination, not a second
renderer. Update GPU state by frequency: assets/materials when changed,
camera/lights per frame, transforms per instance.

Keep lighting scene-wide, exposure output-wide, and material parameters
surface-local. There is no per-object sky rotation, duplicated camera pose, or
sticky light setter whose state leaks into the next scene. GPU packing is
derived data, not a public data model dictated by fixed uniform-array sizes.

### Protocol adapter and retained scene

The control envelope and local framing are **already implemented** in
`common/src/{jsonrpc,methods,framing,server}.rs`: JSON-RPC 2.0 over NativeSocket
or loopback WebSocket, through one handler. Extend `chr-common` and its
TypeScript mirror for approved business methods; do not introduce a second
envelope or choose a replacement transport. Frame/media data need not be JSON.

Place the thin mapping/assembly adapter in `server/runtime`, depending on
`chr-common` and `chr-renderer`; the renderer must not depend on networking,
wire method names, hardware devices, or the business protocol. Native frame
producers may write frame storage through the native data boundary.

External rendering instructions must be available over **both NativeSocket and
WebSocket**: scene/model changes, camera controls, lighting/shadow controls and
approved render submissions use the same JSON-RPC handler and runtime command
adapter. NativeSocket keeps the existing Unix-domain, 4-byte big-endian length
framing; WebSocket keeps one JSON message per text message. There is one
authoritative scene and acknowledgement/error model, not one per transport.
Do not use a WebRTC DataChannel or a direct renderer call as the demo's external
control path. Raw image/GLB data is a separate resource-delivery concern, not
automatically large JSON payloads. Direct typed core calls remain valid for
unit/offscreen tests and parallel development against mocks.

| Input layer | Mapping responsibility |
|---|---|
| Configuration | Output specifications and colour processing fixed before execution; explicit reconfiguration is a separate contract |
| Persistent scene | State-changing commands update objects, model matrices, materials, lights, sky, and visibility; no complete object list per frame |
| Frame input | Associate `FrameKey`, published `image_ref`, logical time, Ti, and a compatible resolved C; native image/world domains must be known |

The initial frame profile is pinhole only: no lens coefficients or placeholder
distortion fields, no real-world depth/segmentation input, and no required atlas
or calibration-record lookup in core. Orthographic and distorted-camera controls
need separate, approved extensions if exposed on this live input boundary.
General animation/playback vocabulary in the broader scene document does not
add GLB animation to this renderer roadmap.

Preserve source-frame identity independently of slot index, arrival order,
RPC ID, and output position. Resolve C from an explicitly compatible frame or
context; never silently reuse stale Ti/C. Validate matrix direction/layout and
units once, retain Ti coefficients, and keep source asset IDs separate from GPU
handles. Output size and colour policy must not drift through per-frame defaults.

Publishing a readable frame and submitting render work are separate logical
operations. A publish result is not render completion; a submit receipt is not
program output. The proposed submit-time scene snapshot records `scene_revision`
and prevents later commands changing in-flight work, but its precise binding
point, method names, effect ordering, results, errors, and batch semantics must
be approved in the contract issue before implementation.

Use bounded frame storage with stale-reference detection, explicit readiness,
and lifetime extending through every consumer and GPU operation. Never recycle
a slot at submission alone or overwrite an in-use frame. Pool-full, missing/late
geometry, cancellation, and deadline policies remain decision gates, not
convenient implicit defaults. Input and output storage remain separate.

Follow [execution semantics](../../docs/execution-model.md): content uses
logical time; protection and pacing use their respective runtime clocks.
JSON-RPC batches do not imply ordered or atomic scene changes. Replay/stepping
and batch-size changes must preserve agreed frame associations and effect
boundaries without promising cross-GPU bitwise identity.

This work does not implement recording/calibration services, tracking algorithms,
codecs, new SDI hardware, or a parallel render-direct WebRTC stream. Preserve
`render -> SDI -> monitor encoder -> WebRTC`; infrastructure evolution remains
with its existing tracker.

### Static models and model matrices

Each model instance has one finite affine 4x4 `model` matrix mapping model-local
coordinates to chr world coordinates: `world_position = model * local_position`
for homogeneous column vectors. It controls the whole model's translation,
rotation, and scale, including all material parts. Different instances may share
one asset while using independent model matrices.

The caller may replace a model matrix each frame. This does not require an
animation player, keyframe interpolation, or per-node transform controls in the
renderer. Main rendering, shadow passes, and bounds use the same transform;
lit normals use the inverse-transpose linear transform for nonuniform scale.

Compose GLB's authored default node transforms during static import and bake
them into model-local geometry once, not again on each model-matrix update.
A static asset containing animation clips can load in its authored default node
pose, with a diagnostic that clips are not played; do not sample an arbitrary
animation frame. Assets requiring skinning or morph deformation are reported as
unsupported rather than silently presented as equivalent static models.

### Mesh storage reference and adaptation

Also reference `xiaoyxue/Gyunity` at `7361064`, specifically
`Gyunity/src/mesh/Mesh.h` and `Gyunity/src/container/README.md`.
The inspected mesh owns vertex/normal/UV/face vectors and a separate triangle
cache; the container documents contiguous ownership, checked sizing, and
borrowed views invalidated by storage changes. These are useful reference
semantics, not an existing GPU asset-store implementation to copy.

For chr, separate immutable/shareable CPU mesh data, material parts and bounds
from per-instance model matrices and renderer-owned GPU residency. Upload
geometry once; transform edits must not rebuild vertex arrays. Use ordinary
checked Rust collections and typed stale-safe handles before introducing a
generic storage framework. Distinguish source asset IDs from resident slots.
Retain CPU data only for explicit needs; do not copy Gyunity's ray-tracing
triangle cache, shared-pointer object graph, or an N-D container where a `Vec`
suffices. Validate size/index overflow before changing live state and preserve
in-flight GPU resource ownership on removal/replacement.

## 3. Camera, homography, and lens contract

### 3.1 Preserve the existing coordinates

World units remain metres: X along pitch length, Y along width, ground Z=0,
and up is **negative Z**. Homogeneous vectors are columns. Ti is a forward,
row-major ground-to-SNC homography, **not** intrinsics or a rotation.

For native dimensions W,H, let s=sqrt(W*H), and define pixel-to-SNC S:

```text
S = [ 2/s   0   -W/s ]
    [  0   2/s  -H/s ]
    [  0    0     1  ]

A(C) = [ 1  0  -Cx/Cz  0 ]
       [ 0  1  -Cy/Cz  0 ]
       [ 0  0   -1/Cz  1 ]

p_ideal = [u, v, 1]^T
p_ideal ~ S^-1 Ti A(C) [X, Y, Z, 1]^T
```

Here C is the optical centre in world coordinates and Cz must be nonzero.
At Z=0 this reduces to `p_ideal ~ S^-1 Ti [X,Y,1]^T`; C is unnecessary
for this ground-only operation. Multiply all homogeneous factors before
dividing: an intermediate point at infinity can have a finite final image.

Retain imported Ti coefficients, including their scale and sign. Do not
silently transpose, invert, or renormalize them. Preserve native image
dimensions and integer pixel centres without a half-pixel offset. Only the GPU
boundary converts Y-down SNC/pixels into Y-up wgpu NDC.

This equation defines the **ideal projective image**. A general distorted
image cannot be represented exactly by a single 3x3 homography. An upstream
Ti fitted directly to distorted observations is an approximation, not proof
that distortion is already encoded; require its image-domain metadata.

### 3.2 Separate projection from parameterization and distortion

| Camera input | Parameters | Interpretation |
|---|---|---|
| Perspective/FOV | Camera-to-world pose, vertical FOV, aspect, near/far | Convenience constructor for a centred, zero-skew pinhole |
| Calibrated pinhole | Camera-to-world pose, K, native extent, near/far | Same perspective family, including off-centre principal point and skew |
| Orthographic | Pose, horizontal/vertical span, near/far | Parallel rays; no finite pinhole K conversion |
| Tracked homography | Ti, C for elevated geometry, native extent | Preserve the projective relation above; ground-only Ti is not a general 3D camera |

For K, use pixel units and the matrix
`[[fx, skew, cx], [0, fy, cy], [0, 0, 1]]`. The calibrated optical frame has
X right, Y down, and Z forward. State explicitly that pose is
**camera-to-world**; its inverse is world-to-camera. Convert these optical
axes to the existing world/GPU conventions at a single boundary, rather than
copying trd's +Y-up world assumptions.

Store one source representation, not separately editable K, FOV, view,
projection, Ti, and C values. Derived matrices may be cached. A tracked source
needs a validated camera orientation/forward-depth convention, or an explicitly
validated projective-camera decomposition, before 3D clipping and lighting are
enabled. Ti alone does not supply these. Never use world Z or unnormalized
`h.z` as wgpu depth; preserve the source coefficients even when deriving another
representation. If both calibrated and tracked forms are supplied, verify
their agreement in pixel space rather than choosing one silently.

Camera operations must expose projection and pixel-to-ray consistently.
Perspective rays share an origin; orthographic rays have varying origins and
a constant direction. Orthographic specular shading must use that direction,
not `normalize(camera_position - surface_position)`.

**Implemented internal API:** `camera::Camera` retains its pinhole/FOV/K/Ti
constructors and accessors. `OrthographicCamera::from_pose(extent,
[horizontal_span, vertical_span], pose, near, far)` is a separate type;
both implement the sealed `CameraView` interface accepted by `MeshRenderer`.
Spans are full world-metre width/height, independent of raster aspect ratio.
The proper rigid, row-major camera-to-world pose places the image centre at
optical `(0,0)`; native pixel centres have no added half-pixel offset.
Orthographic ray origins lie on the pose's depth-zero plane, vary across the
image, and share its forward axis. Clip W is exactly one and depth is
`(optical_z - near)/(far - near)`, with `0 < near < far`.
Invalid spans, poses, clip ranges, nonfinite operations and GPU matrix overflow
fail explicitly. This type cannot import K/Ti or accept lens distortion;
the live pinhole schema and its rejection of unsupported inputs are unchanged.

Both cameras use the same mesh/depth/HDR renderer. Orthographic direct specular
and IBL use `V = -forward`; sky rays are parallel, translation-independent and
follow the same world/environment rotation. Cascades fit the actual per-pixel
ray origins and select by optical forward depth, never orthographic clip W.
The shared CPU/WGSL camera uniform is 112 bytes, including its optical-depth row;
shadow-camera bindings use the same layout. Synthetic CPU/GPU regressions cover
rotated poses, spans, clipping/occlusion, specular/sky orientation and every
cascade. The implementation uses the standard affine orthographic equations
directly, reusing chr's rigid-frame validation and matrix boundary; no trd
camera code, private assets or calibration records are copied.

### 3.3 Distorted lenses

The initial live frame profile has no distortion. The renderer supports two
image models: the ideal **pinhole** (`S^-1 Ti A(C)`) and, as an opt-in extension,
**`radial-poly-native-px-v1`** (implemented; see below). Focal-normalised
**Brown-Conrady radial/tangential distortion** remains a proposal.
The model identifier, calibration extent, normalization K, valid image domain,
and coefficients are explicit. Do not infer the model from coefficient count
or silently treat rational/fisheye calibration as Brown-Conrady.

For normalized ideal coordinates `(x,y)` from `K^-1 p_ideal`:

```text
r2 = x*x + y*y
radial = 1 + k1*r2 + k2*r2*r2 + k3*r2*r2*r2
xd = x*radial + 2*p1*x*y + p2*(r2 + 2*x*x)
yd = y*radial + p1*(r2 + 2*y*y) + 2*p2*x*y
p_distorted ~ K [xd, yd, 1]^T
```

These are K-normalized optical coordinates, **not SNC**. For Ti input,
evaluate `S^-1 Ti A(C)` first, then apply the calibrated lens map in its declared
domain. Ti does not supply missing lens calibration.

Render the ideal scene with correct depth into linear HDR, then apply an
**inverse-mapped image warp**: each distorted destination pixel solves for its
ideal source position. Forward-splatting vertices/pixels creates holes and
cannot correctly bend triangle edges. Use bounded inversion, explicit invalid
pixel coverage, and a validated invertible calibration domain. Size the ideal
render extent to cover required source samples; do not clamp missing border
samples into stretched edge pixels.

Zero coefficients must produce the identity mapping. Distortion is initially
supported in that extension for perspective and tracked projective cameras; reject nonzero
distortion on an orthographic camera until its calibration semantics are defined.
Output resizing/fitting happens after native-domain projection and lens mapping.
Never silently rescale imported Ti or calibration when a window resizes.

If a later caller composites an already-distorted real image, warp only the
virtual layer and its premultiplied alpha, then composite in the same pixel
domain; do not distort the real image twice. Picking must invert the same
mapping, not use an independent approximate camera.

#### Implemented: `radial-poly-native-px-v1`

Deployed broadcast calibrations (#33) use a native-pixel radial polynomial with
its own centre and scale, then an optional per-frame homography, rather than
focal-normalised Brown-Conrady:

```text
a      = ideal native pixel from S^-1 Ti A(C)
rho^2  = |(a - centre) / scale|^2
r      = centre + (a - centre) * (1 + k1 rho^2 + k2 rho^4 + k3 rho^6)
n      = pi(W [r, 1])          frame SNC; without a frame warp W = S
u, v   = (s n + (W, H)) / 2,   s = sqrt(W H)
```

`scale` is a calibration pixel length, not a focal length, so focal-normalised
coefficients are not interchangeable. The frame warp is per-frame data, not lens
calibration. `distortion::CameraModel::{Pinhole, Distorted}` selects the model;
`FrameCompositor::prepare_model` renders ideal HDR CG over a guard band
(`Camera::with_margins` keeps intrinsics and pose) and a GPU pass inverse-maps
each native sample position: exact inverse frame warp, then Newton on the
monotonic radial branch. Native pixels without a preimage receive no CG; the
optional validated support rectangle is reported as extrapolation, not enforced.
Image layers, including captured footage, are not warped. Zero coefficients
without a frame warp reproduce the pinhole composition. The guard band counts
against the composition memory budget. Wire/calibration fields remain gated by
#12/#36/#59.

## 4. Lighting and shadows

### 4.1 One authored lighting model

Use a flat collection of local/direct lights plus one optional sky.
`Directional`, `Point`, `Spot`, and `Area` are direct-light variants;
`Sky` is the fifth family, a scene-wide environment rather than a local emitter.
An area light has a shape, not a separate renderer for each shape.

| Family | Required data | Shading / visibility |
|---|---|---|
| Directional | Travel direction, colour, irradiance; sun may have angular extent | Distant illumination; directional shadow maps |
| Point | Position, colour, radiant intensity, explicit influence range if bounded | Inverse-square attenuation; cubemap shadows |
| Spot | Point-light data, travel direction, inner/outer cone angles | Inverse-square attenuation and smooth cone falloff; perspective shadow map |
| Area | Rectangle dimensions, disk radius, or sphere radius; pose, radiance, sidedness | Integrate the finite emitter and visibility; size affects highlights and penumbrae |
| Sky | OpenEXR `.exr` map + orientation/gain, or physical atmosphere parameters | Diffuse/specular environment illumination, optional visible sky, and environmental visibility |

Use linear RGB and named radiometric quantities: point/spot intensity in W/sr,
directional irradiance in W/m^2 on a perpendicular plane, and area/sky radiance
in W/(m^2 sr), with documented RGB approximation. Do not reuse one unexplained
`intensity` scalar for all units. Lumens/candela UI conversions, if introduced,
must be explicit. Exposure does not change emitted power.

Reject nonfinite or negative emission, zero-length directions, invalid cone
ordering, and nonpositive area dimensions. A planar emitter is one-sided unless
declared two-sided; a sphere emits outward. Zero emission disables contribution.
An explicit influence cutoff is a declared approximation, not an unexplained
replacement for inverse-square falloff. The ideal point-light singularity needs
a documented near-source policy, not trd's arbitrary `0.01 + distance^2`.

### 4.2 Physical atmosphere, not a sky-colour gradient

Use Hillaire's production-ready atmosphere technique (EGSR 2020, sources below)
as the primary raster/compute reference for a LUT-based atmosphere with Rayleigh and Mie scattering, absorption
(including ozone), ground albedo, observer altitude, and approximate multiple
scattering. Define planet/atmosphere radii and coefficient units; validate the
parameter range instead of hard-coding unlabelled Earth constants. Map local
height using `ground_altitude - Z`, consistent with negative-Z-up.

The atmosphere references a sun directional light. One authoritative sun
direction, angular radius, and source emission drives atmospheric evaluation,
the visible sun disk, and direct sun lighting/shadows. Ground-level direct sun
irradiance includes atmospheric transmittance; it is not the unattenuated
top-of-atmosphere value.

Generate diffuse irradiance and roughness-prefiltered specular inputs from the
same sky radiance used by the background. If analytic sun lighting is enabled,
exclude the direct sun disk from environment-light integration to avoid counting
it twice; the visible background may still show the disk. Its specular/direct
contribution must retain the sun's finite angular extent.

HDR and atmosphere are alternative sky sources in this proposal, not two
independently rotating skies. Hiding the backdrop does not disable illumination.
Changing sun/atmosphere parameters invalidates dependent LUTs and environment
lighting; changing output exposure does not. Aerial perspective, clouds, and
general participating media are deferred.

### 4.3 Shadow techniques and honest approximations

| Light / effect | Proposed baseline | Limit |
|---|---|---|
| Directional / sun | Fitted orthographic depth maps; cascades when view coverage needs them; selectable PCF/PCSS | PCSS approximates contact-hardening from angular source size; PCF alone does not |
| Point | Six-face depth cubemap with consistent radial-depth comparisons and PCF/PCSS | Ideal zero-size source casts hard shadows; a finite shadow-source radius is an explicit approximation |
| Spot | Perspective depth map matching the outer cone and range, with PCF/PCSS | Cone attenuation and shadow projection must agree; finite shadow-source radius is explicit |
| Area | Select an efficient raster visibility method under section 4.5; deterministic surface quadrature supplies the validation reference | A single point-light substitute cannot establish shape-correct illumination or shadows |
| Sky | Select visibility-aware environment shading under section 4.5; sampled directional visibility supplies a reference | Expensive reference integration is not the mandatory production path; unoccluded IBL remains a labelled approximation |

For a one-sided area emitter, a direct-light reference estimator has the form
below. It defines a correctness oracle, not the required production algorithm:

```text
Lo ~= (1/sample_count) sum[
    BRDF * Le * visibility * max(n dot L, 0) * max(n_light dot -L, 0)
    / (distance^2 * pdf_area)
]
```

For uniform surface samples, `pdf_area = 1 / emitting_area`. Use the correct
shape distribution and sidedness; sample visibility must include emitter
self-occlusion where relevant. Fixed radiance and increased area increase total
power; fixed total power requires an explicit conversion. Do not approximate
every area light by a point while claiming size-dependent illumination.

Finite-angular sun sampling can reuse directional visibility; sky integration
must weight incident radiance, BRDF, solid angle, and visibility consistently.
For preview, prefiltered IBL without scene occlusion is allowed only as a labelled
quality mode. Sky shadow support is not complete until occlusion affects diffuse
and specular environment contributions. SSAO or a blob cannot stand in for that.

All shadow passes share geometry transforms, alpha-mask rules, and a defined
sidedness policy with the main pass. Declare depth/normal bias units and check
acne, detached shadows, cubemap seams, and cascade transitions.

Expose sample count, resolution, and total shadow-pass/memory budgets. Report
unsupported quality or budget overflow rather than silently dropping lights.
Area and sky sampling can be costly even with a small API: the simplified
architecture does not make these algorithms free. Ray tracing and full path
tracing are not prerequisites for this portable wgpu raster design.

### 4.4 Required PCF and PCSS modes

Support explicit `Off`, `Hard`, `PCF`, and `PCSS` shadow modes for directional,
point, and spot lights, sharing shadow-map generation rather than creating
separate renderers. Selection is per light; quality/resource budgets are shared.
PCF and PCSS are required capabilities, not unspecified future alternatives.

**PCF (percentage-closer filtering)** averages multiple depth-comparison results,
not depths followed by a single comparison. Its configurable kernel softens
shadow-map edges and reduces aliasing. A fixed filter radius does not by itself
model a penumbra that grows with blocker-receiver separation.

**PCSS (percentage-closer soft shadows)** has three explicit steps:

1. Search raw shadow depths for blockers in front of the receiver and estimate
   their mean distance in a consistent linear light-space depth convention.
2. Estimate penumbra size from source size and blocker/receiver separation.
3. Apply PCF with the resulting variable filter radius.

For a finite local source, the usual similar-triangle approximation is
`penumbra_radius ~= source_radius * (receiver_distance - blocker_distance) /
blocker_distance`. For a directional source with angular radius `theta`, use
`penumbra_radius ~= tan(theta) * blocker_receiver_separation` along the light
direction. Convert the result to the map's projected footprint; do not apply
the local-source formula using an invented finite distance to the sun.

Never average nonlinear perspective depth-buffer values as physical distances.
Use linear light-space depth for 2D maps and the declared radial-depth metric
for cubemaps; blocker tests and reconstruction must agree with the stored
encoding. A valid search with no blockers returns fully lit. Define bounded
search/filter radii, reject invalid source sizes, and handle near-plane or
zero-distance cases without division by zero. A zero-size source reduces to
hard visibility with only the declared antialiasing filter, not an artificial
physical penumbra.

The sun uses the atmosphere's authoritative angular radius. For point/spot
lights, a nonzero shadow-source radius in metres is a labelled soft-shadow
approximation: it does not turn inverse-square point shading into finite-emitter
BRDF integration. Use the `Area` family when finite-shape illumination is needed.
PCSS does not replace shape-aware sampled area shadows or sky visibility.
For sampled area/sun/sky integration, filter each sample's visibility with
Hard/PCF as appropriate; do not add a second full-emitter PCSS blur to every
sample and count source size twice.

Expose PCF filter radius in shadow texels, filter sample count, PCSS blocker
sample count, and maximum search/filter radii, separately from physical source
size. Count both PCSS sampling stages in quality budgets. Use deterministic
sampling for reproducible output; unsupported settings must not silently fall
back from PCSS to PCF.

Directional cascades convert the same world-space penumbra to each cascade's
texel scale and blend transitions; stabilize map fitting to avoid camera-motion
shimmer. Point-light kernels operate in a direction/tangent-space footprint
across cubemap faces, not a clamped per-face UV kernel. PCF and PCSS must both
respect bias, map coverage, and alpha-masked caster rules.

### 4.5 Modern rasterization methods, not brute-force defaults

Where trd already implements the needed behavior, audit and simplify it before
reuse. For missing lighting/shadow features, evaluate source-backed modern
production rasterization methods rather than inventing a naive implementation.
"State of the art" is a quality/performance selection criterion, not a claim
that one algorithm is universally best or that an older paper is the newest.

| Area | Preferred reference / candidate | Required qualification |
|---|---|---|
| Finite-area PBR shading | Heitz et al., LTC (SIGGRAPH 2016), for polygon/rectangle integration | LTC approximates unoccluded BRDF integration, not visibility; approve and validate shape-specific disk/sphere handling rather than assuming rectangle tables apply unchanged |
| PCF/PCSS | Stable cascaded/cubemap maps, optimized comparison kernels and bounded blocker search | Preserve the explicitly required PCF/PCSS modes, depth correctness, deterministic sampling and budget controls |
| Scalable soft shadows | Evaluate virtual shadow maps and shadow-map ray tracing (SMRT), as documented by Epic | Virtual shadow maps are not variance shadow maps; SMRT samples raster depth, not hardware triangle-ray intersections. Paging/cache/geometry integration cost and single-depth-layer limitations require evidence before adoption |
| Physical atmosphere | Hillaire, scalable production-ready atmosphere (EGSR 2020) | Implement the supported sky/sun subset in portable raster/compute, with declared units, LUT precision and cache invalidation; no automatic cloud/volume scope expansion |
| Environment occlusion | Evaluate visibility-aware probes/maps or bounded raster visibility sampling | Diffuse contact AO alone is not full sky visibility or specular occlusion; disclose offscreen, thin-geometry and missing-data limitations |

Use high-sample area/sky integration as a synthetic offline or test oracle.
Do not make dozens of shadow views per emitter/environment the real-time default
merely because they are straightforward. Production area shading and visibility
must be combined consistently; multiplying LTC by an unrelated visibility
average is an approximation that needs error characterization.

A focused method-selection issue must record primary sources, supported wgpu
features, candidate implementation complexity, GPU time, memory, temporal
stability, and error against reference scenes. Test multiple blockers, grazing
views, contact, large penumbrae, offscreen occluders and cache invalidation.
Select the simplest method meeting the agreed quality/performance budget;
full virtualized-shadow infrastructure is not mandatory without evidence.
Any temporal reuse must define history invalidation and replay behavior.
Hardware ray tracing, Nanite, and engine-specific APIs are not prerequisites.

#### Bounded synthetic shadow benchmark

Run from the workspace root:

```sh
cargo run --locked --release -p chr-app --example shadow_benchmark
cargo test --locked -p chr-app --example shadow_benchmark
```

On the designated non-NixOS NVIDIA host, use
`nix develop --command bash -c 'NIXPKGS_ALLOW_UNFREE=1 nix run --impure github:nix-community/nixGL#nixGLNvidia -- cargo run --locked --release -p chr-app --example shadow_benchmark'`.
The harness only creates synthetic quads and writes
`output/renderer/shadow-benchmark.json`; it never reads owner assets. Its default
240 rows cover single-map/four-cascade directional, spot and point lights;
Hard, all eight PCF square
kernels, and PCSS 8/8, 32/32, 64/64 blocker/filter samples; contact, separated,
large-penumbra, multiple-caster and grazing-view scenes. Images are 384×384,
maps default to 512². `--resolution 256|512|1024` and `--repetitions 1..20`
bound the workload; three repetitions per row/state are the default.
The four-cascade case uses 12 m maximum optical depth, split lambda 0.5 and
blend fraction 0.1, deliberately fitting the modest synthetic scene.

Only this example requests optional wgpu `TIMESTAMP_QUERY` and
`TIMESTAMP_QUERY_INSIDE_ENCODERS`. Two actual GPU timestamps bracket the existing
`MeshRenderer::encode` (depth, HDR mesh and tone mapping), excluding CPU prepare,
uploads, allocation, pixel readback and timestamp resolve. CPU prepare wall time
is reported separately, never as GPU time. Raw GPU samples and median/min/max
are retained; unsupported timestamp features produce null GPU metrics, an
explicit status and unsuccessful hardware acceptance. Software/unknown adapters
also fail hardware acceptance rather than silently qualifying.

One untimed priming frame precedes each row. **Cold** means an identical caster
model republished in a new scene revision, forcing depth regeneration while
retaining resident resources and pipelines; it is not cold-start compilation or
allocation latency. **Warm** repeats that snapshot. The harness checks exactly
1 single-map / 4 cascaded / 6 point depth passes versus zero warm passes, and separately compares
cold/warm RGBA bytes. Memory is explicit `resolution² × layers × 4` Depth32Float
texel payload (1 MiB single-map / 4 MiB cascaded / 6 MiB point at 512), not total VRAM or allocator
padding. Per-fragment logical budgets count Hard=1, PCF=kernel count,
PCSS=blocker+filter counts, doubled for blended cascades; bilinear comparisons
are not single physical fetches.

Quality uses distinct receiver-strip pixel centres, excluding caster coverage,
and divides inverse-sRGB/Reinhard shadow radiance by an unshadowed baseline.
An independent CPU ray/quad visibility integration samples a uniform emitter
disk, with 48²/96² disk-clipped quadrature convergence recorded. Reports include
finite-emitter and point-emitter MAE, partial-pixel counts (visibility 0.1–0.9),
and reference counts. Automated gates reject invalid baselines, nondeterministic
reuse, visibility >1.05, reference convergence MAE >0.01, Hard point-reference
MAE >0.15, or PCSS 32/64 finite-reference MAE >0.15. These are bounded fixture
regression limits, not universal quality budgets or a claim that PCSS always
outperforms PCF. Additional checks require PCF/PCSS quality counts to change
the separated-scene image and PCSS64 partial-pixel counts to grow with
separation and then source size.

A release Vulkan run on RTX 3090 (NVIDIA 580.159.03, three repetitions, 512² maps)
passed all 240 rows: single-map directional/spot cold/warm row medians ranged
0.070–0.271 / 0.065–0.266 ms; four-cascade directional 0.084–0.262 /
0.076–0.255 ms; point 0.039–0.740 / 0.024–0.733 ms. Maximum PCSS 32/64
finite-reference MAE was 0.071, Hard point-reference MAE 0.046, and quadrature
convergence MAE 0.006. Timings include query overhead and may be affected by
concurrent GPU use, clocks and scheduling; tiny synthetic scenes imply no
general realtime guarantee. The 256²/1024² bounds also pass one repetition per
state. This slice does not evaluate a dedicated
offscreen-caster scene, cube seams, temporal camera trajectories, LTC, atmosphere
or sky. Visibility-only disk integration is not BRDF-weighted transport, and
the point-light reference is a facing disk, not sphere radiometric integration.
Issue #35 remains open for the broader method selection.

#### Bounded finite-emitter illumination evaluation

The [finite-emitter evaluation](area-light-evaluation.md) records licensed,
pinned LTC data, 240 synthetic rectangle/disk/sphere cases, six focused tests
and actual RTX 3090 Vulkan timestamp measurements. It recommends clipped LTC
rectangles and 64-sided disk/sphere silhouettes for an owner-approved experiment,
not production acceptance. The Burley/Fresnel mismatch, grazing fit errors and
under-resolved near-field references remain explicit gates. API/wire contracts,
area-light priority and the separate finite-area visibility work are unchanged.
On 2026-11-12 the owner approved the bounded next experiment and its stated
envelope, quality criteria and GPU microbudget, while retaining GGX/Burley and
explicit radiance/sidedness units. This is not permission to substitute Lambert,
relax failed criteria or enable production area lights. Sky and atmosphere remain
paused.
The approved-profile follow-up now finds resolved pure-metal counterexamples
inside that envelope, including failures of both absolute and relative error
criteria. Independent area/solid-angle oracles and the actual native WGSL BRDF
agree. The current pinned LTC fit therefore fails those reference-accuracy limits;
no production path was added and those limits were not relaxed.
Measurements then showed that no single LTC can meet them. The owner explicitly
approved a separately named approximation profile, `ltc-plausible-v1`: the same
envelope and GPU microbudget, per-emitter error ≤ 0.02·Le + 30% of the reference,
and Lambert×(1−F0) diffuse for area lights only. The tolerance is set from measured
LTC behaviour and is not a reference-accuracy claim. The pinned candidate passes
its 9,632-row sampled screen. The renderer now exposes it as the experimental,
`Light::Area` with hardware reference and trajectory-continuity tests; no wire
mapping or default light is added. For #27 the owner then approved
`area-shadow-pcss-v1`, an explicit exception to the single-point-substitute rule
in section 4.3: one centre cubemap with PCSS sized by the emitter's equal-area
radius, multiplied with LTC, within measured visibility-ratio limits (max 0.65,
penumbra mean 0.15). Its penumbra ignores the emitter outline and glossy BRDF
weighting; see the evaluation for settings, cost and limits.

#### Bounded sky-visibility evaluation

The [sky-visibility evaluation](sky-visibility-evaluation.md) records primary
sources and an independent CPU synthetic comparison of bounded directional
integration, angular probe visibility and their limitations. It does not select
or approve a production method, measure a raster sky candidate's GPU cost, or
change the unoccluded IBL path. The next depth-aware probe experiment and its
provisional budgets require owner agreement; sky implementation #29 retains its
dependencies and atmosphere #28 remains paused.

Primary references:

- [Heitz et al., Real-Time Polygonal-Light Shading with Linearly Transformed Cosines (2016)](https://eheitzresearch.wordpress.com/415-2/).
- [NVIDIA PCF/PCSS sample and original 2005/2008 references](https://archive.docs.nvidia.com/gameworks/content/gameworkslibrary/graphicssamples/opengl_samples/softshadowssample.htm).
- [wgpu timestamp features](https://docs.rs/wgpu/30.0.0/wgpu/struct.Features.html#associatedconstant.TIMESTAMP_QUERY) and [timestamp period](https://docs.rs/wgpu/30.0.0/wgpu/struct.Queue.html#method.get_timestamp_period).
- [Epic, Virtual Shadow Maps and SMRT documentation](https://dev.epicgames.com/documentation/en-us/unreal-engine/virtual-shadow-maps-in-unreal-engine).
- [Hillaire, A Scalable and Production Ready Sky and Atmosphere Rendering Technique (2020)](https://sebh.github.io/publications/).

## 5. Render sequence and implementation stages

The proposed pass order is:

```text
validate inputs and resource budgets
  -> update changed assets / atmosphere / environment caches
  -> render required light visibility maps
  -> render ideal-camera geometry and sky into linear HDR + depth
  -> resolve antialiasing and apply native-domain lens warp
  -> composite any caller-supplied scene-linear image layer in the same pixel domain
  -> one exposure / tone-map / sRGB output transform
  -> display-space debug overlays, presentation or readback
```

Image-layer input is a rendering boundary, not permission to add a video decoder
or timeline. Background visibility is independent of sky lighting. Tone mapping
must not occur separately on each mesh before lighting/composition.
An already display-referred photo/video is not scene-linear radiance; preserving
it requires a separate, explicit output-compositing policy, not automatically
applying the virtual scene's exposure/tone map to it again.

| Stage | Independently demonstrable outcome |
|---|---|
| 0. Design gates | Approve initial scene/config/frame contracts using existing JSON-RPC; independently select and benchmark modern raster lighting/shadow methods absent from trd |
| 1. Scene and pinhole foundation | Retained scene and model matrices; shared mesh/unlit/depth path; FOV/K/pose/Ti+C pinhole inputs; frame adapter, lifetime, and revision binding; keep the triangle available |
| 2. PBR and direct lighting | Static GLB material parts, one PBR path, point/spot/directional lights, HDR sky, hard/PCF shadows, and configured output composition into the existing SDI path |
| 3. Contact-hardening and finite emitters | PCSS; rectangle/disk/sphere area lights with efficient shape-aware shading and soft shadows validated against sampled references, with explicit quality/cost controls |
| 4. Atmosphere and sky visibility | Physical atmosphere, consistent sun/sky lighting, finite-sun shadows, and visibility-aware environment lighting |
| 5. Camera extensions | Orthographic camera and Brown-Conrady lens warp; any live-protocol exposure requires a separately approved schema extension |

Stages 0-5 cover the full renderer roadmap; a stage-2 result must not
be presented as support for all requested lighting and shadows. Keep each
increment behind explicit input/configuration choices so the existing triangle
and coordinate contract do not silently change.
Extend the adapter's mappings and contract tests with each implemented feature;
the concrete protocol decoder depends on agreement on the new input schema.

### Parallel workstreams and priority

The renderer is **high priority**. Separate ownership allows different agents
to progress concurrently without turning every core task into a protocol blocker:

| Workstream | Owns | Handoff |
|---|---|---|
| [Renderer #11](https://github.com/xiaoyxue/chr/issues/11) | `server/renderer`, native/core validation in `server/viewer`, PBR/cameras/lights/shadows and renderer composition | Stable typed scene/camera/target API; synthetic fixtures and the football scene profile |
| [Protocol #36](https://github.com/xiaoyxue/chr/issues/36) | `common`, TypeScript protocol mirror, owning business method docs | Approved DTOs, errors, effects and conformance fixtures over both existing IPC transports |
| [Server #37](https://github.com/xiaoyxue/chr/issues/37) | `server/runtime`, runtime integration and interactive harness behavior | Command mapping, frame lifetime/revision binding, camera/light controls, and SDI/monitor integration |

Agree the minimal core interface early; use mocks until peer implementations
arrive. Coordinate root Cargo manifests/lockfile and shared documentation edits.
Native blocked-by links describe prerequisites for complete integration and
acceptance, not a ban on design/prototyping. Renderer CPU/GPU work does not wait
for the entire server, although the final socket-controlled demo does.

### High-priority football scene demonstration

Schedule contingency [#45](https://github.com/xiaoyxue/chr/issues/45) evaluates
existing renderers, including bgfx and the intended tinyrender candidate, if
integration time threatens delivery. It does not automatically replace wgpu
or block current implementation; a backend change needs an explicit decision.

Tracked separately in [#40](https://github.com/xiaoyxue/chr/issues/40).
The independent algorithm-selection gate is [#35](https://github.com/xiaoyxue/chr/issues/35);
wire-facing schema decisions are [#12](https://github.com/xiaoyxue/chr/issues/12).

Use the latest owner-provided private scene/camera package and local GLB directory as optional,
private acceptance inputs. Resolve the package metadata index and relative
operational paths, not historical absolute provenance paths. This source
supersedes the earlier SC1464 selection. Inspection confirmed source Ti, explicit frame
mapping/provenance, an estimated world camera centre and ground-plane metadata.
These do not by themselves prove a physically exact full K/pose calibration.
Choose an explicit source frame, validate a compatible pinhole view, and retain
the original values for reset. Do not copy calibration values, source media,
GLB bytes, local absolute paths or reference-derived screenshots into Git.

Start with a virtual PBR pitch receiver at `z=0`; pitch dimensions come from
explicit local input/metadata and retain their stated assumption status.
After model scaling/rotation, compute its world AABB over all material parts
and apply `translation_z = -max_z`. Negative Z is up, so **max_z is the
ground-facing bottom**, not min_z. A model's geometry does not need animation.

Place four authored light towers at `(+-L/2, +-W/2, -h)`, one per corner,
with h configurable between 30 and 40 metres; 35 metres is a proposed synthetic
default, not a fitted measurement. Provide point and aimed-spot modes, editable
emission/cones/source size, directional sunlight and Hard/PCF/PCSS controls.
Demonstrate sun-only, four-point, four-spot and towers-plus-sun configurations.
Any point-plus-spot combination on one tower is intentional and energy-accounted,
not accidental duplicate lighting. A full atmospheric sky is optional later.

Provide real-time camera zoom/dolly, orbit/rotation and reset using an editable
preview camera; never rewrite imported calibration. The browser harness controls
the server through WebSocket and views the existing SDI-derived WebRTC output.
NativeSocket/CLI controls must affect the same scene/camera, and both control
paths must have equivalent effects. Renderer-only fixtures can develop first,
but a direct-call viewer alone does not complete this interactive acceptance.
The initial virtual-plane demo is not evidence of solved video distortion,
external occlusion, or physical SDI hardware integration.

## 6. Acceptance criteria and remaining decisions

| Area | Required evidence when implemented |
|---|---|
| Protocol adapter | Approved RPC/native inputs produce equivalent retained-state changes and frame views; preserve existing framing; reject unsupported fields/resources; initial pinhole profile has no distortion fields |
| Control transports | Scene/model/camera/light/shadow/render instructions work through both NativeSocket and WebSocket with identical state/effect semantics; UI/CLI see one scene; media is not the control channel |
| Frame lifetime and binding | Out-of-order image/Ti arrival, stale references, readiness, held consumers, pool exhaustion, submit-time scene changes, logical replay, and batch-size invariance under the approved policies |
| Coordinate preservation | Ground/elevated projections agree with `S^-1 Ti A(C)`; signed Ti scale invariance; no added half-pixel shift |
| Model transforms | Independent instance matrices, whole-model/material-part movement, nonuniform-scale normals, matching bounds/shadows, and static GLB node transforms applied exactly once; explicit unsupported-animation/deformation diagnostics |
| Camera equivalence | FOV and equivalent K/pose render the same pixels; off-centre/skew calibration and portrait/landscape extents work |
| Orthographic camera | Parallel rays, depth-independent projected size, correct specular view direction, correct near/far clipping |
| Distortion | Synthetic radial/tangential cases, identity coefficients, CPU/GPU agreement, inverse-warp borders, failed inversion, and invalid calibration-domain rejection |
| Depth and composition | Occlusion for elevated geometry; behind-camera clipping; no double warp of an already-distorted background |
| Direct lights | Direction signs, inverse-square attenuation away from singularities, spot cones, zero emission, and scene-to-scene state isolation |
| Area emitters | Each shape/sidedness works; sample-count convergence; size-dependent highlights/penumbrae; documented power/radiance behavior |
| Sky | OpenEXR HDR values/channels/colour handling, environment rotation and sky/reflection agreement; sun elevation/altitude response; no double-counted sun; occlusion reduces sky lighting behind blockers |
| Shadows | Moving occluders, alpha-masked casters, nonuniform transforms, bias, seams, transitions, and deterministic sampling |
| PCF | Comparison-result filtering, configurable radius/sample count, stable edges during motion, and consistent 2D/cubemap behavior |
| PCSS | Hard contact and widening penumbrae with separation/source size; no-blocker and zero-size cases; correct linear-depth reconstruction; stable cascade/cubemap boundaries; comparison against sampled-emitter references |
| Raster method selection | Primary sources and portability reviewed; production methods benchmarked against reference integration for quality, GPU time, memory, stability and declared approximation limits |
| Football demo | Private local camera/GLBs stay local; transformed world AABB max_z=0; four 30-40 m corner towers, point/spot/sun configurations and soft shadows; live zoom/orbit/reset over both control transports |
| Output / resources | Single output transform, stable asset lifetimes, explicit budget errors, resize behavior, and equivalent native/offscreen core results |

Use synthetic fixtures only. Future code changes require the repository's
CPU/lint checks and real-hardware pixel checks on Linux. Interactive UI
acceptance belongs on an interactive device, not an SSH smoke test. These are
future acceptance requirements, **not claims of completed validation**.

Before implementing the protocol adapter, resolve the open business schemas,
effect ordering, frame resource/readiness/lifetime, scene-binding, and failure
policies in `docs/render-frame-input.md`; do not reopen the confirmed JSON-RPC
envelope or replace persistent scene state with full per-frame snapshots.
Before implementing the camera/lighting features, confirm the actual calibration model and valid image
domain, input Ti's ideal/distorted-domain metadata, required area shapes beyond
rectangle/disk/sphere, and target image size/frame-time/memory budget. Those
performance constraints determine sampling and cascade limits; no arbitrary
light-count cap or frame-rate promise is fixed here.

## 7. Source map

Use `trd` as an implementation reference, not a wholesale port. For each slice,
record what was reused, simplified, or corrected and why; add synthetic
regressions for corrected behavior. Distinguish an intentional limitation from
a demonstrated bug. In particular, do not copy its +Y-up conventions, fixed
light rig, arbitrary point attenuation epsilon, per-object tone mapping, or
blob-only shadow approximation as this roadmap's final behavior. Keep only the
helpers and abstractions justified by chr's actual callers.

Reference paths below are relative to the local `trd` checkout; no private
reference assets or datasets are copied into this proposal.

| Source | Evidence used |
|---|---|
| `trd: README.md`, `docs/architecture.md` | Feature inventory, shared core, delivery shells, assets, protocol, and media boundaries |
| `trd: crates/trd-core/src/camera.rs` | View/projection/viewport representation, CG/CV inputs, +Y-up convention |
| `trd: crates/trd-core/src/light.rs`, `render/pbr.rs` | Fixed directional rig, empty default point-light slots, scene-wide environment |
| `trd: crates/trd-core/src/shader/pbr.wgsl` | Live directional/point/IBL shading and per-object tone mapping |
| `trd: crates/trd-core/src/render/scene.rs`, `render/options.rs` | Scene/background separation, lighting ownership, modes and overlays |
| `trd: crates/trd-core/src/render/render_config.rs` | Blob-only shadow type and explicit shadow-map rejection |
| `trd: docs/pbr.md` | Material/GLB/environment scope and unsupported auxiliary material features |
| `xiaoyxue/Gyunity: Gyunity/src/mesh/Mesh.h`, `Gyunity/src/container/README.md` at `7361064` | CPU mesh-array ownership, contiguous storage and borrow/resize validation semantics; adapt to chr's indexed GPU meshes instead of porting ray-tracing structures |
| `chr: server/renderer/src/{lib,coordinates,triangle,offscreen,sdi}.rs` | Current triangle rendering, projection math, render targets, and SDI colour conversion |
| `chr: common/src/methods.rs`, `server/runtime/src/{scene,pipeline}.rs` | Implemented control methods, logical-frame triangle state, and SDI/monitor output boundary |
| `chr: docs/{protocol-conventions,render-frame-input,execution-model,calibration-control}.md` | Confirmed control/pinhole/persistent-scene boundaries and explicitly proposed resource/effect details |
| `chr: server/docs/coordinates.md`, `server/AGENTS.md` | Coordinate invariants, shell/core boundary, platform and validation requirements |
