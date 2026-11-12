# chr render harness (example)

Example TypeScript client for `chr-server` (`server/runtime/`): a browser program
monitor and two verification scripts. The server renders a rotating triangle, takes
JSON-RPC 2.0 control over a WebSocket and a NativeSocket (Unix Domain Socket), and
streams its program output back to clients:

```text
args (JSON-RPC) → renderer → SDI frame (UYVY 4:2:2, BT.709) → SDI output port
                                                            → encoder (H.264) → WebRTC → client
```

The renderer's only product is the SDI program output. The simulated SDI port ticks on
its own clock with a bounded queue; it repeats the last frame on underrun and drops
renders when full (`sdi.get_status` reports the counters). The monitor encodes exactly
what the port transmits, repeats included. Encoding is software after a CPU readback;
that is a development shortcut, not the intended GPU-to-hardware-encoder path.

The protocol mirror is `src/protocol.ts`; the Rust source of truth is `common/src/`.
`src/scene-frame.ts` mirrors the limited synthetic `scene_frame_v1` DTOs and exact
payload validation, reexported as `SceneFrame`. It does not enable those methods
in the triangle runtime. Keep its required-nullable fields, method/result maps
and error codes synchronized with `common/src/scene_frame/`.

## Triangle rotation

The legacy **Triangle** panel provides Clockwise (+) and Counter-clockwise (-)
direction choices. Select a direction, then click **Start rotation** to apply it;
restarting changes velocity without resetting the angle. Signed speed entry is
still supported and keeps the direction selector synchronized. Positive angles
rotate clockwise because this triangle uses the Y-down SNC image coordinates.
These legacy commands do not animate the retained-scene overlay in
`--synthetic-program`, `--pitch-demo` or `--software-video`.

With `live=1`, the top-right panel instead becomes **Orange mesh rotation**.
It sends `mesh.start_rotation`, `mesh.stop_rotation` and `mesh.get_rotation`
commands targeting the software-video scene's `mesh`; its separate `ground`
is unchanged. Server advances the pose before each native input admission using
the simulated SDI output-slot clock and configured time base, not browser timers
or source identity. Changing signed speed preserves the pose; Stop holds it.
Closing or disconnecting the page does not stop server animation. Polling only
displays state and never drives motion. External instance edits stop animation
instead of being overwritten. Stop before manual scene edits to avoid revision
conflicts. This opt-in profile permits one animated instance at a time; it does
not repurpose the legacy triangle methods or add a general animation timeline.

## Synthetic scene controls

Start the server with `--synthetic-program --size 640x360 --fps 50` to opt into
the [named synthetic integration profile](../../../server/docs/program-output.md).
Enter its printed runtime and scene IDs in **Local program scene**, then
**Load / reconcile**. The panel supports zoom, dolly, orbit around the synthetic
world origin, local yaw/pitch/roll, reset to the loaded camera, and atomic sun/shadow edits. These are
ordinary WebSocket JSON-RPC `scene.get`/`scene.mutate` calls shared with NativeSocket
clients, not renderer bypasses. The video stays the decoded SDI monitor.

NativeSocket CLI/agent example (omit edit flags to query; `--ws URL` selects the
same methods over WebSocket):

```sh
npm run scene -- --socket /path/to/render.sock --runtime PRINTED_RUNTIME --scene PRINTED_SCENE --camera orbit-up --sun 3 --shadow pcf
```

Edits are serialized and query the current revision. An uncertain reply triggers
reconciliation and requires an explicit reload; it is never blindly retried.
Camera controls require a manual `intrinsics_pose` camera. World up is -Z and poses
are forward row-major matrices with no half-pixel offset. Reset does not create a
new timeline or change input frame identity.

For a real-browser control and decoded-video self-test, while this opt-in server
and the harness are running:

```sh
npm run verify:browser -- --url 'http://127.0.0.1:5173/?program=1&runtime=PRINTED_RUNTIME&scene=PRINTED_SCENE'
```

This clicks every camera action and the lighting/shadow panel, verifies revision
progress and camera reset, and measures changed pixels in decoded monitor video.
On deep Linux worktrees, Chrome may exceed Unix socket path limits with an absolute
profile directory. Create root `output/render-harness/chrome`, then use
`TMPDIR=../../../output/render-harness/chrome` from this package. If a distribution
launcher exits silently, pass `--chrome` with the actual installed Chrome binary.
The legacy `verify` rotation-content check is for the default triangle only;
do not interpret it as synthetic scene acceptance. Unit geometry tests are not
browser/WebRTC evidence.

## Owner-local pitch demo

The separate [pitch-demo-v1 guide](../../../server/docs/pitch-demo.md) describes
`--pitch-demo <local-manifest.json> --pitch-controls <new-local-controls.json>`.
Load that native-written controls file in the panel before Load / reconcile.
It supplies opaque model IDs and the renderer-derived editable pose; no GLB path
is sent through RPC. The original source-frame identity, Ti/C, native extent and
dimension status remain local and immutable, including after reconnects.

The pitch panel adds grounded model selection/scale/yaw/mirror, authored tower
heights, independent sun, four-point/four-spot rig comparisons and
radius/cone/target/emission controls. NativeSocket uses the same helpers:

```sh
npm run scene -- --socket /path/to/control.sock --pitch-controls /path/to/controls.json --camera reset --rig spots+sun --shadow pcss --summary
npm run verify:browser -- --url 'http://127.0.0.1:5173/?ws=ws://127.0.0.1:8787' --pitch-controls /path/to/controls.json --fps 25
```

The real Chrome check uploads metadata through the local file picker, clicks the
actual panel and inspects decoded SDI video. Keep metadata, full CLI snapshots
and screenshots private. `--summary` emits only revision/effect/count aggregates.
No measured pose/calibration, source-video decoding or production-policy approval
is implied.

## Run

The Linux `chr-server --software-video /path/to/video.mp4 --fps 30` opt-in
requires the `gstreamer-media` Cargo feature and root `nix develop .#media`
environment. It composites the local MP4 with the retained sample scene,
passes frames through the existing simulated SDI clock, and uses GStreamer
for source conversion and H.264 encoding. WebRTC signaling and ICE-TCP remain
the existing implementation so the documented SSH forwarding path is retained.
This is not a physical SDI device, a camera-calibration replay, or a full
GStreamer WebRTC backend.

Open the page with `?live=1&runtime=PRINTED_RUNTIME&scene=PRINTED_SCENE&ws=WS_URL`
to connect, load the scene controls and open the program stream automatically.
Without `live=1`, existing manual and self-test behavior is unchanged.

For the owner's `--software-video ... --video-scene ...` calibrated replay,
also pass `calibrated=1`. The rotation panel then controls the registered GLB,
the fixed ground is a transparent shadow catcher, and camera buttons are disabled
to preserve alignment. The label is a UI hint, not calibration discovery or an
RPC extension. Per-frame lens/warp data and asset paths stay server-local.

Requires Node.js 24 or newer, which runs the `.ts` scripts directly.

```sh
# Terminal 1, from the repository root (see server/README.md for GPU access on
# non-NixOS hosts)
cargo run --locked --release -p chr-server -- --webrtc-tcp 127.0.0.1:8788

# Terminal 2, from here
npm install
npm run dev          # viewer at http://127.0.0.1:5173/
```

`chr-server --help` lists the options. By default the server listens on
`ws://127.0.0.1:8787` and `$XDG_RUNTIME_DIR/chr/render.sock`.

## Verify

```sh
npm run typecheck
npm test                          # depacketizer, framing and shared scene/frame fixtures
npm run verify                    # IPC, SDI status, and the WebRTC stream via werift + ffmpeg
npm run verify:browser            # the stream decoded by a headless Chrome
npm run verify:browser -- --tcp-only   # same, with UDP forbidden (SSH forward case)
npm run verify:browser -- --rotation-directions # exercise both signs through the actual UI
npm run verify:browser -- --mesh-rotation --url 'http://127.0.0.1:5173/?ws=ws://127.0.0.1:8787&runtime=PRINTED_RUNTIME&scene=PRINTED_SCENE'
```

`verify` writes the captured streams to the repository's
`output/render-harness/program.h264`, and `verify:browser` saves its
screenshot next to them. Its content check
decodes them with `ffmpeg` and expects frames to change only while rotating; without
`ffmpeg` that check fails and names the capture file. `verify:browser` needs a
Chrome binary (`--chrome` or `CHROME`). If Chrome fails to start inside a Nix shell, clear
`LD_LIBRARY_PATH` for it: `env -u LD_LIBRARY_PATH npm run verify:browser`.

## Viewing from another machine over SSH

SSH forwards carry TCP only, while WebRTC media normally uses UDP. Start the server with
`--webrtc-tcp 127.0.0.1:8788`; each open stream then also offers an ICE-TCP candidate on
one port of `127.0.0.1:8788`–`8791` (lowest free first, up to four streams). One viewer
needs only the page, the WebSocket, and port 8788:

```sh
ssh -N -L 5173:127.0.0.1:5173 -L 8787:127.0.0.1:8787 -L 8788:127.0.0.1:8788 user@render-host
```

Forward 8789–8791 as well to keep more viewers open at the same time.
Then open `http://127.0.0.1:5173/` on the local machine. If the local machine can reach
the server's network directly, the browser may use UDP host candidates instead.
