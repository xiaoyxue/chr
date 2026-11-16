# Owner-local interactive pitch demo

`pitch-demo-v1` is an explicit **local demo configuration**, not production
registration, measured calibration, stadium reconstruction or corrected video.
The default triangle and `synthetic-program-v1` limits remain unchanged.

## Run a reproducible synthetic scene

From the worktree root, inside the existing Nix environment:

```sh
mkdir -p output/pitch-demo
chmod 700 output/pitch-demo
nix develop --command cargo build --locked -p chr-runtime  # also builds the render worker
nix develop --command cargo run --locked -p chr-runtime -- \
  --pitch-demo server/runtime/tests/fixtures/pitch-demo.json \
  --pitch-controls output/pitch-demo/controls.json \
  --size 640x360 --fps 25 --ws 127.0.0.1:8787 \
  --socket "$PWD/output/pitch-demo/control.sock"
```

Non-NixOS NVIDIA hosts can wrap the Cargo command with the existing
`NIXPKGS_ALLOW_UNFREE=1 nix run --impure github:nix-community/nixGL#nixGLNvidia --`
inside `nix develop --command bash -c '…'`. The controls file is created exclusively,
with mode 0600 on Unix: choose a fresh filename for each server start. Never reuse
runtime handles across restarts.

Start `chr-monitor` with the same `--size`/`--fps` (or launch both with
`server/scripts/run-live.sh`; see the [server README](../README.md)) and watch the
program in the harness's read-only monitor page
(`npm --prefix client/example/render-harness run dev`). Edit the scene with the
harness's `npm run scene` CLI below, which reads the generated controls JSON.
It only reads metadata; it never loads GLBs or native filesystem paths through RPC.
Metadata contains source-camera information: keep it and screenshots private.

The CLI provides zoom, dolly, origin-centred orbit, local yaw/pitch/roll, exact
original-pinhole reset, model selection/scale/yaw/mirror, four-point or four-spot
rigs, sun-only and either rig plus sun, and Hard/PCF/PCSS. Heights are authored
30–40 m; **35 m is a demo value, not a surveyed tower height**. Spot targets are
four explicit pitch XY positions. Source radius, cone angles, tower emission and
independent sun direction/angular radius/emission are editable. A rig contains
four points **or** four spots, never both. Set sun emission to zero for a dark
sun-only comparison.
The simplified support poles stop one metre below their emitters, avoiding a
solid support intersecting the punctual light source and blocking its own rig.

All edits are bounded, revision-checked `scene.mutate` calls. A conflict or
uncertain reply is reported, never blindly retried; query again before the next
edit. Reset comes from immutable startup metadata, not a potentially modified
scene loaded after reconnect.

Over NativeSocket:

```sh
nix develop --command npm --prefix client/example/render-harness run scene -- \
  --socket "$PWD/output/pitch-demo/control.sock" \
  --pitch-controls "$PWD/output/pitch-demo/controls.json" \
  --rig points+sun --height 40 --shadow pcss --summary
nix develop --command npm --prefix client/example/render-harness run scene -- \
  --socket "$PWD/output/pitch-demo/control.sock" \
  --pitch-controls "$PWD/output/pitch-demo/controls.json" \
  --camera reset --model 1 --scale 1.2 --yaw 30 --mirror --summary
```

`--rig-settings '<JSON>'` accepts the local harness `RigEdit` object for precise
targets, cone/radius/emission and sun controls (see `src/pitch-controls.ts`).
`--ws ws://127.0.0.1:8787` may replace `--socket`. `--summary` avoids printing
camera coefficients. These are CLI conveniences, not new wire fields or methods.

## Owner-local packages

`--pitch-demo <local-manifest.json>` accepts:

```json
{
  "camera": "normalized-camera.json",
  "models": [
    {
      "path": "model.glb",
      "model": [[1,0,0,0],[0,1,0,0],[0,0,1,0],[0,0,0,1]],
      "anchor_pixel": [320,240],
      "height_m": 5
    }
  ]
}
```

Paths resolve relative to the manifest; explicitly configured absolute local
paths also work. `path: null` explicitly selects the committed synthetic
multipart block. `model` is a forward row-major native model-to-world transform,
after the standard GLB importer basis conversion. Nonuniform scales, rotations
and reflections are supported. Optional `height_m` rescales the transformed
all-parts world AABB to the authored height (0.05–40 m); optional `anchor_pixel`
uses the renderer's pinhole ray and a forward ground intersection. Omitting these
options preserves the supplied transform's linear part and XY translation.
Grounding always moves transformed **max Z**, not min Z, to zero. No mesh
vertex payload is copied by UI camera changes.

The required camera record is:

```json
{
  "width": 640, "height": 360,
  "ti": [[0.02,0,0],[0,0.02,0],[0,0,1]],
  "center": [0,0,-100],
  "pitch": [100,64],
  "source_frame": "synthetic-pitch-frame-0007",
  "center_semantics": "synthetic",
  "ground_status": "synthetic dimensions, not a measured venue"
}
```

Those numbers are synthetic, not a venue template. Real local records must be
normalized from the selected package's metadata index and **relative operational
paths**. Historical absolute provenance paths are not runtime dependencies.
Preserve the explicit source frame identity, native extent and original Ti
coefficients without normalization/transposition. Do not select frames by row
order. Preserve the package's dimension status; never fill missing dimensions
with defaults. Use `center_semantics: "conditional_fixed_ti"` for a conditional
fixed-Ti projective C estimate. This is not a measured camera pose, K/R or
distortion calibration. Missing metadata, invalid matrices, singular transforms
and invalid ground intersections are errors.

The original Ti/C remains immutable in the native registration's initial state
and in the browser's encapsulated startup metadata. Editable intrinsics/pose are
derived only by `chr-renderer::Camera::from_ti` / `camera_to_world`; reset sends
the exact original `ti_c` form and native extent. The first demo uses a virtual
PBR pitch at z=0 with simple authored markings. There is no source decoder,
occlusion, distortion correction, tracking or recording service.
The native blue-input stream has its own synthetic frame keys. It does not invent
capture timestamps or replace the selected source identity held in local metadata.

## Real runtime path and bounded cost

There is one `SceneAdapter`/`SceneControl` and one `FrameSubmissions` /
`FrameResources` owner for both transports. The installer only registers
trusted native assets and a starting scene. It reuses the existing native input
producer, `ProgramRenderer`, renderer `FrameCompositor`, BT.709 UYVY converter,
simulated SDI clock, program tap, and `chr-monitor`'s software encoder and WebRTC
stream. There is no
DataChannel renderer bypass, second scene/image registry or direct preview stream.

The named configuration admits 5 lights and 8 instances per scene; model choices
pre-register 1–8 GLBs (128 MiB source-file limit each), but selection replaces one
model instance. Four tower instances and the receiver bring the initial count
to six. Native raster and output are even and at most 4096×2160, with the same
aspect ratio.
Other admission limits remain 4 scene slots, 8 operations/mutation, 8 published
frames, 4 frames/submission, 4 inflight frames, 8 receipts and 5000 ms deadline.
These are demo capacities, not production-policy approval.

Shadows use 512² depth maps, one directional cascade, 9-tap PCF, and 16 blocker
+ 16 filter taps for PCSS, with search/filter caps of 8 texels. Four points plus
sun require at most **25 shadow passes / 25 MiB depth storage**; four spots plus
sun require **5 / 5 MiB**, within a 32 MiB shadow budget. PCF is bounded to 45
filtered comparisons per shaded fragment for five active lights; PCSS is bounded
to 160 algorithmic taps before hardware filtering, below the configured 1024
sample bound. Cubemap seam handling can multiply physical fetches; these counts
are not a universal shader instruction or bandwidth estimate. PCSS is a bounded
shadow-map approximation, not an area-light integrator; large sources can hit
radius caps, while bias/resolution can erase thin contact detail. No atmosphere
or new area-light model is introduced.

Composition intermediates are bounded to `(3 * native + output) * 8` bytes of
pixels and at least 256 MiB (native, output and a guard-banded canvas, RGBA16F;
about 283 MB at 4096×2160). Native input capacity is
`8 * native_width * native_height * 8` bytes (14.06 MiB at 640×360; 126.56 MiB
at 1920×1080); this budget includes physically draining inputs. Eight possible
shared raw UYVY owners at 640×360 represent at most 3.52 MiB, plus the fixed
eight-slot shared-memory program tap (3.52 MiB), the render worker's two
4-slot rings (input `4 * native_width * native_height * 8` bytes; output 1.76 MiB
at 640×360), its GPU staging and four compressed frames in `chr-monitor`.
Imported mesh/material/texture
allocations depend on the selected assets and are **not included** in those
frame/shadow bounds.

CPU edits still rebuild native scene/resource IDs. Camera/light edits reuse
compositor pipelines; changing the default shadow mode rebuilds the compositor.
Geometry/image payloads are shared, but GPU reuploads and shadow-cache rebuilds
occur on edits. Cold-edit latency differs from warm-frame latency.
Software readback/encoding remains a development path. SDI repeats or drops
according to the existing bounded queues; requested FPS is a clock rate, not
a throughput promise.

## Automated acceptance

```sh
nix develop --command cargo test --locked -p chr-runtime --test pitch_demo
# Run through nixGL on a non-NixOS NVIDIA host:
cargo test --locked -p chr-runtime --test pitch_demo gpu_pitch_program -- --ignored --nocapture
# Explicit local input; no private data is committed or uploaded:
CHR_PITCH_FIXTURE=output/pitch-demo/private-demo.json \
  cargo test --locked -p chr-runtime --test pitch_demo gpu_private_pitch_program -- --ignored --nocapture

nix develop --command npm --prefix client/example/render-harness run typecheck
nix develop --command npm --prefix client/example/render-harness test
nix develop --command npm --prefix client/example/render-harness run build
```

Unit tests cover grounding,
corner/target geometry, exclusive rigs, metadata rejection and immutable reset.
The headless hardware test executes the real program-to-SDI path and compares
independent rig/sun and shadow contributions. Its reported worker duration is
**CPU preparation + GPU queue/work + conversion + readback wall time**, not
encoder time or end-to-end latency.

### Optional native GPU timestamps

The synthetic pitch test reuses the `shadow_benchmark` timestamp utility. It
requests `TIMESTAMP_QUERY | TIMESTAMP_QUERY_INSIDE_ENCODERS` only when both adapter
features exist and prints adapter support, device enablement and nanoseconds/tick.
Without them it retains the ordinary device/path and reports **wall time only**,
never a fabricated GPU duration. Daemon devices still request no timestamp
features and allocate no queries by default. No wire field, dependency or service
is added.

`ProgramRenderer::set_timestamp_queries` accepts optional native query slots 0/1.
The start is immediately before `FrameCompositor::encode`; the end is immediately
after `UyvyConverter::encode`, in the **same command encoder**. This measures GPU
elapsed time for shadow depth passes, PBR rasterization, image/tone/transfer
composition and RGB-to-UYVY conversion. It excludes CPU preparation/driver shader
compilation, prior uploads/queue wait, the pixel staging copy, CPU readback,
timestamp resolve/readback, SDI waiting, encoding and network/decode. GPU
contention/stalls within the bracket remain part of elapsed GPU time.
The separate worker-wall measurement includes preparation, queue execution and
pixel readback, but excludes the diagnostic query resolve/read. Two queries and
two 16-byte buffers are reused; the test verifies identical pixels with queries
disabled and enabled. The ordinary no-query device is also exercised by the
original program and owner-local tests.

One synthetic RTX 3090/Vulkan run on a shared Linux host (1 ns timestamp period,
debug/unoptimized test build), native/output 640×360, 512² maps, one cascade,
9-tap PCF, 16+16-tap capped PCSS, one raster sample, ACES/BT.709:

| Rig + sun | Mode | GPU cold/edit ms | GPU warm median ms | Worker cold/edit ms | Worker warm median ms |
|---|---|---:|---:|---:|---:|
| Four points | Hard | 1.752 | 1.583 | 140.029 | 6.910 |
| Four points | PCF | 3.346 | 3.149 | 140.591 | 11.057 |
| Four points | PCSS | 4.196 | 3.850 | 143.981 | 10.724 |
| Four spots | Hard | 10.634 | 10.582 | 136.939 | 16.038 |
| Four spots | PCF | 11.082 | 11.002 | 143.850 | 18.457 |
| Four spots | PCSS | 21.045 | 21.001 | 151.618 | 28.213 |

Each row has one lighting/default-shadow-mode edit (native resources and depth
maps rebuilt) followed by five unchanged frames (depth maps reusable). This is
a bounded observation, not an isolated throughput benchmark or an FPS/latency
SLA; do not generalize the relative rig costs to other assets/settings. The large
cold worker/GPU difference includes CPU pipeline/resource rebuilding. Fresh
final-SHA samples, warm maxima and camera-edit timing are in the local verification
record. All published timing rows are synthetic; private fixture evidence stays
pass/fail only.

Keep exact tested SHA, command results, adapter, measured timing aggregates and
private-fixture pass/fail in ignored `output/pitch-demo/verification.txt`. No
Windows/native-window UI, physical SDI card or hardware encoder acceptance is
implied by these automated Linux checks.
