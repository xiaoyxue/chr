# Live process architecture

**Status:** `confirmed` — owner decision recorded in #94 (2026-11-13). Partly
`implemented`: `chr-monitor` serves WebRTC/SRT from the program tap (#95), but
`chr-server` still renders in process until the worker lands (#96, #97).
Implementation steps are in
[Live process implementation plan](live-process-implementation.md).

**Scope:** which processes make up the live render path, what each one owns,
how they communicate, and how program output survives renderer and monitor
failures. Physical SDI hardware, algorithm/tracking transport and recording are
referenced, not defined.

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
                                                      │ video input (software now, SDI in #80)
                                                      │ scheduling, deadlines, clean feed
                                                      │ authoritative scene state, health
                                                      │ output clock + output port
             stdin/stdout JSON lines + chr-shm rings  │
        ┌─────────────────────────────────────────────┤
        ▼                                             │ chr-shm tap ring (read-only for readers)
chr-renderer-cli worker                               ▼
  chr-renderer: upload → composite → readback     chr-monitor (optional)
  killable, restartable, stateless                  H.264 → WebRTC / SRT
                                                    own signalling WebSocket
```

| Process | Owns | Must not |
| --- | --- | --- |
| `chr-server` | Input adapters, frame identity, admission, deadlines, clean feed, output clock/port, JSON-RPC control, scene state, worker supervision, health | Create a GPU device; wait on the monitor |
| `chr-renderer-cli worker` | GPU device, renderer resources, composition of one frame per request | Hold authoritative state; touch input/output devices; decide timing |
| `chr-monitor` | Reading the tap, encoding, WebRTC/SRT fan-out, its own signalling endpoint | Feed anything back into program output |

Algorithm/tracking processes are expected to be out of process as well, but
their frame and result channels belong to #60 and are not defined here.

## Repository layout

| Path | Package | Contents |
| --- | --- | --- |
| `server/renderer/` | `chr-renderer` | GPU rendering library (unchanged) |
| `server/runtime/` | `chr-runtime` | Shared runtime library plus the `chr-server` and `chr-renderer-cli` executables |
| `server/viewer/` | `chr-app` | Native window and headless shell (unchanged) |
| `common/` | `chr-common` | JSON-RPC messages and IPC transports (unchanged role) |
| `common/shm/` | `chr-shm` | Shared-memory frame rings between processes; the only crate allowed to use `unsafe` |
| `monitor/` | `chr-monitor` | Separate monitoring process |

`chr-renderer-cli` is an executable of the runtime package rather than its own
package because the worker reuses the runtime's scene mapping, registrations and
program bridge, and because integration tests of the runtime need both
executables (`CARGO_BIN_EXE_*`). The package is renamed from `chr-server` to
`chr-runtime` so that its name describes the library rather than one binary.

`monitor/` sits at the repository root, not under `server/`: `server/` is the
render server and what runs in its process, while the monitor is a separate,
optional process that may run on another host from an SDI loop-out.
`chr-shm` is the protocol shared by `chr-server`, the worker and the monitor,
so it lives under `common/`.

## Channels

| Channel | Between | Transport | Carries |
| --- | --- | --- | --- |
| Control plane | Clients ↔ `chr-server` | Existing JSON-RPC over WebSocket/NativeSocket | Commands and status, never pixels |
| Worker control | `chr-server` ↔ worker | The worker's stdin/stdout, one JSON object per line | Startup spec, render requests, completions |
| Worker pixels | `chr-server` ↔ worker | Two `chr-shm` rings (input, output) | Input images and composited UYVY frames |
| Program tap | `chr-server` → monitor | One `chr-shm` ring | Every transmitted output slot, repeats included |
| Monitor signalling | Clients ↔ `chr-monitor` | JSON-RPC `webrtc.offer` on the monitor's own WebSocket | WebRTC session setup |

Using the worker's stdin/stdout avoids a listening socket and file-descriptor
passing. At 1080p a UYVY frame is about 4 MB; the rings add one copy per
direction per frame on top of the existing CPU upload and readback. GPU-handle
sharing is not required and not claimed.

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

- `chr-server` keeps the output clock. The worker never decides when a frame is
  transmitted.
- Each request has a deadline. A frame the worker does not finish in time is
  transmitted as its **clean feed** (the input image converted to the output
  format, without CG); its submission is reported as failed, not rendered.
  Late completions are discarded by request ID.
- Clean feed is available when the input is 8-bit BT.709 and its extent equals
  the output extent, which is the SDI-in/SDI-out case. Other inputs fall back to
  repeating the last output and are counted.
- Input-locked output with a fixed delay of D frames, genlock and audio
  pass-through are part of physical SDI (#80); the simulated output keeps its
  own clock until then.

## Failure handling

| Failure | Response |
| --- | --- |
| Worker misses a deadline | Clean feed for that frame; counted |
| Worker exits, crashes or reports a fatal error | Clean feed until a replacement is ready; respawn with backoff; counted |
| Worker stops responding (oldest request older than the hang limit) | `SIGKILL`, then respawn |
| GPU device lost inside the worker | Worker exits; handled as a crash |
| Monitor crashes, stalls or has no viewers | No effect on program output; the tap writer never waits |
| `chr-server` itself fails | Out of scope here: hardware bypass/watchdog and a service supervisor (#80) |

A hot-standby worker, fill+key output and cross-process zero-copy are possible
follow-ups, not part of this decision.

## References

- #94 decision; #95 monitor split and `chr-shm`; #96 `chr-renderer-cli`;
  #97 worker supervision and clean feed; #80 physical SDI; #60 tracking results.
