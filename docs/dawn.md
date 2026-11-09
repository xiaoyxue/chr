# Native Dawn experiment

## Status and scope

Implemented and tested on the NVIDIA GeForce RTX 3090 with FFmpeg 8.1.2,
Dawn `66f899221d41dbc6d6257350c4b654a9b375c7f2` (20261106.204713),
Clang 21.1.8, and CMake. The input is MP4 replay, not electrical SDI.
The application rejects non-Vulkan decoded frames and software adapters.
Input metadata comes only from the MP4 container header. The server does not
call `avformat_find_stream_info`, which can otherwise decode software probe
frames before hardware decoder configuration. Missing H.264 codec parameters,
extradata, dimensions, or time base fail closed; there is no frame-probing
fallback for incomplete headers.

The default mode renders the existing prepared scene. `--mode bypass` still
passes video through a Dawn shader but preserves the NV12 bytes.
`--mode marker` additionally writes a 160x80 luma-204 rectangle for an observable
hardware-interoperability test. These diagnostic modes do not represent scene
rendering.

All dependencies are supplied by Nix. The pinned nixpkgs `dawn` package is a
PostScript utility, not WebGPU. This flake therefore imports Google's official
Linux native **prebuilt static Dawn library** using `fetchurl` and a fixed SHA256.
The application itself is compiled with CMake and Clang; this experiment does
not rebuild the upstream Dawn library from source. The upstream CMake package
and actual installed native headers are used, not assumed API signatures.

## Modules and pixel path

| File | Responsibility |
|---|---|
| `native/main.cpp` | Server orchestration, bounded replay/loop, 30 fps clock, identity ledger |
| `native/media.cpp` | MP4 demux, forced Vulkan hardware decode, Vulkan H.264 encode, MPEG-TS/SRT |
| `native/scene.cpp` | Source SHA256 check, exact rational PTS join, artwork and same-frame masks |
| `native/bridge.cpp` | Vulkan/Dawn shared allocations, ownership, layouts, completion fences |
| `native/renderer.cpp` and `scene.wgsl` | Auxiliary uploads and native Dawn GPU composition/conversion |

```text
MP4 compressed packets (CPU)
  -> FFmpeg Vulkan NV12 decode image
  -> GPU image copies of visible Y and UV planes
  -> exported GPU-local R8 and RG8 image memory
  -> Dawn SharedTextureMemoryOpaqueFD / BeginAccess
  -> WGSL encoded-RGB logo composition and packed NV12 R32Uint output
  -> EndAccess / exported binary Vulkan semaphore FDs
  -> GPU image-to-buffer copy, GPU buffer-to-NV12-image copies
  -> FFmpeg h264_vulkan
  -> compressed packets (CPU) -> MPEG-TS file or SRT
```

This is **shared GPU memory plus GPU-local copies**, not a zero-copy claim.
There is no production video CPU mapping, frame download, software decoder
fallback, or software color conversion. The only image uploads are auxiliary
logo mipmaps and masks. Packed video staging memory is `DEVICE_LOCAL` and never
mapped. CPU waits manipulate synchronization, not video pixels.

FFmpeg owns its Vulkan device. Dawn owns a separate Vulkan device on the exact
same physical-device **and driver UUID**. Dedicated opaque-FD allocations are
imported into Dawn once. FFmpeg concurrent frame sharing is explicitly checked;
unsupported exclusive frame ownership is rejected rather than guessed.
Every transfer waits and increments each `AVVkFrame` timeline semaphore under
the frame lock, and updates the public layout/access state. The transfer queue
is locked around submit/wait. The conservative trial waits for each transfer
queue submission before reuse.

External images are released to `VK_QUEUE_FAMILY_EXTERNAL` in `GENERAL` before
`BeginAccess`; the completed transfer provides the initial readiness guarantee.
`EndAccess` exports binary semaphore FDs and the actual old/new layout pair.
The FFmpeg-device transfer queue imports and waits those semaphores, performs
the matching external acquire, and only then consumes the packed shader output.
FFmpeg decoder-reference frames are never modified: a fresh hardware frame
from the pool is used for encoding. Coded padding (1216x656) is kept separate
from the 1212x654 visible raster.

## Scene and identity

The manifest is used read-only. Source compressed-file SHA256 must match its
pin. Source PTS is compared as an exact rational, never rounded into a decoded
frame index. Missing/duplicate/out-of-order frames, unknown PTS, missing masks,
unsupported color matrices, and unsupported frame formats fail closed.
Null Ti entries produce no artwork. Invalid quads are ignored, including
legacy invalid entries with out-of-raster bounds.

The shader preserves all six ordered premultiplied-over layers, full rotated
logo canvases, 2x2 samples at native pixel centers plus/minus 0.25, maximum
singular-value inverse-Jacobian LOD at half derivative, and trilinear mip
interpolation with a transparent border. Opacity 0.5 and same-frame visibility
are applied once after ordered composition.

YUV/RGB conversion is on the GPU. BT.709 and BT.601 matrices and limited/full
range are supported. The source has unspecified color metadata: the documented
fallback is BT.601 limited range, matching the usual FFmpeg/OpenCV decode
assumption, not a claim that the source explicitly declares BT.601. Original
metadata is preserved in the encoder. Pixels not covered by visible artwork
retain their original YUV values; affected 2x2 chroma groups are averaged.
Sampling/chroma interpolation and lossy encoding can differ from OpenCV.
No bit-exact reference equivalence is claimed.

The JSONL ledger records epoch, continuous sequence, 30 fps NDF timecode,
source loop, exact decoded clip PTS/time base, original source PTS,
present index, clip ordinal, Ti row, and output PTS/time base. On replay loops,
clip identity wraps while simulation sequence/timecode continue. The timecode
wraps at 24 hours; epoch/sequence do not. This ledger is not SDI ancillary data
and is not automatically carried/displayed by SRT players.

## Build and run

Run commands in this worktree. In the shared three-pane development session,
invoke them through the session's serialized tmux helper.

```sh
mkdir -p .runtime
nix build --max-jobs 1 --cores 4 --out-link .runtime/result
nix develop --max-jobs 1 --cores 4 --command bash -c \
  'cmake -S . -B build -G Ninja && cmake --build build -j4 && ctest --test-dir build --output-on-failure'

SCENE=/private/prepared/scene.json
./.runtime/result/bin/chr-dawn --scene "$SCENE" \
  --output .runtime/loop.ts --ledger .runtime/loop.jsonl --frames 1500 --loop
./.runtime/result/bin/chr-dawn --scene "$SCENE" \
  --output .runtime/bypass.ts --ledger .runtime/bypass.jsonl --frames 60 --mode bypass
nix develop --command python tools/verify_dawn.py --scene "$SCENE" \
  --output .runtime/loop.ts --ledger .runtime/loop.jsonl --bypass .runtime/bypass.ts
```

`--frames` is mandatory and bounded; without `--loop`, EOF ends processing.
`--pace` uses a monotonic 30 fps clock. Without it, file tests run as fast as
possible. The timing report excludes asset load/device setup, includes encoder
draining, and is not end-to-end playback latency. `cpu_video_transfers: 0`
describes the implemented invariant, not an external PCIe traffic measurement.

The flake's native source closure is allowlisted to CMake and `native/`; private
assets, logs, and outputs remain outside it. The launcher retains the selective
host NVIDIA library bridge; it does not change host drivers or global settings.
If auxiliary assets need preparation, `nix develop .#prepare` supplies the
existing preparer's Python dependencies through Nix; no pip/uv is needed.

## SRT test and Windows

```sh
nix develop --command bash tools/test_dawn_srt.sh "$SCENE"
```

The script starts a bounded user-systemd unit, checks the localhost UDP socket,
receives compressed MPEG-TS, independently decodes/verifies all 150 frames,
and stops the unit on exit. Only `127.0.0.1:19002` is used. It must not replace
an already-running unit. A one-second final drain keeps the last frames
available across the tested 120 ms SRT latency; arbitrary larger latency and
network reconnection are not implemented. Receiver EOF can be reported by
FFmpeg as an SRT input/output error after successful complete delivery.

The H.264 High / 8-bit 4:2:0 MPEG-TS stream is suitable for a Windows VLC or
ffplay build with SRT support. Windows playback itself has not been exercised
on this Linux host. The selected private-network service, if authorized
separately, can use a listener URL on the chosen private IP; a Windows caller
would open `srt://PRIVATE_IP:PORT?mode=caller&latency=120000`.
This experiment leaves no persistent listener and does not advertise port 9001.

## Verification results

- Unit tests: equivalent rational PTS, rejected near-but-not-exact PTS, absent
  PTS, mask seeking/missing masks, missing Ti, independent loop/timecode identity,
  24-hour wrap, rejected non-CFR scene, and incomplete MP4 header rejection passed.
- After removing stream-info frame probing, the rebuilt Nix application
  independently verified **60/60 hardware-processed frames**, 51 with Ti,
  exact timestamps, and unchanged initial no-Ti output.
- Marker shader: three hardware-encoded frames independently decoded; the
  entire 160x80 marker had `YMIN=YMAX=YAVG=204`.
- Full scene plus loop: **1,500 frames**, **1,473 with Ti**, one source wrap,
  **zero PTS mismatches**, **3.470 s / 432.2 fps** in the measured unpaced run.
  Final packaged-build recheck: **3.411 s / 439.7 fps** for the same 1,500 frames.
  Independent decoding confirmed 1,500 H.264 frames at 1212x654 and exact
  1/30-second timestamps.
- The first nine no-Ti frames decode identically to the Dawn bypass pipeline;
  frames with Ti differ. An encoded preview visibly contains the projected
  artwork with scene occlusion.
- Independent first-use check against the **original MP4**, not merely bypass:
  decoded RGB MAE for ordinals 0–8 was **0.6820, 0.8796, 0.8882, 0.8982,
  0.9350, 0.9841, 0.9494, 0.9693, 0.9607** code values. Frame zero visually
  contains the correct soccer scene, not a cleared/green image. Absolute-error
  p99 was 4 for frame zero and 5 for the other eight. Private verification
  images and `first-frames-reference.json` are in mode-700 `.runtime/`.
- Independent CPU-decoded comparison against the authorized 1,482-frame
  `render/logo.mp4`: SSIM **0.988869** overall (Y 0.987295, U 0.991603,
  V 0.992433). This is a **post-lossy-encoding** comparison, not pre-encode
  reference equality or an isolated shader-quality metric.
- Packaged Nix executable: localhost SRT test delivered **150/150 frames**,
  exact cadence and identity, 141 Ti entries. Reported 5.975 s includes the
  additional one-second transport drain; source pacing was 30 fps.
- Vulkan synchronization validation was enabled for a 12-frame run. There
  were **no bridge-specific or synchronization-hazard reports**. Seven unique
  validation IDs concerning video profile/images and leaked image views also
  occur in the untouched FFmpeg hardware decode/encode baseline. Therefore
  this is not described as an entirely validation-clean Vulkan stack.

Known baseline IDs: `VUID-VkImageCreateInfo-pNext-06811`,
`VUID-VkVideoBeginCodingInfoKHR-pPictureResource-07240`,
`VUID-VkVideoBeginCodingInfoKHR-slotIndex-07245`,
`VUID-VkVideoBeginCodingInfoKHR-flags-07244`,
`VUID-vkCmdDecodeVideoKHR-pDecodeInfo-07142`,
`VUID-vkCmdEncodeVideoKHR-pEncodeInfo-08206`,
`VUID-vkDestroyDevice-device-05137`.

The raw validation, marker, acceptance, and transport logs are in ignored
`.runtime/`. The baseline diagnostics remain an upstream/runtime qualification
limit; they have not been suppressed or falsely reported as fixed.
