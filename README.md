# chr

A GPU-resident, video-only streaming compositor and SDI-like replay simulator.

## Backend trials

Two isolated Git worktrees implement the same frame and scene contract:

| Backend | Implementation | Build system | Dependencies |
|---|---|---|---|
| wgpu | Rust with native Vulkan interoperability | Cargo, built through Nix | Nix |
| Dawn | C++ with native shared GPU resources | CMake and Clang | Nix |

Use the same source assets and correctness criteria for both trials. Temporary
SRT test listeners bind localhost on separate ports. Only the selected, verified
backend becomes the private-network playback service. A successful hardware
decode probe alone is not evidence that a complete compositor avoids CPU video
frame transfers.

## Required pipeline

```text
MP4 -> FFmpeg hardware decode -> uncompressed GPU images
    -> SDI-like frame pacing, source identity, and continuous timecode
    -> native wgpu composition -> GPU images
    -> FFmpeg hardware encode -> MPEG-TS/SRT
    -> Windows SRT player
```

Decoded video pixels must not be downloaded to host memory. GPU-local copies
and GPU color conversion are allowed. Compressed packets, timestamps, scene
configuration, and CPU orchestration are not subject to that restriction.
Uploading source artwork and cached masks is separate from transferring video
frames to the CPU.

The compositor must integrate FFmpeg libraries and native wgpu in one process,
with explicit Vulkan resource lifetime, image-layout, and synchronization
handling. An ordinary FFmpeg `rawvideo` pipe cannot transport GPU image handles
and is not an acceptable substitute.

The selected input mode is software simulation, not physical SDI. It preserves
the source raster rather than claiming conformance to a standard SDI video mode.
Timecode identifies a frame; a monotonic frame clock controls playback cadence.
The source frame identity remains separate from the continuously advancing
simulation timecode, including when the MP4 loops.

## Physical SDI versus simulation

FFmpeg can output real SDI through supported hardware, such as a DeckLink output
card, with the corresponding driver, SDK, and FFmpeg device support. Software
alone cannot create an electrical SDI interface. The currently inspected FFmpeg
build does not enable DeckLink.

Physical RP188/ATC timecode insertion, capture, and GPU-resident card I/O require
separate capability checks. A file's `-timecode` option does not establish that
the same timecode is inserted into SDI ancillary data, and an SDI card does not
automatically provide a path that avoids host-memory video frames.

## How SDI and timecode drive logo composition

The renderer consumes captured GPU images, not the electrical SDI signal.
An SDI capture device and its SDK separate active video from ancillary data.
The source adapter must bind the image, its readiness synchronization, and its
captured timecode to the same frame.

```text
SDI active video + RP188/ATC
    -> capture adapter
    -> GPU frame + input timecode + source identity + capture timestamp
    -> join the matching camera transform and occlusion mask
    -> wgpu logo composition
    -> GPU encoder -> SRT -> Windows
```

The software source adapter exposes the same logical frame contract:

| Field | Meaning |
|---|---|
| Stream ID and epoch | Distinguish sources, restarts, and discontinuities |
| Sequence number | Monotonic frame identity within the epoch |
| Input timecode | Frame label, including rate and drop-frame flags |
| Capture PTS and duration | Rational timing, distinct from the timecode label |
| GPU surface and format | Resource reference, dimensions, planes, and colorimetry |
| Readiness synchronization | Completion fence or semaphore and resource ownership |
| Source clip ordinal | Replay-only link to the original frame map |

GPU resource references are process/API resources, not pointers that can simply
be serialized into JSON and used by another process.

Timecode does not determine perspective. In replay, source identity and clip
ordinal select the existing frame-map entry and its Ti row. For example, clip
ordinal 9 selects Ti row 0 in SC1464; ordinals 0 through 8 have no Ti. In a live
SDI deployment, a camera-tracking or estimation system must supply the matching
transform independently. A mask also belongs to one source frame and must not
silently be reused for a different frame.

Preserve the input frame identity through rendering. The capture or simulation
clock controls frame cadence; timecode labels frames. Output scheduling and
processing latency are separate concerns. Timecode can wrap or repeat, so its
text representation alone is not a globally unique lookup key.

SRT transports the encoded result; it does not automatically preserve SDI
ancillary timecode or make a Windows player display it. Debug display requires
an explicit overlay, while machine-readable timecode requires a supported
metadata path or a separate frame/PTS ledger.

A future physical capture adapter must demonstrate a supported direct GPU-memory
path. Optimized pinned-host-memory transfers or hardware marketing labels are
not sufficient evidence that decoded/captured video pixels avoid host RAM.

## Current status

The wgpu backend implements hardware decode, GPU-resident logo composition,
hardware encode, source-PTS association, source looping, simulated timecode,
and SRT output. A 1,500-frame trial crossed a source loop with no timestamp
mismatches; a separate SRT receiver decoded all 150 frames in a bounded test.
Initial uncovered frames were also compared against the original video, not
only against a bypass of the same renderer.

The implementation targets the validated RTX 3090/Vulkan setup. It makes four
GPU-local plane copies per frame; it is not literal zero-copy. Software decoder
probing and CPU video-frame fallback are forbidden. This remains an experimental
replay system, not a production SDI or camera-tracking implementation.

See `docs/architecture.md` for module boundaries and `docs/wgpu-backend.md`
for the memory handoff and detailed commands.

## Quick start

Create a private asset configuration from `config/example.toml`, then:

```sh
nix develop --command python tools/prepare_scene.py \
  --config /private/assets.toml --output /private/prepared
nix build --cores 4 --max-jobs 1
result/bin/chr --scene /private/prepared/scene.json \
  --output 'srt://127.0.0.1:19001?mode=listener&latency=120000&timeout=5000000&linger=1' \
  --status /private/status.json --timecode-overlay
```

The listener waits for a player. Use an explicitly authorized private interface
instead of localhost for Windows playback. Reconnecting after an output error
requires restarting the server, or supervising it with a user service.

## Dependencies

Use Nix for all dependencies; do not install packages with pip or uv.
`flake.lock` pins the package collection. Host NVIDIA user-space libraries must
match the already-installed kernel driver; `nix/host-gpu.sh` exposes only these
driver libraries to Nix programs without adding the host libc to their search
path. It does not replace the system driver or change Home Manager.

```sh
nix develop
```

FFmpeg hardware-decode probe, using an authorized local asset:

```sh
nix develop --command ffmpeg \
  -init_hw_device vulkan=gpu:0 \
  -hwaccel vulkan -hwaccel_device gpu -hwaccel_output_format vulkan \
  -i /path/to/video.mp4 -an -frames:v 3 -f null -
```

Adding `-c:v h264_vulkan` before `-f null -` also exercises hardware encoding.
These probes do not demonstrate wgpu interoperability by themselves.

## Frame and scene contract

The first scene uses the existing SC1464 ground-quad JSON, forward Ti,
frame-map, full-canvas logos, and cached scene-occlusion masks. Preserve the
existing logo assignments and rotations, ordered premultiplied composition,
global opacity of 0.5, and encoded-RGB blending behavior. Frames without Ti
must remain free of artwork.

Transport PTS must be associated with the source frame map; Ti row numbers are
not video frame numbers. Looping, discontinuities, delayed metadata, and
reconnection must not silently attach a stale pose or mask to a frame.

## Windows playback target

The planned output is a single-viewer SRT listener on an explicitly selected
private interface and UDP port 9001. Use a Windows player with SRT support,
such as an appropriate VLC or ffplay build. Ordinary SSH `-L` forwarding is
TCP-only and does not forward SRT's UDP transport.

The initial demo is video-only, matching the source clip. Audio preservation,
SDI I/O, arbitrary live camera estimation, CUDA processing passes, and general
3D scene rendering are future extensions, not implemented capabilities.

## Private assets

Keep videos, logos, masks, camera data, runtime configuration, and logs outside
Git. `.runtime/` is ignored. Do not copy restricted assets into the Nix source
closure or expose an unauthenticated stream on a public interface.
