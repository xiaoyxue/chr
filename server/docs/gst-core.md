# GStreamer core pipeline (#154)

**Status:** `implemented` on Linux in simulation: the only live path of every
tracked program (`chr-server --tracked-scene` or `--description`). Done:

- phase 1: the pipeline, fixed delay, per-slot clean feed, relinking;
- phase 2: fractional rates, rates and formats from the caps, runtime format
  changes, the `follow`/`fixed` output policy;
- phase 3 (what needs no card): interlaced formats, input loss
  (hold/black/slate), and the DeckLink input/output code paths with the card's
  clock as the pipeline clock, **built and tested without a card but not
  verified on hardware** (the development host has none);
- phase 4 (simulated): audio with the same delay D, timecode and captions
  carried across the capture hop and output with their frame;
- phase 5: the switch-over. The core is the default and only path of tracked
  programs; the timer-paced tracked path, the shared-memory capture ring and
  its writer `chr-sdi-sim` are removed, `chr-sdi-in` is the one capture
  process, `stack.py` starts it, and the core reports its telemetry
  ([Phase 5](#phase-5-switch-over)).

Open: verification on a DeckLink card with house sync (#80), the 1-hour soak
of the acceptance criteria, and a new vchao stream without a model reload.

`chr-server` owns one GStreamer pipeline from the input to the output. Its time
is the pipeline clock (the system clock in simulation; with `--output decklink`
the output card's), not a software timer:

```text
chr-sdi-in (INPUT)                         chr-server (CORE, one pipeline "chr-core")
file: ffmpeg | fdsrc ! rawvideoparse ─┐
  ! videoconvert                      │    capture-<n>: unixfdsrc ! RGBA,<any extent, rate, scan> ! [number] ! tee
pattern: videotestsrc ────────────────┤      ├─ queue leaky ! deinterlace (first field) ! appsink ──► driver:
DeckLink: decklinkvideosrc            │      │     tracking hop ─► camera (Ti + C + #150 lens) ─► render hop ─┐
  mode=auto ! videoconvert ───────────┤      └─ queue leaky ! videoconvertscale ! UYVY,<output extent> ! appsink ┤
  ! RGBA ! clocksync ! [stamp:        │                                       selector (per slot, deadline) ◄──┘
  VchaoFrame, timecode, captions]     │                                       + input-loss fill, timecode, captions
  ! queue leaky ! unixfdsink ─unixfd──┘    program: appsrc ! livesync ! tee ─┬─ queue ! appsink sync | decklinkvideosink
audio: ffmpeg | rawaudioparse,        │    (caps: the input's rate and scan, └─ queue leaky ! appsink (program tap)
  audiotestsrc or decklinkaudiosrc    │     or a fixed rate)
  ! S32LE 48 kHz ! clocksync ! [stamp]│    audio-<n>: unixfdsrc ! appsink ─► [capture + D] ─► appsrc ! audioconvert
  ! queue leaky ! unixfdsink ─unixfd──┘                                       ! appsink sync | decklinkaudiosink
  (<socket>.audio)                         hop-<n>: appsrc ! unixfdsink / unixfdsrc ! appsink, per worker session
```

## Elements and the Nix shells

| Element | Package | In the shells | Use |
| --- | --- | --- | --- |
| `unixfdsrc`, `unixfdsink`, `rawvideoparse` | gst-plugins-bad 1.28.8 | yes | capture and worker hops; file input |
| `decklinkvideosrc`, `decklinkvideosink`, `decklinkaudiosrc/sink` | gst-plugins-bad 1.28.8 | yes (no card on the dev host) | DeckLink input and output, phases 3 and 4 |
| `deinterlace` | gst-plugins-good 1.28.8 | **added** | the CG branch's progressive frames of an interlaced input |
| `interlace` | gst-plugins-bad 1.28.8 | yes | evaluated, not needed (below) |
| `audiotestsrc`, `audioconvert`, `rawaudioparse` | gst-plugins-base, -bad | yes | audio of the test pattern and files |
| `clocksync`, `queue`, `tee` | GStreamer core | yes | |
| `videoconvert(scale)`, `videotestsrc`, `appsrc`, `appsink` | gst-plugins-base | yes | |
| `livesync`, `fallbackswitch` | gst-plugins-rs 0.15.3 | **added** | `livesync`: rate lock; `fallbackswitch`: evaluated, not used |

gst-plugins-rs is in the pinned nixpkgs and its binary cache. The flake copies
just `libgstlivesync.so` and `libgstfallbackswitch.so` into a small derivation
(`chr-gst-livesync`) on `GST_PLUGIN_SYSTEM_PATH_1_0` of both shells, the
checks and the wrapped `chr-server` package programs: the whole package
(about 55 plugins, a 1 GB closure with GTK 4 and more) would load on every
registry scan. Phase 3 adds gst-plugins-good's `libgstdeinterlace.so` to the
same derivation the same way (it needs only GStreamer and ORC); `yadif` is not
built in nixpkgs' gst-plugins-bad (it is GPL). `flake.lock` is unchanged.

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
  1500 ms; it replaced `--tracking-wait-ms`, which now fails). Frame `t`'s slot is
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
  the first one after a tracking dropout or restart. A joined frame that finds
  the render worker's four credits taken waits for one, in capture order,
  until its slot is decided (phase 5): vchao answers in batches of eight.
- **Output.** Program frames carry their due time as PTS (the `appsrc` reports
  no latency), `livesync` emits each one when due, and the output sink takes it
  on the clock (`processing-deadline` 0, so the pipeline adds no latency). The
  output therefore follows the input frame for frame at a constant D; in
  simulation there is no separate output clock. With a DeckLink output the
  card's clock is the pipeline clock (phase 3, below).

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
(#148), render protocol v4 with the bound scene snapshot of every frame
(`scene.get` on the program scene, as a generated program's submissions carry) and
its `time_base` (the core's requests count nanoseconds of capture time). The
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
its cadence (filled as `--input-loss` says, phase 3), and a new capture session
numbers its frames after the old ones. The audio session (`audio-<n>`) is
connected, lost and reconnected the same way, on its own.

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
| chr-shm program tap (layout **CHRSHM02**; and the capture ring until phase 5): `meta[3] = num << 32 \| den` | binary |
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

`--fps` is the nominal rate only (a frame rate: `25` for a 1080i50 program): the rate of the program at start, the
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
   first CG slot of the new format, every slot sent as clean feed is counted
   as transition clean feed (`transition_clean_slots`); each still goes out at
   its due time, as its own frame. A slot with no frame at all (the output
   repeats) is not counted.
3. **New tracking session.** The driver sees the first frame of the new format,
   kills the tracking worker (its frames in flight get no camera, so their
   slots are clean) and starts a new one with the new caps and a new stream id
   (not counted as a restart). With `chr-tracking-sim-cli` that takes a frame or
   two; with vchao's `pitch-analysis` it is a process start and model load
   (below).
4. **Output.** With `follow` the program's caps change at the first frame of the
   new rate: `livesync` and the simulated output sink take the new rate (the
   sink is an `appsink`; a DeckLink sink cannot follow, so a card output needs
   `fixed`, phase 3), and the tap's slots carry it; `sdi.get_status` reports the new mode from
   that slot on, D after the capture's change. With `fixed` nothing changes
   downstream.
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
- **`fixed:<rate>`**: the output stays at `<rate>` whatever the input does
  (`fixed:50i`, a field rate with an `i`, is 25 interlaced frames: phase 3). The
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
  budget). The fixed output is progressive: an interlaced input's clean feed
  is deinterlaced like its CG frames (phase 3).

### vchao: a new stream costs a model load

vchao takes a caps change within a stream as fatal, so each format change is a
new `pitch-analysis` process (no interface change is needed or made). In the
live stack's log `pitch-analysis` takes about 10 s from its start to listening
on its output (01:55:12 → 01:55:22, 1212x654p30: map and model load,
`estimator-ready`), and the first pose follows about a second later: about 11 s,
some 330 slots at 30 fps or 550 at 50 fps, of clean feed per format change. A
new stream without a model reload (keep the model, reset the stream) is the
request to vchao; until then a format change is a short CG outage.

## Phase 3: DeckLink, interlaced, input loss

### DeckLink input and output (not verified on hardware)

The development host has no DeckLink card (`lspci` shows none). The code paths
are built, construct the elements GStreamer 1.28.8's `decklink` plugin
provides, map formats to the cards' modes and fail clearly without a card;
tests cover argument parsing, the mode mapping, that the program's caps
negotiate with the sink's pad template in every mapped mode, the priming of
both sinks (with a simulated sink) and "no DeckLink device". Capture,
playout, genlock, the card clock and its start, RP 188 timecode, captions and
embedded audio on a real card are **not verified**.

- **Input** (`chr-sdi-in --source decklink --device N [--mode auto|<mode>]
  [--connection sdi|hdmi|optical-sdi|auto]`, `gst_core::decklink`):
  `decklinkvideosrc` with `mode=auto` and `video-format=auto` takes the format
  (extent, rate, scan) from the signal; a fixed mode uses `8bit-yuv`.
  `drop-no-signal-frames=true` makes a lost signal an input loss (no frames,
  below) instead of black frames, `timecode-format=rp188any` and
  `output-cc=true` attach `GstVideoTimeCodeMeta` and `GstVideoCaptionMeta`,
  which the input carries to the core (phase 4). `decklinkaudiosrc` of the same
  device gives 2, 8 or 16 channels (`--audio-channels`). A changed signal
  renegotiates caps: the input starts a new stream, exactly as the test
  pattern's `--then` switch, so the core follows it (phase 2). The card is not
  paced by `clocksync` (it delivers in real time and timestamps on its own).
- **Output** (`chr-server --output decklink [--device N]
  --output-rate fixed:<rate>`): `decklinkvideosink` replaces the simulated
  `appsink` after `livesync`, `decklinkaudiosink` the audio `appsink`. Its mode
  is the fixed output format's (table below), for the whole run:
  `decklinkvideosink` keeps its mode once it plays (new caps of the same size
  are "nothing relevant", others "reconfiguration not supported"), so a card
  output **requires `--output-rate fixed:<rate>`** (`fixed:50i` for 1080i50,
  `fixed:59.94` for 1080p59.94) and refuses `follow` at start; the input's
  other rates are converted (phase 2's grid; an interlaced fixed output keeps
  an interlaced input's fields, a progressive one deinterlaces it). Switching
  the card's mode on the fly (the sink to READY, a new mode, primed again) is
  left for a test with a card. `cc-line=9` and `timecode-format=rp188any`
  output the slot's captions and timecode. Without a card `Core::start` fails
  with `no DeckLink device 0 (0 found): install Blackmagic Desktop Video …`.
- **Start.** A DeckLink clock stands still until the card plays, and the card
  plays once its video sink, and with audio its audio sink, got caps; the
  video sink gets them with the first frame `livesync` passes on, and the
  selector's deadlines wait on that clock. So the core primes both right
  after the pipeline plays (`select::prime`): a black program frame at running
  time 0, due at once, and 10 ms of silence at 0 in the card's fixed audio
  format (`--audio-channels 2|8|16`, the input's audio converted to it), so the
  card starts whether or not the input has audio. `livesync` repeats the black
  frame until the first program frame (it counts as a fill, then repeats).
- **Clock and genlock.** With a DeckLink output, the sink's clock (the card's,
  locked to its reference input when one is connected) is the pipeline clock;
  in simulation the system clock stays. Every capture time, a frame's
  `source_ns` or an audio buffer's, is on the same-host system clock; the core
  maps it to its pipeline clock through the two clocks' current offset
  (`Timeline::of_system`; identical on the system clock). The two clocks drift
  apart by parts per million: the offset is read at every frame, so D holds in
  the card's time. The output's statistics count a card's slots as they are
  handed to the sink, ahead of air.

| Format | `Rate` (frames/s) | Name | DeckLink mode |
| --- | --- | --- | --- |
| 1920x1080 interlaced, top first | 25 | `1920x1080i50` | `1080i50` |
| 1920x1080 interlaced, top first | 30000/1001 | `1920x1080i59.94` | `1080i5994` |
| 1920x1080 interlaced, top first | 30 | `1920x1080i60` | `1080i60` |
| 1920x1080 progressive | 23.98, 24, 25, 29.97, 30, 50, 59.94, 60 | `1920x1080p50` … | `1080p2398` … `1080p60` |
| 1280x720 progressive | 50, 59.94, 60 | `1280x720p50` … | `720p50`, `720p5994`, `720p60` |
| 3840x2160 progressive | 23.98 … 60 | `3840x2160p50` … | `2160p2398` … `2160p60` |
Any other format is refused with the list of modes. SD (`ntsc`, `pal`) is not
mapped: its caps have non-square pixels (10/11, 12/11) and BT.601, which the
core's square-pixel BT.709 frames do not negotiate with; an SD signal on a card
in `mode=auto` does not negotiate either. Host setup and the
checklist: `docs/operations.md`.

### Interlaced formats

**Rates are frame rates.** An interlaced frame carries both fields, woven line
by line; `Rate` counts frames, as GStreamer's caps do: 1080i50 has
`framerate=25/1`, `interlace-mode=interleaved`, `field-order=top-field-first`
(25 frames, 50 fields per second). Every rule of phase 2 holds unchanged: a
captured frame owns one slot, due its capture time plus D; deadlines, D and the
selector count frames. Only names use the field rate, as broadcast does:
`1920x1080i50`, `1920x1080i59.94` (`sdi.get_status`'s `video_mode`), and
`chr-sdi-in`'s rates take an `i` suffix with the field rate (`50i` is 25
interlaced frames per second). The scan is part of the format (`sdi::Format`):
1080p25 and 1080i50 are different formats with the same rate, and a change
between them is a format change (a new stream, phase 2).

- **Input.** A file's `field_order` (FFprobe; `tt`/`bt` top first, `bb`/`tb`
  bottom first, by display order) makes it interlaced; FFmpeg converts each
  field's 4:2:0 chroma on its own (`scale=interl=1`) and `rawvideoparse
  interlaced=true top-field-first=…` sets the caps. The test pattern takes
  interlaced caps (`videotestsrc` draws both fields of one instant); a card
  sets them from the signal.
- **Tracking and rendering get progressive frames.** The CG branch's
  `deinterlace mode=auto method=linear fields=<first field>` turns each
  interlaced frame into one progressive frame of its first field
  (top or bottom, from the caps), line-interpolated, at the frame's own time and
  rate: one CG frame per slot, rendered for the instant the frame starts.
  `linear` uses no other field, so it **adds no latency** and holds no
  history across a format change or a loss; progressive frames pass through
  untouched (the same memfd). The motion-adaptive methods (`greedyh`,
  `tomsmocomp`) would keep more vertical detail for tracking at the cost of up
  to two fields of history in the CG branch (40 ms at 1080i50, out of D); not
  needed for the stand-in and solid workers measured here, and a change of
  `method` only. The deinterlaced frame is new memory: the CG `appsink`
  proposes the shared-memory allocator, so `deinterlace` writes it into a
  memfd that the tracking and render sockets send without a copy. It does not
  keep buffer offsets, so a probe carries each frame's sequence over by its
  timestamp. The tracking hop's caps stay progressive (VchaoFrame unchanged).
- **The clean branch keeps the interlaced picture**: `videoconvertscale` to
  UYVY at the output's extent keeps both fields in their order (and its caps),
  so a slot without CG shows the input as it came.
- **The output is interlaced.** The program caps follow the input's scan (with
  `follow`): `livesync`, the output sink and the tap take `interlace-mode` and
  `field-order` from them, and a DeckLink sink its `1080i50` mode. A CG frame
  of an interlaced program is the composite of the first field's picture, sent
  as both fields of the frame: exactly what `interlace field-pattern=2:2`
  makes of a progressive frame (a test checks the bytes are identical), so no
  element is inserted, and none has to be removed when the input switches
  between progressive and interlaced. The CG slots of an interlaced program
  therefore move at the frame rate (25 pictures per second, PsF-like); clean
  slots keep the input's 50 fields. Rendering both fields (two renders per
  frame at the field rate, woven 1:1) is the upgrade if CG motion must be
  field-rate.
- **`fixed:<rate>`** output is progressive: an interlaced input's clean feed
  is deinterlaced the same way before it is converted. **`fixed:<field
  rate>i`** (`fixed:50i`) is interlaced: an interlaced input's clean feed keeps
  its fields, a progressive input's pictures go out as both fields of a frame.
- The monitor shows interlaced program frames woven, as they are (the tap does
  not carry the scan).

### Input loss

`--input-loss hold|black|slate[:<png>]` (default `hold`) says what fills the
slots while the input delivers no frames:

- `hold`: the last program frame again; `black`; `slate`: SMPTE colour bars, or
  `slate:<png>` a PNG scaled to the output (letterboxed). The still frame is
  made once at start, at the output's extent.
- **Detection.** The selector keeps the cadence itself: when no frame was
  captured for one and a half periods and no frame is pending for the
  program's next slot, that slot is filled, a period after the previous
  program frame, and every slot after it until a captured frame is due again.
  A single missing frame (the next one pending) is not a loss: `livesync`
  repeats as before. A capture process that stops (`SIGSTOP`), dies, or loses
  its signal (`--then <s> loss`, a card's no-signal) all look the same.
- **Recovery does not move D.** The first frame after the loss leaves at its
  own capture time plus D, as every frame does; the slots until it are
  filled, and the last fill lasts until it is due, so the program stays
  contiguous: a restarted input in another phase shows as one interval of up
  to one and a half periods (a genlocked input keeps its phase). A
  stopped input's backlog is no input: `chr-sdi-in` drops frames released more
  than a period late (`stale frames dropped`), as a card never delivers a stale
  frame, and the core drops a slot decided after it was due.
- **Counted**: `sdi.get_status`'s `input_loss_slots` and `input_losses` (TS
  mirror), the output's `input-loss fills` and the selector's `input loss N
  slots in M losses` in the log. Fill slots carry no capture time; the tap
  keeps writing them.

## Phase 4: audio, timecode, captions

### Audio with the same delay

- **Path.** The input sends its audio over a second unixfd socket beside the
  frames' (`<capture socket>.audio`): 48 kHz, 32-bit samples (SDI audio is 24
  bits), 2/8/16 channels, buffers of about 10 ms. Its own socket keeps audio
  independent of the video's socket and queue (which drop frames when the core
  is behind) and reuses the transport the frames use: `unixfdsink` copies the
  small buffers into shared memory and the core maps them read-only. Each
  buffer carries its capture time (the system-clock time `clocksync` released
  it at, a card's capture time) in chr's capture meta.
- **Same delay, same clock.** The core maps each buffer's capture time to its
  running time exactly as a frame's (`Timeline::of_system`), adds D and hands it
  to the program's audio: `appsrc ! audioconvert ! <sink>`, the sink rendering
  each buffer on the pipeline clock when due (`appsink sync=true` in
  simulation, `decklinkaudiosink` on a card). A frame and the audio captured
  with it therefore leave together; lip sync is the input's.
  The parser's DTS is cleared: a sink syncs on the DTS when there is one.
- **Never dropped or blocked by tracking or render.** Audio does not pass the
  selector, the workers or `livesync`; worker crashes, stalls, format changes
  and slow renders cannot touch it. The program `appsrc` holds D plus a
  second, leaky. A lost input is silence; its audio session reconnects on its
  own.
- **Sources.** A file's first audio stream (a second FFmpeg process decoding
  only the audio, paced by its own reader, so neither stream can stall the
  other; looped files keep A/V aligned if their streams last equally long,
  FFmpeg filling gaps with silence), the test pattern's `audiotestsrc
  wave=ticks` (a tick each second), a card's `decklinkaudiosrc`.
- **Telemetry.** `sdi.get_status.audio`: sample rate, channels, samples out,
  the mean capture-to-output delay and `offset_us` (audio delay minus video
  delay over the last report; TS mirror `AudioStatus`), and the log line's
  `audio: … audio - video … ms`.
- **Not done:** audio in chr-monitor's SRT transport stream (AAC). The tap
  carries video only; carrying audio needs a second ring beside the tap and an
  AAC branch in the monitor: future.

### Timecode

GStreamer 1.28's unixfd carries only metas that can serialize themselves;
`GstVideoTimeCodeMeta` cannot: a frame stamped by `timecodestamper` arrived
over `unixfdsrc` with no meta (probe in this phase). The capture hop is chr's
own, so `chr-sdi-in` carries the timecode in chr's capture meta
(`chr-capture-ancillary`, a `GstCustomMeta` with a JSON string like the
VchaoFrame meta; `gst_core::ancillary`), and the selector puts it back on the
program frame as `GstVideoTimeCodeMeta`, which `decklinkvideosink` outputs as
RP 188. The tracking hop rebuilds each frame from its memory with only the
VchaoFrame meta: vchao never sees chr's fields.

- **Source:** a card's RP 188 (any); else a file's `timecode` tag, counted on
  frame by frame (continuous across loops of the file); else the time of day
  (UTC) at the stream's start, counted on. Drop-frame at the 1000/1001 rates;
  interlaced timecodes count frames (field 1).
- **Frame identity:** each program frame's meta carries its timecode beside its
  sequence; `sdi.get_status.last_timecode` is the latest slot's
  (`hh:mm:ss:ff`, `;ff` drop-frame); the output counts `timecode breaks`
  (consecutive frames whose timecodes are not one frame apart; a new stream
  after a format change or a restarted input starts its own).

### Captions

`GstVideoCaptionMeta` (CEA-608/708) does not serialize over unixfd either, so
captions travel with the timecode in chr's capture meta (type and bytes) and
are put back on the program frame as `GstVideoCaptionMeta`;
`decklinkvideosink` inserts them on line 9 (`cc-line`). Sources: a card's
(`output-cc`), and for tests the test pattern's synthetic CEA-608 field-1
pairs (`--captions`, two digits counting frames, odd parity). FFmpeg's raw
pipe does not carry a file's A/53 captions: files have none. Counted:
`sdi.get_status.caption_frames`, the output's `captioned`.

## Phase 5: switch-over

### What runs where

- **A tracked program** (`--tracked-scene`, `--description`) always runs on
  the core. `--pipeline` is gone (it fails with a pointer to this), and so is
  the timer-paced tracked path: the injection loop that read the shared-memory
  capture ring (`chr_shm::input`, `input/ring.rs`), the `TrackingJoin` that
  released joined frames at a fixed delay on the loop's output ticks, the
  render loop's clean-frame channel, and `chr-sdi-sim`, the ring's writer.
  `chr-sdi-in` is the one capture process (a file, a test pattern or a
  DeckLink card); `chr-shm` keeps only the program tap.
- **A generated program** (the default triangle, `--synthetic-program`,
  `--pitch-demo`, `--software-video` with `--video-scene`) has no capture
  input whose clock the output could follow, so it keeps the internal clock:
  the render loop (`pipeline.rs`) paces itself and the simulated SDI port
  (`sdi.rs`) sends one slot per period and writes the tap, as before. Driving
  these through the core would need a synthetic INPUT process for a program
  that does not track anything and gain nothing; the internal clock is the
  simplest correct design there. Its `--render-deadline-ms` is its own; a
  tracked program refuses it.

### Flags and configuration

| Before | Now |
| --- | --- |
| `--pipeline timer\|gst` | removed: a tracked program is the core's |
| `--tracking-wait-ms <n>` | removed (fails, naming its replacement): `--program-delay-ms <n>` is D for every tracked program, with or without a tracking worker |
| `--input-ring <path>` | removed: `--capture-socket <path>` (default `$XDG_RUNTIME_DIR/chr/capture.sock`) |
| `chr-sdi-sim --video --scene [--send-cameras]` | `chr-sdi-in --video [--send-cameras <manifest>] [--server-socket <path>]` |
| `stack.toml` `tracking_wait_ms` | `program_delay_ms`; the old key is still read, with a warning, and refused next to the new one |
| – | `stack.toml` `input_loss`, `output_rate`, `audio` (`--input-loss`, `--output-rate`, `--no-audio`) |

The core's own flags (`--capture-socket`, `--program-delay-ms`,
`--output-rate`, `--input-loss`, `--output`, `--device`, `--audio-channels`,
`--no-audio`) require a tracked program.

### External tracking on the core

Without `--tracking-worker` no tracking worker runs, and the cameras come as
`tracking.frame` ([SDI simulation](sdi-simulation.md#trackingframe)). Its
`sequence` is the frame's `frame_id` in the input's current stream (what
`chr-sdi-in` stamps, from 0 per stream); the driver tells the feed the program
sequence of the newest stream's first frame (`TrackingFeed::rebase`), so a
camera finds its frame across input restarts and format changes. A frame waits
for its camera until its slot is decided, exactly as for a worker's answer.
`chr-sdi-in --send-cameras <manifest>` replays a calibrated clip's cameras
(`Ti`, `C`, lens, and the warp of `frame_id % warps`), optionally late
(`--tracking-delay-ms`) or with gaps (`--drop-tracking-every`).

### Telemetry

`sdi.get_status` carries `core` for a tracked program (#131; TS mirror
`CoreStatus` in the render harness, shown on its monitor page):

| Field | Meaning |
| --- | --- |
| `program_delay_ms` | D |
| `decision_margin_ms` | how long before it is due each slot is decided (4 ms) |
| `cg_slots`, `clean_slots`, `empty_slots` | slots decided since start with their CG, their clean feed, or neither (the output repeats) |
| `late_cg`, `late_decisions` | rendered frames that came after their slot was decided; slots decided after they were due |
| `cg_ratio` | `cg / (cg + clean)` since start |
| `window` | the last complete 10 s report: `seconds`, `cg_slots`, `clean_slots`, `cg_ratio`, and `{min, mean, max, count}` in ms of `slack_ms` (per-slot slack: how long before its slot's decision each CG slot's frame arrived), `render_budget_ms` (time left until the slot's decision when the frame went to the render worker) and `delay_ms` (capture to output at the output) |

Repeats (`repeated_frames`), input loss (`input_loss_slots`, `input_losses`),
format changes and their clean slots (`format_changes`,
`transition_clean_slots`) stay `sdi.get_status`'s own fields; the camera model is
`tracking.get_status.camera_model`. The counters refresh every second, the
window every 10 s with the log line, which now also prints the per-slot slack.
`stack.py status` shows all of it:

```text
  program (core)    D 800 ms, decided 4 ms before due; CG 80.9 % since start, 100.0 % in the last 10 s
  slots             1436 CG, 338 clean feed, 0 empty (repeated), 0 late CG, 0 decided late; input loss 0 slots in 0 losses; 0 format changes (0 clean slots)
  timing            slack of CG slots 309.9/477.7/615.8 ms, render budget at submit 317.0/491.6/635.6 ms, capture to output 800.1/800.3/804.8 ms (min/mean/max)
```

and flags a last window with less than half its slots CG as a `PROBLEM`.

### The stack

`stack.py` (through `run-tracked.sh`) starts `chr-sdi-in --video <clip>` on
`<dir>/capture.sock`, chr-server's core on it with `--program-delay-ms`,
`--input-loss`, `--output-rate` and `--no-audio` from `stack.toml`, its workers
(the stand-in, or vchao's `pitch-analysis` through `with-cuda-driver.sh`), and
chr-monitor on the tap; with `tracking = "external"` it adds
`--send-cameras <scene>` to `chr-sdi-in` instead of a worker. The output
follows the clip's rate.

### CPU of the clean feed

The core converts every frame's clean feed (RGBA to UYVY at the output size)
ahead of its deadline: at 1080p30 `chr-server` used 34.9 % of a core with the
GPU render worker (phase 5 stack run below), against 10–15 % on the former
timer path, which converted only the frames that needed it. Converting lazily,
only for a slot without CG, would have to happen in the 4 ms between the
decision and the due time (a 1080p conversion takes about that on one thread),
or decide clean earlier and so shorten every frame's render budget. That trade
is not taken here; the CPU is affordable on the render host.

### Measured

`stack.py` with test ports beside the live stack on the RTX 3090 host, release
build:

| Run | Output | Slots | Timing (min/mean/max) | Other |
| --- | --- | --- | --- | --- |
| Replay stand-in, panda clip 1920x1080p30 (342 frames) with the onsite start description, D 200 ms (`tracking_wait_ms = 200`, migrated with a warning) | 1920x1080p30, 30.0 slots/s, 0 repeats | 1443 CG, 15 clean (6 at the start, 9 while `description.load` replaced the render worker), 100 % CG in the last 10 s | slack 179.6/185.2/188.9 ms, render budget 192.8/194.2/195.2 ms, delay 200.1/200.2/200.7 ms | camera model `radial` (worker); SRT caller: 1920x1080 30 fps; `description.load` of a one-asset package accepted (1 imported); CPU chr-server 35 %, chr-sdi-in 33 % + FFmpeg 61 %, chr-monitor 10 %, render worker 17 % |
| The same, `tracking = "external"` (`chr-sdi-in --send-cameras`) | 1920x1080p30, 0 repeats | 553 CG, 5 clean (start) | slack 123.7/183.7/188.3 ms, budget 191.4/193.4/195.6 ms, delay 200.0/200.2/201.0 ms | camera model `radial+warp` |
| vchao `pitch-analysis`, SC1464 fixture 1212x654p30, D 800 ms, beside the live stack's own `pitch-analysis` (GPU 94 % busy, 5.2 GB) | 1212x654p30, 1 repeat (start) | 300 of 300 CG per 10 s after the 11 s model load (338 clean before); without the render-credit wait 62 % | slack 315/482/617 ms, budget 323/498/638 ms, delay 800.1/800.2/803.2 ms; interval 30.30/33.333/36.46 ms (sd 0.29) | camera model `pinhole` (none); SRT caller: 1212x654 30 fps; `description.load` accepted; CPU chr-server 16 %, chr-sdi-in 12 % |

## Run

From the repository root, in `nix develop .#media` (FFmpeg for `--video`;
`--test-pattern WxH rate` needs only the default shell):

```sh
ffmpeg -f lavfi -i testsrc2=size=1920x1080:rate=60000/1001:duration=30 \
  -vf setparams=color_primaries=bt709:color_trc=bt709:colorspace=bt709:range=tv,setsar=1 \
  -c:v libx264 -pix_fmt yuv420p output/gst-core/clip5994.mp4
target/linux/release/chr-sdi-in --video output/gst-core/clip5994.mp4 &
target/linux/release/chr-server --description <description.json> \
  --size 1920x1080 --fps 59.94 --tracking-worker \
  --tracking-worker-arg --manifest --tracking-worker-arg <calibrated-video-v1.json> \
  --program-delay-ms 100 [--output-rate fixed:50]
```

A signal whose format changes: the test pattern switches after each `--then`
interval, and `--cycle` starts the list over; `loss` and `resume` take the
signal away and bring it back (any source):

```sh
target/linux/release/chr-sdi-in --test-pattern 1920x1080 25 \
  --then 30 1920x1080 50 --then 30 1920x1080 59.94i --then 20 loss --then 5 resume
```

An interlaced clip with a parity patch, a flash and a beep (the measurements'):

```sh
ffmpeg -filter_complex "testsrc2=s=1920x1080:r=50:d=30[b];color=c=gray:s=192x1080:r=50:d=30,\
format=yuv420p,geq=lum='40+4*mod(N\,40)':cb=128:cr=128[p];[b][p]overlay,drawbox=x=192:w=192:\
h=1080:color=white:t=fill:enable='lt(mod(n\,50)\,2)',tinterlace=interleave_top,setfield=tff" \
  -f lavfi -i "aevalsrc='if(lt(mod(t,1),0.02),0.5*sin(2*PI*1000*t),0)':s=48000:c=stereo:d=30" \
  -c:v libx264 -crf 16 -pix_fmt yuv420p -flags +ilme+ildct -x264-params tff=1 \
  -timecode 10:00:00:00 -c:a pcm_s16le output/gst-core/i50.mov
target/linux/release/chr-sdi-in --video output/gst-core/i50.mov &
target/linux/release/chr-server --fps 25 --size 1920x1080 ... \
  --input-loss slate:/srv/chr/slate.png
```

Both default to `$XDG_RUNTIME_DIR/chr/capture.sock` (`--socket`,
`--capture-socket`); the audio socket is `<socket>.audio` (`--no-audio` on
either side turns audio off). With a card: `chr-sdi-in --source decklink
--device 0` and `chr-server … --output decklink --device 1 --output-rate
fixed:50i` (not verified on hardware; `docs/operations.md`). chr-server logs the output every 10 s: slots, CG,
clean feed, repeats, duplicates (fixed rate), skipped frames, the interval at
each rate and the capture-to-output delay (min/mean/max and standard
deviation), the tap's slot − frame, the selector's decisions with format
changes and their clean slots, input-loss fills, timecode breaks and captioned
slots, the audio (buffers, capture-to-output delay, audio minus video), the
frames joined with a camera and given up without one, and the render budget:
the time left until each rendered frame's slot is decided when it was
submitted. `chr-monitor` reads the program tap and
follows its mode; `cargo run --release -p chr-shm --example tap_stats -- <tap>
<s> [<x0> <x1>]` reports the same cadence for either pipeline from the tap,
per rate, and with columns the field parity of CG and other slots (odd minus
even rows: positive when an interlaced clip's later bottom field is brighter
and the fields kept their order).
`CHR_WORKER_TEST_SOLID=<luma>` makes `chr-renderer-cli worker` answer with solid
frames without a GPU, for timing and fault tests.

## Measurements

"Timer loop" below is the former default path of tracked programs (the capture
ring and the timer-paced render loop), measured for comparison; phase 5 removed
it.

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

### Phases 3 and 4: interlaced, input loss, audio, timecode

Same host and setup (live stack beside it, `--program-delay-ms 100`,
`chr-tracking-sim-cli`, solid render worker unless noted, release build).
Clips: 1920x1080 `testsrc2` with a parity patch (columns 0–191: luma 40 + 4 ×
picture number mod 40, so an interlaced frame's later bottom field is 4
brighter), a white flash (columns 192–383, one frame per second) and a 20 ms
1 kHz beep at each flash, H.264 woven top field first (`tinterlace`,
`+ilme+ildct`), PCM audio, timecode 10:00:00:00. "Parity" is `tap_stats`'s
odd minus even rows over the patch; "audio − video" the core's mean audio
delay minus video delay per 10 s report.

| Run | Output slots (CG / clean / fills / repeats) | Interval mean, sd, min–max (output) | Delay min / mean / max | Tap | Audio − video |
| --- | --- | --- | --- | --- | --- |
| 1080i50, 60 s | 1495 (1495 / 0 / 0 / 0) | 40.0000, 0.149, 39.45–40.63 ms | 100.05 / 100.21 / 100.87 ms | 1250 slots, slot − frame constant | −0.10 … −0.14 ms |
| 1080i59.94, 60 s | 1791 (1791 / 0 / 0 / 0) | 33.3666, 0.159, 32.80–33.78 ms | 100.04 / 100.22 / 100.69 ms | 1498, constant | −0.11 … −0.14 ms |
| 1080i50, every slot clean (render after D), 30 s | 757 (0 / 757 / 0 / 0) | 40.0000, 0.072, 39.71–40.36 ms | 100.06 / 100.12 / 100.45 ms | parity of 500 slots 2.99 / 4.00 / 5.00 | −0.03 ms |
| 1080i59.94, every slot clean, 30 s | 907 (0 / 907 / 0 / 0) | 33.3666, 0.060, 33.04–33.61 ms | 100.05 / 100.13 / 100.41 ms | parity of 599 slots 2.99 / 4.00 / 4.99 | −0.03 … −0.04 ms |
| 1080i50, GPU render worker, 40 s | 855 (848 / 7 / 0 / 0) | 40.0011, 0.144, 39.60–40.96 ms | 100.08 / 100.19 / 101.05 ms | CG parity 0.00 (both fields one picture) | −0.10 ms |
| 1080i50, worker faults, 95 s | 2369 (2282 / 87 / 0 / 0) | 40.0001, 0.150, 39.43–40.73 ms | 100.03 / 100.21 / 101.03 ms | without CG per fault (render kill / stop, tracking kill / stop): 7 / 57 / 7 / 16 | −0.08 … −0.14 ms; 937–938 audio buffers in every 10 s, faults included |
| Test pattern 1080p50 → p59.94 → i50 → i59.94, 20 s each | 3594 (3591 / 3 / 0 / 0) | 19.9998 / 16.6833 / 40.0000 / 33.3666 ms | 100.04 / 100.23 / 100.68 ms | one clean slot per change | −0.10 … −0.17 ms |

- **Interlaced:** the cadence is the frame rate's to within a microsecond and
  D is the same as at progressive rates. The output caps and
  `sdi.get_status` say `1920x1080i50` / `1920x1080i59.94`. A clean slot keeps
  both fields in their order (parity +4.00, every slot between 3 and 5); a CG
  slot is one picture (parity 0). The deinterlaced first field reached both
  workers through memfds (every slot CG with the solid and the GPU worker).
- **Audio:** lip sync at the timestamps is a tenth of a millisecond (the audio
  sink renders on its due time, the video sink a few hundred µs after), through
  worker kills and stops, format changes in both directions between progressive
  and interlaced, and input loss. On content, the integration test's beep and
  flash clips (192x128, every slot clean so the flash shows) put the beep
  0.041 ms (two samples, the detector's threshold) after the flash at 50p,
  1080i50's 25i, 59.94p and 29.97i.
- **Timecode:** the file's 10:00:00:00 counted on, no break in any run
  (`timecode breaks` 0) but at the three format changes (each a new stream).

**Input loss.** The 1080i50 clip with `SIGSTOP` of chr-sdi-in at 20 s and
`SIGCONT` at 25 s, `kill -9` at 35 s and a new chr-sdi-in at 38 s, 60 s each;
the test pattern with a scripted loss:

| Run | Output slots (CG / fills / repeats) | Losses | Interval mean, sd, max | Irregular | Delay min / mean / max |
| --- | --- | --- | --- | --- | --- |
| `hold` | 1494 (1251 / 243 / 0) | 2 | 40.0058, 0.265, 48.66 ms | 0 | 100.05 / 100.23 / 100.64 ms |
| `black` | 1494 (1251 / 243 / 0) | 2 | 40.0071, 0.302, 50.40 ms | 1 | 100.03 / 100.21 / 100.61 ms |
| `slate:<png>` | 1494 (1256 / 238 / 0) | 2 | 40.0080, 0.340, 52.08 ms | 1 | 100.06 / 100.21 / 100.55 ms |
| Test pattern 1080p50, `black` | 2994 (2547 / 447 / 0) | 2 | 20.0027, 0.240, 28.50 ms | 1 | 100.04 / 100.26 / 102.22 ms |
| Test pattern 1080i50, `--then 20 loss --then 5 resume --then 10 loss --then 1 resume`, `slate` | 1498 (1348 / 150 / 0) | 2 | 40.0002, 0.137, 40.44 ms | 0 | 100.04 / 100.22 / 100.58 ms |

- The stopped input costs its 5 s plus about 1.2 s in which the resumed
  decoder's backlog is dropped as stale (155 fills); the killed one 3 s plus
  its start and connection (88 fills). Every frame before and after left D
  after its capture; nothing was repeated by `livesync`.
- A new input process runs in its own phase: the last fill lasts until its
  first frame is due, one interval of up to one and a half periods (48–52 ms at
  25 fps, 28.5 ms at 50 fps). A scripted loss keeps the phase: no irregular
  interval at all. Before the last fill was cut to the new phase, `livesync`
  sent that first frame 26–34 ms late and repeated once.
- `frames skipped` (9 at 25 fps, 26 at 50 fps) at a restarted input are the
  frames the new process sent before the core connected (it numbers a new
  stream on by `frame_id`), not lost slots.
- Audio stopped and came back with the input (its own session reconnected),
  at −0.1 ms throughout.

**Soak:** 10 min 20 s at 1080i50 from the clip with audio, no faults: 15 495
slots, all CG, no repeat, fill or skipped frame, interval mean 40.0000 ms (sd
0.145 ms, 37.36–42.80 ms, none a quarter period off), capture → output delay
100.04 / 100.21 / 102.90 ms, no timecode break; the tap read 15 250 slots at
39.999983 ms with one slot − frame value; audio − video −0.08 … −0.17 ms in all
61 reports, 937–938 audio buffers each. chr-server used 48 % of a core and 84
MiB (78 MiB at the end, 45 descriptors), chr-sdi-in 29 % plus FFmpeg's two
decoders.

## Limitations

- Frames of the core are not render submissions: `render.get` does not see
  them, and `render.get_worker_status` counts restarts but not clean feed.
  The core's log line and `sdi.get_status` (slots, repeats, last frame, and
  its `core` telemetry) report the output.
- DeckLink input and output, the card clock and genlock, RP 188 timecode,
  captions on line 9 and embedded audio on a card are built and tested
  without a card only: not verified on hardware.
- An interlaced program's CG slots show one picture per frame (the first
  field's, both fields from it); its clean slots keep both fields.
- Audio is not in chr-monitor's SRT/WebRTC streams (the tap is video only).
- Timecode restarts with each new stream (a format change, a restarted input);
  files' A/53 captions do not pass FFmpeg's raw pipe.
- The decision margin is a fixed 4 ms; the clean conversion costs CPU
  (videoconvertscale, 2 threads per started 1080p; [Phase 5](#cpu-of-the-clean-feed)).
- Startup frames, before both workers are ready, are clean feed.
- A format change restarts the tracking worker: with vchao about 11 s without
  CG until vchao can start a new stream without reloading its model.
- Frames of an extent other than the program camera's are clean feed; the
  output keeps `--size` (the clean feed is scaled), so the tap is never
  re-created. Only the test pattern switches formats in simulation; the file
  input plays one clip.
- With `fixed`, the output's delay varies by up to an input period by design.
- A DeckLink output keeps one mode: it needs a fixed output rate. SD modes
  are not mapped.

Next (#154): verify phases 3 and 4 on a DeckLink card with house sync (#80);
the 1-hour soak; a vchao stream without a model reload.
