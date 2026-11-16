# Live process implementation plan

**Status:** `proposed` — implementation plan for the `confirmed`
[Live process architecture](live-process-architecture.md). Field names and
defaults below are the intended first version; each step's pull request updates
this document if they change. Steps 1 (#99), 2 (#95), 3 (#96) and 4 (#97) are
`implemented`; step 4 is verified on Linux only. #141 replaced step 4's worker
rings and stdin/stdout lines with GStreamer unixfd sockets (worker protocol
version 2).

**Scope:** delivery order (tracked in #100), package changes, the worker protocol, the `chr-shm`
ring layout (program tap and capture input ring), supervision and clean feed, the monitor process, and acceptance
tests. Physical SDI (#80) and tracking results (#60) are excluded.

**Depends on:** [Live process architecture](live-process-architecture.md),
[Synthetic runtime program output](../server/docs/program-output.md),
[Frame submissions](../server/docs/frame-submissions.md).

## Delivery order

Each step is one pull request that keeps the workspace green and the existing
demos runnable. Step 2 lands as two: `chr-shm` first, because the worker also
needed it (until #141), then the program tap, `chr-monitor` and the harness.

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
- Linux only. The live service does not run on Windows; Windows machines only
  preview its SRT output (for example in VLC).

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
            [--encoder openh264|gstreamer|nvenc|x264] [--allow-origin <origin>]
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
- `--srt` and the GStreamer encoders need the `gstreamer-media` feature. The
  monitor's GStreamer helpers are its own; it does not depend on `chr-runtime`.
- Since #144 the tap stays at the program's size (up to 7680x4320) and the
  monitor encodes a preview scaled down to fit 1920x1080 (aspect kept, never
  scaled up, even sides). `gstreamer` scales with `videoconvertscale` and
  encodes with NVENC `nvh264enc` when a probe frame encodes at start, else x264;
  `nvenc`/`x264` force one; openh264 samples the nearest pixels. WebRTC and SRT
  share that stream; `monitor.get_status` adds `encoder`, `preview_width` and
  `preview_height`.
- The render harness takes a separate monitor URL for video (`?monitor=`,
  `npm run verify -- --monitor`); control stays on `chr-server`. The development
  launcher `server/scripts/run-live.sh` starts both processes with matching
  `--size`/`--fps`/tap and restarts whichever exits while the other keeps
  running; `--no-monitor` leaves the monitor to be started separately.

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

> **Changed by #141:** the worker rings and the stdin/stdout JSON lines of
> version 1 were replaced by GStreamer `unixfdsink`/`unixfdsrc` sockets. This
> section describes the current version 2; the measurements further down record
> the version 1 rings. The tracking worker (#128) uses the same transport; its
> contract, including the transport, is in
> [tracking worker](../server/docs/tracking-worker.md#transport). vchao's
> adapter is to speak this protocol (#140).

`chr-server` starts one `chr-renderer-cli worker --input-socket <path>
--output-socket <path>` (path from `--renderer-cli`, default:
`chr-renderer-cli` next to the `chr-server` executable; the flake package
installs both) and replaces it when it fails. stderr is the worker's log;
stdout joins it, stdin is unused.

- **Transport** (`server/runtime/src/worker/transport.rs`): one Unix socket per
  direction in a private 0700 directory (`$XDG_RUNTIME_DIR/chr/worker-<pid>/`,
  otherwise the temp directory) that `chr-server` removes on shutdown.
  `chr-server` runs `appsrc ! unixfdsink` on `input.sock` and listens before it
  starts the worker; the worker runs `appsrc ! unixfdsink` on `output.sock` and
  `unixfdsrc ! appsink` on `input.sock`; `chr-server` connects its
  `unixfdsrc ! appsink` to `output.sock` once it exists. Sinks wait for their
  client (`wait-for-connection`), so `hello` and `ready` are not lost.
- **Messages:** every buffer is one message, held as serde JSON (tagged by
  `type`) in the string field `json` of one `GstCustomMeta` named
  `chr-worker-message`. Pixels are the buffer's single memory, memfd-backed
  (GStreamer shm allocator) and passed as a file descriptor; `unixfdsink`'s own
  copy is disabled (`min-memory-size=-1`). Control messages are buffers without
  memory. All buffers on a socket have the caps `application/x-chr-worker`.
- The end of the worker's input stream (EOS) or the loss of `chr-server` makes
  the worker exit.

| Direction | `type` | Payload | Fields |
| --- | --- | --- | --- |
| server → worker | `hello` | – | `version` (2), `program` (program specification), `runtime_id`, `output` (`width`, `height`, `fps`), `assets` |
| worker → server | `ready` | – | `adapter` (logged GPU adapter) |
| worker → server | `fatal` | – | `message`; the worker then exits |
| server → worker | `render` | input image | `id`, `image` (`ImageDescription`), `frame` (`FrameInput`: frame key and geometry), `scene` (`SceneSnapshot`) |
| server → worker | `triangle` | – | `id`, `frame`, `angle_rad` (default triangle mode) |
| worker → server | `done` | UYVY frame | `id` |
| worker → server | `failed` | – | `id`, `message` |

- The program specification is one of `triangle`, `synthetic`,
  `software_video`, `video_scene { path }`, `tracked_scene { path }`,
  `description { path }`, `pitch_demo { path }`. The worker
  builds registrations, configuration and lens replay from it with the given
  runtime ID, so registrations and IDs match `chr-server`'s; it rejects a
  mismatch with the snapshot's configuration. It logs the GPU adapter and
  rejects software adapters (`fatal`).
- The worker handles requests in order and replies in order. It rebinds each
  snapshot through `SceneAdapter::bind` and the frame geometry through
  `SceneAdapter::frame_camera`, caching the bound scene for the last
  snapshot, and composes with `ProgramRenderer::render_frame`
  (explicit frame parts instead of an `Execution`), which keeps the compositor
  per shadow mode as the in-process bridge does. It reads the input image in
  place from the mapped payload and checks its size against `image`.
- **Credits:** at most 4 requests are outstanding. Without a credit the frame is
  clean feed (`NoCredit`); a credit returns with the request's `done` or
  `failed`, or when the worker is replaced. `chr-server`'s `appsrc` never
  blocks and its queue is bounded, so a stopped worker cannot grow
  `chr-server`'s memory: it holds at most 4 input images and 4 UYVY frames.
- **One copy per captured frame:** a tracked program's frames are copied once,
  from the capture input ring into shared memory; the tracking worker and the
  render worker receive that same memory. Other programs' inputs (synthetic,
  software video) are copied into shared memory when sent, as they were copied
  into the input ring before. The UYVY result is copied once by the worker
  into shared memory and once by `chr-server` out of it.
- Test-only: `CHR_WORKER_TEST_DELAY_MS` delays every worker reply by that many
  milliseconds. The worker inherits it from `chr-server`'s environment.

### Supervision

```text
Starting ──ready──► Ready ──exit / fatal / hang──► Backoff ──timer──► Starting
```

- `--render-deadline-ms` (default: 3/4 of the frame period, so the clean-feed
  conversion still fits in the frame; the wait never passes the loop's next
  frame boundary): after it, the frame is clean feed; a late completion only
  returns its credit. A lost GPU device makes the worker exit, handled as a crash. `--worker-hang-ms`
  (default 2000): if the oldest outstanding request is older, the worker is
  killed (`SIGKILL`) and replaced. A worker that is not ready within 60 s is
  replaced. Backoff starts at 250 ms, doubles to 5 s and resets after 10 s in
  `Ready`.
- `chr-server` creates no wgpu device in any mode, including the default
  triangle (its frames are rendered through `triangle` requests).
- New JSON-RPC `render.get_worker_status` (Rust and TypeScript mirror):
  `state` (`starting`, `ready` or `backoff`), `pid`, `restarts`,
  `deadline_misses`, `clean_feed_frames`, `repeated_for_unavailable_clean_feed`,
  `last_error`.

### Clean feed

- On a deadline miss, worker failure, no ready worker or no free credit,
  `chr-server` marks the execution failed (a worker still rendering it holds
  its own reference to the shared input memory) and submits the clean feed
  for that frame without a program association.
- Conversion uses `chr_renderer::uyvy_from_bt709` on the CPU (BT.709
  RGBA8/BGRA8 → UYVY limited range, pair-mean chroma, ties-to-even), bit-identical
  to the GPU `UyvyConverter`; colour math stays in the renderer library. A GPU
  test compares the two (0 of 262144 bytes differ).
- Inputs that are not 8-bit BT.709 or whose extent differs from the output are
  not converted; the port repeats its last frame and the case is counted
  (`repeated_for_unavailable_clean_feed`). The triangle has no input, so its
  missed frames are repeats.

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
- Recorded measurements: ring copy cost per frame and restart-to-first-CG time (below).

Recorded on a Linux NVIDIA host (nixGL). The ignored GPU tests in
`server/runtime/tests/render_worker.rs` show worker output byte-identical to the
in-process bridge for the same synthetic execution, and, with the synthetic
program at 192x128p50: `kill -9` of the worker keeps the output at cadence with
clean-feed frames, and CG returns after the restart (new worker ready in about
1 s); `SIGSTOP` is detected after the test's 500 ms hang limit and the worker is
replaced; a worker delayed to 60 ms per frame against a 20 ms deadline gives
clean feed at cadence with no drops and no restart. Manual release-build runs:
the default triangle with `chr-monitor` passed `npm run verify` (12/12, including
the worker check); with software video input (640x360p30 generated clip),
`kill -9` of the worker gave 16 clean-feed frames while the output stayed at
cadence (65 slots in about 2.1 s, 1 repeat, 0 drops), and a new worker was ready
within 2 s.

Measurements (release builds, same host):

| Measurement | Result |
| --- | --- |
| Ring write / read, 1920x1080 RGBA8 input (8.3 MB) | 1.3 ms / 1.1 ms |
| Ring write / read, 1280x720 RGBA8 input (3.7 MB) | 0.39 ms / 0.38 ms |
| Ring write / read, 1920x1080 UYVY output (4.1 MB) | 0.46 ms / 0.46 ms |
| Ring write / read, 1280x720 UYVY output (1.8 MB) | 0.15 ms / 0.16 ms |
| `kill -9` to new worker ready, calibrated video scene with a 1.35M-triangle, 47 MB GLB (includes the 250 ms backoff) | 1.03 s |
| `kill -9` to first CG frame, same scene at 1280x720p30 | 1.24 s (35 frames without CG) |

Ring costs are medians of 200 frames from `cargo test --release -p chr-shm
--test copy_cost -- --ignored --nocapture`, so a 1080p-input, 720p-output frame
added about 2.7 ms of copies, split between the two processes.

Since #141 (unixfd transport), a 1920x1080 RGBA8 frame costs 0.75 ms to copy
into recycled shared memory (0.8 ms into a new `Vec`; 4.1 ms into a fresh memfd,
which is why released memories are reused) and 0.2–0.45 ms to send, receive and
map, from `cargo test --release -p chr-runtime --lib transfer_cost -- --ignored
--nocapture`. Per tracked captured frame `chr-server` now makes one copy where
it made four (input ring → `Vec`, → tracking ring, → frame pool, → render input
ring), and the workers read the input in place instead of copying it out of a
ring. The kill/stop/delay GPU tests and the byte-identical comparison with the
in-process bridge pass over the new transport.

End to end (release, nixGL, the onsite description at 1280x720p30 with the
tracking worker and a 1920x1080 camera, 60 s), the unixfd build and the ring
build both transmitted every frame with CG (no clean feed, no worker restart,
every frame tracked, render 3.8 ms wall per frame in `chr-renderer-cli bench`).
Over the last 40 s the unixfd build used 9 % of a core in `chr-server` (ring
build 22 %), 13 % in the render worker (16 %) and 1 % in the tracking worker
(4 %); resident memory was 169 / 195 / 11 MiB (211 / 213 / 34 MiB), flat over
the run. In the restart
measurement the input (1920x1080) differs from the output extent, so the frames
without CG repeated the last frame instead of carrying clean feed. A rotating
1.35M-triangle model rendered at 30 fps with 0 deadline misses after GPU mesh
buffers were reused across scene revisions (#106). Scene edits during a restart
were not measured separately. Physical SDI is not verified (#80). The live
service is Linux-only; SRT preview from a Windows PC in VLC was checked against
this stack (the Linux host must allow the SRT UDP port through its firewall).

## Resolutions and performance

Since #144 the camera programs take a native camera extent and an output size up
to 8K UHD (7680x4320, both even, same aspect; `program::MAX_EXTENT`). Only
`--synthetic-program` and `--software-video` keep their 1280x720 demonstration
limit (`program::DEMO_MAX_EXTENT`). The SDI port and the program tap run at the
output size; `chr-monitor` encodes a preview of at most 1920x1080 (above).

### Memory per resolution

| | 1280x720 | 1920x1080 | 3840x2160 | 4096x2160 | 7680x4320 |
| --- | --- | --- | --- | --- | --- |
| RGBA8 input frame | 3.7 MB | 8.3 MB | 33 MB | 35 MB | 133 MB |
| UYVY output frame | 1.8 MB | 4.1 MB | 17 MB | 18 MB | 66 MB |
| Input ring + program tap (8 + 8 slots, in `dir`) | 44 MB | 100 MB | 398 MB | 425 MB | 1.59 GB |
| Shared input frames in `chr-server`, about | 70 MB | 158 MB | 630 MB | 672 MB | 2.5 GB |
| Frame-resource pool budget (`64 * w * h`) | 59 MB | 133 MB | 531 MB | 566 MB | 2.1 GB |
| Composition budget, GPU (`max(256 MiB, (3 * native + output) * 8)`) | 268 MB | 268 MB | 268 MB | 283 MB | 1.06 GB |

The shared input frames are the capture backlog (the wait in frame periods plus
two, at least four; five at the default 3-period wait), two ready frames, the
four frames the render worker may hold and eight released memories kept for
reuse (`RECYCLED`). Every one of these queues has a fixed length, so memory
grows with the frame size only. The composition budget covers the native and
output RGBA16Float canvases and a guard-banded ideal canvas of up to twice the
native area; MSAA targets and shadow maps are outside it. Description assets
keep their own limits (64 assets, 1 GiB, 256 MiB per file). A synthetic program's
own constant input image is only made when it injects one, so a described 8K
program no longer allocates 133 MB for it in both `chr-server` and the worker.

### Measurements

Recorded on the RTX 3090 host (i9-10900X, 62 GiB RAM, 6.3 GB `$XDG_RUNTIME_DIR`)
with `server/scripts/resolution_bench.py` (synthetic camera: a testsrc2 clip, a
fixed top-down camera, four image quads, 4x MSAA, radial lens) while the live
stack's render worker kept running on the same GPU. **Every wall time and every
deadline or clean-feed figure below is distorted by that second Vulkan process**
(next section); GPU times, memory and the monitor's encoder are not.

| | 1280x720 | 1920x1080 | 3840x2160 | 7680x4320 |
| --- | --- | --- | --- | --- |
| Render GPU time per frame, mean (`bench`) | 1.07 ms | 2.12 ms | 7.16 ms | 24.4 ms |
| Render wall time per frame, mean (`bench`, distorted) | 374 ms | 837 ms | 117 ms | 147 ms |
| Render worker GPU memory | | 465–721 MiB | 1021 MiB | 3595 MiB |
| `dir` (tmpfs) used | | 95 MiB | 380 MiB | 1519 MiB |
| `chr-server` RSS (of it shared frames) | | 198–210 (166–174) MiB | 713–760 (664–680) MiB | 2368–2738 (2278–2595) MiB |
| `chr-monitor` RSS / CPU at 30 slots/s | | 212 MiB / 12–20 % | 333 MiB / 104 % | 807 MiB / 95–98 % |
| Monitor preview | | 1920x1080 NVENC | 1920x1080 NVENC | 1920x1080 NVENC |

- **1920x1080p30, onsite description** (`start.json`, the recorded 1080p clip,
  tracking worker; 60 s): 30 slots/s, 29.9 frames/s paired with a camera, every
  frame tracked, none without a camera. Every frame missed the render deadline
  and went out as clean feed, as the 837 ms wall time predicts. `chr-server` used
  90 % of a core, mostly converting clean feed; the worker 88 %.
- **3840x2160p30** (30 s): 29.8 frames/s tracked; 204 deadline misses, 700
  repeated slots. At **5 fps**, 0 deadline misses and 0 clean feed, CG on every
  frame of the recorded SRT preview.
- **7680x4320p30** (30 s): the software camera delivered only 6.3–6.7 frames/s
  (`chr-tracking-sim` and its `ffmpeg` decoder at about 220 % each, limited by
  moving 66 MB UYVY frames through the decoder pipe), so most slots repeated; 59
  deadline misses. At **2 fps**, 0 deadline misses and 0 clean feed, CG on every
  frame. The monitor encoded about 20 of the 30 slots/s, counting the rest as
  skipped: it reads, copies and hands each slot to the encoder on one thread.
- At **1 fps**, 1080p still missed every deadline (750 ms), as its distorted wall
  time exceeds it.
- The SRT preview was H.264 Constrained Baseline 1920x1080 (level 4.0) at every
  size, received by GStreamer `srtsrc` and read by `ffprobe`. The monitor's CPU
  is mostly the downscale: `videoconvertscale` costs about 28 ms of CPU per 8K
  frame and 17 ms per 4K frame (bilinear without a chroma pass; 34 and 22 ms
  with it, 13 and 8 ms nearest-neighbour).

The GPU times put 8K at about 24 ms of a 33 ms frame at 30 fps on this GPU; 8K at
50 or 60 fps does not fit. The 8K camera simulation, not the program, limited the
8K run's rate. The clean numbers still have to be taken with nothing else on the
GPU, from the repository root inside `nix develop .#media`:

```sh
for size in 1280x720 1920x1080 3840x2160 7680x4320; do
  python3 server/scripts/resolution_bench.py prepare $size
  python3 server/scripts/resolution_bench.py bench $size
  python3 server/scripts/resolution_bench.py run $size --seconds 60
done
python3 server/scripts/resolution_bench.py run 1920x1080 --seconds 60 \
  --scene <manifest.json> --video <1080p clip> --description <start.json> --asset-root <asset-root>
cargo run --release -p chr-renderer --example upload_probe   # through nixGL on non-NixOS hosts
```

### A second Vulkan process on the same GPU

On this host (BAR1 256 MiB, no Resizable BAR) a render process started next to a
running render worker spends most of each frame in CPU writes to and reads from
memory it maps from the GPU driver, not on the GPU. `upload_probe`, run next to
the live worker, times a memcpy of one RGBA8 frame into such memory:

| Frame | Fresh `MAP_WRITE` (BAR1 first) | Fresh `MAP_READ` (host cached) | Reused mapped buffer | `queue.write_texture` |
| --- | --- | --- | --- | --- |
| 1080p, 8.3 MB | 828–845 ms | 837–845 ms | 833–849 ms | 838–844 ms |
| 4K, 33 MB | 102–106 ms | 102–104 ms, then 4 ms | 4 ms | 6 ms, then 103 ms |
| 8K, 133 MB | 16 ms | 16 ms | 16 ms | 49–53 ms |

With `GLIBC_TUNABLES=glibc.cpu.x86_non_temporal_threshold=0x100000`, which makes
glibc's `memcpy` use non-temporal stores from 1 MiB on, the 1080p rows drop to
25–27 ms while 4K and 8K stay the same. So the cost is not the GPU, and not
BAR1 alone: the host-cached type is just as slow. Ordinary stores into these
mappings run at about 10 MB/s, as if the second process's mappings were
uncached, while non-temporal stores (glibc uses them above about 3/4 of the L3
cache, so for 4K and 8K frames) and large dedicated allocations stay fast. wgpu
30 makes every `queue.write_texture` allocate a fresh `MAP_WRITE` staging buffer,
which gpu-allocator places in `DEVICE_LOCAL | HOST_VISIBLE` (BAR1, a 246 MiB
Vulkan heap with about 113 MiB of budget left next to the live worker) when it
fits and in host memory otherwise. The workaround above did not fix a 1080p
worker in a stack, whose readbacks read such memory too. Confirming the cause
needs the memory types of the second process's mappings (as root:
`/sys/kernel/debug/x86/pat_memtype_list`), the probe with the live stack stopped,
and Resizable BAR. Until then run one render process per GPU.

## Documentation to update with the code

`AGENTS.md`, `README.md`, `docs/development.md`, `server/AGENTS.md`
(monitoring rule and layout), `server/README.md`, `server/docs/program-output.md`,
`docs/system-architecture.md`, the render harness README and the status lines of
both live-process documents.
