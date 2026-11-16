# Synthetic runtime program output

`chr-server --synthetic-program` explicitly installs **synthetic-program-v1**.
Without that flag, the rotating triangle, signaling and status behavior is unchanged.
This is the approved trusted-local synthetic integration, not production
registration/authentication, capture, tracking-loss, hot-configuration or latency policy.

The separate [owner-local pitch demo](pitch-demo.md) uses this same program
chain with explicit local camera/model registration and a named five-light
configuration; it does not change `synthetic-program-v1` limits.

From the repository root (`cargo run` builds only `chr-server`; the package
build also produces its render worker, `chr-renderer-cli`):

```sh
nix develop --command cargo build --locked -p chr-runtime
nix develop --command cargo run --locked -p chr-runtime -- \
  --synthetic-program --size 640x360 --fps 50
```

On a non-NixOS NVIDIA host:

```sh
nix develop --command bash -c \
  'NIXPKGS_ALLOW_UNFREE=1 nix run --impure github:nix-community/nixGL#nixGLNvidia -- cargo run --locked -p chr-runtime -- --synthetic-program --size 640x360 --fps 50'
```

The startup line declares `runtime_id`, `scene_id` and `configuration_id`.
Pass the first two to the example harness's `npm run scene -- --runtime … --scene …`
to edit the camera, sun and shadows, and watch the result in its read-only program
monitor page, whose WebRTC stream comes from a `chr-monitor` started next to the
server (see the [server README](../README.md) or use
`server/scripts/run-live.sh --synthetic-program …`). `render.get_configuration` takes the printed
runtime ID. NativeSocket clients use exactly the same JSON-RPC methods and IDs.
No discovery RPC, DataChannel commands or arbitrary file/URL registration is added.
Run namespaces include process ID, nanosecond startup time and a nonwrapping
process-local counter; input/output stream and timeline IDs are scoped to that run.
Never retain handles across restarts. Configuration is immutable until restart.

## Named synthetic configuration

The flag selects explicit integration settings, **not production defaults**:
even native/output dimensions at most 1280×720 (a limit of this demonstration
and `--software-video` only; the camera programs built on the same configuration
take native and output extents up to 4096×2160), integral 1–120 FPS,
contain fit, BT.709 display background/output, exposure multiplier 1, ACES,
one raster sample, 256-pixel shadow maps, 9-tap PCF, one cascade, bounded
PCSS settings, 16 MiB shadow/canvas budgets and a 1 MiB environment budget.
The complete values are returned by `render.get_configuration`.
The port rejects sRGB output and fractional-rate configurations rather than
silently relabeling or overriding them. Input descriptors retain their real
RGBA8/BGRA8/RGBA16F format, transfer and alpha convention.

There are 4 scene slots, 8 instances/scene, 4 lights/scene, 8 operations/mutation,
8 published frame slots, 4 frames/submission, 4 inflight frames, 8 retained
submissions and a 5000 ms synthetic render deadline. Input pixels have a separate
`8 * width * height * 8` byte budget. These named limits include physical draining;
they do not approve waiting/late-input policy for production.

The trusted native injector registers a constant opaque blue image, publishes it
through `SceneControl`, submits it with `scene_camera`, and revokes its publisher
hold after admission. It retains at most four generated receipts and explicitly
releases them once terminal. Its frame sequence/timeline are independent of output
slots. It does not release external clients' receipts. Camera/light edits affect
subsequent admissions, never already-bound work. A native ground mesh and elevated
red triangle are registered in process; there are no private assets or GLB prerequisites.
Legacy `triangle.*` methods still report/update their triangle state in this mode;
they do not rotate the retained program scene. Use `scene.mutate` for program edits.

## Ownership and execution

One `SceneAdapter` owns scene state; its bounded `SceneControl` queue is installed
in the same `ControlHandler` for both transports. One `FrameSubmissions` state owns
admission/resources; clones share it. A 5 ms native timer polls it independently
of GPU execution. `chr-server` holds no GPU device: its render thread calls
`next_ready` before handing the input pixels to the render worker, so the
physical `WorkGuard` already exists, and sends the bound scene snapshot and frame
input to `chr-renderer-cli worker` over a GStreamer unixfd socket. The pixels
travel as memfd-backed memory: a captured frame is already in that memory (the
tracking worker received the same), other inputs are copied into it once. The worker installs the
same program with the same runtime ID and rebinds them through
`SceneAdapter::bind` and `SceneAdapter::frame_camera`; its renderer-owned
`FrameCompositor` receives the immutable bound scene, native camera and exact
`NativeSettings`. No compositor, shader, camera or colour algorithm is copied
into the runtime.

Composition writes separate BT.709 RGBA8 storage. The renderer's existing UYVY
converter now accepts descriptor-validated BT.709 textures; it applies the existing
Rec.709 limited-range matrix without an extra transfer. The legacy triangle
converter retains its existing sRGB approximation for compatibility.
UYVY readback in the worker waits for the submitted GPU queue, including the
source upload and composition. `chr-server` calls `Execution::rendered` only after
the worker reports `done` for that request. A deadline miss, worker failure, no
ready worker or no free credit calls `Execution::failed` instead (a worker still
rendering the input holds its own reference to the shared memory) and transmits
the frame's clean feed;
see the [server README](../README.md#render-worker). In the in-process bridge
that tests use, a failed GPU wait quarantines the execution; it never
claims a fence, releases physical capacity early or keeps allocating in a retry loop.

The synthetic path deliberately uses software readback, not a zero-copy/hardware
encoder claim. `chr-server` copies each worker output out of the `done` buffer's
shared memory into a fresh allocation, held as a separate immutable
`Arc<Vec<u8>>`, and drops the buffer, which returns the memory to the worker. The scheduler
hands them to `SdiInput` nonblockingly. SDI retains the full output owner for its
three queued frames and one current frame. The program tap copies every
transmitted slot into its fixed eight-slot shared-memory ring and retains no
allocation; the tests' in-process observer may hold two more. Scheduler-owned
outputs are bounded by its four inflight slots. Thus at most eight raw UYVY
allocations are retained across those stages (ten with the test observer, often
fewer because holds share allocations); clean-feed frames bypass the scheduler
and count against SDI's four. The worker rings are gone (#141): the render
worker's 4 credits bound the shared input images it holds and the UYVY frames it
returns (4 each), the tracking worker's 8 credits bound its frames, and a
captured frame shared by both counts once. `chr-server` keeps up to 8 released
shared memories for reuse. Add the worker process's bounded GPU staging; there
is no draining overflow pool. In the separate `chr-monitor`, encoded fan-out retains
four compressed frames, not raw output pixels.

Only the simulated SDI clock records actual transmission and first-slot
association. Repeats keep that first slot and allocation. Public `render.release`
does not free current output. Full handoff reports `queue_full`; a disconnected
port or dropped untransmitted output reports `output_failed`. A slow, absent or
killed `chr-monitor` never blocks SDI or rendering: the tap writer never waits
for readers. The tap carries the exact transmitted slot, including repeats, with
its output slot index and source frame; the monitor encodes the newest slot and
stamps encoded frames with that output slot. No second render-direct stream exists.

Scene mutation currently rebuilds native resources and changes resource IDs.
Camera/light edits reuse the compositor pipelines; the renderer evicts stale
resources on prepare, but edits still cause reuploads and shadow cache rebuilds.
A changed default shadow mode rebuilds the compositor. This narrow reuse avoids
recompiling pipelines for every camera edit; it is not incremental scene mapping
or a guaranteed real-time performance claim.

On shutdown, native admission is closed and queued CPU edits drain, even if
transport connections retain endpoints. The `chr-render` thread is joined before
SDI and its program tap are destroyed; it then ends the worker's input stream,
kills a worker that has not exited within 2 s and removes its socket directory. Logical cancellation
is not a GPU fence.

## Self-tests and limits

```sh
nix develop --command cargo test --locked -p chr-runtime -p chr-common
nix develop --command bash -c \
  'NIXPKGS_ALLOW_UNFREE=1 nix run --impure github:nix-community/nixGL#nixGLNvidia -- cargo test --locked -p chr-runtime --test program_output --test render_worker -- --ignored --nocapture --test-threads=1'
```

The ignored hardware test checks actual synthetic pixels, camera/light/shadow
differences, frozen admission revisions, physical execution completion,
first-transmitted-slot association, repeated output ownership after public
release, and that the program tap carries the transmitted pixels and slot
metadata and keeps advancing while nobody reads it. `chr-monitor` tests cover
software encoding of tap slots. The ignored `render_worker` tests check that the
worker's UYVY output is byte-identical to the in-process bridge for the same
execution and, against a `chr-server` process, that the output keeps its cadence
with clean feed when the worker is killed (`kill -9`), stopped (`SIGSTOP`, replaced
after the hang limit) or slower than the render deadline, and that CG returns
after a restart.
CPU tests cover queue-full/drop-before-transmit, cancelled physical draining,
repeated resource reuse, explicit port rejection and retained-control IPC parity.

The harness has typecheck/unit/build gates; its `npm run scene` helpers have unit
tests for revisioned camera/light/shadow edits and reset, and `verify:browser`
checks that a real Chrome decodes the program stream. Use a project-local `TMPDIR`
for its isolated browser profile when required.

No physical capture/output card, hardware encoder, production authorization,
calibration/recording/tracking service, Windows or interactive native-window
acceptance is established here. This integration alone does not close all #40
live-product acceptance.
