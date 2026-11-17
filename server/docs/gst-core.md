# GStreamer core pipeline (#154, phases 1 and 2, experimental)

**Status:** `experimental` simulation prototype behind `chr-server --pipeline gst`
on Linux. The timer-paced loop (`--pipeline timer`, the input ring and the
simulated SDI port) stays the default. Phases 1 (the pipeline, fixed delay,
per-slot clean feed, relinking) and 2 (fractional rates, rates and formats from
the caps, runtime format changes, the `follow`/`fixed` output policy) of #154 are
done; DeckLink input/output with genlock (phase 3), audio, ancillary data and
timecode (phase 4) and the switch-over (phase 5) are not started.

`chr-server` owns one GStreamer pipeline from the input to the output. Its time
is the pipeline clock (the system clock in simulation; the output card's in
phase 3), not a software timer:

```text
chr-sdi-in (INPUT)                         chr-server (CORE, one pipeline "chr-core")
file: ffmpeg | fdsrc ! rawvideoparse ─┐
  ! videoconvert                      │    capture-<n>: unixfdsrc ! RGBA,<any extent and rate> ! [number] ! tee
pattern: videotestsrc ────────────────┤      ├─ queue leaky ! appsink ──► driver: tracking hop ─► camera
  ! RGBA + VchaoFrame wire caps       │      │                             (Ti + C + #150 lens) ─► render hop ─┐
  ! clocksync ! [stamp] ──────────────┤      └─ queue leaky ! videoconvertscale ! UYVY,<output extent> ! appsink ┤
  ! queue leaky ! unixfdsink ─unixfd──┘                                       selector (per slot, deadline) ◄──┘
                                           program: appsrc ! livesync ! tee ─┬─ queue ! appsink sync (OUTPUT)
                                           (caps: the input's or a fixed rate) └─ queue leaky ! appsink (program tap)
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
  (from 0 in each stream, continuous across loops of a file), the frame duration, and a
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
- **Waiting for the camera.** Frames go to the render worker in capture
  order, each as soon as its camera is in. A frame whose camera cannot come
  any more is given up at once (clean feed): its answer had none (an
  `unavailable` pose) or it was never sent to the tracking worker (not ready,
  restarting, at its in-flight limit). Only a frame whose answer is still due
  waits, at most until its slot is decided; the worker answers in order, so
  no later camera is in yet. Every rendered frame therefore gets the whole
  budget left after tracking (about D − 4 ms − the tracking latency), also
  the first one after a tracking dropout or restart.
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

## Rates and format changes (phase 2)

### The rate type

Every rate in chr is a `chr_common::rate::Rate`: `num/den` frames per second,
reduced, 1 to 120 fps, terms below 2^31 (GStreamer fractions). Broadcast rates
are exact: 24000/1001, 24, 25, 30000/1001, 30, 50, 60000/1001, 60.

- **Text** (`--fps`, `--output-rate fixed:<rate>`, `chr-sdi-in`, the monitor):
  an integer (`50`), a fraction (`30000/1001`), or the conventional names of the
  1000/1001 rates (`23.976`/`23.98`, `29.97`, `47.95`, `59.94`, `119.88`). Other
  decimals are refused: `29.97` names `30000/1001`, never `2997/100`.
- **JSON** (descriptions' and manifests' `fps`, `sdi.get_status`'s `rate`, the
  render worker's `hello`): an integer rate stays an integer, so every existing
  file and client reads unchanged; any other rate is the string `"30000/1001"`.
- **Time.** Frame `n` starts `n * den * 1e9 / num` ns after frame 0, rounded
  down (`Rate::frame_ns`): never a sum of rounded periods, so nothing drifts
  (29.97 for an hour: a summed period would be 72 µs early and keep going). A
  frame's nominal duration is that rounded down, `floor(den * 1e9 / num)`.
- **Mode names** carry a label: `1920x1080p59.94`, `1920x1080p23.98`.

Where it is used:

| Place | Form |
| --- | --- |
| `chr-server`, `chr-renderer-cli`, `chr-monitor` `--fps`; `chr-sdi-in --test-pattern`/`--then` | text |
| Descriptions' `camera.fps`, calibrated-video manifests' `fps` | JSON |
| `sdi.get_status`: `video_mode` (label), `rate`; TS mirror `Rate = number \| string` | JSON |
| Render worker protocol (**v4**): `hello.output.fps`; each `render` carries its frame's `time_base` | JSON |
| Program configuration `output_rate` (seconds per frame, `den/num`) | time base |
| Tracking (VchaoFrame): caps `framerate=num/den`, `duration_ns` = the rounded-down period, `vchao-replay-duration-ns` the same; PTS from the clock | unchanged contract |
| chr-shm input ring and program tap (layout **CHRSHM02**): `meta[3] = num << 32 \| den` | binary |
| Timer loop: SDI output slots and render deadlines at `epoch + Rate::frame_time(n)` | exact |

The VchaoFrame contract is unchanged: chr sends vchao no field of its own, and a
VchaoFrame's `duration_ns` was always the rounded-down period. The render
protocol went to version 4 and the shared-memory layout to `CHRSHM02` because
their wire changed; old and new processes refuse each other.

### Rate and format from the caps

The core's capture accepts RGBA of any extent and of 1–120 fps; it takes the
frame's format from the caps that come with it, never from `--fps`:

- **Deadlines** do not depend on the rate: frame `t`'s slot is due at `t + D` and
  decided at `t + D - 4 ms`, whatever the rate; D stays in ms. The selector
  holds a program delay's worth of slots at 120 fps (plus slack), so a faster
  input needs no reconfiguration.
- **Each slot carries its frame's rate**, from its branch's caps; the program
  frame's duration is that rate's period.
- **`livesync`'s rate** is the program `appsrc`'s caps: with the `follow` policy
  the selector sets new caps (UYVY, the output's extent, the new rate) right
  before the first program frame of a new rate, so `livesync`, the output sink
  and the tap all change at that frame.
- **The tracking hop** gets the frame's extent and rate in its caps; the render
  worker gets the frame's extent in its image description and its time base in
  each request.

`--fps` is the nominal rate only: the rate of the program at start, the
default program delay (3 periods), the program's animation rate, and the
description's or manifest's `fps`, which must equal it.

### A change of format at runtime

A new format is a new stream all the way: `chr-sdi-in` starts a new stream id
with `frame_id`s from 0 whenever its caps change (as vchao requires), and
renegotiates the socket's caps in the same session. The core then:

1. **Numbers on.** A probe before the capture `tee` gives every frame its
   program sequence from its stream id and `frame_id` (`capture::Streams`); a new
   stream numbers on from the highest sequence so far, so both branches agree.
   A new stream in another format is a format change: it is logged, counted
   (`sdi.get_status`'s `format_changes`) and starts a transition.
2. **Clean feed during the transition.** From the oldest undecided slot to the
   first CG slot of the new format, every slot without CG is counted as
   transition clean feed (`transition_clean_slots`); each still goes out at
   its due time, as its own frame.
3. **New tracking session.** The driver sees the first frame of the new format,
   kills the tracking worker (its frames in flight get no camera, so their
   slots are clean) and starts a new one with the new caps and a new stream id
   (not counted as a restart). With `chr-tracking-sim-cli` that takes a frame or
   two; with vchao's `pitch-analysis` it is a process start and model load
   (below).
4. **Output.** With `follow` the program's caps change at the first frame of the
   new rate: `livesync` and the simulated output sink take the new rate (the
   sink is an `appsink`; a DeckLink sink will need its mode switched, phase 3),
   and the tap's slots carry it. With `fixed` nothing changes downstream.
5. **chr-monitor follows** the tap without a restart: each slot carries its
   mode; on a new one the monitor restarts its encoder in that mode (the next
   frame is a keyframe; the preview size may change), and the media time of
   WebRTC and SRT runs on (`monitor.get_status`'s `mode_changes`). The tap is
   only re-created with a new size if the output's extent changed, which the
   core never does: the output keeps `--size`.
6. **D is unchanged:** deadlines do not depend on the rate, so every frame of
   either format leaves exactly D after its capture.

The render worker needs nothing: requests carry the frame's extent and a time
base. The core's requests time frames in nanoseconds of capture time
(`time_base` 1/1000000000), so animations run on across rate changes.

**Another extent** (for example 1080p to 720p) is followed the same way, but its
frames go out as clean feed, scaled to the output: the program's camera and
lens are defined at the description's native extent. They are counted as `not
rendered: other_extent` in the core's log; CG returns with the camera's extent.

### Output rate policy

`--output-rate follow|fixed:<rate>` (default `follow`):

- **`follow`** (default): the output runs at the input's rate and changes with
  it; every input frame is one output slot, and the capture-to-output delay is
  exactly D. A downstream that cannot change rate sees a change.
- **`fixed:<rate>`**: the output stays at `<rate>` whatever the input does. The
  program has its own grid of slots at `<rate>`, starting at the first frame's
  due time; each decided frame fills the grid slots from its due time until the
  next frame is due: the latest frame due is shown, as GStreamer's `videorate`
  does. Slower input repeats frames (25 to 50: each twice; 29.97 to 50: 3:2-like
  alternation), faster input drops them (50 to 25: every other one); both are
  counted (`rate conversion` repeats and drops, and `duplicates` on the
  output). The trade-offs, by design: the capture-to-output delay varies
  between D and D plus one input period, motion judders when the rates are not
  multiples, and a dropped frame's CG is never seen. `videorate` itself is not
  used: it outputs a frame only when the next one arrives, which costs an input
  period of latency (every slot late, or that period taken from the render
  budget). Interlaced conversion (deinterlace/interlace) is phase 3.

### vchao: a new stream costs a model load

vchao takes a caps change within a stream as fatal, so each format change is a
new `pitch-analysis` process (no interface change is needed or made). In the
live stack's log `pitch-analysis` takes about 10 s from its start to listening
on its output (01:55:12 → 01:55:22, 1212x654p30: map and model load,
`estimator-ready`), and the first pose follows about a second later: about 11 s,
some 330 slots at 30 fps or 550 at 50 fps, of clean feed per format change. A
new stream without a model reload (keep the model, reset the stream) is the
request to vchao; until then a format change is a short CG outage.

## Run

From the repository root, in `nix develop .#media` (FFmpeg for `--video`;
`--test-pattern WxH rate` needs only the default shell):

```sh
ffmpeg -f lavfi -i testsrc2=size=1920x1080:rate=60000/1001:duration=30 \
  -vf setparams=color_primaries=bt709:color_trc=bt709:colorspace=bt709:range=tv,setsar=1 \
  -c:v libx264 -pix_fmt yuv420p output/gst-core/clip5994.mp4
target/linux/release/chr-sdi-in --video output/gst-core/clip5994.mp4 &
target/linux/release/chr-server --pipeline gst --description <description.json> \
  --size 1920x1080 --fps 59.94 --tracking-worker \
  --tracking-worker-arg --manifest --tracking-worker-arg <calibrated-video-v1.json> \
  --program-delay-ms 100 [--output-rate fixed:50]
```

A signal whose format changes: the test pattern switches after each `--then`
interval, and `--cycle` starts the list over:

```sh
target/linux/release/chr-sdi-in --test-pattern 1920x1080 25 \
  --then 30 1920x1080 50 --then 30 1920x1080 29.97
```

Both default to `$XDG_RUNTIME_DIR/chr/capture.sock` (`--socket`,
`--capture-socket`). chr-server logs the output every 10 s: slots, CG,
clean feed, repeats, duplicates (fixed rate), skipped frames, the interval at
each rate and the capture-to-output delay (min/mean/max and standard
deviation), the tap's slot − frame, the selector's decisions with format
changes and their clean slots, the frames joined with a camera and given up
without one, and the render budget: the time left until each rendered frame's
slot is decided when it was submitted. `chr-monitor` reads the program tap and
follows its mode; `cargo run --release -p chr-shm --example tap_stats -- <tap>
<s>` reports the same cadence for either pipeline from the tap, per rate.
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
- **Tracking dropouts** (a later fix, 60 s runs at 50 fps, render worker
  answering after 15 ms): before it, a frame without a camera held back the
  frames behind it until its own deadline, so the first rendered frame after
  it got about one period of budget and the burst behind it ran out of render
  credits. With every 25th frame `unavailable` (`--unavailable-every 25`) the
  tap showed 211 slots without CG of 2500 instead of the 100 unavailable
  frames, and the minimum render budget was 17.4 ms; now it shows exactly 100
  and the budget stays 90.9–95.3 ms. After `kill -9` and `SIGSTOP` of the
  tracking worker the first frame of the new worker got 18.9 and 19.2 ms
  (one extra clean slot each); now it gets 92.4 and 92.2 ms, and the clean
  slots are exactly the 13 and 32 frames without a camera.
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

### Phase 2: fractional rates and format changes

Same host and setup as phase 1 (live stack beside it, `--program-delay-ms
100`, `chr-tracking-sim-cli`, solid render worker unless noted), 1920x1080
`testsrc2` clips at each rate, 60 s per run. "Output" is chr-server's
measurement at the output sink (interval mean to 0.1 µs), "tap" is `tap_stats`
on the program tap (polled every 200 µs; its mean is over the whole run).

| Run | Output slots (CG / clean / repeats / skipped) | Interval mean, sd, min–max (output) | Exact period | Tap: interval mean, sd, slot − frame | Capture → output delay min / mean / max |
| --- | --- | --- | --- | --- | --- |
| 25 fps | 1498 (1498 / 0 / 0 / 0) | 40.0008, 0.143, 39.58–41.21 ms | 40 ms | 40.000197 ms, 0.187, constant | 100.05 / 100.24 / 101.29 ms |
| 29.97 (30000/1001) | 1795 (1795 / 0 / 0 / 0) | 33.367, 0.172, 30.94–35.63 ms | 33.366667 ms | 33.366827 ms, 0.291, constant | 100.05 / 100.26 / 102.53 ms |
| 50 fps | 2995 (2995 / 0 / 0 / 0) | 20.0002, 0.170, 16.60–23.40 ms | 20 ms | 19.999832 ms, 0.301, constant | 100.05 / 100.25 / 103.67 ms |
| 59.94 (60000/1001) | 3590 (3590 / 0 / 0 / 0) | 16.6836, 0.152, 15.15–18.21 ms | 16.683333 ms | 16.683358 ms, 0.222, constant | 100.04 / 100.24 / 101.83 ms |
| 59.94, GPU render worker | 3282 (3261 / 21 / 0 / 0) | 16.6833, 0.127, 16.19–17.24 ms | 16.683333 ms | 16.683332 ms, 0.255, constant | 100.05 / 100.21 / 100.65 ms |
| Timer loop 29.97, 45 s | – | – | 33.366667 ms | 33.366656 ms, 0.132; 1 repeat, 4 skipped, 4 values | – |

- **Cadence:** every rate's mean interval is its exact period to within a
  microsecond (output) or a fraction of one (tap), and no slot was a quarter
  period off. The 21 clean slots of the GPU run are its start, before the
  worker under nixGL was ready. The timer loop runs 29.97 on its own exact
  clock (slots at `n × 1001/30000` s), with the free-running drift against its
  input that phase 1 showed at integer rates.
- **CPU** (one core = 100 %): chr-server 29.5 / 36.3 / 54.4 / 64.2 % at
  25 / 29.97 / 50 / 59.94 fps (phase 1 at 50: 55.5 %), chr-sdi-in plus
  FFmpeg decoding about the same again; the test pattern input takes 6 %.
  chr-server's RSS 75–91 MiB.

**Format changes at runtime.** `chr-sdi-in --test-pattern 1920x1080 25 --then
30 1920x1080 50 --then 30 1920x1080 30000/1001`, 100 s, with `chr-monitor`
(x264) and an SRT caller (FFmpeg) for 90 s:

| Run | Output slots (CG / clean / repeats / duplicates) | Interval mean, sd per rate (output) | Delay min / mean / max | Format changes, slots without CG in them | chr-monitor, SRT caller |
| --- | --- | --- | --- | --- | --- |
| `follow` | 3445 (3443 / 2 / 0 / 0) | 25: 39.9999 ms, 0.132; 50: 19.9998 ms, 0.244; 29.97: 33.3665 ms, 0.169 | 100.05 / 100.25 / 102.79 ms | 2, 1 + 1 | followed both changes without a restart; 3190 frames in 90.06 s, PTS steps of 40 ms ×554, 20 ms ×1498, 33.4 ms ×1137, largest 40 ms |
| `fixed:50` | 4994 (3442 / 2 / 0 / 1550) | 50: 20.0000 ms, 0.232 (the only rate) | 100.06 / 103.73 / 120.27 ms | 2, 1 + 1 | no change to follow; 4501 frames in 90.00 s, every step 20 ms |

- **No restart:** chr-server kept its PID; the tracking worker started a new
  stream at each change (not counted as a restart); the tap's slot − frame
  stayed one value through both changes with `follow`.
- **Transition:** with the stand-in the new tracking stream answers within a
  frame, so each change cost one slot of clean feed (the frame in flight to
  the killed worker); the selector, `sdi.get_status` and the tap agree.
- **D:** constant across the changes with `follow`. With `fixed:50` the delay
  spans D to D plus one input period (40 ms at 25 fps) by design; the 1550
  duplicates are the 25 and 29.97 fps frames shown twice (or alternately once
  and twice), and nothing was repeated by `livesync` or dropped.

**Soak:** 10 min 15 s at 1080p59.94 from the clip, no faults: 36 857 slots,
all CG, no repeat, no skipped frame, interval mean 16.6834 ms (exact
16.683333 ms; sd 0.156 ms, 15.86–17.50 ms, none a quarter period off), capture
→ output delay 100.04 / 100.24 / 101.12 ms; the tap read 36 263 slots at a mean
of 16.683339 ms with one slot − frame value. chr-server's RSS stayed at 87 MiB
and its descriptors at 34. A first attempt on a busier host lost 37 s to the
*input*: FFmpeg's 59.94 fps H.264 decode in chr-sdi-in fell behind real time
(525–583 frames per 10 s instead of 599), its frames reached the core after
their slots were due, and the output repeated (2211 slots) until the input
caught up; D was unchanged before and after. A real input card cannot fall
behind that way; the test pattern input costs 6 % instead of about 120 %.

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
- A format change restarts the tracking worker: with vchao about 11 s without
  CG until vchao can start a new stream without reloading its model.
- Frames of an extent other than the program camera's are clean feed; the
  output keeps `--size` (the clean feed is scaled), so the tap is never
  re-created. Only the test pattern switches formats in simulation; the file
  input plays one clip.
- With `fixed`, the output's delay varies by up to an input period by design;
  interlaced conversion is phase 3.

Next (#154): `decklinkvideosrc mode=auto` and `decklinkvideosink` on the card's
genlocked clock, its mode switched on a format change, interlaced formats
(phase 3, with #80); audio, ANC and timecode with the same D (phase 4);
removing the timer loop, the capture ring and the old join (phase 5).
