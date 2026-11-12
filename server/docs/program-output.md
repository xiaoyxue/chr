# Synthetic runtime program output

`chr-server --synthetic-program` explicitly installs **synthetic-program-v1**.
Without that flag, the rotating triangle, signaling and status behavior is unchanged.
This is the approved trusted-local synthetic integration, not production
registration/authentication, capture, tracking-loss, hot-configuration or latency policy.

From the repository root:

```sh
nix develop --command cargo run --locked -p chr-server -- \
  --synthetic-program --size 640x360 --fps 50
```

On a non-NixOS NVIDIA host:

```sh
nix develop --command bash -c \
  'NIXPKGS_ALLOW_UNFREE=1 nix run --impure github:nix-community/nixGL#nixGLNvidia -- cargo run --locked -p chr-server -- --synthetic-program --size 640x360 --fps 50'
```

The startup line declares `runtime_id`, `scene_id` and `configuration_id`.
Copy the first two into the example harness's **Synthetic program scene** panel,
connect its existing WebSocket and open its existing WebRTC stream. Alternatively
pass `?runtime=…&scene=…` to the page. `render.get_configuration` takes the printed
runtime ID. NativeSocket clients use exactly the same JSON-RPC methods and IDs.
No discovery RPC, DataChannel commands or arbitrary file/URL registration is added.
Run namespaces include process ID, nanosecond startup time and a nonwrapping
process-local counter; input/output stream and timeline IDs are scoped to that run.
Never retain handles across restarts. Configuration is immutable until restart.

## Named synthetic configuration

The flag selects explicit integration settings, **not production defaults**:
even native/output dimensions at most 1280×720, integral 1–120 FPS,
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
of GPU execution. The render thread calls `next_ready` before uploading pixels,
so the physical `WorkGuard` already exists. Renderer-owned `FrameCompositor`
receives the immutable bound scene, native camera and exact `NativeSettings`.
No compositor, shader, camera or colour algorithm is copied into the runtime.

Composition writes separate BT.709 RGBA8 storage. The renderer's existing UYVY
converter now accepts descriptor-validated BT.709 textures; it applies the existing
Rec.709 limited-range matrix without an extra transfer. The legacy triangle
converter retains its existing sRGB approximation for compatibility.
UYVY readback waits for the submitted GPU queue, including the source upload and
composition. Only successful physical completion calls `Execution::rendered`.
A failed GPU wait quarantines the execution and stops that render worker; it never
claims a fence, releases physical capacity early or keeps allocating in a retry loop.

The synthetic path deliberately uses software readback, not a zero-copy/hardware
encoder claim. Output bytes are a separate immutable `Arc<Vec<u8>>`. The scheduler
hands them to `SdiInput` nonblockingly. SDI retains the full output owner for its
three queued frames and one current frame; its two-slot monitor tap and one active
encoder may retain those same allocations. Scheduler-owned outputs are bounded
by its four inflight slots. Thus at most eleven raw UYVY allocations are retained
across those stages (often fewer because holds share allocations), plus bounded
GPU worker staging; there is no draining overflow pool. Encoded fan-out retains
four compressed frames and only a native provenance sidecar, not raw output pixels.

Only the simulated SDI clock records actual transmission and first-slot
association. Repeats keep that first slot and allocation. Public `render.release`
does not free current output. Full handoff reports `queue_full`; a disconnected
port or dropped untransmitted output reports `output_failed`. A slow/absent
monitor never blocks SDI or rendering. The monitor encodes the exact transmitted
slot, including repeats, and propagates its native provenance/slot to encoded
frames. No second render-direct stream exists.

Scene mutation currently rebuilds native resources and changes resource IDs.
The program compositor is rebuilt at bound scene-effect changes, so edits can
cause reuploads and shadow cache rebuilds. This is not an incremental optimization
or a guaranteed real-time performance claim.

On shutdown, native admission is closed and queued CPU edits drain, even if
transport connections retain endpoints. The render worker is joined before SDI
and monitor owners are destroyed; logical cancellation is not a GPU fence.

## Self-tests and limits

```sh
nix develop --command cargo test --locked -p chr-server -p chr-common
nix develop --command bash -c \
  'NIXPKGS_ALLOW_UNFREE=1 nix run --impure github:nix-community/nixGL#nixGLNvidia -- cargo test --locked -p chr-server --test program_output -- --ignored --nocapture'
```

The ignored hardware test checks actual synthetic pixels, camera/light/shadow
differences, frozen admission revisions, physical execution completion,
first-transmitted-slot association, repeated output ownership after public
release, bounded slow monitoring and software encoding of that transmitted slot.
CPU tests cover queue-full/drop-before-transmit, cancelled physical draining,
repeated resource reuse, explicit port rejection and retained-control IPC parity.

The harness has typecheck/unit/build gates and an optional real Chrome self-test:
run its existing `verify:browser` with a URL containing
`program=1&runtime=…&scene=…`. It clicks the actual camera/light/shadow controls,
checks revisions/reset, and detects a pixel change in the **decoded SDI video**.
Use a project-local `TMPDIR` for its isolated browser profile when required.

No physical capture/output card, hardware encoder, production authorization,
calibration/recording/tracking service, Windows or interactive native-window
acceptance is established here. This integration alone does not close all #40
live-product acceptance.
