# Tracking worker

**Status:** `implemented` process contract with a replay stand-in. The real
tracking module is not part of the repository yet; it replaces the stand-in by
speaking this protocol.

`chr-server --tracking-worker <calibration>` starts one tracking worker, gives it
every captured frame, and takes the camera of each frame from it. This is the
`chr → live video → tracking → per-frame Ti → chr` flow of
[system architecture](../../docs/system-architecture.md). chr-server supervises the
tracking worker as it does the render worker. The tracking worker may hold its
own GPU device; for now both workers share one GPU.

```text
chr-server ── frame N: chr-shm ring + `track` line ──────────► tracking worker
           ◄── `camera` (Ti, C, frame_warp of frame N) | `unavailable` ──┘
```

The cameras go into the same join as `tracking.frame`: frames and cameras are
paired by capture sequence only. A frame whose camera does not arrive within
`--tracking-wait-ms` (default three frame periods) is skipped: it is not
rendered, and the SDI port repeats its previous frame. An old camera or a
placeholder camera is never used. That is the existing simulation
behaviour, not a production policy (see
[frame input](../../docs/render-frame-input.md)).

## Executables

| Executable | Role |
| --- | --- |
| `chr-tracking-sim-cli` (`chr-runtime`, `server/runtime/src/tracking_sim_cli.rs`, next to `chr-renderer-cli`) | Stand-in worker. It reads every frame from the ring but answers with the **recorded** camera of a `calibrated-video-v1` manifest (fixed `ti` and `center`, warp `sequence % frame_warps`). It replays; it never estimates. |
| The real tracking module | Any executable, in any language. chr-server runs it as `<executable> worker [args]...` (`--tracking-cli <path>`, `--tracking-worker-arg <arg>`). |

`chr-tracking-sim-cli worker` options inject faults:
- `--delay-ms <n>` spends `n` ms on every frame;
- `--unavailable-every <n>` answers every n-th frame as unavailable;
- `--exit-after <n>` exits on the n-th request;
- `--hang-after <n>` stops answering from the n-th request on.

## Protocol

One JSON object per line on the worker's stdin (requests) and stdout (replies);
a line is at most 64 KiB. Logs go to stderr. EOF on stdin asks the worker to exit.
The authoritative types are in `server/runtime/src/worker/tracking/protocol.rs`.

| Direction | Message | Fields |
| --- | --- | --- |
| server → worker | `{"type": "hello", ...}` | `version` (1), `input_ring` (path), `width`, `height`, `fps`, `calibration` (path to the tracking reference) |
| worker → server | `{"type": "ready", "name": ...}` | Sent once the reference is loaded; `name` describes the method |
| worker → server | `{"type": "fatal", "message": ...}` | Startup failed; the worker then exits |
| server → worker | `{"type": "track", "id": 7, "sequence": 42, "slot": 3}` | Frame `sequence` waits in ring slot `slot` |
| worker → server | `{"type": "camera", "id": 7, "camera": {...}}` | `camera` is a `tracking.frame` object whose `sequence` is the request's (decimal string) |
| worker → server | `{"type": "unavailable", "id": 7, "reason": ...}` | The frame has no camera |

A `camera` holds:
- `ti`: nine numbers, forward row-major, ground XY to ideal image SNC;
- `camera_center`: world metres, negative Z up;
- `frame_warp`: nine numbers or `null`.

All three follow [coordinates](coordinates.md) and
[tracking simulation](tracking-simulation.md#trackingframe). chr-server validates
each camera as it validates `tracking.frame`.

Every `track` gets exactly one answer, in any order and possibly late.

## Frame ring

`input_ring` is a `chr-shm` ring that chr-server creates. Its layout and seqlock
are described in `common/shm/src/lib.rs`: a 64-byte file header, then per slot a
64-byte header (sequence, index, meta, len) and the payload.

| Part | Content |
| --- | --- |
| Slots | 8; at most 8 frames await an answer at once |
| Payload | RGBA8, BT.709 code values, `width` × `height` × 4 bytes |
| Header `index` | The request `id` |
| Header `meta[0]` | The capture sequence |

chr-server rewrites a slot only after its request was answered or the worker was
replaced. A reader still checks that `index` equals the request's `id` and
re-reads the slot sequence to detect a torn copy. It answers `unavailable` if
the slot does not hold the frame.

## Supervision

```text
Starting ──ready──► Ready ──exit / fatal / hang──► Backoff ──timer──► Starting
```

- **Startup:** a worker that is not `ready` within 60 s is replaced.
- **Hang:** a worker that leaves a frame unanswered for longer than `--worker-hang-ms`
  (default 2000 ms) is killed and restarted, with the backoff growing from
  250 ms to 5 s.
- **While no worker is ready:** frames are not sent, so they get no camera and are
  skipped. The SDI output continues by repeating its last frame; killing the
  stand-in at 30 fps costs about seven frames.
- **Capture restart:** after the capture restarts its sequence numbers,
  unanswered frames are forgotten and their answers ignored.

`tracking.get_worker_status` returns:
- the state (`starting`, `ready`, `backoff`), PID and restarts;
- counters: requests, cameras, unavailable answers, and frames not sent;
- the last error.

With a tracking worker, `tracking.frame` is rejected. `tracking.get_status`
still reports the join.

## Running it

```sh
server/scripts/run-tracked.sh --scene clip-manifest.json --video clip.mp4 --tracking-worker
```

`--tracking-worker` passes the manifest to chr-server's worker and starts
`chr-tracking-sim` with `--no-tracking`, so the simulator is the camera only.
`stack.py` does this by default (`tracking = "worker"` in `stack.toml`).
`tracking = "external"` keeps the previous behaviour, in which the simulator sends
the cameras.
