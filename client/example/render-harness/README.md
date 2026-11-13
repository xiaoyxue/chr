# chr render harness (example)

Example TypeScript client for `chr-server` (`server/runtime/`) and `chr-monitor`
(`monitor/`): a read-only browser program monitor, a scene CLI and two verification
scripts. The server
renders a rotating triangle through its supervised `chr-renderer-cli worker`
process and takes JSON-RPC 2.0 control over a WebSocket and a
NativeSocket (Unix Domain Socket); the separate monitor process streams its program
output back to clients:

```text
args (JSON-RPC) → renderer → SDI frame (UYVY 4:2:2, BT.709) → SDI output port
                → program tap (shared memory) → chr-monitor: encoder (H.264) → WebRTC → client
```

The renderer's only product is the SDI program output. The simulated SDI port ticks on
its own clock with a bounded queue; it repeats the last frame on underrun and drops
renders when full (`sdi.get_status` reports the counters). `chr-monitor` encodes
exactly what the port transmits, repeats included, and serves `webrtc.offer` and
`monitor.get_status` on its own WebSocket; `chr-server` no longer answers
`webrtc.offer`. Stopping the monitor never affects control or program output.
Encoding is software after a CPU readback; that is a development shortcut, not the
intended GPU-to-hardware-encoder path.

The protocol mirror is `src/protocol.ts`; the Rust source of truth is `common/src/`.
`src/scene-frame.ts` mirrors the limited synthetic `scene_frame_v1` DTOs and exact
payload validation, reexported as `SceneFrame`. It does not enable those methods
in the triangle runtime. Keep its required-nullable fields, method/result maps
and error codes synchronized with `common/src/scene_frame/`.

## Synthetic scene controls

Start the server with `--synthetic-program --size 640x360 --fps 50` to opt into
the [named synthetic integration profile](../../../server/docs/program-output.md).
Edit its camera, sun and shadows with the scene CLI over NativeSocket (or `--ws URL`
for the same methods over WebSocket); omit edit flags to query:

```sh
npm run scene -- --socket /path/to/render.sock --runtime PRINTED_RUNTIME --scene PRINTED_SCENE --camera orbit-up --sun 3 --shadow pcf
```

Edits are ordinary `scene.get`/`scene.mutate` calls, not renderer bypasses. Each
edit queries the current revision; an uncertain reply is reported and never
blindly retried. Camera edits require a manual `intrinsics_pose` camera. World up
is -Z and poses are forward row-major matrices with no half-pixel offset. Reset
does not create a new timeline or change input frame identity. Watch the result in
the program monitor page.

## Owner-local pitch demo

The separate [pitch-demo-v1 guide](../../../server/docs/pitch-demo.md) describes
`--pitch-demo <local-manifest.json> --pitch-controls <new-local-controls.json>`.
Pass that native-written controls file to the scene CLI. It supplies opaque model
IDs and the renderer-derived editable pose; no GLB path is sent through RPC. The
original source-frame identity, Ti/C, native extent and dimension status remain
local and immutable, including after reconnects:

```sh
npm run scene -- --socket /path/to/control.sock --pitch-controls /path/to/controls.json --camera reset --rig spots+sun --shadow pcss --summary
```

Keep metadata, full CLI snapshots and screenshots private. `--summary` emits only
revision/effect/count aggregates. No measured pose/calibration, source-video
decoding or production-policy approval is implied.

## Program monitor page

The page shows the program output and diagnostics only; it sends no control
commands. **Connect** opens two independent connections, to `chr-server` (`?ws=`,
default `ws://<page host>:8787`) and to `chr-monitor` (`?monitor=`, default `:8789`).
Each reconnects by itself every 2 s while connected: losing `chr-server` keeps the
video playing, and losing `chr-monitor` closes the stream, keeps the server panels
updating and reopens the stream when the monitor returns. Pasted URLs lose
surrounding whitespace and quotes.

Panels poll once a second; counters show their rate since the last poll and turn
amber while a problem counter rises:

- **SDI output**: video mode, slots out, repeated and dropped slots, source frame.
- **Render worker**: state and PID, restarts, deadline misses, clean-feed frames,
  last error. Worker state, PID and error changes are logged.
- **Tracking**: `tracking.get_status` join counters of a tracked program;
  other programs show "not a tracked program".
- **chr-monitor**: encoded and skipped slots, WebRTC viewers, SRT callers.
- **WebRTC stream**: connection and ICE state, the selected candidate path
  (`tcp`/`udp`, candidate types and remote address), RTT, bitrate, resolution,
  fps, codec, decoder, decoded/dropped/key frames, decode time, jitter buffer,
  network jitter, packets lost, NACK/PLI and freezes.

`?transport=tcp` keeps only the monitor's ICE-TCP candidates, so a viewer behind an
SSH port forward skips UDP attempts that cannot succeed. `?auto=1` connects, opens
the stream and writes a JSON report for `verify:browser`.

## Run

The Linux `chr-server --software-video /path/to/video.mp4 --fps 30` opt-in
requires the `gstreamer-media` Cargo feature and root `nix develop .#media`
environment. It composites the local MP4 with the retained sample scene,
passes frames through the existing simulated SDI clock, and uses GStreamer
for source conversion. Start `chr-monitor --fps 30 --encoder gstreamer` (or use
`server/scripts/run-live.sh --media … --monitor-arg --encoder --monitor-arg gstreamer`)
for the same GStreamer H.264 encoding. This is not a physical SDI device, a
camera-calibration replay, or a full GStreamer WebRTC backend.

Requires Node.js 24 or newer, which runs the `.ts` scripts directly.

```sh
# Terminal 1, from the repository root (see server/README.md for GPU access on
# non-NixOS hosts); the build also produces the render worker, chr-renderer-cli
cargo build --locked --release -p chr-runtime
cargo run --locked --release -p chr-runtime

# Terminal 2, from the repository root (same --size/--fps as chr-server)
cargo run --locked --release -p chr-monitor -- --webrtc-tcp 127.0.0.1:8790

# Terminal 3, from here
npm install
npm run dev          # viewer at http://127.0.0.1:5173/
```

Instead of terminals 1 and 2, `server/scripts/run-live.sh --monitor-arg --webrtc-tcp
--monitor-arg 127.0.0.1:8790` starts both (see `server/README.md`).

`chr-server --help` and `chr-monitor --help` list the options. By default the server
listens on `ws://127.0.0.1:8787` and `$XDG_RUNTIME_DIR/chr/render.sock`, and the
monitor on `ws://127.0.0.1:8789`; the page's two URL fields select them (see
[Program monitor page](#program-monitor-page)).

## Verify

```sh
npm run typecheck
npm test                          # depacketizer, framing and shared scene/frame fixtures
npm run verify                    # IPC, render worker and SDI status, monitor status, and the WebRTC stream via werift + ffmpeg
npm run verify:browser            # the stream decoded by a headless Chrome
npm run verify:browser -- --tcp-only   # same over ?transport=tcp, requiring an ICE-TCP path (SSH forward case; needs chr-monitor --webrtc-tcp)
```

`verify` reaches `chr-server` at `--ws` (`CHR_WS_URL`, default `ws://127.0.0.1:8787`)
and `chr-monitor` at `--monitor` (`CHR_MONITOR_URL`, default `ws://127.0.0.1:8789`).
It checks that the render worker is ready and renders every frame (no restart and
no clean-feed or repeated frames over 0.5 s, from `render.get_worker_status`),
that `chr-server` rejects `webrtc.offer` as method not found, negotiates
WebRTC with the monitor and checks `monitor.get_status`; `--skip-webrtc` skips the
WebRTC and monitor checks. It writes the captured streams to the repository's
`output/render-harness/program.h264`, and `verify:browser` saves its
screenshot next to them. Its content check
decodes them with `ffmpeg` and expects frames to change only while rotating; without
`ffmpeg` that check fails and names the capture file. `verify:browser` needs a
Chrome binary (`--chrome` or `CHROME`). If Chrome fails to start inside a Nix shell, clear
`LD_LIBRARY_PATH` for it: `env -u LD_LIBRARY_PATH npm run verify:browser`.

## Viewing from another machine over SSH

SSH forwards carry TCP only, while WebRTC media normally uses UDP. Start `chr-monitor` with
`--webrtc-tcp 127.0.0.1:8790`; each open stream then also offers an ICE-TCP candidate on
one port of `127.0.0.1:8790`–`8793` (lowest free first, up to four streams). One viewer
needs only the page, both WebSockets (8787 for `chr-server`, 8789 for `chr-monitor`),
and port 8790:

```sh
ssh -N -L 5173:127.0.0.1:5173 -L 8787:127.0.0.1:8787 -L 8789:127.0.0.1:8789 \
  -L 8790:127.0.0.1:8790 user@render-host
```

Forward 8791–8793 as well to keep more streams open at the same time.
Then open `http://127.0.0.1:5173/?transport=tcp` on the local machine. Without
`transport=tcp`, a local machine that can reach the server's network directly may use
UDP host candidates instead.
