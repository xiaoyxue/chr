# GStreamer core pipeline (#154, phase 1, experimental)

**Status:** `experimental` simulation prototype behind `chr-server --pipeline gst`
on Linux. The timer-paced loop (`--pipeline timer`, the input ring and the
simulated SDI port) stays the default and is unchanged. This is phase 1 of #154:
fractional and changing rates (phase 2), DeckLink input/output with genlock
(phase 3), audio, ancillary data and timecode (phase 4) and the switch-over
(phase 5) are not started.

`chr-server` owns one GStreamer pipeline from the input to the output. Its time
is the pipeline clock (the system clock in simulation; the output card's in
phase 3), not a software timer:

```text
chr-sdi-in (INPUT)                         chr-server (CORE, one pipeline "chr-core")
file: ffmpeg | fdsrc ! rawvideoparse ─┐
  ! videoconvert                      │    capture-<n>: unixfdsrc ! RGBA,<camera> ! tee
pattern: videotestsrc ────────────────┤      ├─ queue leaky ! appsink ──► driver: tracking hop ─► camera
  ! RGBA + VchaoFrame wire caps       │      │                             (Ti + C + #150 lens) ─► render hop ─┐
  ! clocksync ! [stamp] ──────────────┤      └─ queue leaky ! videoconvertscale ! UYVY,<output> ! appsink ─────┤
  ! queue leaky ! unixfdsink ─unixfd──┘                                       selector (per slot, deadline) ◄──┘
                                           program: appsrc ! livesync ! tee ─┬─ queue ! appsink sync (OUTPUT)
                                                                             └─ queue leaky ! appsink (program tap)
                                           hop-<n>: appsrc ! unixfdsink / unixfdsrc ! appsink, per worker session
```

## Elements and the Nix shells

| Element | Package | In the shells | Use |
| --- | --- | --- | --- |
| `unixfdsrc`, `unixfdsink`, `rawvideoparse` | gst-plugins-bad 1.28.8 | yes | capture and worker hops; file input |
| `decklinkvideosrc`, `decklinkvideosink`, `decklinkaudiosrc/sink` | gst-plugins-bad 1.28.8 | yes (no card on the dev host) | phase 3 |
| `clocksync`, `queue`, `tee` | GStreamer core | yes | |
| `videoconvert(scale)`, `videotestsrc`, `appsrc`, `appsink` | gst-plugins-base | yes | |
| `livesync`, `fallbackswitch` | gst-plugins-rs 0.15.3 | **added** | `livesync`: rate lock; `fallbackswitch`: evaluated, not used |

gst-plugins-rs is in the pinned nixpkgs and its binary cache. The flake copies
just `libgstlivesync.so` and `libgstfallbackswitch.so` into a small derivation
(`chr-gst-livesync`) on `GST_PLUGIN_SYSTEM_PATH_1_0` of both shells, the
checks and the wrapped `chr-server` package programs: the whole package
(about 55 plugins, a 1 GB closure with GTK 4 and more) would load on every
registry scan. `flake.lock` is unchanged.

### Why not `fallbackswitch`

`fallbackswitch` fails over between *streams*: the active pad stays active
until it has had no buffer for `timeout`, and on a switch it drops the other
pad's buffers whose running time precedes the switch. #132 needs a decision per
*slot*: the slot gets its frame's CG, else that frame's own clean feed. A probe
(`server/scripts/gst_fallbackswitch_probe.py`, 50 fps, clean feed on the
fallback pad, CG on the main pad with frame 10 missing, 20–30 missing as for
a crashed worker and 40 late) gave:

| Configuration | Result |
| --- | --- |
| PTS = due time, `timeout` 5 ms | Slots 10, 11, 20, 28, 31 and 40 never output (repeated downstream); of the 13 frames without CG only 9 (21–27, 29, 30) show their clean feed, and slot 41 shows clean although its CG came |
| PTS = due time, `timeout` 0 | Every frame without CG shows its clean feed, but the timeout races CG frames due at the same instant: 14 of the 47 slots whose CG came go out clean, and slot 31 is lost; it varies from run to run |
| PTS = capture, `latency` = D, `timeout` 10 ms | Collapses after the first switch: 48 of the 60 slots are lost |

The core therefore decides slots itself: an `appsink` per branch, a selector
thread that waits on the pipeline clock, and the program `appsrc`. A real
GStreamer element (an aggregator subclass) would be the natural form, but
GObject subclassing in Rust needs `unsafe` glue that the workspace forbids.
`livesync` keeps the output at the caps' rate and repeats the previous frame
only when no frame came at all (input loss).

## Timing

- **Capture time.** `chr-sdi-in` paces the source with `clocksync` on the
  system clock and stamps every frame as it leaves: `offset` = `frame_id`
  (from 0, continuous across loops of a file), the frame duration, and a
  VchaoFrame meta (`chr_tracker::protocol`) with `source_ns`, its time on the
  same-host system clock. `unixfdsink` sends the PTS as that absolute clock
  time and the core's `unixfdsrc` turns it back into the core's running time
  `t`, so both processes agree on the instant without a common pipeline.
- **Program delay D** (`--program-delay-ms`, default 3 frame periods, at most
  1500 ms; it replaces `--tracking-wait-ms` in this mode). Frame `t`'s slot is
  due at `t + D`. The selector decides it at `t + D - 4 ms`: the rendered frame
  if it has arrived, else the clean feed of the same frame. Every frame's slot
  is decided, in capture order; a rendered frame that comes later is dropped
  (`late CG`), never shown in another slot.
- **Output.** Program frames carry their due time as PTS (the `appsrc` reports
  no latency), `livesync` emits each one when due, and the output sink takes it
  on the clock (`processing-deadline` 0, so the pipeline adds no latency). The
  output therefore follows the input frame for frame at a constant D; in
  simulation there is no separate output clock. Genlock is phase 3.

## Frames and memory

- `chr-sdi-in`: `unixfdsink` offers `GstShmAllocator`, so the converter writes
  every frame straight into a memfd and nothing is copied to cross the socket
  (`min-memory-size=-1` refuses any other memory). The converter's pool has no
  upper bound and never stalls the capture; frames come back when the core
  releases them. The leaky queue in front of the socket (4 frames) drops the
  oldest when the core does not read.
- The core's CG branch gives the **same memfd** to the tracking worker and the
  render worker and releases it once both are done; the render worker's UYVY
  output goes to the output without a copy. The clean branch converts each
  frame once (RGBA to UYVY at the output size). The program tap is the one copy
  of the output, as before.
- Everything is bounded: the selector and the join hold at most D's frames
  plus 8; the tracking worker holds at most `in_flight_limit(D)`; the render
  worker 4 requests; the capture and hop queues 2 frames each and drop.

## Workers and relinking

Both workers keep their protocols and supervisors: VchaoFrame for tracking
(#148), render protocol v3 with the bound scene snapshot of every frame
(`scene.get` on the program scene, as the timer loop's submissions carry). The
transport's `Host::Bin` puts each worker session's two sockets into the core's
pipeline as bins `hop-<n>`, added when the supervisor starts the worker and
removed when it kills or replaces it; so a crashed, hung or stopped worker is
replaced and its hop **relinked into the running pipeline** without restarting
chr-server. Meanwhile its frames' slots get clean feed. chr-server's sockets
never wait for a connection (vchao's teardown deadlock): the tracking hop never
did, and the render worker's `hello` now goes out once the worker has connected.
Errors of `hop-*` and `capture-*` bins are logged; their owners recover. Any
other pipeline error stops chr-server.

The capture is a bin too (`capture-<n>`): when `chr-sdi-in` goes away its stream
ends, the core removes the bin and connects again every 500 ms; the output keeps
its cadence (`livesync` repeats the last frame), and a new capture session
numbers its frames after the old ones.

## Rates (phase 1)

Rates are integers in phase 1. `chr-sdi-in` refuses a file whose
`r_frame_rate` is not `N/1` (fractional rates are phase 2), and the core's
capture caps are the program camera's extent and `--fps`, which must be the
input's: other caps fail negotiation, the capture is retried and the output
repeats. Frame durations are `floor(1e9 / fps)` ns, as the VchaoFrame contract
has them. Runtime rate changes, fractional rates and the follow/fixed output
policy are phase 2.

## Run

From the repository root, in `nix develop .#media` (FFmpeg for `--video`;
`--test-pattern WxH fps` needs only the default shell):

```sh
ffmpeg -f lavfi -i testsrc2=size=1920x1080:rate=50:duration=30 \
  -vf setparams=color_primaries=bt709:color_trc=bt709:colorspace=bt709:range=tv,setsar=1 \
  -c:v libx264 -pix_fmt yuv420p output/gst-core/clip50.mp4
target/linux/release/chr-sdi-in --video output/gst-core/clip50.mp4 &
target/linux/release/chr-server --pipeline gst --description <description.json> \
  --size 1920x1080 --fps 50 --tracking-worker \
  --tracking-worker-arg --manifest --tracking-worker-arg <calibrated-video-v1.json> \
  --program-delay-ms 100
```

Both default to `$XDG_RUNTIME_DIR/chr/capture.sock` (`--socket`,
`--capture-socket`). chr-server logs the output every 10 s: slots, CG,
clean feed, repeats, skipped frames, interval and capture-to-output delay
(min/mean/max and standard deviation), the tap's slot − frame, and the
selector's decisions. `chr-monitor` reads the program tap unchanged;
`cargo run --release -p chr-shm --example tap_stats -- <tap> <s> <fps>` reports
the same cadence for either pipeline from the tap.
`CHR_WORKER_TEST_SOLID=<luma>` makes `chr-renderer-cli worker` answer with solid
frames without a GPU, for timing and fault tests.

## Measurements

Phase 1 runs on the development host (RTX 3090, the live stack running beside
them), 1920x1080 `testsrc2` clips (H.264, BT.709) at 25, 30 and 50 fps,
`--program-delay-ms 100`, the `chr-tracking-sim-cli` replay and, unless noted,
the render worker answering with solid frames (`CHR_WORKER_TEST_SOLID`, no GPU).
Each fault run lasts 95 s: `kill -9` of the render worker at 25 s, `SIGSTOP`
of the render worker at 40 s (replaced after the 2 s hang limit), `kill -9` of
the tracking worker at 55 s, `SIGSTOP` of the tracking worker at 70 s (replaced
once it holds its in-flight limit). "Output" is chr-server's own measurement at
the output sink; "tap" is `tap_stats` reading the program tap, the same tool
for both pipelines.

| Run | Output slots (CG / clean / repeats / skipped) | Interval mean, sd, min–max (output) | Capture → output delay min / mean / max | Tap: repeats, skipped, slot − frame | Slots without CG per fault (render kill / stop, tracking kill / stop) |
| --- | --- | --- | --- | --- | --- |
| core 25 fps | 2373 (2288 / 85 / 0 / 0) | 40.000, 0.142, 39.12–40.46 ms | 100.04 / 100.24 / 100.98 ms | 0, 0, constant | 6 / 57 / 6 / 16 |
| core 30 fps | 2847 (2743 / 104 / 0 / 0) | 33.333, 0.168, 32.51–34.27 ms | 100.05 / 100.26 / 101.05 ms | 0, 0, constant | 8 / 69 / 8 / 19 |
| core 50 fps | 4745 (4573 / 172 / 0 / 0) | 20.000, 0.166, 17.78–22.31 ms | 100.05 / 100.26 / 102.60 ms | 0, 0, constant | 13 / 114 / 13 / 32 |
| core 50 fps, GPU render worker | 4626 (4411 / 215 / 0 / 0) | 20.000, 0.162, 19.40–20.70 ms | 100.04 / 100.25 / 100.80 ms | 0, 0, constant | 28 / 128 / 15 / 33 |
| timer loop 50 fps, same faults | – | tap: 20.000, 0.169, 17.00–22.82 ms | – | 53, 54, 8 values (−39 … −32) | 16 / 115 / 19 / 38 |
| timer loop 25 / 30 / 50 fps, no faults, 30 s | – | tap sd 0.141 / 0.124 / 0.104 ms | – | 0 / 1 / 0 repeats, 1 / 2 / 2 skipped, 2–3 values | – |

- **Cadence:** no slot of the core's output was more than a quarter period off
  in any run. Without faults the output stayed within ±0.6 ms of the period
  (two 40 s runs at 50 fps), with faults within ±2.3 ms. The core's tap reader
  sees sd 0.22–0.28 ms, the timer loop's 0.10–0.14 ms: the core's tap is written
  one thread hop after the output, and its output follows the input's
  `clocksync` instead of a timer of its own.
- **Delay D:** every frame, CG or clean, left 100.04–102.6 ms after its
  capture; the tap's slot − frame never changed. The timer loop's output clock
  runs free of the input: its slot − frame drifted by one or two slots in 30 s
  without faults and over 8 values with them, which shows as repeats and
  skipped frames.
- **Faults:** clean feed only for the fault's duration, then CG again after the
  worker's replacement was relinked; no repeat, no skipped frame. A stopped
  render worker costs the 2 s hang limit (`--worker-hang-ms`) plus its restart;
  the GPU worker's restart adds Vulkan initialization (28 vs 13 slots).
- **The real render worker** composited 1080p50 within the 100 ms delay with
  the live stack on the same GPU (4411 CG slots, 1 late); #146's slow uploads
  did not show in these runs.

CPU (one core = 100 %) and RSS over 10 s of steady output, without faults:

| Process | 25 fps | 30 fps | 50 fps |
| --- | --- | --- | --- |
| chr-server, core | 31.5 %, 75 MiB | 36.0 %, 75 MiB | 55.5 %, 83 MiB |
| chr-server, timer loop | 10.4 %, 183 MiB | 11.3 %, 179 MiB | 18.9 %, 206 MiB |
| chr-sdi-in (+ FFmpeg decoding) | 27.0 % (+66 %), 43 MiB | 32.1 % (+77 %), 43 MiB | 50.2 % (+119 %), 47 MiB |
| chr-sdi-sim (+ FFmpeg decoding) | 30.1 % (+47 %), 117 MiB | 36.3 % (+52 %), 117 MiB | 57.7 % (+84 %), 118 MiB |
| Render worker: solid / GPU | 2.4 %, 33 MiB | 3.0 %, 33 MiB | 4.8 %, 41 MiB / 24.9 %, 223 MiB |
| Tracking stand-in | 0.6 %, 11 MiB | 0.7 %, 11 MiB | 1.1 %, 11 MiB |

The core spends its extra CPU on converting every frame's clean feed ahead of
its deadline (RGBA to UYVY at 1080p); the timer loop converts only the frames
that need it. Its RSS is lower: it maps no capture ring (64 MiB at 1080p).

**Soak:** 10 min 20 s at 1080p50 on the core without faults (solid render
worker): 30 995 slots, all CG, no repeat, no skipped frame, interval mean
20.000 ms (sd 0.167 ms, 17.10–23.17 ms, none more than a quarter period off),
capture → output delay 100.04 / 100.26 / 103.26 ms, the tap's slot − frame the
same for all 30 500 slots it read. chr-server's RSS went from 83 to 85 MiB and
its open descriptors stayed at 33; the 1-hour run of the acceptance criteria is
not done yet.

## Limitations

- Frames of this mode are not render submissions: `render.get` does not see
  them, and `render.get_worker_status` counts restarts but not clean feed.
  The core's log line and `sdi.get_status` (slots, repeats, last frame) report
  the output.
- Input loss holds the last frame (`livesync`); black or slate, and when the
  output returns, are not implemented.
- The decision margin is a fixed 4 ms; the clean conversion costs CPU
  (videoconvertscale, 2 threads per started 1080p).
- `stack.py` does not start this mode.
- Startup frames, before both workers are ready, are clean feed.

Next (#154): fractional rates and caps changes at runtime with a new tracking
stream (phase 2); `decklinkvideosrc mode=auto` and `decklinkvideosink` on the
card's genlocked clock, interlaced formats (phase 3, with #80); audio, ANC and
timecode with the same D (phase 4); removing the timer loop, the capture ring
and the old join (phase 5).
