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

## Run

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
