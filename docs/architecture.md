# Server and player modules

## Selected simulation

The server reads a private MP4 and simulates an uncompressed, timecoded input.
This is not a physical SDI interface and does not claim SDI raster conformance.
The source video raster and its camera geometry remain unchanged.

| Module | Responsibility | Pixel location |
|---|---|---|
| Asset preparation | Validate source identity, prepare logo mipmaps, masks, and per-frame transforms | CPU preparation of auxiliary assets only |
| Frame source | FFmpeg demux and hardware decode; preserve source PTS | Compressed packets on CPU; decoded pixels on GPU |
| Simulation clock | Pace frames and assign epoch, sequence, and 30 fps NDF timecode | Metadata only |
| Frame association | Match source PTS to clip ordinal, Ti, and mask | Metadata only |
| GPU compositor | Project logos, sample premultiplied textures, apply occlusion and opacity | GPU |
| Output | GPU color conversion, hardware encode, MPEG-TS mux, SRT delivery | Pixels on GPU; compressed packets on CPU |
| Windows player | Receive and decode the SRT stream | Player-managed |

```text
Server
  MP4 -> hardware decoder -> GPU frame ----------------------+
                           source PTS -> frame association   |
  simulation clock -> epoch / sequence / timecode            |
  prepared scene -> Ti / logos / same-frame mask ------------+
                                                            |
                       GPU compositor <---------------------+
                              |
                       hardware encoder
                              |
                       MPEG-TS / SRT listener
                              |
Windows                       |
  VLC or ffplay <-------------+
```

## Backend boundary

The wgpu/Rust and Dawn/C++ trials use the same prepared scene manifest. Backend
code owns GPU resources and synchronization; it must not replace resource
sharing or GPU-local copies with frame downloads. The source and encoder must
select hardware frame formats explicitly and fail rather than silently fall
back to CPU frames.

An incoming frame carries a GPU resource lease and a readiness synchronization
primitive. A renderer may not recycle the resource before downstream GPU work
has finished. Sharing a GPU device does not remove the need for image-layout
transitions, queue synchronization, or lifetime management.

The simulation timecode advances across MP4 loops. The source ordinal wraps
independently and continues to select the original clip's camera data.
Output PTS, source PTS, timecode, and frame identity are distinct values.

## Prepared scene format

`tools/prepare_scene.py` produces private runtime assets:

- `scene.json`: source pins, dimensions, rational frame rate, per-frame source
  identity, inverse UV-to-native-pixel homographies, and clipped raster bounds.
- `logo-N.rgba32f`: little-endian float32 premultiplied RGBA mip levels.
  Each manifest level declares its dimensions and byte offset. Complete PNG
  canvases and the existing rotations are retained.
- `masks.r8`: tightly packed native-resolution uint8 masks in clip-ordinal order.
  A value of 255 means fully occluded. Byte offset is
  `clip_ordinal * width * height`.

The inverse homography maps native integer-center pixel coordinates into
unit-square UV. Matrix coefficients are row-major. Logo order follows the
reference declaration. Resolve each logo's 2x2 samples before ordered
premultiplied-over composition; then apply global opacity and scene visibility
once. Neither opacity nor artwork affects the cached mask.

The packed mask file is about 1.18 GB for this clip. It is an auxiliary cache,
not decoded video. All generated files must remain private and outside the
tracked Nix source closure.

## Comparison and acceptance

Both backend trials must report actual adapter identity, hardware frame format,
processed frame counts, timestamp mismatches, throughput, and their memory
handoff mechanism. Visual quality is compared before attributing differences
to the SRT encoder's lossy output.

Do not equate hardware decoding, GPU adapter discovery, or a functioning SRT
stream with a verified GPU-resident compositor. Completion requires the whole
path, with matching camera/mask identity and an independently decodable output.

Only the selected implementation is left running as the private playback
service. Experimental listeners bind loopback and use separate ports.
