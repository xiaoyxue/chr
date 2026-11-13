# chr server

The live render server: a Rust + wgpu renderer and the code that runs in the
same process with it. The live service (`chr-server`, its render worker and
`chr-monitor`) runs on **Linux**; Windows PCs only preview its SRT output. The
renderer and viewer also build on Windows for local development. The Cargo workspace is at the
repository root; its server crates live here:

| Directory | Crate | Role |
| --- | --- | --- |
| `renderer/` | `chr-renderer` | Coordinate math, GPU initialization, and rendering; no networking or device dependencies |
| `runtime/` | `chr-runtime` | Headless render server (`chr-server`): JSON-RPC control, render pipeline, render worker supervision, simulated SDI output, shared-memory program tap; `chr-renderer-cli` renders program stills and runs as the render worker |
| `viewer/` | `chr-app` | Thin winit shell for windows, resize, and presentation, plus headless PNG export |
| `docs/` | | Server contracts, such as `docs/coordinates.md` |
| `scripts/` | | Windows development helpers, a Linux software video-input probe and the `run-live.sh` launcher |

The video monitor is a separate process, `chr-monitor`, in the repository-root
`monitor/` crate (not under `server/`: it is optional and may run on another host
from an SDI loop-out). It reads the program tap that `chr-server` writes, encodes
it and streams it over WebRTC/SRT; it never controls the program. See
[Program monitor](#program-monitor-chr-monitor).

### Runtime module ownership

| Path under `runtime/src/` | Responsibility |
| --- | --- |
| `input/software.rs` | FFmpeg/GStreamer MP4 acquisition and native input sequence assignment |
| `input/ring.rs` | Frames from a capture process through the shared-memory input ring (`--tracked-scene`) |
| `calibration/replay.rs` | Demo-local lens preparation and native-frame association across validated clip loops |
| `calibration/tracking.rs` | Per-frame tracked cameras (`tracking.frame`) paired with captured frames by capture sequence |
| `program/description.rs`, `program/assets.rs` | Render descriptions: startup program, `description.load`, asset reading, verification and import |
| `program/` | Synthetic/pitch/video scene assembly, admission, and the GPU rendering bridge |
| `pipeline.rs` | Render loop: output pacing, triangle commands, program scheduling and clean feed; holds no GPU |
| `worker/` | Render worker protocol (`protocol.rs`), the worker side run by `chr-renderer-cli worker` (`serve.rs`) and its supervisor in `chr-server` (`supervisor.rs`) |
| `sdi.rs` | Simulated SDI output clock, bounded output queue, and the shared-memory program tap of transmitted slots |
| `gstreamer_support.rs` | Private mode validation and pipeline-error handling for the software input |

`chr-server` creates no GPU device. As decided in #94, its `chr-render` thread
sends every frame to a supervised `chr-renderer-cli worker` process (#97), which
calls the `chr-renderer` library; see [Render worker](#render-worker). #95 moved
video monitoring out of `chr-server` into `chr-monitor`.

Algorithm estimation stays in repository-root `algorithm/`. Runtime calibration
consumption does not implement that algorithm: the existing replay loads local
lens data at startup and associates it by native stream/frame identity.
`program/video.rs` retains the demo-specific GLB and scene setup. The opt-in
[tracked camera simulation](docs/tracking-simulation.md) receives `Ti`, `C` and a
frame warp per frame from `chr-tracking-sim`; it is a simulation, not a production
tracking service. Physical SDI adapters and a general public distortion protocol
remain separate work; the initial frame contract is still pinhole-only.

The default draws an RGB triangle using the same pipeline in a native window,
an offscreen hardware test, and the render server. The explicit
[`--synthetic-program` opt-in](docs/program-output.md) connects retained scene/image
composition to the existing simulated SDI and `chr-monitor` WebRTC monitor. Physical video
input, output cards and calibration are not included.

The runtime library now provides an explicitly installed
[retained scene control adapter](docs/retained-control.md), using the shared
scene/frame DTOs and the same handler over both IPC transports. Its synthetic
admission-port tests remain CPU-only; the separate program-output hardware test
exercises the opt-in GPU/resource/SDI/monitor integration.

The runtime library also exposes an isolated, bounded
[native synthetic frame-resource owner](docs/frame-resources.md), connected through
the submission scheduler in that opt-in. It does not implement physical capture/output.

The Linux renderer roadmap is described in
[Simplified renderer scope and design](docs/renderer-design.md)
([Chinese](docs/renderer-design.zh-CN.md)). It distinguishes the initial
pinhole/persistent-scene contract from later camera and lighting extensions;
these are design targets, not implemented features.

## Software MP4 input through FFmpeg and GStreamer

The Linux helper `scripts/mp4_gstreamer_source.py` reads an owner-supplied local
MP4, runs FFmpeg at realtime speed, and consumes complete frames in a bounded
GStreamer appsink:

```text
MP4 -> FFmpeg -> UYVY raw-video pipe -> fdsrc -> rawvideoparse -> appsink
```

This is **software input, not physical SDI**. It does not yet feed `chr-server`,
render overlays, output SDI, encode WebRTC, record media, or connect audio.
The existing renderer and control paths are unchanged. No source path or video
is bundled with the helper.

Run from the feature worktree root:

```sh
nix develop .#media
python -B server/scripts/mp4_gstreamer_source.py /path/to/input.mp4
python -B server/scripts/mp4_gstreamer_source.py /path/to/input.mp4 --loop --frames 360
```

Without `--loop`, decoding stops at EOF. `--frames N` requires exactly N frames
and fails if the input ends early; combine it with `--loop` to cross the file
boundary. `--loop` without a frame limit runs until interrupted. `--unpaced`
disables realtime pacing for a bounded diagnostic run. Ctrl-C/termination stops
the owned FFmpeg process and the GStreamer pipeline.

Input must explicitly declare progressive, square-pixel, limited-range BT.709
SDR video with a positive constant rate and even width. Dimensions and the
rational frame rate are probed, not silently rescaled. FFmpeg explicitly converts
the video to packed UYVY; audio, subtitles and other streams are omitted.
Autorotation is disabled, so the feed uses the stored pixel orientation.
Declared rates are normalized to CFR by FFmpeg; this is not a VFR-preservation
path. Missing/unsupported metadata fails instead of guessing colour or field order.

`rawvideoparse` generates a new software timeline from frame count and rate,
continuous across file loops. These timestamps are **not** original MP4 PTS,
captured SDI timestamps, timecode, tracking identities or calibration frame keys.
The source is backpressured when the two-buffer appsink fills; it is an
input probe, not the live server's drop/deadline policy. The final JSON reports
actual received frame count, bytes, caps, first/last software PTS and pixel
checksums. No media or metadata file is written.

Synthetic checks run without private inputs or hardware:

```sh
nix develop .#media --command python -B -m unittest discover \
  -s server/scripts -p test_mp4_gstreamer_source.py
nix build .#checks.x86_64-linux.media-source --no-link --no-update-lock-file
```

## Live software SDI simulation

The Linux-only `gstreamer-media` feature connects a local MP4 to the existing
retained renderer, simulated SDI clock and browser controls:

```text
MP4 -> FFmpeg -> GStreamer UYVY/RGBA -> retained renderer
    -> simulated SDI output -> program tap -> chr-monitor: GStreamer H.264 -> WebRTC -> WebUI
WebUI -> chr-server WebSocket JSON-RPC -> retained scene/camera state
```

```sh
nix develop .#media
server/scripts/run-live.sh --media \
  --software-video /path/to/video.mp4 --size 1280x720 --fps 30 \
  --no-socket --ws 127.0.0.1:18771 \
  --monitor-arg --encoder --monitor-arg gstreamer \
  --monitor-arg --ws --monitor-arg 127.0.0.1:18776 \
  --monitor-arg --webrtc-udp --monitor-arg none \
  --monitor-arg --webrtc-tcp --monitor-arg 127.0.0.1:18772
```

Use the existing host-driver wrapper below if Vulkan cannot access the hardware.
The source must be progressive, square-pixel, limited-range BT.709, with the
same integral rate and aspect ratio as the selected output. The software profile
retains the synthetic configuration's 1280x720 maximum. A 1920x1080 source is
explicitly scaled to the selected 1280x720 output; no audio is connected.
The sample geometry is deliberately small and **not calibrated to the video**.
The orange mesh and gray ground are separate instances. The live WebUI's
top-right mesh controller sends server-owned rotation commands, leaving the
receiver fixed. The `mesh.start_rotation`, `mesh.stop_rotation` and
`mesh.get_rotation` commands target `{runtime_id,scene_id,instance_id}`; Start
also requires `angular_velocity_rad_per_s`. They are enabled only with
`--software-video`. Server advances the local XY pose before each native input
admission using the SDI output-slot clock, preserving normal immutable scene
revisions and admitted-frame provenance. Speed changes preserve the angle;
Stop holds the pose. Page disconnects do not stop animation. External instance
edits stop it instead of being overwritten. Stop before other scene edits to
avoid optimistic-revision conflicts. Only one instance can animate at a time.
No private model, camera calibration or source file is bundled.

The input adapter assigns a new software stream sequence before its two-buffer
queue drops old frames; those identities and gaps survive native admission.
Frame resources and scene snapshots use the existing owner/scheduler. Output
still ticks independently, repeating the previous frame on underrun. Every
transmitted slot, repeats included, enters the program tap. With
`--encoder gstreamer`, `chr-monitor` encodes those slots with software x264 (the
old `chr-server` chose this encoder automatically for `--software-video`); this
is not zero-copy or hardware encoding.

WebRTC in `chr-monitor` uses the existing implementation and ICE-TCP forwarding, not
GStreamer's WebRTC backend. JSON-RPC and the default triangle/synthetic/pitch
scene/frame methods remain unchanged. The separate input probe above still performs no
rendering. Physical SDI, tracking, recording and production policy are not implied.

Use `client/example/render-harness` as the program monitor. It is read-only:
pass the server's WebSocket URL (`ws=ws://127.0.0.1:18771`) and the monitor's
(`monitor=ws://127.0.0.1:18776`); scene edits go through `npm run scene` or another
JSON-RPC client. Browser traffic can remain TCP-only through
SSH: forward the UI HTTP port, WebSocket ports 18771 and 18776 and ICE-TCP ports
18772-18775. Monitor disconnection, or stopping `chr-monitor`, never blocks
program output.

Pass `--srt 127.0.0.1:9001` to `chr-monitor` (`--monitor-arg --srt --monitor-arg
127.0.0.1:9001`, or an explicitly reachable interface address) to expose a direct
MPEG-TS/SRT listener. It needs the `gstreamer-media` feature but works with any
program and either encoder, not only `--software-video`. Open
`srt://SERVER_IP:9001` in VLC or a GStreamer `srtsrc`. Remote callers need UDP
reachability and a source-restricted firewall rule; SSH `-L` forwards TCP only.
The native path is:

```text
simulated SDI -> program tap -> chr-monitor: H.264 -> bounded encoded fan-out -> WebRTC
                                                                     -> h264parse -> mpegtsmux -> srtsink
```

Both outputs share the same encoded SDI access units; SRT never receives or
decodes WebRTC and never re-encodes H.264. A slow SRT consumer loses queued
access units and resumes at an IDR without stalling WebRTC or SDI. The SRT
listener keeps encoding without browser viewers, requests an IDR on caller
connect, and accepts reconnecting callers. This is software simulation, not
physical SDI or hardware encoding. The media shell includes the bad plugins
needed for H.264 parsing, MPEG-TS and SRT.

Focused checks inside the media shell:

```sh
cargo fmt --all -- --check
cargo clippy --locked -p chr-runtime -p chr-monitor --all-targets --features chr-runtime/gstreamer-media,chr-monitor/gstreamer-media -- -D warnings
cargo test --locked -p chr-runtime -p chr-monitor --features chr-runtime/gstreamer-media,chr-monitor/gstreamer-media
```

### Calibrated local clip replay

Add `--video-scene /path/to/scene.json` to `--software-video` to replace the
uncalibrated mesh with an owner-supplied static GLB, Ti/C camera, radial lens,
per-frame warp and a transparent GPU shadow catcher (the approach in #89).
This worktree incorporates the upstream renderer's #84–#88 changes; the public
scene/frame protocol is unchanged.

The startup-only JSON has these fields (paths are relative to the manifest):
`width`, `height` (native pixels), `fps`, `ti` (forward row-major 3x3 ground-to-SNC),
`center` (world metres), `radial` (`centre_px`, `scale_px`, three `coefficients`,
nullable `support` as `[min_xy,max_xy]`), `frame_warps` (one row-major 3x3
reference-native-pixel-to-frame-SNC matrix per decoded presentation frame),
`model` (GLB path), `height_m`, `anchor_pixel` (ideal reference native pixel)
and `pitch_m` (`[length,width]`). Model height and ground placement are authored,
not inferred. All private paths, coefficients and renders must stay local.

Native images remain at calibration resolution (up to 4096x2160); only the
composed output is fitted to the selected size (up to 1280x720, same aspect).
Startup decodes/probes the clip to require an exact warp count, matching native
extent/rate and zero-based contiguous CFR presentation timestamps. The decoder
does not resample frames. Its software sequence is assigned before input queue
drops; `sequence % clip_frame_count` selects the original warp across gaps and
loops. Repeated SDI slots reuse the composed result, not a new warp. This mapping
is valid only for this explicitly validated file replay, not live capture,
timecode, arbitrary VFR or tracking.

Only CG and its shadows are distorted; the display-referred BT.709 footage is
not warped or tone-mapped. Composition uses 4x MSAA, PCSS, an authored sun and
four corner spots. Lighting is not estimated from the footage. The existing
`mesh`/`ground` IDs let a JSON-RPC client (`mesh.start_rotation`) rotate the GLB
without moving its shadow catcher. Leave the camera alone: edits would invalidate
the clip alignment. This is diagnostic replay:
no scene occlusion, audio, physical SDI, hardware encoding or real-time estimator.

### Tracked camera simulation

`server/scripts/run-tracked.sh --scene scene.json --video clip.mp4 --srt <ip:port>`
replays the same manifest and clip as a tracked camera: `chr-tracking-sim` writes
each frame into a shared-memory input ring and sends its `Ti`/`C`/frame warp as
`tracking.frame`; `chr-server --tracked-scene` pairs them by capture sequence and
renders through the worker to the SDI port, which `chr-monitor` streams. See
[Tracked camera simulation](docs/tracking-simulation.md).

### Render description

`chr-server --description <json>` draws a `chr-render-description-v1` scene
(GLB and image-quad assets placed by 4×4 matrices, one asset shared by many
instances) through the same tracked camera. With `--asset-root <dir>`, clients
replace the scene at runtime with `description.load`; the football client sends
its Fusion match this way. Add `--description <json> --asset-root <dir>` to
`run-tracked.sh`. See [Render description](docs/render-description.md).

## M1: camera, indexed meshes and depth

The shared `MeshRenderer` now draws static scene instances and material-part
colours with camera depth. This is an opaque **unlit diagnostic baseline**, not
PBR lighting, GLB loading, shadows or the interactive socket-controlled demo.
The normal viewer/server triangle paths remain unchanged.

From the repository root, render the synthetic pitch and grounded test models:

```sh
nix develop
cargo run --locked -p chr-app --example mesh_milestone
```

The example saves `output/renderer/milestone-m1.png` and requires hardware GPU
rendering. On a non-NixOS NVIDIA host, use the nixGL wrapper documented below:

```sh
NIXPKGS_ALLOW_UNFREE=1 nix run --impure github:nix-community/nixGL#nixGLNvidia -- \
  cargo run --locked -p chr-app --example mesh_milestone
```

This is an offline core validation artifact, not a new client media stream or
an alternative control channel.

## Static GLB import

`chr_renderer::glb::import_glb` reads caller-owned GLB bytes into static mesh
parts, PBR factors and shared embedded textures. It preserves default node
transforms and converts glTF +Y-up to chr -Z-up once. Supported textures are
embedded PNG/JPEG decoded to RGBA8. No animation, skinning, morph evaluation,
alpha blending or external resource loading is performed.

Inspect a locally supplied model without adding it to the repository:

```sh
cargo run --locked -p chr-app --example glb_info -- /path/to/model.glb
```

The importer is independent of rendering. M1 remains an unlit diagnostic;
the same mesh renderer now also offers `prepare_pbr`.

## M2: local GLB and direct-light PBR

The PBR path supports material maps, correct colour-space mipmaps, normal mapping,
alpha masking, direct directional/point/spot lights, and one HDR exposure/tone-map
pass. M2 captures leave shadows and IBL disabled. Later milestones add them;
interactive control integration remains follow-up work; experimental area lights are
described below.

The offline example uses a private native diagnostic JSON fixture containing
`width`, `height`, forward row-major `ti`, world `center`, and `pitch` dimensions.
Prepare those values from the local package metadata and explicit frame map;
do not infer frame identity from row order or rewrite source coefficients.
This fixture is not a public scene/control protocol.

```sh
cargo run --locked -p chr-app --example pbr_milestone -- \
  /path/to/private-camera.json output/renderer/m2 /path/to/model.glb
```

Use the documented nixGL wrapper when needed. The example saves owner-local
`sun.png`, `points.png` and `spots.png`, comparing sunlight with four authored
35 m corner towers. These are static PBR diagnostics, not shadow or physical
camera-calibration acceptance. Do not upload private inputs or resulting images.

It also writes `standard.png` and `model-1.png`, `model-2.png`, etc. using a
level 45-degree FOV/look-at pinhole camera framed around the model bounds.
These views have zero skew and keep world-vertical lines vertical on screen,
without changing model orientation, placement, lighting or imported calibration.
Use them to distinguish projection-induced tilt from the asset's own geometry.

## M3: directional and spot shadows

`PbrSettings::shadows` now supports `Off`, `Hard` and `Pcf` for directional and
spot lights. Directional lights use a stable scene-fitted orthographic map;
spots use a cone-matched perspective map. This bounded-scene implementation does
uses a single directional map. M5 adds point-light cubemaps and M8 adds cascades.

Settings expose resolution, a square bilinear-comparison PCF radius in texels,
normalized depth bias, world-metre normal bias, and a memory budget. The depth
bias is a fraction of the depth span for directional maps and of the far distance,
applied to linear depth, for spot and cubemap maps; spot maps therefore keep a
metre-scale bias even when scene geometry beside or behind the light pulls their
fitted near plane close to it. Defaults are shadow-off, 2048 texels and a 256 MiB budget. Receiver-plane depth correction
prevents sloping surfaces from shadowing themselves. Caster alpha masks and
sidedness match the main material path.

Depth maps are reused when scene state and map settings are unchanged, including
camera-only motion and Hard/PCF switching. `shadow_passes_last_frame()` reports
actual depth passes encoded, not the number of enabled lights.

The same example additionally writes `shadow-off.png`, `shadow-hard.png` and
`shadow-pcf.png`. These use one ordinary pinhole overview, unchanged models and
the same sun-plus-four-spot rig, isolating the shadow setting. The legacy M2 and
front-inspection captures remain unshadowed. PCF filters edges; it is not PCSS
contact-hardening or finite-area-light integration.

## M4: directional and spot PCSS

`ShadowMode::Pcss` reuses those depth maps for linear-depth blocker search,
source-size penumbra estimation and variable-radius comparison filtering.
Each directional light supplies `angular_radius` in radians; each local light
supplies `source_radius` in metres. Zero gives exact Hard visibility. These
parameters affect shadow softness, not the punctual BRDF or emitted power.
M5 below extends this path to point shadows.

`ShadowSettings::pcss` defaults to 32 blocker + 32 filter samples, independent
64-texel search/filter caps. The shared shadow sample budget defaults to 1024
logical operations per fragment across lights.
Each stage accepts 1..=64 samples; invalid settings and budget overflow fail,
never silently downgrade to PCF. Counts represent logical texture operations;
comparison samples use hardware bilinear filtering. Deterministic disk sampling
needs no temporal history. Caps deliberately limit wide-penumbra accuracy, and
very thin blockers can be missed by the bounded search.

The example also writes `shadow-pcss.png` with the same M3 rig and
`shadow-pcss-wide.png` with source radii multiplied by four, unchanged irradiance
and intensity. Baseline sources are a 0.00465-radian sun and 0.5-metre spots.
No sky/IBL or LTC is included. PCSS is high-priority work; area/LTC is deferred
at low priority. Broader performance acceptance remains separate.

## M5: point-light cubemap shadows

Point lights now support Hard/PCF/PCSS using six faces in the shared depth array.
Each face stores `distance_to_light / far_radius`, not perspective depth.
The far sphere encloses visible scene bounds; the 0.01-metre near sphere excludes
casters and shadow lookups inside it without changing light attenuation.

Point PCF/PCSS kernels use a direction/tangent-space footprint. Every sample,
including the four neighbours of each bilinear comparison, is remapped across
cube faces. Receiver-plane comparisons use the radial distance at the fetched
texel, including grazing/far-boundary cases. PCSS averages radial blocker
distances and conservatively bounds its search using the nearest scene-bound
radius. Source-size approximation, deterministic quality caps, alpha masks,
sidedness and cache invalidation follow the existing shadow contract.
Point filter radii use face-centre texel angular units (`2 / resolution`);
they do not clamp a kernel to its originating face.

Memory accounting includes all six faces per point light and never silently
reduces quality. The new `point-off.png`, `point-hard.png`, `point-pcf.png` and
`point-pcss.png` captures use sun + four points at an explicit 1024 resolution:
25 depth layers, 100 MiB. Reusing maps across Hard/PCF/PCSS still avoids depth
redraws. The earlier sun + spot captures retain their 2048 setting.
Neither preview adds sky/IBL, LTC or live controls.

## M6: per-light shadow modes

Every `Light` has `shadow_mode: Option<ShadowMode>`. `None` inherits
`PbrSettings::shadows.mode`; `Some(Off)` disables only that light's shadows,
not its illumination. Explicit Hard/PCF/PCSS overrides also work when the
frame default is Off. `ShadowMode` remains available at the crate root and is
also exported from the GPU-independent `scene` module.

Only enabled shadow lights consume map layers. Effective PCSS lights
validate their quality even if the
frame default is Off. The cache includes the active light set; changing the
default between filtering modes reuses maps when that set is unchanged.
Editing authored overrides changes the scene snapshot and conservatively
rebuilds its maps. Unlit diagnostics ignore all shadow overrides.

`point-mixed.png` retains the sun/four-point illumination and 1024 resolution:
sun PCSS, then corner points Hard/PCF/PCSS/Off in the example's X/Y iteration
order. This uses 19 depth layers (76 MiB); the last point still illuminates.
Automatic GPU checks isolate coloured light contributions, allocation/cache
changes and invalid quality/budget cases. This remains offline diagnostics,
not NativeSocket/WebSocket scene-control acceptance.

## M7: PCF quality and shared sample budgets

`ShadowSettings::pcf_samples` selects a square kernel with
1, 4, 9, 16, 25, 36, 49 or 64 bilinear comparisons. The default remains 9;
1 performs a single centre comparison. Larger kernels distribute samples over
the same `filter_radius` footprint rather than inventing a physical penumbra.
Point kernels retain their cross-face remapping. Sample-count changes reuse maps.

`max_samples_per_fragment` now belongs to `ShadowSettings`, not `PcssSettings`.
It counts effective Hard as 1, PCF as `pcf_samples`, and PCSS as blocker + filter
samples; Off costs zero. Invalid active quality or total-budget overflow is an
error, without lowering quality or dropping lights. This is a logical-operation
budget: one filtered comparison can access four depth texels.

The example adds `point-pcf-64.png` and `point-mixed-all.png`. The latter keeps
all five lights shadow-enabled, unlike the explicitly labelled one-shadow-Off
`point-mixed.png` fixture. Both retain unchanged illumination. Directional
performance acceptance remains separate from these functional diagnostics.

## M8: directional cascades

`ShadowSettings::cascades` accepts one scene-fitted map (the unchanged default)
or 2..=4 view-frustum cascades. `max_distance` defaults to 200 metres, clamped
to the camera far plane. `split_lambda` blends linear/logarithmic splits;
`blend_fraction` defaults to 0.1 of each interval, including a final fade to
unshadowed beyond the explicitly configured distance.

Overlapping frustum-slice spheres keep map extents stable under rotation, with
world-texel-snapped centres. The light-depth interval retains all visible scene
bounds so offscreen casters still contribute. Every cascade converts the same
PCSS world-space penumbra to its own texel footprint. Blended intervals budget
two visibility evaluations; memory budgets count every map.

Camera-dependent cascades recompute their fit on view changes, but reuse maps
when snapped projections and scene state are unchanged. Single-map and point/
spot behavior is unchanged. `sun-single.png` and `sun-cascades.png` compare one
and four maps at 1024 with identical sun-only lighting and ordinary pinhole view.
Automated tests cover overlap, camera stability, all shadow modes at splits,
PCSS reference visibility, offscreen casters, mixed cube/cascade offsets, range
fading and budget failures. These are not a realtime frame-rate guarantee.

## M9: OpenEXR image-based lighting

`SkyLight` now supplies diffuse and specular environment illumination plus an
independently visible background. Decode bytes with `exr::import_exr`, then share
the resulting `Arc<EnvironmentMap>` through scene lighting. Rotation/gain are
shared by reflections and background; exposure remains the final output step.
Panorama coordinates put +X at (0.5, 0.5), +Y at (0.75, 0.5), and upward -Z at V=0.

Diffuse IBL uses three-band SH with exact per-pixel solid-angle integrals and a
Lambert convolution. Specular uses deterministic GGX roughness prefiltering and
a split-sum BRDF LUT with the direct path's correlated Smith convention. Six
full-resolution roughness layers avoid collapsing rough directional content
into a single final panorama texel. Float32 textures use explicit bilinear reads,
without requiring optional float32-filtering support. Normalized GPU storage
with a separate scale retains HDR values before the existing RGBA16F output bound.

`PbrSettings::environment` controls prefilter width (64..=1024, powers of two),
sample count (1..=1024) and active resource budget (default 256 MiB). Defaults are
256x128 layers and 64 samples; the BRDF LUT uses 64x64 texels and 256 samples.
Source/quality changes rebuild cached data; rotation, gain, background visibility
and output exposure do not. `environment_preprocessed_last_prepare()` reports
actual source-dependent preprocessing. Scene switches without a sky clear it.

This is explicitly **unoccluded IBL**: material AO affects indirect illumination
only and does not implement scene sky visibility. Three-band SH, split-sum's
normal-view prefilter and single-scattering GGX are approximations; diffuse IBL
is Lambert rather than the direct path's Burley model. Higher-order/visibility/
multiple-scattering methods remain separate work.

The example adds background/hidden/rotated environment captures. Append
`--environment /path/to/local.exr` to load an owner-local EXR; otherwise these
captures use a labelled synthetic studio panorama, not physical atmosphere.
Automated CPU/GPU checks cover HDR energy, roughness, reflection/background
rotation, visibility independence, AO/direct-shadow separation, cache replacement
and budget errors. No manual image approval is required.

## Experimental area lights (`ltc-plausible-v1`)

`Light::Area` adds unoccluded rectangle, disk and outward-sphere emitters with
explicit centre, front normal, tangent, dimensions, linear RGB radiance in
W/(m^2 sr) and planar sidedness. `AreaShape::radiance_from_flux` converts a fixed
total power to radiance so a size change can keep power instead of radiance fixed.
Shading uses the pinned official LTC GGX table with Schlick amplitude for
specular and `base*(1-metal)*(1-F0)*Lambert` diffuse, integrating four rectangle
vertices or `AREA_CURVE_SEGMENTS` (64) curved/silhouette vertices clipped to the
receiver hemisphere. Accuracy is the owner-approved approximation profile in
`docs/area-light-evaluation.md` (per emitter `<= 0.02*Le + 30%` of the reference
inside its domain), not reference accuracy; outside that domain the same
approximation is unqualified. Area lights draw no visible emitter; add an emissive
mesh when the emitter itself should appear (inside a sphere emitter, or in or behind
a planar emitter's plane, so it casts no shadow on its own light).

Area-light shadows follow the per-light `shadow_mode` like other lights and use the
owner-approved `area-shadow-pcss-v1` approximation: one centre cubemap filtered by
PCSS with `AreaShape::equivalent_radius` as the source radius, multiplied with the
unoccluded LTC result. Penumbrae ignore the emitter outline and glossy BRDF
weighting. The default 64-texel PCSS caps clip metre-scale penumbrae; scenes with
shadowed area lights should use `resolution: 1024`, `max_search_radius` and
`max_filter_radius` of 512 texels and 16 + 16 PCSS samples (24 MiB per area light).
`cargo test --locked --release -p chr-renderer --test area_shadow_gpu -- --ignored --nocapture`
enforces the approved error limits against a ray-cast reference, checks cache
invalidation and emitter stand-ins, and prints 1080p frame costs.

The table is redistributed under `server/renderer/src/ltc/LICENSE.ltc`, which
requires retaining its notice **and citing** Heitz, Dupuy, Hill and Neubelt,
"Real-Time Polygonal-Light Shading with Linearly Transformed Cosines", ACM TOG
35(4), 2016, alongside any binary that contains it, including the renderer,
viewer and runtime binaries.

GPU checks: `cargo test --locked -p chr-renderer --test area_light_gpu -- --ignored`
reproduces the pinned candidate values, compares six fixtures with an independent
high-sample reference of the shared GGX/Burley material, and checks sidedness,
sphere interiors, fixed-radiance/fixed-power size changes and continuity along
moving-emitter and view trajectories.

## Lens distortion (`radial-poly-native-px-v1`)

`distortion::CameraModel` selects the ideal pinhole or a distorted native image:
`RadialDistortion` (explicit centre and scale in native pixels, `k1..k3`, optional
validated support rectangle) plus an optional per-frame `FrameWarp` homography from
reference native pixels to frame SNC. `LensDistortion` offers CPU forward/inverse
mapping with explicit failures; `FrameCompositor::prepare_model` renders ideal CG
over a guard band and inverse-warps it on the GPU, leaving footage layers unwarped.
See `docs/renderer-design.md` section 3.3. GPU checks (`distortion_gpu`) cover
zero-coefficient identity, sub-0.3 px marker alignment for both models, unwarped
footage with no CG at non-invertible pixels, guard-band budgeting, and output
resizing (Contain/Cover/Stretch fit the distorted native image without rescaling
Ti or calibration). No wire
or calibration-storage fields are added.

## Native image composition

`composition::FrameCompositor` records renderer-only image/CG composition into a
caller-supplied whole texture. `ImageView::new` validates a single-sample 2D
sampleable colour texture and its explicit Linear/sRGB/BT.709 transfer pairing.
GPU contents and same-device/producer synchronization remain caller contracts.
Primaries are sRGB/Rec.709 D65 throughout; transfer tags do not perform gamut conversion.
It does not acquire frame resources, parse `FrameKey`, schedule frames or stream media.

Layers have explicit stages: scene-linear background/foreground around CG, then
display-referred background/foreground around the scene output transform. All
intermediates use linear-light premultiplied alpha. Straight inputs are decoded
and premultiplied before filtering, preventing transparent-colour fringes.
MSAA CG is resolved in HDR, unpremultiplied for tone mapping and premultiplied
again for composition. Display plates/overlays bypass scene exposure and tone mapping.

Scene and display-background rectangles use native camera pixels; display
foreground rectangles use output pixels. The configured Contain/Cover/Stretch
fit applies equally to native CG and native image layers, without altering Ti,
intrinsics or pixel-centre conventions. Sky remains part of CG; hide its
background when a plate should be visible behind geometry.

`CompositionOutput` fixes extent, fit, PBR/output transform, transfer, output
alpha representation, clear colour and additional-canvas budget. Default output
is straight sRGB over opaque black. Linear output is display-linear after the
configured scene tone map, not an HDR export bypass. Encoded premultiplied output
means premultiplication in linear light before encoding. Unsupported target
format/transfer/extent/sample/mip combinations fail explicitly.

Call `prepare`, `encode`, then submit before preparing another frame. Bind groups
retain GPU handles, but the runtime must retain external leases and prevent
producer overwrites until completion. `set_sample_count` enables shaded 1x/4x CG;
diagnostic display views remain on `MeshRenderer`, rather than being tone-mapped
as scene radiance.

```sh
cargo run --locked -p chr-app --example composition_milestone
```

`PbrMaterial::shadow_catcher` marks an invisible receiver, typically a ground plane
standing in for captured footage. In shaded PBR views it outputs premultiplied
black with alpha `1 - shadowed/unshadowed`, the luminance ratio of its direct light
with and without visibility (unoccluded IBL counted in both), computed in linear HDR
on the GPU. "Over" blending therefore multiplies whatever lies behind it, the clear
colour or a display background, by that ratio; it occludes and casts shadows like
real ground and is drawn after opaque geometry. Ratios are luminance-weighted, so
coloured light gives grey shadows; virtual light never brightens the plate. The
shared wire material has no catcher field yet.

This synthetic offscreen example verifies that changing CG exposure leaves its
display plate and overlay unchanged. GPU regressions cover colour domains,
premultiplied filtering, BT.709, native Ti alignment, virtual depth, orthographic/
pinhole MSAA, source replacement, resize and budget/target failures, and
`shadow_catcher_gpu` checks that catchers halve a plate where one of two equal
lights is blocked, leave it unchanged without shadows, and draw no colour alone. These checks
need no server and do not claim live protocol or physical SDI acceptance.

## Render server

`chr-server` has its render worker draw the triangle offscreen (1280x720p50 by
default) and takes JSON-RPC 2.0 commands (`triangle.start_rotation`,
`triangle.stop_rotation`, `triangle.get_state`, `sdi.get_status`,
`render.get_worker_status`) over a NativeSocket at
`$XDG_RUNTIME_DIR/chr/render.sock` and a loopback WebSocket at
`ws://127.0.0.1:8787`. It does not encode or stream video (`webrtc.offer` returns
`method not found`). The renderer's only product is the SDI program output;
clients receive that output back as WebRTC from the separate `chr-monitor`:

```text
args (JSON-RPC) -> control -> pipeline -> render worker -> SDI frame (UYVY 4:2:2, BT.709 limited)
                                 -> sdi: program output port (own clock, bounded queue,
                                         repeat on underrun)
                                      -> program tap (shared memory, never waits)
                                           -> chr-monitor: H.264 -> WebRTC/SRT -> clients
```

| Module (`runtime/src/`) | Role |
| --- | --- |
| `control.rs` | Control plane: JSON-RPC methods (the args) onto the pipeline and status |
| `pipeline.rs`, `scene.rs` | Render loop in logical frame time; produces SDI frames through the worker, or clean feed |
| `worker/` | Starts, feeds and replaces the `chr-renderer-cli worker` process |
| `sdi.rs` | SDI output port, simulated until hardware is chosen; writes the program tap |

`sdi.get_status` reports `video_mode`, `pixel_format`, `frames_output`,
`repeated_frames`, `dropped_frames` and `last_source_frame`; it no longer has
`monitor_dropped_frames` (monitor counters are in `chr-monitor`'s
`monitor.get_status`). From the repository root:

```sh
cargo run --locked --release -p chr-runtime -- --help
cargo build --locked --release -p chr-runtime   # also builds chr-renderer-cli, the render worker
cargo run --locked --release -p chr-runtime
```

### Render worker

`chr-server` never creates a GPU device, including for the default triangle. It
starts `chr-renderer-cli worker` (`--renderer-cli <path>`, default:
`chr-renderer-cli` next to the `chr-server` executable; the package build and
the flake package provide both, while `cargo run` builds only `chr-server`) and
sends it one request per frame as a JSON line on its stdin; replies come back on
its stdout and its stderr is
`chr-server`'s log. Input images and rendered UYVY frames travel through two
4-slot `chr-shm` rings in a private 0700 directory,
`$XDG_RUNTIME_DIR/chr/worker-<pid>/` (or the temp directory), removed on
shutdown. The worker installs the same program with the server's runtime ID,
logs its adapter and rejects software adapters. The protocol is in
[Live process implementation plan](../docs/live-process-implementation.md#step-4-worker-supervision-and-clean-feed-97).

- `--render-deadline-ms <n>` (default: 3/4 of the frame period, leaving time
  for the clean-feed conversion): a frame the worker has not finished by then,
  or by the render loop's next frame boundary, is sent as **clean feed**; a late
  completion only frees its ring slot. A worker whose GPU device is lost exits
  and is replaced like a crash.
- `--worker-hang-ms <n>` (default 2000): a worker whose oldest outstanding
  request is older is killed (`SIGKILL`) and replaced. Exits and fatal errors
  are replaced too, after a backoff of 250 ms doubling to 5 s (reset after 10 s
  of healthy running); a worker not ready within 60 s is replaced.

Clean feed is the frame's input image converted to UYVY on the CPU
(`chr_renderer::uyvy_from_bt709`, bit-identical to the GPU converter) and sent
without CG or a program association; the submission is reported as failed. It
is used on a deadline miss, a worker failure, when no worker is ready, or when
no ring slot is free. Inputs that are not 8-bit BT.709 at the output extent, and
the triangle, which has no input, repeat the last output frame instead.
`render.get_worker_status` reports `state` (`starting`, `ready`, `backoff`),
`pid`, `restarts`, `deadline_misses`, `clean_feed_frames`,
`repeated_for_unavailable_clean_feed` and `last_error`.

Hardware acceptance (`kill -9`, `SIGSTOP`, deadline misses and worker/in-process
pixel parity) runs from the repository root:

```sh
cargo test --locked -p chr-runtime --test render_worker -- --ignored --nocapture
```

### Program monitor (chr-monitor)

The SDI output thread copies every transmitted output slot, repeats included,
into the **program tap**: a `chr-shm` ring of 8 slots at `--monitor-tap <path>`
(default `$XDG_RUNTIME_DIR/chr/program.tap`, created mode 0600 in a 0700
directory; `none` disables it). Each slot carries the UYVY 4:2:2 frame plus its
output slot index, source frame, program flag, size and rate. The writer never
waits for readers, so killing, stalling or not running `chr-monitor` has no
effect on program output or `sdi.get_status` cadence.

`chr-monitor` polls the tap at half the frame period and, while anyone watches
(WebRTC viewers or an SRT listener), encodes the newest slot and counts skipped
ones. It waits for the tap if it does not exist yet and reopens it when
`chr-server` restarts. Its loopback WebSocket (default `ws://127.0.0.1:8789`)
serves `webrtc.offer` (previews close with that connection) and
`monitor.get_status` (`frames_encoded`, `frames_skipped`, `viewers`,
`srt_callers`); it has no program control. `--size` and `--fps` must match
`chr-server`; slots in another mode are skipped and logged. Encoding is software
after a CPU readback (openh264 by default, or GStreamer x264 with
`--encoder gstreamer`); this is a development shortcut, not the intended
GPU-to-hardware-encoder path or physical SDI loop-out (#80).

```text
chr-monitor [--tap <path>] [--size <WxH>] [--fps <n>] [--ws <addr>] [--encoder openh264|gstreamer]
            [--srt <ip:port>] [--webrtc-udp <addr|none>] [--webrtc-tcp <addr>] [--allow-origin <origin>]
```

Run it next to `chr-server` with the same `--size`/`--fps`:

```sh
cargo run --locked --release -p chr-monitor -- --webrtc-tcp 127.0.0.1:8790
```

`--webrtc-tcp` adds ICE-TCP candidates on four consecutive ports (8790-8793
here) so a browser on another machine can connect through an SSH port forward
(also forward the monitor WebSocket, 8789). The range must not include the
`--ws` port; the monitor refuses to start otherwise. `--srt` and `--encoder gstreamer` need
`--features gstreamer-media` inside `nix develop .#media`.

`server/scripts/run-live.sh [--media] [--no-monitor] [--monitor-arg <arg>]... [chr-server options]`
builds and starts both from the repository root inside the development shell.
`--media` enables `gstreamer-media` for both; `--size`, `--fps` and
`--monitor-tap` (as `--tap`) go to both; `--monitor-arg X` passes `X` to
`chr-monitor`; every other option goes to `chr-server`. The processes are
independent: one that exits is restarted after a short backoff (1 s, doubling up
to 10 s while it keeps failing) and the other keeps running; Ctrl-C stops both.
`--no-monitor` starts only `chr-server`, so `chr-monitor` can be started, stopped
or moved on its own with the same `--size`, `--fps` and tap:

```sh
server/scripts/run-live.sh --monitor-arg --webrtc-tcp --monitor-arg 127.0.0.1:8790
```

The browser program monitor (read-only: video plus server, worker, tracking,
monitor and WebRTC diagnostics) and verification scripts are in
`client/example/render-harness/`.

### Program stills (chr-renderer-cli)

`chr-renderer-cli render` renders one frame of a `chr-server` program without a
server. It installs the program with the same code, creates the scene through
`scene.create`, admits one input frame and composes it with the program bridge;
only the end differs: the BT.709 RGBA program image is written as PNG code values
(no colour conversion and no sRGB tag) instead of being converted to UYVY for the
port. A GPU test checks that the PNG, converted by the same UYVY converter, equals
the frame `chr-server` transmits.

```sh
cargo run --locked --release -p chr-runtime --bin chr-renderer-cli -- \
  render --synthetic-program --size 640x360 --output output/renderer-cli/synthetic.png
```

`--pitch-demo <json>`, `--video-scene <json>` or `--tracked-scene <json>` select the
other programs (a video or tracked scene also needs its clip rate in `--fps`). `--state <json>` replaces the initial
scene with a `chr-common` `SceneState` (its `configuration_id` is replaced by the
program's); `--background <png>` replaces the native input, read as BT.709 RGBA
code values at the program's native extent; `--frame <n>` sets the input frame
sequence, which selects the lens model of a video scene or the replayed camera
of a tracked scene. All inputs are validated
before a GPU is requested. The output is never replaced without `--overwrite`, and
software GPU adapters are rejected. A general scene file with arbitrary model paths
is not supported; it must not become a de facto asset or distortion schema.

Every run also saves a 960x640 sRGB image to repository-root
`output/renderer/triangle.png`, from any directory in the checkout. Outside a
checkout, the packaged app uses `output/renderer/triangle.png` under the working
directory. The image is retained locally and replaced on the next run; root
`output/` is ignored by Git. To export without opening a window (including Linux
SSH), run from the repository root in the development environment:

```sh
cargo run --locked -p chr-app -- --headless
```

## Windows (PowerShell 7, no Nix)

Install Rust with rustup (MSVC host), Visual Studio Build Tools with
**Desktop development with C++** and a Windows SDK, and your vendor GPU driver.
Then, from the repository root:

```powershell
. .\server\scripts\dev-env.ps1
cargo run --locked -p chr-app
```

The helper only prepares the current shell; it never installs tools or changes
global Rust settings. The default wgpu adapter selection requests high performance.
To select a backend explicitly:

```powershell
$env:WGPU_BACKEND = 'dx12' # or 'vulkan'
cargo run --locked -p chr-app
Remove-Item Env:WGPU_BACKEND
```

## Linux

Nix is both a development shell and a build system. The single `flake.nix` and
`flake.lock`, the Cargo workspace (`Cargo.toml`, `Cargo.lock`), and the Rust
toolchain file (`rust-toolchain.toml`) live at the repository root. Preserve the
lockfiles unless intentionally updating inputs. From the repository root:

```sh
nix develop
cargo run --locked -p chr-app
```

Or build, check, and run the packaged application from the repository root:

```sh
nix build
nix flake check
nix run . -- --headless
```

Use `nix run` without `--headless` for the native window. The default package,
named `chr-app` package, default app, Nix formatter, and formatting/Clippy/test
checks remain available.

Nix flakes in Git only include tracked files. Add new project files explicitly
before a build; never add the private root `reference/` directory. The build
source is restricted to the root Cargo manifests and the Cargo sources and `.wgsl`
shaders under `server/`, `monitor/` and `common/`; private references, generated outputs,
caches, and worktrees are excluded. The shell sets an absolute `CARGO_TARGET_DIR`
pointing to `target/linux`, so changing directories does not create another
cache. Cargo outside this shell uses `target/` by default. Root Nix `result` /
`result-*` links, root `output/`, and root `target/` are ignored.

Plain Cargo also works without Nix. Install Rust stable, a C/C++ linker,
pkg-config, X11/Wayland development libraries, and a working Vulkan/OpenGL driver.
For example, on Ubuntu:

```sh
sudo apt-get install build-essential pkg-config libx11-dev libxcursor-dev \
  libxi-dev libxrandr-dev libwayland-dev libxkbcommon-dev libvulkan1 libegl1
cargo run --locked -p chr-app
```

Run the window under a graphical desktop session. Linux acceptance runs on a
native Linux GPU host, not WSL; SSH is sufficient for offscreen tests and PNG
export. Inspect the logged adapter rather than accepting a software renderer.
On non-NixOS Linux, Nix may need nixGL to reach host drivers; for NVIDIA,
inside `nix develop`:

```sh
NIXPKGS_ALLOW_UNFREE=1 nix run --impure github:nix-community/nixGL#nixGLNvidia -- \
  cargo run --locked -p chr-app
```

Use the corresponding nixGL wrapper for your GPU. DX12 is Windows-only;
Vulkan is the normal native Linux backend.

For the built package, run the wrapper from the repository root:

```sh
NIXPKGS_ALLOW_UNFREE=1 nix run --impure github:nix-community/nixGL#nixGLNvidia -- \
  ./result/bin/chr-app --headless
```

## Checks and controls

Run Cargo checks from the repository root inside the development shell:

```sh
cargo fmt --all -- --check
cargo clippy --locked --workspace --all-targets -- -D warnings
cargo test --locked --workspace
cargo test --locked -p chr-renderer -- --ignored --nocapture
cargo run --locked -p chr-app -- --smoke-test
```

The GPU tests check real offscreen pixels, RGB orientation, coverage, aspect,
and rotation in landscape and portrait targets, including padded-row readback,
plus the UYVY conversion against BT.709 reference values; they are ignored by
ordinary CPU-only tests. PNG export uses this same readback path and pipeline.
The smoke test exits after three presented frames and requests a resize.
Interactively verify resize, minimize/restore, display scaling, and Escape/close.
Escape or the window close button exits the normal demo.

## Coordinates and contribution rules

See `docs/coordinates.md` and `AGENTS.md` in this directory. World coordinates
are pitch-centred metres with **negative Z upwards**. Forward row-major Ti maps
ground coordinates to SNC, which uses `sqrt(width * height)` and native pixel
centres without a half-pixel shift. The GPU boundary alone flips image Y to
wgpu's Y-up NDC.

The triangle uses synthetic ground geometry and identity Ti. Local `reference`
materials and measurements are private, ignored by Git, and not redistributed.
