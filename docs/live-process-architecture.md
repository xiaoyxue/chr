# Live process architecture

**Status:** `confirmed` — owner decision recorded in #94 (2026-11-13).
`implemented` on Linux: `chr-monitor` serves WebRTC/SRT from the program tap
(#95), and `chr-server` renders every frame through a supervised
`chr-renderer-cli worker` with clean feed on failure (#96, #97). Since #141 both
workers talk to `chr-server` over GStreamer unixfd sockets instead of
stdin/stdout lines and `chr-shm` rings. Since #154 every tracked program runs on
one GStreamer pipeline in `chr-server` (the GStreamer core), its frames coming
from `chr-sdi-in` as memfds; the program tap is the only `chr-shm` ring left.
Windows is not verified; physical SDI (#80) is not implemented (the DeckLink code
paths of #154 are not verified on hardware).
Implementation steps are in
[Live process implementation plan](live-process-implementation.md).

**Scope:** which processes make up the live render path, what each one owns,
how they communicate, and how program output survives renderer and monitor
failures. Physical SDI hardware and recording are referenced, not defined. The
tracking worker's channel is defined in
[tracking worker](../server/docs/tracking-worker.md).

**Depends on:** [System architecture](system-architecture.md),
[Frame ingestion and rendering](render-frame-input.md),
[Execution model](execution-model.md).

## Confirmed decisions

- Production rendering runs **out of process**. `chr-server` owns video input,
  the output clock, scheduling, JSON-RPC control, the authoritative scene state
  and health reporting, and supervises a restartable render worker.
- A renderer crash, driver fault or hang must not interrupt program output.
  In-process recovery (recreating a lost wgpu device, restarting a panicked
  thread) cannot survive aborts, segfaults, OOM kills or hangs, which is why the
  process boundary is required.
- The headless renderer entry point is `chr-renderer-cli`. Its `worker`
  subcommand is the process `chr-server` spawns; its `render` subcommand renders
  to an image for people and CI. `chr-app` remains the native window shell.
- `chr-renderer` stays a library without networking or device I/O.
- Video monitoring leaves `chr-server`. An optional `chr-monitor` process
  monitors **only the transmitted program output** (SDI out), through a
  read-only tap or, on hardware, a physical loop-out. It never watches the
  render worker directly.
- Health (drops, repeats, deadline misses, clean-feed frames, worker restarts)
  is reported by `chr-server`, not inferred by the monitor.

## Processes

```text
                       JSON-RPC (WebSocket / NativeSocket)
WebUI / CLI / tests ────────────────────────────────► chr-server
chr-sdi-in ──unixfd: memfd frames + audio──────────► │ GStreamer core: capture, per-slot
 (file / test pattern / DeckLink)                     │ deadline (capture + D), clean feed
                                                      │ authoritative scene state, health
                                                      │ output on the pipeline clock
  unixfd sockets: JSON in GstCustomMeta + memfd       │
        ┌───────────────────────┬─────────────────────┤
        ▼                       ▼                     │ chr-shm tap ring (read-only for readers)
chr-renderer-cli worker   tracking worker             ▼
  chr-renderer: upload →    frame → Ti, C, warp     chr-monitor (optional)
  composite → readback      (chr-tracking-sim-cli     H.264 → WebRTC / SRT
  killable, restartable,     replays until the real   own signalling WebSocket
  stateless                  module replaces it)
```

| Process | Owns | Must not |
| --- | --- | --- |
| `chr-server` | Input adapters, frame identity, admission, deadlines, clean feed, output clock/port, JSON-RPC control, scene state, worker supervision, health | Create a GPU device; wait on the monitor |
| `chr-renderer-cli worker` | GPU device, renderer resources, composition of one frame per request | Hold authoritative state; touch input/output devices; decide timing |
| Tracking worker (`chr-tracking-sim-cli` until the real module) | The tracking reference, per-frame estimation (the stand-in replays), its own GPU device if it needs one (one GPU is shared for now) | Hold authoritative state; touch input/output devices; decide timing or reuse another frame's camera |
| `chr-monitor` | Reading the tap, encoding, WebRTC/SRT fan-out, its own signalling endpoint | Feed anything back into program output |

Tracking runs out of process as a worker that `chr-server` supervises like the
render worker (#128). `chr-server` hands it every captured frame, the same
shared memory the render worker receives, and joins its cameras with the frames
by capture sequence; see
[tracking worker](../server/docs/tracking-worker.md). The protocol is
language-neutral, so the real tracking module can replace the replay stand-in.
#60 keeps the questions this does not settle: the tracking reference lifecycle
(#59), `FrameKey` versus capture sequence, and production missing/late policy.
An external tracker can still send cameras as `tracking.frame` when no tracking
worker runs ([tracked camera simulation](../server/docs/sdi-simulation.md)).

## Repository layout

| Path | Package | Contents |
| --- | --- | --- |
| `server/renderer/` | `chr-renderer` | GPU rendering library (unchanged) |
| `server/runtime/` | `chr-runtime` | Shared runtime library plus the `chr-server` executable, its workers `chr-renderer-cli` and `chr-tracking-sim-cli`, and `chr-sdi-in`, the capture input of the GStreamer core (#154) |
| `server/viewer/` | `chr-app` | Native window and headless shell (unchanged) |
| `common/` | `chr-common` | JSON-RPC messages and IPC transports (unchanged role) |
| `common/shm/` | `chr-shm` | The shared-memory program tap; the only crate allowed to use `unsafe` |
| `monitor/` | `chr-monitor` | Separate monitoring process |

`chr-renderer-cli` and `chr-tracking-sim-cli` are executables of the runtime
package rather than their own packages because the worker reuses the runtime's scene mapping, registrations and
program bridge, and because integration tests of the runtime need both
executables (`CARGO_BIN_EXE_*`). The package is renamed from `chr-server` to
`chr-runtime` so that its name describes the library rather than one binary.

`monitor/` sits at the repository root, not under `server/`: `server/` is the
render server and what runs in its process, while the monitor is a separate,
optional process that may run on another host from an SDI loop-out.
`chr-shm` is the protocol shared by `chr-server` and the monitor, so it lives
under `common/`.

## Channels

| Channel | Between | Transport | Carries |
| --- | --- | --- | --- |
| Control plane | Clients ↔ `chr-server` | Existing JSON-RPC over WebSocket/NativeSocket | Commands and status, never pixels |
| Worker messages | `chr-server` ↔ each worker | Two GStreamer unixfd sockets per worker (`unixfdsink` listens, `unixfdsrc` connects); each buffer is one message, JSON in a `GstCustomMeta`: `chr-worker-message` for the render worker, vchao's `VchaoFrame` for the tracking worker (#148) | Startup spec, render requests and completions; frames out and back with their selected `Ti` |
| Worker pixels | `chr-server` ↔ each worker | The same buffers' memfd-backed memory, passed as file descriptors | Captured frames (the capture's memory, shared by both workers) and composited UYVY frames |
| Capture input | `chr-sdi-in` → `chr-server` | Two GStreamer unixfd sockets (`unixfdsink` in `chr-sdi-in` listens, the core's `unixfdsrc` connects): frames as memfds with VchaoFrame, timecode and captions metas, and audio | Captured RGBA8 frames and audio (a file, a test pattern or a DeckLink card) |
| Program tap | `chr-server` → monitor | One `chr-shm` ring | Every transmitted output slot, repeats included |
| Monitor signalling | Clients ↔ `chr-monitor` | JSON-RPC `webrtc.offer` on the monitor's own WebSocket | WebRTC session setup |

The worker sockets live in `chr-server`'s private 0700 runtime directory and
are passed to the worker on its command line. File-descriptor passing lets one
captured frame reach both workers without further copies; at 1080p a UYVY
frame is about 4 MB, copied once by the worker into shared memory and once by
`chr-server` out of it, on top of the CPU upload and readback. `chr-server`
bounds what each worker holds in flight (render 4 requests; tracking
`ceil((--program-delay-ms + 250 ms) × fps)` frames), so a slow or stopped worker
holds a bounded number of buffers. GPU-handle
sharing is not required and not claimed. The transport is specified in
[tracking worker](../server/docs/tracking-worker.md#transport-and-startup); any language
with GStreamer bindings can implement a worker (vchao's `pitch-analysis` does, #148).
The program tap stays on `chr-shm`: the monitor is not a supervised worker, and a
ring never makes its writer wait. The capture input never waits either: its
leaky queue drops the oldest frame when the core does not read.

## State ownership

- **Registrations** (models, environments, camera contexts) are immutable after
  startup. `chr-server` sends the worker a program specification and the run's
  runtime ID; both processes build identical registrations from it.
- **Scene state** is owned by `chr-server`. Each render request carries the bound
  scene snapshot (scene ID, revision and state) and the frame input. The worker
  derives renderer scenes from that snapshot and may cache them by
  `(scene_id, scene_revision)`.
- The worker is **stateless across restarts**: a new worker needs only the
  startup specification and the next request.

## Timing and clean feed

- A **tracked program** (`--tracked-scene`, `--description`) runs on the
  GStreamer core ([below](#gstreamer-core-pipeline-154)): the pipeline clock (the
  system clock in simulation, the output card's with a DeckLink output) times
  the output, and every frame leaves exactly the program delay D after its
  capture. The worker never decides when a frame is transmitted.
- A **generated program** (the triangle, `--synthetic-program`, `--pitch-demo`,
  `--software-video`) has no capture to follow: `chr-server` keeps an internal
  output clock (a render loop and a simulated SDI port), and each request has a
  deadline within the frame period.
- A frame without its CG in time is transmitted as its **clean feed** (the
  input image converted to the output format, without CG); its submission is
  reported as failed, not rendered. Late completions are discarded by request
  ID.
- Clean feed is available when the input is 8-bit BT.709; an input of another
  extent (with the output's aspect) is scaled to the output. Other inputs fall
  back to repeating the last output and are counted.
- A tracked frame without a camera (no ready tracking worker, an `unavailable`
  pose, or a camera later than its slot's decision) is clean feed too, in its
  own slot, at the same delay D as its neighbours (#148, #154).

## GStreamer core pipeline (#154)

Every tracked program runs on **one GStreamer pipeline** owned by `chr-server`
(phase 5 removed the timer-paced tracked path, the shared-memory capture ring,
its writer `chr-sdi-sim` and `--pipeline`). Design, status and measurements:
[GStreamer core](../server/docs/gst-core.md).

- **Input:** a separate INPUT process, `chr-sdi-in`, plays a file at its own
  rate (or a test pattern) in real time, or captures a DeckLink card
  (`decklinkvideosrc mode=auto`, not verified on hardware), and sends every
  frame over a unixfd socket as memfd memory stamped with its identity
  (VchaoFrame), timecode and captions, and its audio over a second socket.
- **Timing:** each captured frame owns one output slot, due a fixed program
  delay D (`--program-delay-ms`) after its capture on the pipeline clock. At
  the slot's deadline a selector puts in the frame's CG if the workers
  delivered it, else that frame's own clean feed; `livesync` keeps the rate and
  repeats only when no frame came. The output follows the input frame for
  frame; the measured capture-to-output delay stays at D through worker
  crashes and stalls.
- **Workers:** unchanged protocols and supervisors; each worker session's
  sockets are bins of the running pipeline, removed with the session and
  relinked for its replacement, without restarting `chr-server`. Joined frames
  wait for a render credit until their slot's deadline (a tracking worker
  answers in batches).
- **Not used:** `fallbackswitch` (gst-plugins-rs) switches streams on a
  timeout and cannot give a single late frame its own clean feed.
- **Rates and formats** (phase 2): rates are exact fractions everywhere
  (`chr_common::rate::Rate`: `50`, `30000/1001`); the core takes the frame's
  extent and rate from the capture's caps, which may change at runtime. A new
  format is a new tracking stream (a restarted tracking worker, as vchao
  requires) with clean feed until its first CG, deadlines and D stay as they
  are, and the output follows the input's rate (`--output-rate follow`) or
  converts it to a fixed one (`fixed:<rate>`); chr-monitor follows the tap's
  mode without a restart.
- **Interlaced, input loss, audio and ancillary data** (phases 3 and 4):
  interlaced frames keep their fields on the output and are deinterlaced (first
  field) for tracking and rendering only; while the input delivers nothing the
  slots are filled (`--input-loss hold|black|slate`) on the cadence, and the
  first frame after it leaves at its own capture time plus D; audio, timecode
  and captions leave with their frame's delay D. With `--output decklink` the
  card's clock, genlocked to its reference input, is the pipeline clock (not
  verified on hardware).
- **Telemetry** (phase 5, #131): `sdi.get_status.core` reports D, the slots'
  CG / clean feed / empty counts, the CG ratio, and over the last 10 s the
  per-slot slack of the CG slots, the render budget at submit and the
  capture-to-output delay; `stack.py status` shows them.
- **External tracking:** without a tracking worker, cameras come as
  `tracking.frame`, keyed by the frame's `frame_id` (`chr-sdi-in
  --send-cameras` replays a calibrated clip's).

## Failure handling

| Failure | Response |
| --- | --- |
| Worker misses a deadline | Clean feed for that frame; counted |
| Worker exits, crashes or reports a fatal error | Clean feed until a replacement is ready; respawn with backoff; counted |
| Worker stops responding (oldest request older than the hang limit) | `SIGKILL`, then respawn |
| GPU device lost inside the worker | Worker exits; handled as a crash |
| Tracking worker exits, fails or leaves a frame unanswered past the hang limit | Killed and respawned with backoff; frames meanwhile have no camera and go out as their clean feed, at D |
| Capture process exits or loses its signal | The core reconnects every 500 ms; the output keeps its cadence, filled as `--input-loss` says; recovery keeps D |
| Monitor crashes, stalls or has no viewers | No effect on program output; the tap writer never waits |
| `chr-server` itself fails | Out of scope here: hardware bypass/watchdog and a service supervisor (#80) |

A hot-standby worker, fill+key output and GPU zero-copy between processes are
possible follow-ups, not part of this decision.

## References

- #94 decision; #95 monitor split and `chr-shm`; #96 `chr-renderer-cli`;
  #97 worker supervision and clean feed; #80 physical SDI; #60 tracking results;
  #128 tracking worker; #141 unixfd worker transport.
