# Simplified renderer scope and design

Status: **roadmap; CPU scene foundations implemented**. This document compares the local `trd`
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
not a live scene-control protocol: GLB/EXR decoding, texture bindings, GPU mesh/PBR
rendering, area/atmosphere lighting and runtime adaptation remain follow-up work.
The existing triangle path is unchanged.

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

### 3.3 Distorted lenses

The initial live frame profile has no distortion. For a later, separately
approved extension, propose `None` and **Brown-Conrady radial/tangential distortion**.
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

Primary references:

- [Heitz et al., Real-Time Polygonal-Light Shading with Linearly Transformed Cosines (2016)](https://eheitzresearch.wordpress.com/415-2/).
- [NVIDIA PCF/PCSS sample and original 2005/2008 references](https://archive.docs.nvidia.com/gameworks/content/gameworkslibrary/graphicssamples/opengl_samples/softshadowssample.htm).
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

Tracked separately in [#40](https://github.com/xiaoyxue/chr/issues/40).
The independent algorithm-selection gate is [#35](https://github.com/xiaoyxue/chr/issues/35);
wire-facing schema decisions are [#12](https://github.com/xiaoyxue/chr/issues/12).

Use the user-provided local SC1464 bundle and local GLB directory as optional,
private acceptance inputs. Inspection confirmed source Ti, explicit frame
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
