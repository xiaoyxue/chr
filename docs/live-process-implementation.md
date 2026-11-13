# Live process implementation plan

**Status:** `proposed` — implementation plan for the `confirmed`
[Live process architecture](live-process-architecture.md). Field names and
defaults below are the intended first version; each step's pull request updates
this document if they change. Steps 1 (#99), 2 (#95) and 3 (#96) are `implemented`; step 4 is not.

**Scope:** delivery order (tracked in #100), package changes, the worker protocol, the `chr-shm`
ring layout, supervision and clean feed, the monitor process, and acceptance
tests. Physical SDI (#80) and tracking results (#60) are excluded.

**Depends on:** [Live process architecture](live-process-architecture.md),
[Synthetic runtime program output](../server/docs/program-output.md),
[Frame submissions](../server/docs/frame-submissions.md).

## Delivery order

Each step is one pull request that keeps the workspace green and the existing
demos runnable. Step 2 lands as two: `chr-shm` first, because the worker also
needs it, then the program tap, `chr-monitor` and the harness.

| Step | Issue | Change |
| --- | --- | --- |
| 1 | #99 | Rename the `server/runtime/` package `chr-server` → `chr-runtime`; the binary stays `chr-server` (`default-run`). Update `-p` commands, the flake package and docs. No behaviour change. |
| 2 | #95 | Add `common/shm/` (`chr-shm`). Add root `monitor/` (`chr-monitor`) with the encoders, WebRTC hub and SRT output moved from `server/runtime/src/monitor/`. `chr-server` writes a program tap and drops its WebRTC/SRT code. Update the render harness. |
| 3 | #96 | Add the `chr-renderer-cli` executable to `chr-runtime` with the `render` subcommand. |
| 4 | #97 | Add `chr-renderer-cli worker`, move all GPU work out of `chr-server`, and add supervision, deadlines and clean feed. |

## Step 1: package rename

- `server/runtime/Cargo.toml`: `name = "chr-runtime"`, `default-run = "chr-server"`,
  `[[bin]] name = "chr-server"`. Rust paths change from `chr_server::` to
  `chr_runtime::`. `Cargo.lock` changes only by the package name.
- Commands become `cargo run --locked -p chr-runtime -- …` and
  `cargo test --locked -p chr-runtime …`. Process names, CLI options and
  JSON-RPC are unchanged.

## Step 2: `chr-shm`, program tap and `chr-monitor` (#95)

### `chr-shm` ring

One file per ring, created by the writer in a private directory (mode 0700,
files 0600; `/dev/shm` on Linux) and mapped by every process that uses it. The
crate is the only one with an `unsafe` lint exception: it declares its own
`[lints]` with `unsafe_code = "deny"` and allows it only in the mapping module,
with a safety comment per block. No reference into shared memory leaves the
crate; data moves through explicit copies and atomics.

```text
file header (64 bytes)
  magic        8 bytes  "CHRSHM01"
  slot_count   u32
  reserved     u32
  slot_bytes   u64      payload capacity per slot
  write_count  u64      atomic; incremented after each completed write
  reserved     …
slot i (64-byte header + slot_bytes payload, 64-byte aligned)
  sequence     u64      atomic seqlock: odd while a write is in progress
  index        u64      writer-defined frame index
  meta         [u64; 4] writer-defined metadata
  len          u64      valid payload bytes
  payload      slot_bytes
```

- Write: increment `sequence` to odd, release fence, copy header fields and
  payload, store the next even `sequence` with release, increment `write_count`.
- Read: load `sequence` (acquire); if odd, the slot is busy. Copy, acquire
  fence, reload `sequence`; the copy is valid only if both loads match.
- Writers never wait for readers. Torn reads are detected and discarded.
- The writer stages the file and renames it into place, so a reader never maps
  a partial header. `push` writes slots round-robin; readers of such rings (the
  program tap) find the newest slot from `write_count`.
- Linux first. A Windows file-mapping backend is required before Windows
  acceptance of steps 2 and 4.

### Program tap

- `chr-server --monitor-tap <path|none>` (default
  `$XDG_RUNTIME_DIR/chr/program.tap`). The SDI output thread writes every
  transmitted slot, repeats included, into an 8-slot ring:
  `index` = output slot, `meta[0]` = source frame, `meta[1]` = 1 when the slot
  carries a program frame, `meta[2]` = `width << 32 | height`, `meta[3]` = fps
  (`chr_shm::tap`, shared by both processes).
- The output thread's monitor channel and `monitor_dropped_frames` are gone;
  `sdi.get_status` dropped that field (Rust and TypeScript mirror). An optional
  in-process observer remains for tests that check slot provenance.
- `chr-server` no longer has `--srt`, `--webrtc-udp` or `--webrtc-tcp`.

### `chr-monitor`

```text
chr-monitor [--tap <path>] [--size <WxH>] [--fps <n>] [--ws <addr>]
            [--webrtc-udp <addr|none>] [--webrtc-tcp <addr>] [--srt <ip:port>]
            [--encoder openh264|gstreamer] [--allow-origin <origin>]
```

- `--size`/`--fps` (defaults 1280x720, 50) must match the server: the ring
  layout is checked on open and each slot's `meta` mode before encoding;
  mismatches are skipped and logged once. The monitor polls at half the frame
  period and encodes the newest slot while anyone watches; skipped indices are
  counted. It waits for a missing tap and reopens an idle one, because a
  restarted `chr-server` replaces the file; encoder and RTP time stay monotonic
  across such restarts.
- Serves JSON-RPC `webrtc.offer` (same params/result as before) and a new
  `monitor.get_status` (`frames_encoded`, `frames_skipped`, `viewers`,
  `srt_callers`) on its own WebSocket, default `127.0.0.1:8789`, using the
  `chr-common` transport. `chr-server` no longer answers `webrtc.offer`.
- `--srt` works with any program and either encoder; it no longer requires
  `--software-video`.
- `--srt` and `--encoder gstreamer` need the `gstreamer-media` feature. The
  monitor's GStreamer helpers are its own; it does not depend on `chr-runtime`.
- The render harness takes a separate monitor URL for video (`?monitor=`,
  `npm run verify -- --monitor`); control stays on `chr-server`. The development
  launcher `server/scripts/run-live.sh` starts both processes with matching
  `--size`/`--fps`/tap and stops both when either exits.

### Step 2 acceptance

- Unit tests: ring create/open validation, seqlock rejection of torn and
  overwritten slots, writer progress with a stalled reader, header mismatch.
- A cross-process test writes from one process and reads from another.
- `program_output` hardware tests read the tap and compare it with transmitted
  slots and provenance.
- `kill -9` of `chr-monitor` and a deliberately slow reader leave
  `sdi.get_status` cadence counters unchanged.
- Existing WebRTC and SRT regressions pass in `chr-monitor`.

Recorded on a Linux NVIDIA host (640x360p50, triangle): the harness
`verify` passed with WebRTC through `chr-monitor` (about 50 fps decoded, 0
skipped); `kill -9` of the monitor left the port at 50 slots/s with 0 repeats
and drops, and a restarted monitor streamed again from the live tap. SRT with
both encoders was received by GStreamer `srtsrc` at about 50 fps. The ffmpeg SRT
caller in the Nix shell does not complete a handshake with GStreamer `srtsink`,
also outside chr, so ffmpeg is not used as an SRT receiver.

## Step 3: `chr-renderer-cli render` (#96)

```text
chr-renderer-cli render (--synthetic-program | --pitch-demo <json> | --video-scene <json>)
    [--size <WxH>] [--fps <n>] [--state <scene-state.json>] [--background <png>]
    [--frame <n>] --output <png> [--overwrite]
```

- Uses the same program specifications (`program::spec::ProgramSpec`) and
  registration code as `chr-server`, so the output matches what the worker would
  render for the same snapshot. The frame takes the live path (`scene.create`,
  frame publish and submit, the scheduler's execution, `ProgramRenderer`); only
  the RGBA readback replaces the UYVY hand-off.
- `--fps` exists because a video scene validates its clip rate. There is no
  `--samples`: the program profiles fix their sample count, and the CLI must
  match the server.
- All inputs are validated before a GPU is requested.
- `--state` takes a `chr-common` `SceneState`; without it the program's initial
  scene is used. Its `configuration_id` is replaced by the program's, which
  derives from the run's random runtime ID. `--frame` selects the frame identity (lens model for
  `--video-scene`). `--background` replaces the program's input image.
- Writes the BT.709 RGBA composition as PNG code values without further colour
  conversion (no sRGB chunk). Refuses to overwrite unless `--overwrite`. Logs the adapter and
  rejects software adapters.
- A general scene file with arbitrary model paths is a follow-up; it must not
  become a de facto public distortion or asset schema.

### Step 3 acceptance

CPU tests for argument and input validation; an ignored GPU test that a
synthetic program rendered by the CLI matches the in-process program bridge
pixel for pixel on the same device.

Recorded: the GPU test runs the CLI executable, converts its PNG with the
bridge's UYVY converter and compares it with the UYVY frame transmitted by the
SDI port in the same process; they are identical.

## Step 4: worker, supervision and clean feed (#97)

### Worker protocol

`chr-server` starts `chr-renderer-cli worker` once (path from `--renderer-cli`,
default: next to the `chr-server` executable). stdin/stdout carry one JSON
object per line, at most 1 MiB; stderr is the worker's log. EOF on stdin makes
the worker exit.

| Direction | `type` | Fields |
| --- | --- | --- |
| server → worker | `hello` | `version` (1), `program` (program specification), `runtime_id`, `output` (`width`, `height`, `fps`), `input_ring`, `output_ring` |
| worker → server | `ready` | `adapter` (logged GPU adapter) |
| worker → server | `fatal` | `message`; the worker then exits |
| server → worker | `render` | `id`, `input_slot`, `output_slot`, `image` (`ImageDescription`), `frame` (`FrameInput`), `scene` (`SceneSnapshot`) |
| server → worker | `triangle` | `id`, `output_slot`, `frame`, `angle_rad` (default triangle mode) |
| worker → server | `done` | `id` |
| worker → server | `failed` | `id`, `message` |

- The program specification is one of `triangle`, `synthetic`,
  `software_video`, `video_scene { path }`, `pitch_demo { path }`. The worker
  builds registrations, configuration and lens replay from it with the given
  runtime ID; it rejects a mismatch with the snapshot's configuration.
- The worker handles requests in order. It caches the renderer scene for the
  last `(scene_id, scene_revision)` and the compositor per shadow mode, as the
  in-process bridge does today.
- Input and output rings have 4 slots each, sized for the native input and the
  output UYVY frame. A slot is reused only after its request completed, failed,
  or the worker was reaped.

### Supervision

```text
Starting ──ready──► Ready ──exit / fatal / hang──► Backoff ──timer──► Starting
```

- `--render-deadline-ms` (default: one frame period): after it, the frame is
  clean feed. `--worker-hang-ms` (default 2000): if the oldest outstanding
  request is older, the worker is killed. Backoff starts at 250 ms, doubles to
  5 s and resets after 10 s in `Ready`.
- `chr-server` creates no wgpu device in any mode, including the default
  triangle (its frames are rendered through `triangle` requests).
- New JSON-RPC `render.get_worker_status` (Rust and TypeScript mirror):
  `state`, `restarts`, `deadline_misses`, `clean_feed_frames`,
  `repeated_for_unavailable_clean_feed`, `last_error`.

### Clean feed

- On a deadline miss, worker failure or no free slot, `chr-server` marks the
  execution failed (its input was already copied into the ring, so no GPU access
  remains) and submits the clean feed for that frame without a program
  association.
- Conversion uses a CPU BT.709 RGBA8/BGRA8 → UYVY limited-range function in
  `chr-renderer`, matching the GPU converter's matrix and chroma averaging; colour
  math stays in the renderer library. A GPU test compares the two.
- Inputs that are not 8-bit BT.709 or whose extent differs from the output are
  not converted; the port repeats its last frame and the case is counted.

### Step 4 acceptance (no SDI hardware)

With the synthetic program and with the software video input:

- `kill -9` of the worker: output slots continue at cadence, clean-feed frames
  are counted, CG returns after restart with the current scene revision.
- `SIGSTOP` of the worker: killed and replaced within the hang limit.
- An injected delay above the deadline (test-only worker option): those frames
  are clean feed, late completions are ignored, nothing else is dropped.
- Scene edits during a restart apply to the first frame after recovery without
  a mixed snapshot.
- Rendered program pixels match the current in-process output for the existing
  `program_output` scenarios.
- Recorded measurements: ring copy cost per frame and restart-to-first-CG time.

## Documentation to update with the code

`AGENTS.md`, `README.md`, `docs/development.md`, `server/AGENTS.md`
(monitoring rule and layout), `server/README.md`, `server/docs/program-output.md`,
`docs/system-architecture.md`, the render harness README and the status lines of
both live-process documents.
