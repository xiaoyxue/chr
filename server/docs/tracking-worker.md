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
chr-server ── `track` N + frame N (memfd) over unixfd ──────► tracking worker
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
| `chr-tracking-sim-cli` (`chr-runtime`, `server/runtime/src/tracking_sim_cli.rs`, next to `chr-renderer-cli`) | Stand-in worker. It maps every frame it receives but answers with the **recorded** camera of a `calibrated-video-v1` manifest (fixed `ti` and `center`, warp `sequence % frame_warps`). It replays; it never estimates. |
| The real tracking module | Any executable, in any language with GStreamer bindings. chr-server runs it as `<executable> worker --input-socket <path> --output-socket <path> [args]...` (`--tracking-cli <path>`, `--tracking-worker-arg <arg>`). vchao's tracking adapter is to speak this protocol (#140, #141). |

`chr-tracking-sim-cli worker` options inject faults:
- `--delay-ms <n>` spends `n` ms on every frame;
- `--unavailable-every <n>` answers every n-th frame as unavailable;
- `--exit-after <n>` exits on the n-th request;
- `--hang-after <n>` stops answering from the n-th request on.

## Transport

Version 2 of the protocol travels over GStreamer's `unixfd` elements
(gst-plugins-bad, GStreamer ≥ 1.24), the same transport as the render worker
(`server/runtime/src/worker/transport.rs`). There is one Unix socket per
direction, in chr-server's private 0700 directory
`$XDG_RUNTIME_DIR/chr/tracking-<server pid>/`, removed on shutdown:

```text
chr-server  appsrc ! unixfdsink socket-path=input.sock    ──►  unixfdsrc ! appsink   worker
chr-server  appsink ! unixfdsrc socket-path=output.sock   ◄──  unixfdsink ! appsrc   worker
```

- The sending side listens (`unixfdsink`, `wait-for-connection=true`) and the
  receiving side connects (`unixfdsrc`). chr-server listens on `input.sock`
  before it starts the worker. The worker listens on `output.sock` (replacing a
  stale file) and connects to `input.sock`; chr-server connects to
  `output.sock` once it exists. Nothing is lost to the startup order: a sink
  holds its buffers until a client connects.
- **Every buffer is one message.** It carries exactly one `GstCustomMeta`
  named `chr-worker-message` whose structure has a string field `json`: the
  message as a JSON object tagged by `type`. Both sides must register a custom
  meta of that name (`gst_meta_register_custom`) before receiving, or `unixfdsrc`
  rejects the buffer.
- A message with pixels has them as the buffer's single memory, memfd-backed
  (GStreamer's shm allocator), passed as a file descriptor without a copy
  (`unixfdsink` would copy other memory into a memfd; chr-server disables that
  copy, so its payloads are always shared). A control message is a buffer
  without memory. Every buffer on a socket has the
  caps `application/x-chr-worker`; formats and extents come from `hello`.
- Received memory is read-only. Dropping the buffer releases it to the sender.
  When a process dies, its sockets close and the peer releases everything it
  held for it.
- The end of the worker's input stream (EOS) or the loss of chr-server asks the
  worker to exit. stdin is not used; stdout joins stderr, the worker's log.

## Protocol

The authoritative types are in `server/runtime/src/worker/tracking/protocol.rs`.

| Direction | Message | Payload | Fields |
| --- | --- | --- | --- |
| server → worker | `{"type": "hello", ...}` | – | `version` (2), `width`, `height`, `fps`, `calibration` (path to the tracking reference) |
| worker → server | `{"type": "ready", "name": ...}` | – | Sent once the reference is loaded; `name` describes the method |
| worker → server | `{"type": "fatal", "message": ...}` | – | Startup failed; the worker then exits |
| server → worker | `{"type": "track", "id": 7, "sequence": 42}` | the frame | Frame `sequence` |
| worker → server | `{"type": "camera", "id": 7, "camera": {...}}` | – | `camera` is a `tracking.frame` object whose `sequence` is the request's (decimal string) |
| worker → server | `{"type": "unavailable", "id": 7, "reason": ...}` | – | The frame has no camera |

A `camera` holds:
- `ti`: nine numbers, forward row-major, ground XY to ideal image SNC;
- `camera_center`: world metres, negative Z up;
- `frame_warp`: nine numbers or `null`.

All three follow [coordinates](coordinates.md) and
[tracking simulation](tracking-simulation.md#trackingframe). chr-server validates
each camera as it validates `tracking.frame`.

Every `track` gets exactly one answer, in any order and possibly late.

## Frames

A `track` payload is RGBA8, BT.709 code values, `width` × `height` × 4 bytes,
tightly packed. chr-server copies each captured frame once, from the capture
input ring into shared memory, and hands that same memory to the tracking and
the render worker: the tracking worker must only read it.

chr-server grants **8 credits**: at most 8 frames await an answer at once. Without
a credit (or a ready worker) a frame is not sent and counts as `not_sent`. A
worker holds at most those 8 frames; chr-server's queue towards it stays bounded
and never blocks the capture loop. The stand-in answers `unavailable` for a payload
of another size.

## Supervision

```text
Starting ──ready──► Ready ──exit / fatal / hang──► Backoff ──timer──► Starting
```

- **Startup:** a worker that is not `ready` within 60 s is replaced.
- **Exit:** a worker that exits is replaced; once connected, the end of its
  output stream reports the exit after its last messages (such as `fatal`).
- **Hang:** a worker that leaves a frame unanswered for longer than `--worker-hang-ms`
  (default 2000 ms) is killed (SIGKILL) and restarted, with the backoff growing from
  250 ms to 5 s. Its frames are released with its sockets and every credit returns.
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
