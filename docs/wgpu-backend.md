# wgpu/Rust backend

## Modules

| Module | Responsibility |
|---|---|
| `tools/prepare_scene.py` | Backend-neutral private asset preparation |
| `src/gpu.rs` | Create a native wgpu/Vulkan device with video decode/encode queues |
| `native/media.c` and `src/media.rs` | FFmpeg Vulkan frames, demux/encode/mux, GPU plane transfers |
| `src/scene.rs` | Source-PTS association and continuous 30 fps NDF timecode |
| `src/render.rs` | GPU assets, bind groups, command submission, resource-state handoff |
| `src/compose.wgsl` | NV12 sampling, projective mipmapped logo composition, optional timecode display |
| `src/pack.wgsl` | Pack composed pixels into an NV12 GPU buffer |
| `src/main.rs` | Simulation clock, source looping, status reporting, output lifecycle |
| `tools/verify_wgpu.py` | Independent encoded-output verification, outside the server |

## Memory and synchronization

FFmpeg borrows the Vulkan instance/device owned by wgpu. Video queues and
`VK_KHR_video_maintenance1` are enabled explicitly when creating the device.
Software decode fallback is refused.
The pinned MP4 must provide complete codec parameters in its header. The server
does not call `avformat_find_stream_info`, which could otherwise software-decode
probe frames before hardware decoder setup.
The compressed source file's SHA256 is checked against the prepared scene at
every startup, before allocating rendering assets.

The frame path has **four GPU-local plane copies per frame**, not literal
zero-copy:

1. Copy the decoded luma and chroma planes into renderer-owned GPU textures.
2. Execute the WGSL compositor and GPU NV12 packing pass.
3. Copy the packed buffer's luma and chroma planes into an FFmpeg hardware
   encoder frame.

No decoded/rendered video pixels are mapped, downloaded, or processed by the
CPU. CPU work handles compressed packets, frame metadata, auxiliary asset
uploads, and command submission. The first implementation waits for GPU
completion between handoffs; it is deliberately conservative, not a fully
asynchronous optimized pipeline.
An initial GPU-only warmup resolves wgpu's first-use resource initialization
before native Vulkan writes decoded pixels into the input textures.

FFmpeg frame timeline semaphores, image layouts, concurrent sharing, wgpu
resource transitions, and the shared graphics-queue lock are all part of the
handoff. wgpu resources remain alive while native Vulkan copies use their
handles. CPU waits are synchronization, not pixel transfers.

The renderer preserves original Y/UV code values for untouched pixels/blocks
when no artwork or timecode affects them. Unknown source color metadata uses
BT.601 limited-range conversion; declared BT.709 and full-range input flags
select their corresponding conversions. Hardware encoding is lossy.

## Run

Build the reproducible package:

```sh
nix build --cores 4 --max-jobs 1
```

Prepare assets once in a new private directory using the shared preparation
tool, then run a bounded file-output trial:

```sh
result/bin/chr \
  --scene /private/prepared/scene.json \
  --output /private/test.ts \
  --frames 1500 --unpaced \
  --ledger /private/test.jsonl
```

Omit `--unpaced` for 30 fps pacing. The source loops while simulation timecode
and output timestamps keep advancing. `--frames 0` (the default) runs until
stopped. A ledger is allowed only for a bounded run; `--status` writes a
bounded-size current-state JSON file instead.

```sh
result/bin/chr \
  --scene /private/prepared/scene.json \
  --output 'srt://127.0.0.1:19001?mode=listener&latency=120000' \
  --status /private/status.json \
  --timecode-overlay
```

The SRT output waits for one player. Use an explicitly authorized private
interface instead of localhost only when serving a remote player.
`--timecode-overlay` draws the simulation timecode on the GPU; omit it for
reference-scene comparisons. `--mode bypass` disables artwork for a controlled
comparison through the same GPU/hardware-codec pipeline.

## Scope and limits

This trial selects the validated RTX 3090 and a 30 fps, even-height raster with
width divisible by four. It uses cached masks and camera transforms, not live
tracking. It supports the existing six ordered quads and three logo textures.
It is a video-only, SDI-like simulation, not physical SDI capture.

The RGB compositor implements the existing opacity/occlusion ordering and
inverse-Jacobian mip LOD. GPU float precision, bilinear filtering, chroma
conversion, and re-encoding prevent a claim of byte-identical reference video.

An independent verifier may decode the encoded output on the CPU. That does
not insert a CPU-frame stage into the running server.

The bounded SRT check uses a loopback listener, independently decodes all 150
received frames, and stops its temporary user service:

```sh
nix develop --command bash tools/test_wgpu_srt.sh /private/prepared/scene.json
```

Successful SRT completion drains for one second before closing, for the tested
120 ms latency configuration. Abrupt disconnects do not guarantee tail delivery.
Persistent playback should be supervised by a user service; reconnecting starts
a new simulation epoch rather than claiming seamless continuity.
