# Tracking worker

**Status:** `implemented` on vchao's VchaoFrame v1 contract (#148). vchao's
`pitch-analysis` runs as the tracking worker unchanged; it is verified on the
SC1464 fixture (see [Measured](#measured)). The `chr-tracking-sim-cli` replay
stand-in speaks the same contract and remains the regression reference. Each
frame's camera model follows the lens that arrives with it ([Lens](#lens), #150).

`chr-server --tracking-worker` starts one tracking worker, gives it every
captured frame, and takes the camera of each frame from it. This is the
`chr → live video → tracking → per-frame Ti → chr` flow of
[system architecture](../../docs/system-architecture.md). chr-server supervises
the tracking worker as it does the render worker. The tracking worker may hold
its own GPU device; for now both workers share one GPU.

```text
chr-server ─unixfd: frame + VchaoFrame{selection: null}───────────► tracking worker
           ◄─unixfd: same frame + VchaoFrame{selection.pose.t}────┘   (pitch-analysis, or the stand-in)
     └ joins by frame_id ↔ program sequence → render worker → per-slot selector → output
```

The tracking hop speaks vchao's raw-frame boundary unchanged: the contract of
`live-video/src/unixfd.rs` and `live-video/README.md#native-unixfd-boundary` as
merged in Hong-Xiang/vchao#196 (`f209fc9`, the `external/vchao` pin). chr mirrors its types in
`chr_tracker::protocol` (`server/tracker/src/protocol.rs`) and does not depend on vchao's
crates. The render worker keeps chr's own protocol
([live process implementation](../../docs/live-process-implementation.md)).

## Code layout

chr's side of tracking is the pure-logic crate `chr-tracker`
(`server/tracker/`), the renderer's sibling (#151). It has no processes,
sockets, GStreamer or async runtime, and does not depend on the GPU renderer:

| Module | Content |
| --- | --- |
| `protocol` | The VchaoFrame v1 mirror, chr's [lens extensions](#chr-lens-extensions), and the meta's [validation](#validation) as functions of its JSON |
| `pose` | A returned pose as its frame's camera ([Mapping to cameras](#mapping-to-cameras)) and that camera as render geometry |
| `lens` | The camera-model choice ([Lens](#lens)) and the session's fixed lens |
| `join` | Frames paired with cameras by sequence until each frame's deadline ([program delay](#latency-and-program-delay)), and the [clean-feed](#clean-feed) decision |
| `replay` | The stand-in's poses and lens from a `calibrated-video-v1` manifest |

Geometry is checked by the renderer's own constructors (`Ti::new`,
`RadialDistortion::new`, `FrameWarp::new`): `chr-runtime` passes them to
`chr-tracker` as `chr_tracker::Checks` (`server/runtime/src/calibration/lens.rs`),
so a camera chr-tracker accepts is one the render worker can draw.
`chr-runtime` keeps the processes and their transport: the supervisor
(`server/runtime/src/worker/tracking/supervisor.rs`), the VchaoFrame caps,
buffers and meta on GStreamer (`server/runtime/src/worker/tracking/wire.rs`),
the unixfd transport shared with the render worker
(`server/runtime/src/worker/transport.rs`), chr-server's wiring and the
`chr-tracking-sim-cli` process.

## Executables

| Executable | Role |
| --- | --- |
| `chr-tracking-sim-cli` (`chr-runtime`, `server/runtime/src/tracking_sim_cli.rs`, over `chr_tracker::replay`) | Stand-in worker and the regression reference. It returns every frame with `selection.pose = fresh`: `t` is the **recorded** `Ti` of a `calibrated-video-v1` manifest, and its left endpoint is the frame itself. It replays; it never estimates. That `t` maps to the ideal image, so every frame also carries the manifest's lens in chr's [lens extension](#chr-lens-extensions); it sends no frame warp ([Lens](#lens)). |
| vchao's `pitch-analysis` | The real tracking worker (`atlas-vq-estimate`, N=8/S=4 grid, LogH interpolation, 500 ms overload rule), built from `external/vchao` ([Running vchao's pitch-analysis](#running-vchaos-pitch-analysis)). |

chr-server runs the worker as:

```text
<tracking-cli> [tracking-worker-args...] --stream-id <id> --input-socket <A> --output-socket <B>
```

- `--tracking-cli <path>` names the executable; the default is
  `chr-tracking-sim-cli` next to chr-server.
- `--tracking-worker-arg <arg>` (repeatable) adds its own arguments. In each one,
  `{stream_id}` is replaced by the session's stream ID, so per-session files
  such as `pitch-analysis --ledger` get new paths after a restart.
- `--tracking-threads <n>` sets `OMP_NUM_THREADS=<n>` for the worker alone,
  capping the OpenMP/PyTorch CPU threads of a tracker such as `pitch-track-live`.
  Unset, the worker inherits chr-server's environment and starts about 100
  threads. Measured on the 1080p50 sim50 clip (RTX 6000 Ada), 4 threads cut the
  tracker from about 740% to 380% CPU at the same `Ti` rate (32.4 → 33.1 per
  second), leaving the CPU to chr-sdi-in, chr-server and the renderer. Do not
  pin the tracker to the performance cores (`taskset -c 0-15`): that lowered the
  rate to 26.9 per second.
- There is no `worker` subcommand, `hello`, `ready` or `fatal` message:
  startup configuration is on the command line, and failures are the exit status
  and the log.

For `pitch-analysis` the worker arguments are, for example,
`--asset-manifest <map>/manifest.json --batch-frames 8 --estimate-every 4
--interpolation logh --overload-ms 500 --ledger <runs>/{stream_id}.arrow`.
`--expected-frames` is optional there and is left out for a live stream.

The stand-in takes `--manifest <json>` instead. Its fault injection options are:
- `--delay-ms <n>` holds every frame n ms before returning it. Frames keep
  flowing, like a tracker's lookahead.
- `--unavailable-every <n>` returns every n-th frame (by `frame_id`) as `unavailable`.
- `--exit-after <n>` exits with status 3 on the n-th frame.
- `--hang-after <n>` stops returning frames from the n-th on.

## Transport and startup

The transport is GStreamer's `unixfd` elements (gst-plugins-bad), the same as
the render worker (`server/runtime/src/worker/transport.rs`). chr-server needs
GStreamer 1.28 or later. A worker's own `unixfdsink` needs 1.26 or later for
`wait-for-connection`. There is one Unix socket per direction, in chr-server's
private 0700 directory `$XDG_RUNTIME_DIR/chr/tracking-<server pid>/`. It is
removed on shutdown.

```text
chr-server  appsrc ! unixfdsink socket-path=input.sock   ──►  unixfdsrc ! appsink   worker
chr-server  appsink ! unixfdsrc socket-path=output.sock  ◄──  unixfdsink ! appsrc   worker
```

- The sending side listens (`unixfdsink`) and the receiving side connects
  (`unixfdsrc`).
  - chr-server listens on `input.sock` (A) before it starts the worker.
  - The worker loads its model and map, then listens on `output.sock` (B). It
    waits for its one consumer, then connects to A.
  - chr-server connects to B once it exists.
- Neither side's sink uses `wait-for-connection`, as in vchao's merged code:
  `unixfdsink` waits for a connection inside `render` holding the base sink's
  preroll lock, so tearing it down after its peer was lost can deadlock. Each
  side pushes only while its peer is connected (`num-clients`).
- **Readiness** is both connections: the worker becomes `ready` when chr-server
  reads its output socket and the worker reads its input socket. Before that,
  frames are not sent. A worker that leaves its input socket is replaced.
- **One consumer:** `pitch-analysis` fails when its consumer disconnects or a
  second one connects. chr-server is the only consumer, and restarts the worker
  when it fails.
- Received memory is read-only. Dropping a buffer releases it to the sender.
  When a process dies, its sockets close and the peer releases everything it
  held for it.
- The end of the worker's input stream (EOS) asks the worker to exit after
  returning every frame it accepted. stdin is not used; stdout joins stderr,
  the worker's log.

## Frames

Every buffer on both sockets is one video frame:
- its single memory is the packed RGBA8 frame (BT.709 code values,
  `width` × `height` × 4 bytes) in memfd-backed memory, passed as a file
  descriptor without a copy;
- `offset` = `frame_id`, `offset_end` = `frame_id + 1`;
- PTS = `source_ns`, the duration is the frame period, and there is no DTS.

Nothing copies a captured frame: `chr-sdi-in` writes it into a memfd, and
chr-server's GStreamer core hands that same memory (or, for an interlaced input,
the deinterlaced first field's) to the tracking worker and the render worker.
The worker returns it unchanged; it must only read it.

### Caps

The caps are vchao's `wire_caps` for a source-stage stream:

```text
video/x-raw, format=RGBA, width=<w>, height=<h>, framerate=<num>/<den>,
  pixel-aspect-ratio=1/1, interlace-mode=progressive,
  vchao-stream-id=<id>, vchao-clock-domain=same-host-absolute-gst-system-clock-v1,
  vchao-layout=packed-rgba-v1, vchao-stage=source,
  vchao-replay-frames=(guint64)1, vchao-replay-duration-ns=(guint64)<frame period>
```

chr's capture is a live stream, not a looped clip. vchao's caps nevertheless
require a positive replay window, so chr sends a one-frame window.
`pitch-analysis` only passes it on. The caps never change within a stream: the
framerate is the exact fraction (`30000/1001` for 29.97), and a new extent or
rate is a new stream, a new worker session (the GStreamer core's format
changes, [gst-core](gst-core.md#a-change-of-format-at-runtime)).

### `VchaoFrame` meta

Each buffer carries exactly one `GstCustomMeta` named `VchaoFrame` with one
string field `json`. Both sides register the meta before receiving, or
`unixfdsrc` rejects the buffer. The JSON object is version 1:

| Field | chr-server sends | Worker returns |
| --- | --- | --- |
| `version` | `1` | `1` |
| `stream_id` | The session's stream ID, also in the caps | The same |
| `frame_id` | 0, 1, 2, … within the session | The same, in the same order |
| `source_ns` | Capture time on the same-host `GstSystemClock` (`CLOCK_MONOTONIC`); strictly increasing | The same |
| `clock_domain` | `same-host-absolute-gst-system-clock-v1` | The same |
| `duration_ns` | `1e9 × den / num` (one frame), rounded down; equals the buffer duration | The same |
| `selection` | `null` | `{selected_ns, overdue, pose}` |

- `selected_ns` is when the choice became final.
- `overdue` means the frame was released by the overload rule.
- `pose` is tagged by `status`:

| `status` | Fields | chr's camera |
| --- | --- | --- |
| `fresh` | `t`, `left` (the frame's own estimate; `t` bit-equal to `left.t`) | `ti = t` |
| `interpolated` | `t`, `left`, `right`, `alpha` | `ti = t` |
| `reused` | `t`, `left` (`t` bit-equal to `left.t`), nullable `interpolation_failure` | `ti = t` |
| `unavailable` | none; never an identity matrix | none: the frame is skipped |

Endpoints are `{frame_id, sequence, source_ns, observed_ns, t}` and keep their
own estimate's keys. Unknown fields are errors (`deny_unknown_fields`), except
chr's two optional lens extensions below.

`t` is the row-major float64 forward map from ground XY metres to **this
frame's real (distorted) SNC**, on homogeneous column vectors
([coordinates](coordinates.md)). JSON uses round-trip float formatting
(serde_json `float_roundtrip`, on both sides). chr copies the nine values into
the camera's `ti` in the same order, so `Ti` crosses the hop and reaches the
render worker bit for bit.

### chr lens extensions

chr's mirror accepts two optional fields that vchao's contract does not have and
`pitch-analysis` never sends. chr-server never sends them either. A worker whose
`t` maps to the **ideal** (undistorted) image sends them; the stand-in sends
`lens` only.

| Field | Where | Value |
| --- | --- | --- |
| `lens` | top level of the returned meta | `null` (or absent), or `{model: "radial-poly-native-px-v1", centre_px: [x, y], scale_px, k: [k1, k2, k3], support?: [[min_x, min_y], [max_x, max_y]]}`, native pixels ([renderer design §3.3](renderer-design.md)) |
| `frame_warp` | `fresh`, `interpolated` and `reused` poses | `null` (or absent), or nine numbers: row-major, reference native pixel to this frame's SNC, applied after the lens's radial term |

- `lens` belongs to the worker **session**. The first returned frame fixes it,
  `null` included; a later frame of the same stream with another `lens` is a
  worker failure, and the worker is restarted as a new session.
- `frame_warp` needs the session's `lens`.
- Both are validated at the boundary; an invalid one is a worker failure:
  finite values, `scale_px > 0`, a nonempty support (the renderer's
  `RadialDistortion::new`, which also computes the radius at which the
  polynomial stops increasing: points beyond it are never drawn), and an
  invertible `frame_warp` (`FrameWarp::new`).
- Any other unknown field is still an error.

## Mapping to cameras

chr-server joins each returned frame to its capture sequence by `frame_id`. It
keeps the mapping explicit: the frames in flight are a queue of
`(frame_id, capture sequence)` in send order. A camera is:

```text
{ sequence: <capture sequence>, ti: t, camera_center: <station C>,
  lens: <the session's lens or none>, frame_warp: <the pose's frame_warp or null> }
```

It goes into the same join as `tracking.frame` cameras: frames and cameras are
paired by capture sequence only, never with another frame's camera.

### Station C

- The worker returns only `t`. The optical centre of every camera is the static
  station centre `C`, in world metres with negative Z up.
- `--tracking-centre <x> <y> <z>` sets it. Without it, chr-server uses the
  reference camera's `center` of the `--tracked-scene` manifest or the
  description's `camera.reference`. If neither exists, chr-server refuses to start.
- `pitch-render --centre` takes the same value. For the SC1464 fixture it is
  `14.52857216404143 60.620266710663365 -9.91514362632655`.

### Lens

The lens belongs to whoever produces `Ti` (#150). Each tracked frame is rendered
with the first of:

1. **Tracking data:** the lens the frame's tracking data carries: the tracking
   worker's session `lens` with the pose's `frame_warp`, or `tracking.frame`'s
   `lens` and `frame_warp`.
2. **CLI:** chr-server's `--tracking-lens-k <k1> <k2> <k3> --tracking-lens-centre
   <x> <y> --tracking-lens-scale <px>` (all three or none) with an optional
   `--tracking-lens-support <min_x> <min_y> <max_x> <max_y>`; `stack.toml`'s
   `tracking_lens_k`, `tracking_lens_centre`, `tracking_lens_scale` and
   `tracking_lens_support`. An invalid CLI lens stops chr-server at startup.
   A CLI lens has no frame warp.
3. **Neither:** the pinhole camera `S^-1 Ti A(C)`.

- When the tracking data's lens and the CLI lens differ, the tracking data's
  wins and chr-server logs a warning (once per differing lens).
- vchao's `pitch-analysis` estimates `t` against the real, distorted image and
  sends no lens, so its frames are rendered pinhole, unless a CLI lens is given.
- The stand-in sends its manifest's lens and **no** frame warp: its frames are
  rendered `radial`. It never sees capture sequences, only the session's
  `frame_id`, so it cannot tell which of the clip's per-frame warps belongs to a
  frame, and a misaligned warp jitters more than none. The replay therefore
  omits the calibration package's per-frame warps; on the panda clip they move
  the image by about 1–3 px. A worker that does send `frame_warp` is rendered
  `radial+warp`, pixel for pixel as the calibrated replay renders the same frame
  (GPU test `gpu_tracked_frames_match_the_calibrated_replay`), and
  `tracking.frame` from `chr-sdi-in --send-cameras` sends each frame's warp by
  `frame_id`.
- A lens in the description or manifest is **never** applied to tracked frames
  implicitly; only the stand-in, `chr-sdi-in --send-cameras` and still renders
  of a reference camera replay it, as tracking data.
- **Debug switch:** `--tracking-lens auto|pinhole` (default `auto`). `pinhole`
  forces the ideal camera and ignores every lens source, frame warps included.
- **How the lens reaches the renderer:** per frame. The chosen lens travels in
  the frame's `ti_c` geometry (`lens` next to `frame_warp`) through
  `render.submit` to the render worker's `render` request, and the renderer
  builds that frame's `CameraModel` from it (`LensDistortion` is per frame
  already). The render worker holds no lens between frames, so a lens that first
  arrives with a session's first frame, or changes with a new session, needs no
  worker restart, and no frame is ever rendered with another session's lens.
- chr-server logs the lens choice at startup and each worker session's lens when
  its first frame returns. `tracking.get_status` reports the model of the last
  frame rendered with its camera as `camera_model: {model, source}`: `model` is
  `pinhole`, `radial` or `radial+warp`, `source` is `worker` (the tracking
  data), `cli`, `none` or `forced-pinhole`. `stack.py status` shows it.

## Validation

chr-server validates every returned frame at the boundary. Any violation is a
worker failure: the worker is killed and restarted. It is not a frame without
a camera. The checks are:
- the meta is present, version 1, in the clock domain, and has no unknown fields;
- the buffer `offset` and duration match `frame_id` and `duration_ns`;
- the `stream_id` (meta and caps) is the session's;
- `frame_id` is the oldest frame in flight: frames come back in order, exactly
  once, and only frames that were sent;
- the frame size is unchanged;
- `selection` is set;
- every matrix is finite, and `fresh`/`reused` `t` is bit-equal to `left.t`;
- the [lens extensions](#chr-lens-extensions) are valid and the session's
  `lens` does not change.

A singular `t` is rejected by the join like any invalid camera; the frame goes
without a camera.

## Flow control and restarts

- **No gaps:** `pitch-analysis` requires `frame_id` 0, 1, 2, … without gaps,
  strictly increasing `source_ns`, and fixed caps. It fails after an input
  stall of 2 s. While a worker is ready, chr-server therefore sends **every**
  captured frame. A frame is never skipped on the way to the worker.
- **In-flight limit:** the worker may hold at most
  `ceil((D + 250 ms) × fps)` frames (at least 8), where `D` is the program
  delay `--program-delay-ms`. Examples: 38 at 1000 ms and 30 fps, 88 at 1500 ms and
  50 fps. `pitch-analysis` holds at most `ceil(500 ms / period) + 10` frames,
  plus its 8-frame input queue: 25 + 8 at 30 fps.
  - A worker that would exceed the limit is replaced, and the frame counts as
    `not_sent`. Frames it holds that long would come back too late to be
    rendered anyway.
  - chr-server's send queue is bounded by the same limit, so memory stays
    bounded and the capture never blocks.
- **Restarts:** a restarted worker is a new session. It gets a new stream ID,
  `chr-<server pid>-<session>`, and its `frame_id`s start at 0 again, as
  `pitch-analysis` requires (a stream change within a session is fatal there).
  Frames captured while no worker is ready are not sent: they belong to no
  stream, get no camera and go out as clean feed.
- **Capture restart:** after a new capture session (a restarted `chr-sdi-in`),
  frames in flight are forgotten. They still come back in order and keep their
  `frame_id`s, but their answers are ignored. A new input format starts a new
  worker session ([GStreamer core](gst-core.md#a-change-of-format-at-runtime)).

## Supervision

```text
Starting ──output socket connected──► Ready ──exit / invalid frame / hang / full──► Backoff ──timer──► Starting
```

- **Startup:** a worker that does not listen on its output socket within 120 s
  (`pitch-analysis`' own startup limit) is replaced.
- **Exit:** a worker that exits is replaced. Once connected, the end of its
  output stream reports the exit after its last frames.
- **Hang:** a worker that holds a frame longer than `--worker-hang-ms` (default
  2000 ms) is killed (SIGKILL) and restarted. The backoff grows from 250 ms to
  5 s. Its frames are released with its sockets.
- **While no worker is ready** (starting, loading its model, restarting):
  frames are not sent, so they get no camera and go out as clean feed. The
  output keeps its cadence and its delay.

## Clean feed

A frame without its camera is transmitted as its **clean feed**: its own
pixels, converted to UYVY and scaled bilinearly to the output size if the
camera's differs, without CG. It is never rendered with another frame's camera.
This covers frames sent while no worker is ready, frames returned `unavailable`
(as in vchao, "unavailable geometry passes the original pixels through
unchanged"), and frames whose camera or render is not back when their slot is
decided. Each goes out in its own slot, the program delay after its capture,
as every frame does ([GStreamer core](gst-core.md#timing)). `tracking.get_status`
counts the frames without a camera as `frames_clean_feed` and
`frames_without_tracking`; `sdi.get_status.core` counts every slot, CG or clean
feed, with the CG ratio, the per-slot slack and the render budget.

## Latency and program delay

Every tracked program runs on chr-server's [GStreamer core](gst-core.md) (#154):
a frame captured at `t` leaves at `t + D`, the program delay
(`--program-delay-ms`, `program_delay_ms` in `stack.toml`), with its CG if its
camera and render are back 4 ms before, else as its own clean feed; its
neighbours keep their CG. Frames go to the render worker in capture order as
soon as their camera is in, and wait for one of its four credits until their
slot is decided: `pitch-analysis` returns frames in batches (a frame waits for
the next scheduled estimate to resolve), more at once than the render worker
takes. A frame whose camera has not arrived by then is clean feed, and its late
camera is rejected.

- `--program-delay-ms` defaults to three frame periods and may be up to
  1500 ms. It replaced `--tracking-wait-ms` (#154 phase 5), the wait of the
  former timer-paced path, which released joined frames on its own output
  ticks; that flag now fails with a pointer to its replacement.
  - The stand-in answers at once, so the default suffices.
  - `pitch-analysis` releases a frame when its selection is final (LogH
    lookahead to the next target, about 4 frames plus one estimate), or after
    at most 500 ms with a reused or unavailable pose (`overdue`). Give it about
    800 ms.
- The program delay is the whole capture-to-output delay: about 0.8 s with
  `pitch-analysis`. On the SC1464 fixture it selected each frame's pose 198 ms
  (median), 305 ms (95th percentile) and at most 376 ms after capture, so 600 ms
  would also suffice there ([Measured](#measured)); `sdi.get_status.core`'s
  per-slot slack shows the margin left.
- **Memory:** every frame waiting for its slot holds its memfd (RGBA8: 8 MiB at
  1080p, 32 MiB at 4K) and its clean feed (UYVY at the output size): about
  D × fps frames, some 0.4 GiB at 800 ms and 1080p30.
- The worker's two sockets are bins of the core's pipeline, relinked for every
  new session.
- Without a tracking worker (`tracking.frame`), frames wait for their camera the
  same way, until their slot is decided.

## Image coordinates

vchao's SNC is chr's: `x = (2u - W) / sqrt(W·H)`, `y = (2v - H) / sqrt(W·H)`
(vchao `README.md#images-snc`, `atlas-vq-estimate` `geometry.py`). vchao
leaves the pixel-centre convention to each producer; `atlas-vq-estimate`
maps detector pixel coordinates without a half-pixel shift, which is chr's
convention of integer sample centres ([coordinates](coordinates.md)).

## Status

`tracking.get_worker_status` returns:
- the state (`starting`, `ready`, `backoff`), the worker's file name (`worker`),
  the current `stream_id`, the PID and the restarts;
- counters: frames sent (`requests`), frames returned with a camera (`cameras`)
  and `unavailable`, and frames `not_sent`;
- the last error.

`tracking.get_status` reports the join (and `sdi.get_status.core` the slots:
the program delay, CG / clean / empty slots, the CG ratio, the per-slot slack
and the render budget, see [GStreamer core](gst-core.md#telemetry)):
- `poses_fresh`, `poses_interpolated`, `poses_reused`, `poses_unavailable`:
  every returned frame, by pose;
- `poses_overdue`: frames released by the worker's overload rule, whatever
  their pose;
- `frames_clean_feed`: frames sent as clean feed for want of a camera;
- `camera_model`: `{model, source}` of the last frame rendered with its camera,
  `null` before the first ([Lens](#lens)).

These fields default to 0 (`null`) for older clients. With a tracking worker,
`tracking.frame` is rejected.

## Running it

```sh
server/scripts/run-tracked.sh --scene clip-manifest.json --video clip.mp4 --tracking-worker
```

- `--tracking-worker` runs the stand-in with the manifest
  (`--tracking-worker-arg --manifest ...`), which sends the manifest's lens. It starts
  `chr-sdi-in` without `--send-cameras`, so the input is the camera only.
- `--tracking-cli <path>` runs another worker instead. Pass its arguments as
  `--tracking-worker-arg <arg>`, and set `--tracking-centre` and
  `--program-delay-ms`.
- `stack.py` does this from `stack.toml`: `tracking = "worker"`, `tracking_cli`,
  `tracking_worker_args`, `tracking_threads`, `tracking_centre` and `program_delay_ms`, and the
  `tracking_lens*` keys (`server/scripts/stack.example.toml`).
- `run-tracked.sh` passes the `--tracking-lens*` options to chr-server.
- `tracking = "external"` runs no tracking worker: `chr-sdi-in --send-cameras`
  sends the manifest's recorded cameras as `tracking.frame`
  ([SDI simulation](sdi-simulation.md)).

## Running vchao's pitch-analysis

Build it from the pinned submodule (the closure is about 7 GB, mostly
PyTorch/CUDA wheels; vchao's flake has no binary cache, so a first build fetches
and builds it). chr's own builds and checks never need the submodule:

```sh
git submodule update --init external/vchao
nix build ./external/vchao#pitch-analysis --out-link output/vchao/pitch-analysis
```

- `pitch-analysis` runs vchao's `atlas-vq-estimate` (Python, CUDA). Its
  wrapper looks for the NVIDIA driver only in `/run/opengl-driver/lib` (NixOS)
  and replaces `LD_LIBRARY_PATH`. On other hosts run it through
  `server/scripts/with-cuda-driver.sh`, which preloads `libcuda.so.1` from the
  inherited `LD_LIBRARY_PATH`, under nixGL as `run-tracked.sh` and `stack.py`
  run chr-server.
- Arguments: the map's `--asset-manifest`, `--batch-frames 8
  --estimate-every 4 --interpolation logh --overload-ms 500` and a new
  `--ledger <dir>/{stream_id}.arrow` per session. chr-server adds
  `--stream-id` and both sockets.
- Give the station centre with `--tracking-centre` and the program delay with
  `--program-delay-ms`. It sends no lens: its frames are rendered
  pinhole unless a `--tracking-lens-*` lens is given.
- `pitch-analysis` fails on an input stall of 2 s or an estimator without
  progress for 5 s; chr-server then restarts it as a new stream, with clean feed
  meanwhile. On `stack.py stop` it receives the end of its input, returns every
  frame it accepted and writes its `analysis-complete` summary to the log.

`docs/operations.md` has the `stack.toml` keys for it.

vchao's live N16ROT tracker `pitch-track-live` (`nix build
./external/vchao#pitch-track-live`) is a drop-in replacement with the same
contract, arguments and CUDA wrapper: it estimates every frame as it comes
(`--batch-frames 1 --estimate-every 1`), releases a frame it holds after
`--overload-ms` with its last causal pose and needs an N16ROT map. It ran as
the worker of the DeckLink run (18.7 fresh `Ti`/s at 1080p30, capture to
estimate p50 37 ms, p95 82 ms, max 226 ms;
[GStreamer core](gst-core.md#on-a-card-decklink-duo-2-1080p30)).

### Measured

SC1464 fixture (1212x654p30 clip; private, not in the repository), RTX 3090
shared with a running stack, 150 s, on the former timer-paced path (looped by
`chr-sdi-sim` into the retired capture ring, `--tracking-wait-ms 800`):

| Quantity | Value |
| --- | --- |
| Startup to `ready` (model and map load) | 9.5 s; its 278 frames went out as clean feed |
| Frames sent / returned | 4,137, all returned in order (`analysis-complete` at shutdown) |
| Poses | 1,015 fresh, 3,045 interpolated, 64 reused, 13 unavailable; the 13 unavailable are frames 0–12, overdue before the first estimate |
| Estimates | 1,016 launched, 1,016 accepted: about 7.2 fresh `Ti` per second (the N=8/S=4 grid at 30 fps targets 7.5) |
| Capture to selected pose | 198 ms median, 305 ms 95th percentile, 376 ms maximum; chr-server to worker 1.2 ms median |
| Frames held by the worker | at most 16 (`peak_video_queue`; its capacity 26, chr-server's limit 32) |
| Restarts | none; the clip loop (frame 359 to 0) did not disturb the stream |
| GPU memory | `atlas-vq-estimate` 1.8 GiB; `pitch-analysis` itself none |

With the GPU to itself (no other stack), the full program for 77 s: 2,306 SDI
slots, none repeated or dropped; 2,058 frames rendered with CG from vchao's `Ti`
(2 of them clean feed after a render deadline miss), 252 clean-feed frames while
the worker started, 2 frames dropped by the join at startup. The CG test quads
sit on the pitch markings in the SRT preview. GPU memory: render worker 463 MiB,
`atlas-vq-estimate` 1.8 GiB. CPU: `atlas-vq-estimate` 112 %, `chr-monitor`
(OpenH264) 83 %, the simulator's `ffmpeg` 29 %, `chr-sdi-sim` 15 %, `chr-server`
and the render worker 7 % each, `pitch-analysis` 4 %.

On the GStreamer core (#154 phase 5; `stack.py` with `chr-sdi-in` looping the
clip, D 800 ms, RTX 3090 shared with the live stack and its own
`pitch-analysis`): after the 11 s model load (its frames clean feed) every slot
had CG (300 of 300 per 10 s), 0 repeats, the output interval 33.333 ms mean
(30.30–36.46 ms, sd 0.29), capture to output 800.1/800.2/803.2 ms, per-slot
slack of the CG slots 315–617 ms (mean 482 ms), render budget at submit 323–638
ms; `description.load` accepted. Before joined frames waited for a render
credit, a batch of 8 cameras against 4 credits cost about 4 of every 8 slots
their CG (CG 62 % per 10 s). CPU: `chr-server` 16 %, `chr-sdi-in` 12 % plus
FFmpeg's decoding.

#146 does not hit a CUDA process next to a Vulkan one: `chr-renderer-cli bench`
of this description (1212x654, 300 frames) takes 1.80 ms wall per frame
(95th percentile 2.08 ms, GPU 0.56 ms) alone and 2.32 ms (95th percentile
3.39 ms, GPU 0.51 ms) next to a running `pitch-analysis`. A second Vulkan
process is what slows uploads by orders of magnitude.
