# SDI capture and tracked camera simulation

**Status:** `implemented` opt-in simulation on Linux. It simulates the #60 tracking
path with a replayed calibrated clip; it is not a production tracking service,
capture card or calibration contract. The default daemon and the initial
`scene_frame_v1` pinhole profile are unchanged.

> **Renamed in #142:** the simulator was called `chr-tracking-sim`, and camera
> sending was its default (`--no-tracking` turned it off). It is now
> `chr-sdi-sim`, which feeds frames only unless given `--send-cameras`. This
> document was `tracking-simulation.md`.

> **GStreamer core (#154):** the experimental `chr-server --pipeline gst` takes
> its frames from `chr-sdi-in` over a unixfd socket instead of this ring: any
> progressive clip at an integer rate (or a test pattern) at its own pace, no
> manifest ([GStreamer core](gst-core.md)).

`chr-sdi-sim` stands in for the SDI capture card: it decodes a calibrated clip
in a loop and writes every frame into `chr-server`'s capture input ring. A real
capture process (#80) will take its place. By default it feeds frames only, and
the server's [tracking worker](tracking-worker.md) supplies the cameras over
vchao's VchaoFrame contract; `stack.py` runs that mode by default. This document
also describes the external mode (`chr-sdi-sim --send-cameras`), in which the
simulator sends each frame's recorded camera itself, and the pairing both modes
share.

In the external mode `chr-sdi-sim` stands in for a camera with a tracking
system. It sends every frame of a calibrated clip into `chr-server`, and it
sends each frame's camera separately. `chr-server --tracked-scene` pairs both by capture sequence, renders
the CG with that camera, and transmits the program on its simulated SDI port.
`chr-monitor` streams that output over SRT or WebRTC, as for every other program.

```text
chr-sdi-sim ──frames (chr-shm input ring, "SDI in")───────────────┐
   (clip + manifest)                                               ▼
  --send-cameras ──tracking.frame {sequence, Ti, C, lens, frame_warp}──► chr-server ──worker──► SDI out
                   (JSON-RPC over the NativeSocket)                │                      │
                                                                   │             program tap
                                                                   ▼                      ▼
                                                   join by capture sequence        chr-monitor ──► SRT / WebRTC
```

## Startup and per-frame parameters

| When | Parameter | Source |
| --- | --- | --- |
| Startup | Native extent and rate, scene (GLB, its height, anchor and pitch size), lighting | `calibrated-video-v1` manifest given to `--tracked-scene` |
| Every frame | Image | One slot of the input ring, `index` = capture sequence |
| Every frame | `Ti`, `C`, lens, `frame_warp` | `tracking.frame` with the same capture sequence; the simulator sends the manifest's `radial` lens with every camera |

The manifest's own `ti` and `center` are used only to place the model (the
anchor pixel's ground ray). Its `radial` lens and `frame_warps` are not applied
by the server; the simulator replays them as tracking data
([tracking worker](tracking-worker.md#lens): the tracking data's lens, else
chr-server's `--tracking-lens-*` lens, else pinhole). The scene camera is `{"mode": "tracked"}`, so a frame
without a tracked camera cannot be rendered.

Frame `i` is projected as in [renderer design](renderer-design.md#implemented-radial-poly-native-px-v1),
with `Ti_i`, `C_i` and `W_i` from that frame's sample:

```text
a  = S^-1 Ti_i A(C_i) [X, Y, Z]     ideal native pixel
r  = radial(a)                      the frame's lens
n  = pi(W_i [r, 1])                 frame SNC; without frame_warp, W_i = S
```

Replaying a calibrated clip sends the same `Ti`/`C` for every frame and the clip's
warp of `sequence % warps`. A tracked frame then renders exactly the pixels of
the calibrated replay's frame (`gpu_tracked_frames_match_the_calibrated_replay`).

## Channels

### Input ring

`chr_shm::input`: an 8-slot ring created by the capture process, default
`$XDG_RUNTIME_DIR/chr/input.ring`. Each slot holds one RGBA8 frame with BT.709
code values at the native extent; `index` is the capture sequence and `meta`
carries width, height and rate, which the server checks. The server reads every
slot in write order and counts frames the ring overwrote before it read them,
including a slot reused by a later write that completed before the copy. A
new ring file (a restarted capture process) restarts capture sequences; the
server forgets pending frames and cameras and keeps its own input sequence
increasing.

The server copies each frame once, straight from its slot into memfd-backed
shared memory; the backlog holds that memory, and the tracking worker and the
render worker receive it without another copy (the worker channels are GStreamer
unixfd sockets, see [tracking worker](tracking-worker.md#transport-and-startup)). The input
ring itself stays a `chr-shm` ring: the capture process is not a supervised
worker and never waits for the server.

A physical SDI input would deliver UYVY; converting it is part of #80 and not
simulated here.

### `tracking.frame`

Answered only by a server running a tracked program
(`chr_common::methods::tracking`):

```json
{"sequence": "42",
 "ti": [9 numbers, row-major, ground XY -> ideal SNC],
 "camera_center": [x, y, z],
 "frame_warp": [9 numbers, row-major, reference native pixel -> frame SNC] | null,
 "lens": {"model": "radial-poly-native-px-v1", "centre_px": [x, y], "scale_px": s,
          "k": [k1, k2, k3], "support": [[min_x, min_y], [max_x, max_y]]}}
```

`sequence` is decimal text, as all `U64` values on the wire. `lens` is optional
(`support` within it too); `ti` maps to the ideal image with a lens and to the
frame itself without one. The result is `null`. A singular `Ti` or warp, an
invalid lens (nonfinite, `scale_px <= 0`, an empty support) or a `frame_warp`
without a `lens` is rejected with `incompatible_input`. A camera
for a frame that was already rendered or skipped is rejected with
`invalid_state`, a duplicate with `conflict`, and a full backlog (64 cameras
ahead of their frames) with `capacity`. `tracking.get_status` returns the
counters below.

The server turns a joined sample into the frame's `ti_c` geometry for
`render.submit`, with the lens it chose ([tracking worker](tracking-worker.md#lens)).
That geometry carries an optional `lens` and `frame_warp`, the only wire
additions of this extension. Both are omitted when absent, so pinhole messages
are unchanged; admission rejects an invalid `lens` with `incompatible_input` and
a `frame_warp` without a `lens` with `unsupported_profile`.

## Pairing and late input

Images and cameras arrive independently and are paired by capture sequence
only, never by arrival order. Once per output period the server:

1. reads new frames from the input ring into a backlog that outlasts the wait
   (one frame per output period of the wait plus two, at least four);
2. resolves frames oldest first: a frame with its camera is joined; a frame
   whose wait (`--tracking-wait-ms`, default 3 frame periods, at most 1500 ms)
   ended without one is skipped. A frame never overtakes an older unresolved
   frame;
3. outputs the oldest resolved frame that is due: a joined frame is rendered,
   a frame without camera goes out as clean feed. In this external mode a frame
   is due once joined; with a tracking worker, exactly the wait after its
   capture, a constant program delay
   ([tracking worker](tracking-worker.md#latency-and-program-delay)). Up to 2
   due frames wait for later periods, which absorbs capture bursts; older ones
   are dropped, so latency returns to the tracking delay after a stall.

A frame without its camera is never rendered with another frame's camera, and
its late camera is rejected. It goes out as clean feed in its own output period:
its own pixels without CG, scaled to the output size when the camera's differs
(#148). Before #148 the output port repeated its previous frame instead.

| `tracking.get_status` | Meaning |
| --- | --- |
| `samples_received` | Cameras accepted into the backlog |
| `samples_late` | Cameras rejected because their frame was already rendered or skipped |
| `frames_joined` | Frames submitted for rendering with their camera |
| `frames_without_tracking` | Frames whose camera did not arrive in time, including frames a tracking worker returned `unavailable` or never received |
| `frames_clean_feed` | Frames sent as clean feed because they had no camera |
| `frames_dropped` | Joined frames given up to keep the latency bounded |
| `frames_overwritten` | Frames the input ring overwrote before the server read them |
| `poses_fresh`, `poses_interpolated`, `poses_reused`, `poses_unavailable` | Frames a tracking worker returned, by pose; 0 in external mode |
| `poses_overdue` | Frames a tracking worker released by its overload rule |
| `camera_model` | `{model, source}` of the last frame rendered with its camera: `pinhole`, `radial` or `radial+warp`, from `worker` (the tracking data), `cli`, `none` or `forced-pinhole` |

## Run

Inside `nix develop .#media` from the repository root (wrap with nixGL on a
non-NixOS NVIDIA host, see the [server README](../README.md)):

```sh
server/scripts/run-tracked.sh --scene scene.json --video clip.mp4 --srt 127.0.0.1:9001
```

It builds the release binaries and starts the three processes on a private
socket, input ring and program tap. They run independently: a process that
exits is restarted after a short backoff while the others keep running (the
server replaces a stale socket, the simulator reconnects, and the server and
monitor reopen a replaced ring or tap); Ctrl-C stops all. A restarted
`chr-server` starts again from its startup scene, so a scene loaded at runtime
must be sent again. `--dir <path>` uses a fixed directory for the socket, ring
and tap, kept on exit, and `--no-monitor` skips `chr-monitor` so it can run on
its own with `--tap <dir>/program.tap` and the same `--size`/`--fps`. The output
rate is the manifest's `fps`. `--size` sets the output (default 1280x720; even,
up to 4096x2160, with the camera's aspect, for example the camera's own
1920x1080),
`--sim-arg`/`--monitor-arg` pass options through, and every other option goes to
`chr-server`. `--description <json>` draws a
[render description](render-description.md) instead of the manifest's scene; the
simulator still replays the manifest's cameras. View the stream with an SRT caller, for example VLC on
`srt://<host>:9001`; the host must allow that UDP port.

`chr-sdi-sim` options: `--send-cameras` sends the recorded cameras
(`run-tracked.sh` passes it without `--tracking-worker`); with it,
`--tracking-delay-ms <n>` sends every camera that long after its frame, and
`--drop-tracking-every <n>` withholds every n-th camera to exercise frames
without one. It requires the clip's decoded frame count to
equal the manifest's `frame_warps`, like the calibrated replay.

`chr-renderer-cli render --tracked-scene <json> --frame <n>` renders frame `n`
with the manifest's `Ti`/`C`, lens and warp `n` through the same tracked path.

## Acceptance

```sh
cargo test --locked -p chr-runtime --features chr-runtime/gstreamer-media
cargo test --locked -p chr-runtime --features chr-runtime/gstreamer-media \
  --test program_output gpu_tracked -- --ignored --nocapture
```

- Unit tests: pairing out of arrival order, skipped frames and late cameras,
  burst spreading and bounded backlog, ring order, gaps, overwritten slots and a
  replaced ring, options of both executables.
- `tracked_frames_carry_their_own_camera_and_warp`: per-frame geometry reaches
  the admitted execution; a tracked scene rejects a scene camera.
- `frame_warps_need_the_frames_own_valid_lens`: a warp without the frame's lens,
  or an invalid lens, is rejected at admission.
- `gpu_tracked_frames_match_the_calibrated_replay` (hardware GPU): tracked
  frames carrying the replay's camera produce the replay's UYVY output byte for
  byte, and different warps move the CG.

Recorded on a Linux RTX 3090 host (Vulkan, nixGL, release build) with an
owner-local 1920x1080p30 clip of 342 frames, a 1.35M-triangle GLB and 1280x720p30
output: over 30 s the unimpaired run joined 902 of 902 output slots, with 0
repeats, 0 port drops and 0 deadline misses. The SRT stream, received with
GStreamer `srtsrc`, showed the composited CG. With 50 ms tracking delay and every
10th camera withheld, about 17% of the output slots repeated: each missing camera
stalls the output for up to the wait, after which the backlog is dropped. These
are observations on a shared host, not latency or throughput guarantees.

## Limits

- The lens travels per frame but a tracking worker's is fixed per session;
  zoom-dependent radial coefficients are not modelled, and nothing in the
  calibration records requires them yet.
- No timecode, genlock or clock recovery: the server's output clock and the
  simulator's decode clock run independently at the same nominal rate.
- No clean feed for frames without tracking, and no camera prediction.
- The TypeScript render harness does not mirror `lens`, `frame_warp` or the
  `tracking.frame` method; it mirrors `tracking.get_status`.
