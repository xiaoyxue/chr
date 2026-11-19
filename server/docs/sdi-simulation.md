# SDI capture and tracked camera simulation

**Status:** `implemented` simulation on Linux. It simulates the #60 tracking
path with a replayed calibrated clip; it is not a production tracking service,
capture card or calibration contract. The initial `scene_frame_v1` pinhole
profile is unchanged.

> **History.** The simulator was `chr-tracking-sim` (camera sending by
> default), renamed `chr-sdi-sim` in #142 (frames only unless `--send-cameras`),
> and retired in #154 phase 5 together with the shared-memory capture input
> ring it wrote and the timer-paced tracked path that read it (its
> `TrackingJoin` fixed-delay release, `--input-ring`, `--tracking-wait-ms`).
> `chr-sdi-in`, the input process of chr-server's GStreamer core, is now the one
> capture process, and `--send-cameras` moved to it. This document was
> `tracking-simulation.md`.

`chr-sdi-in` stands in for the SDI capture card: it plays a calibrated clip at
its own rate in a loop (or a test pattern; or captures a DeckLink card) and
hands every frame to `chr-server`'s [GStreamer core](gst-core.md) as memfd memory
over a unixfd socket, with its audio over a second one. A real capture card
(#80) takes the clip's place in the same process (`--source decklink`). By
default it feeds frames only, and the server's [tracking worker](tracking-worker.md)
supplies the cameras over vchao's VchaoFrame contract; `stack.py` runs that mode
by default. This document also describes the external mode
(`chr-sdi-in --send-cameras <manifest>`, `chr-server` without
`--tracking-worker`), in which `chr-sdi-in` sends each frame's recorded camera
itself, and the pairing both modes share.

In the external mode `chr-sdi-in` stands in for a camera with a tracking
system. It sends every frame of a calibrated clip into `chr-server`, and it
sends each frame's camera separately. `chr-server --tracked-scene` pairs both by
the frame's `frame_id`, renders the CG with that camera, and outputs every frame
the program delay after its capture, with its CG or its own clean feed.
`chr-monitor` streams that output over SRT or WebRTC, as for every other program.

```text
chr-sdi-in ──frames + audio (unixfd, memfd; VchaoFrame frame_id, capture time)──┐
 (clip at its rate)                                                              ▼
 --send-cameras <manifest> ──tracking.frame {sequence = frame_id, Ti, C, lens,──► chr-server (GStreamer core)
                             frame_warp} (JSON-RPC over the NativeSocket)        │  join by frame_id until the
                                                                                  │  slot's deadline ──► render worker
                                                                                  ▼
                                                    output (D after capture) ──► program tap ──► chr-monitor ──► SRT / WebRTC
```

## Startup and per-frame parameters

| When | Parameter | Source |
| --- | --- | --- |
| Startup | Native extent and rate, scene (GLB, its height, anchor and pitch size), lighting | `calibrated-video-v1` manifest given to `--tracked-scene` |
| Every frame | Image, capture time, `frame_id` | `chr-sdi-in` over the capture socket (VchaoFrame `source` stamp) |
| Every frame | `Ti`, `C`, lens, `frame_warp` | `tracking.frame` with `sequence` = the frame's `frame_id`; `chr-sdi-in --send-cameras` sends the manifest's `radial` lens with every camera |

The manifest's own `ti` and `center` are used only to place the model (the
anchor pixel's ground ray). Its `radial` lens and `frame_warps` are not applied
by the server; `chr-sdi-in --send-cameras` replays them as tracking data
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

### Capture socket

`chr-sdi-in` listens on `--socket` (default `$XDG_RUNTIME_DIR/chr/capture.sock`;
`stack.py` uses `<dir>/capture.sock`) and `chr-server --capture-socket` connects
to it, reconnecting every 500 ms while it is gone. Each frame is RGBA8 with
BT.709 code values at the clip's extent in a memfd, stamped with its `frame_id`
(from 0 in each stream, continuous across loops of the file) and its capture
time; nothing copies it on the way to the workers. The audio goes over
`<socket>.audio`. The details (pacing, stale frames, streams, the core's
numbering) are in [GStreamer core](gst-core.md#timing).

A physical SDI input delivers UYVY; `chr-sdi-in --source decklink` converts it
the same way (verified at 1080p30 on a DeckLink Duo 2).

### `tracking.frame`

Answered only by a server running a tracked program without a tracking worker
(`chr_common::methods::tracking`; with `--tracking-worker` it is rejected):

```json
{"sequence": "42",
 "ti": [9 numbers, row-major, ground XY -> ideal SNC],
 "camera_center": [x, y, z],
 "frame_warp": [9 numbers, row-major, reference native pixel -> frame SNC] | null,
 "lens": {"model": "radial-poly-native-px-v1", "centre_px": [x, y], "scale_px": s,
          "k": [k1, k2, k3], "support": [[min_x, min_y], [max_x, max_y]]}}
```

`sequence` is decimal text, as all `U64` values on the wire. It is the frame's
`frame_id` in the input's current stream: chr-server adds the program sequence
of that stream's first frame, so cameras stay paired across input restarts and
format changes (each a new stream from `frame_id` 0); a camera sent before its
stream's first frame reached chr-server can be paired with the previous
stream's numbering and is then rejected or unused. `lens` is optional
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

Images and cameras arrive independently and are paired by sequence only, never
by arrival order (`chr_tracker::join::DeadlineJoin`). Every captured frame owns
an output slot due at its capture time plus the program delay D
(`--program-delay-ms`, default 3 frame periods, at most 1500 ms) and decided
4 ms before. A frame leaves the join, in capture order, as soon as its camera
is in the feed: it then waits for a render credit until its slot is decided,
and is rendered. A frame whose camera has not come by its slot's decision goes
out as its clean feed: its own pixels without CG, scaled to the output size,
in its own slot. A frame is never rendered with another frame's camera, and
its late camera is rejected. The output never skips or repeats a slot for a
missing camera, and the capture-to-output delay stays D
([GStreamer core](gst-core.md#timing)).

| `tracking.get_status` | Meaning |
| --- | --- |
| `samples_received` | Cameras accepted into the backlog |
| `samples_late` | Cameras rejected because their frame was already rendered or skipped |
| `frames_joined` | Frames submitted for rendering with their camera |
| `frames_without_tracking` | Frames whose camera did not arrive in time, including frames a tracking worker returned `unavailable` or never received |
| `frames_clean_feed` | Frames sent as clean feed because they had no camera |
| `poses_fresh`, `poses_interpolated`, `poses_reused`, `poses_unavailable` | Frames a tracking worker returned, by pose; 0 in external mode |
| `poses_overdue` | Frames a tracking worker released by its overload rule |
| `camera_model` | `{model, source}` of the last frame rendered with its camera: `pinhole`, `radial` or `radial+warp`, from `worker` (the tracking data), `cli`, `none` or `forced-pinhole` |

## Run

Inside `nix develop .#media` from the repository root (wrap with nixGL on a
non-NixOS NVIDIA host, see the [server README](../README.md)):

```sh
server/scripts/run-tracked.sh --scene scene.json --video clip.mp4 --srt 127.0.0.1:9001
```

It builds the release binaries and starts `chr-sdi-in`, `chr-server` and
`chr-monitor` on a private control socket, capture socket and program tap.
They run independently: a process that exits is restarted after a short backoff
while the others keep running (the server reconnects to a restarted capture,
`chr-sdi-in` reconnects its cameras, and the monitor reopens a replaced tap);
Ctrl-C stops all. A restarted `chr-server` starts again from its startup scene,
so a scene loaded at runtime must be sent again. `--dir <path>` uses a fixed
directory for the sockets and tap, kept on exit, and `--no-monitor` skips
`chr-monitor` so it can run on its own with `--tap <dir>/program.tap` and the
same `--size`/`--fps`. The output follows the clip's rate; the manifest's `fps`
is chr-server's nominal rate. `--size` sets the output (default 1280x720; even,
up to 4096x2160, with the camera's aspect, for example the camera's own
1920x1080), `--no-audio` drops the clip's audio, `--input-arg`/`--monitor-arg`
pass options through, and every other option goes to `chr-server`
(`--program-delay-ms`, `--input-loss`, `--output-rate`, the lens options).
`--description <json>` draws a [render description](render-description.md)
instead of the manifest's scene; the cameras still replay the manifest's. View
the stream with an SRT caller, for example VLC on `srt://<host>:9001`; the host
must allow that UDP port.

`chr-sdi-in` camera options: `--send-cameras <manifest>` sends the recorded
cameras (`run-tracked.sh` passes it without `--tracking-worker`) to
`--server-socket` (default `$XDG_RUNTIME_DIR/chr/render.sock`); with it,
`--tracking-delay-ms <n>` sends every camera that long after its frame, and
`--drop-tracking-every <n>` withholds every n-th camera to exercise frames
without one. A camera is the manifest's `Ti`/`C` and lens with the warp of the
clip frame the frame shows, `source_index % warps` like the calibrated replay,
sent with `sequence` = the frame's `frame_id`. `source_index` counts the clip's
decoded frames through its loops (the parser's timestamp in frames), so frames
the input drops (a lost signal, stale frames after a stall) leave no gap in the
`frame_id`s but keep every later camera on its own picture. `--send-cameras`
needs `--video` and refuses to start unless the clip decodes (FFprobe
`-count_frames`) to exactly one frame per `frame_warps` entry at the manifest's
`width` x `height`.

`chr-renderer-cli render --tracked-scene <json> --frame <n>` renders frame `n`
with the manifest's `Ti`/`C`, lens and warp `n` through the same tracked path.

## Acceptance

```sh
cargo test --locked -p chr-runtime --features chr-runtime/gstreamer-media
cargo test --locked -p chr-runtime --features chr-runtime/gstreamer-media \
  --test program_output gpu_tracked -- --ignored --nocapture
```

- Unit tests (`chr-tracker`): pairing out of arrival order, frames given up at
  their deadline or as soon as no camera will come, late cameras rejected,
  `tracking.frame` sequences counted from the stream's first frame; options of
  `chr-server` and `chr-sdi-in`.
- `external_cameras_join_their_frames_by_frame_id_and_feed_the_telemetry`
  (`tests/gst_core.rs`): with every tenth camera withheld, exactly those slots
  are clean feed, every other slot has CG, nothing repeats, and
  `sdi.get_status.core` counts them.
- `cameras_answered_in_batches_wait_for_render_credits`: cameras arriving eight
  at a time, more than the render worker's four credits, all get CG.
- `tracked_frames_carry_their_own_camera_and_warp`: per-frame geometry reaches
  the admitted execution; a tracked scene rejects a scene camera.
- `frame_warps_need_the_frames_own_valid_lens`: a warp without the frame's lens,
  or an invalid lens, is rejected at admission.
- `gpu_tracked_frames_match_the_calibrated_replay` (hardware GPU): tracked
  frames carrying the replay's camera produce the replay's UYVY output byte for
  byte, and different warps move the CG.

Recorded with `stack.py` (#154 phase 5) on a Linux RTX 3090 host beside the live
stack, release build, an owner-local 1920x1080p30 clip of 342 frames with its
manifest and the onsite start description, D 200 ms: in the external mode
(`tracking = "external"`) 553 of 558 slots had CG after the start (the 5 clean
ones before the render worker was ready), 0 repeats, capture to output
200.0/200.2/201.0 ms, per-slot slack of the CG slots 123.7/183.7/188.3 ms, camera
model `radial+warp`. These are observations on a shared host, not latency or
throughput guarantees.

## Limits

- The lens travels per frame but a tracking worker's is fixed per session;
  zoom-dependent radial coefficients are not modelled, and nothing in the
  calibration records requires them yet.
- No camera prediction: a frame without its camera is clean feed.
- The external cameras are keyed by `frame_id`; right at an input restart a
  camera can arrive before its stream's first frame and be lost.
- The TypeScript render harness does not mirror `lens`, `frame_warp` or the
  `tracking.frame` method; it mirrors `tracking.get_status`.
