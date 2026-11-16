# Tracking worker

**Status:** `implemented` on vchao's VchaoFrame v1 contract (#148). vchao's
`pitch-analysis` runs as the tracking worker unchanged; it is verified on the
SC1464 fixture (see [Measured](#measured)). The `chr-tracking-sim-cli` replay
stand-in speaks the same contract and remains the regression reference.

`chr-server --tracking-worker` starts one tracking worker, gives it every
captured frame, and takes the camera of each frame from it. This is the
`chr → live video → tracking → per-frame Ti → chr` flow of
[system architecture](../../docs/system-architecture.md). chr-server supervises
the tracking worker as it does the render worker. The tracking worker may hold
its own GPU device; for now both workers share one GPU.

```text
chr-server ─unixfd: frame + VchaoFrame{selection: null}───────────► tracking worker
           ◄─unixfd: same frame + VchaoFrame{selection.pose.t}────┘   (pitch-analysis, or the stand-in)
     └ joins by frame_id ↔ capture sequence → render worker → SDI
```

The tracking hop speaks vchao's raw-frame boundary unchanged: the contract of
`live-video/src/unixfd.rs` and `live-video/README.md#native-unixfd-boundary` as
merged in Hong-Xiang/vchao#196 (`f209fc9`, the `external/vchao` pin). chr mirrors its types in
`server/runtime/src/worker/tracking/protocol.rs` and does not depend on vchao's
crates. The render worker keeps chr's own protocol
([live process implementation](../../docs/live-process-implementation.md)).

## Executables

| Executable | Role |
| --- | --- |
| `chr-tracking-sim-cli` (`chr-runtime`, `server/runtime/src/tracking_sim_cli.rs`) | Stand-in worker and the regression reference. It returns every frame with `selection.pose = fresh`: `t` is the **recorded** `Ti` of a `calibrated-video-v1` manifest, and its left endpoint is the frame itself. It replays; it never estimates. The manifest's frame warps are dropped: VchaoFrame has none. |
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

chr-server copies each captured frame once, from the capture input ring into
shared memory. It hands that same memory to the tracking worker and the render
worker. The worker returns it unchanged; it must only read it.

### Caps

The caps are vchao's `wire_caps` for a source-stage stream:

```text
video/x-raw, format=RGBA, width=<w>, height=<h>, framerate=<fps>/1,
  pixel-aspect-ratio=1/1, interlace-mode=progressive,
  vchao-stream-id=<id>, vchao-clock-domain=same-host-absolute-gst-system-clock-v1,
  vchao-layout=packed-rgba-v1, vchao-stage=source,
  vchao-replay-frames=(guint64)1, vchao-replay-duration-ns=(guint64)<frame period>
```

chr's capture is a live stream, not a looped clip. vchao's caps nevertheless
require a positive replay window, so chr sends a one-frame window.
`pitch-analysis` only passes it on. The caps never change within a stream.

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
| `duration_ns` | `1e9 / fps`, rounded down; equals the buffer duration | The same |
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
own estimate's keys. Unknown fields are errors (`deny_unknown_fields`).

`t` is the row-major float64 forward map from ground XY metres to **this
frame's real (distorted) SNC**, on homogeneous column vectors
([coordinates](coordinates.md)). JSON uses round-trip float formatting
(serde_json `float_roundtrip`, on both sides). chr copies the nine values into
the camera's `ti` in the same order, so `Ti` crosses the hop and reaches the
render worker bit for bit.

## Mapping to cameras

chr-server joins each returned frame to its capture sequence by `frame_id`. It
keeps the mapping explicit: the frames in flight are a queue of
`(frame_id, capture sequence)` in send order. A camera is:

```text
{ sequence: <capture sequence>, ti: t, camera_center: <station C>, frame_warp: null }
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

- vchao's `t` maps to the real, distorted image. A program tracked by a
  VchaoFrame worker therefore registers **no** radial lens: the render worker
  draws with a pinhole camera model. The `k1` of the manifest or description,
  for example the frozen calibration chain's, must not be applied on top.
- This is the default, `--tracking-lens identity`.
- `--tracking-lens program` applies the manifest's or description's lens
  instead. Use it only for cameras whose `Ti` maps to the ideal (undistorted)
  image, such as the stand-in's recorded `Ti`. `run-tracked.sh` and `stack.py`
  pass it for the stand-in only.
- chr-server logs the station centre and the lens of the tracking worker at startup.

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
- every matrix is finite, and `fresh`/`reused` `t` is bit-equal to `left.t`.

A singular `t` is rejected by the join like any invalid camera; the frame goes
without a camera.

## Flow control and restarts

- **No gaps:** `pitch-analysis` requires `frame_id` 0, 1, 2, … without gaps,
  strictly increasing `source_ns`, and fixed caps. It fails after an input
  stall of 2 s. While a worker is ready, chr-server therefore sends **every**
  captured frame. A frame is never skipped on the way to the worker.
- **In-flight limit:** the worker may hold at most
  `ceil((wait + 250 ms) × fps)` frames (at least 8), where `wait` is
  `--tracking-wait-ms`. Examples: 38 at 1000 ms and 30 fps, 88 at 1500 ms and
  50 fps. `pitch-analysis` holds at most `ceil(500 ms / period) + 10` frames,
  plus its 8-frame input queue: 25 + 8 at 30 fps.
  - A worker that would exceed the limit is replaced, and the frame counts as
    `not_sent`. Frames it holds that long would come back too late to be
    rendered anyway.
  - chr-server's send queue is bounded by the same limit, so memory stays
    bounded and the capture loop never blocks.
- **Restarts:** a restarted worker is a new session. It gets a new stream ID,
  `chr-<server pid>-<session>`, and its `frame_id`s start at 0 again, as
  `pitch-analysis` requires (a stream change within a session is fatal there).
  Frames captured while no worker is ready are not sent: they belong to no
  stream, get no camera and go out as clean feed.
- **Capture restart:** after the capture restarts its sequence numbers, frames
  in flight are forgotten. They still come back in order and keep their
  `frame_id`s, but their answers are ignored.

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
  frames are not sent, so they get no camera and go out as clean feed. The SDI
  output keeps its cadence and its delay.

## Clean feed

A frame without its camera is transmitted as its **clean feed**: its own
pixels, converted to UYVY and scaled bilinearly to the output size if the
camera's differs, without CG. It is never rendered with another frame's camera.
This covers frames sent while no worker is ready, frames returned `unavailable`
(as in vchao, "unavailable geometry passes the original pixels through
unchanged"), and frames whose camera is later than the wait. The join releases
them at the same fixed delay as joined frames, and the render loop transmits
them in their place among the rendered frames. `tracking.get_status` counts
them as `frames_clean_feed`; `frames_without_tracking` counts the frames whose
camera did not arrive. Frames a render deadline miss turns into clean feed are
counted by the render worker (`render.get_worker_status`).

## Latency and program delay

- With a tracking worker, the join releases every joined frame exactly
  `--tracking-wait-ms` after its capture. This is a constant program delay,
  quantized to the output period. `pitch-analysis` returns frames in bursts:
  a frame waits for the next scheduled estimate to resolve. Releasing each frame
  as soon as it is joined would drop most of a burst. A frame whose camera has
  not arrived by then is skipped, and its late camera is rejected.
- `--tracking-wait-ms` defaults to three frame periods and may be up to
  1500 ms.
  - The stand-in answers at once, so the default suffices.
  - `pitch-analysis` releases a frame when its selection is final (LogH
    lookahead to the next target, about 4 frames plus one estimate), or after
    at most 500 ms with a reused or unavailable pose (`overdue`). Give it about
    800 ms.
- The SDI program delay is then the wait plus rendering: about 0.8 s. On the
  SC1464 fixture `pitch-analysis` selected each frame's pose 198 ms (median),
  305 ms (95th percentile) and at most 376 ms after capture, so 600 ms would
  also suffice there ([Measured](#measured)).
- **Memory:** every waiting frame holds its RGBA8 pixels: 8 MiB at 1080p,
  32 MiB at 4K. The capture backlog holds `wait × fps + 2` frames, about
  0.6 GiB at 1500 ms and 1080p50.
- Without a tracking worker (`tracking.frame`), frames still leave as soon as
  their camera arrives.

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

`tracking.get_status` reports the join:
- `poses_fresh`, `poses_interpolated`, `poses_reused`, `poses_unavailable`:
  every returned frame, by pose;
- `poses_overdue`: frames released by the worker's overload rule, whatever
  their pose;
- `frames_clean_feed`: frames sent as clean feed for want of a camera.

These fields default to 0 for older clients. With a tracking worker,
`tracking.frame` is rejected.

## Running it

```sh
server/scripts/run-tracked.sh --scene clip-manifest.json --video clip.mp4 --tracking-worker
```

- `--tracking-worker` runs the stand-in with the manifest
  (`--tracking-worker-arg --manifest ... --tracking-lens program`). It starts
  `chr-sdi-sim` without `--send-cameras`, so the simulator is the camera only.
- `--tracking-cli <path>` runs another worker instead. Pass its arguments as
  `--tracking-worker-arg <arg>`, and set `--tracking-centre` and
  `--tracking-wait-ms`.
- `stack.py` does this from `stack.toml`: `tracking = "worker"`, `tracking_cli`,
  `tracking_worker_args`, `tracking_centre` and `tracking_wait_ms`
  (`server/scripts/stack.example.toml`).
- `tracking = "external"` keeps the previous behaviour, in which the simulator
  sends the cameras.

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
- Give the station centre with `--tracking-centre` and the wait (program
  delay) with `--tracking-wait-ms`; the lens is `identity` by default.
- `pitch-analysis` fails on an input stall of 2 s or an estimator without
  progress for 5 s; chr-server then restarts it as a new stream, with clean feed
  meanwhile. On `stack.py stop` it receives the end of its input, returns every
  frame it accepted and writes its `analysis-complete` summary to the log.

`docs/operations.md` has the `stack.toml` keys for it.

### Measured

SC1464 fixture (1212x654p30 clip, looped by `chr-sdi-sim`; private,
not in the repository), RTX 3090 shared with a running stack, 150 s,
`--tracking-wait-ms 800`:

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
