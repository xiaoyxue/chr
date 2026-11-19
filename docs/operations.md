# Onsite operations guide

How to launch and operate the onsite stack (render server, studio, monitor), and
where every file belongs. Placeholders such as `<asset-root>` stand for paths you
choose on the onsite server; keep machine-specific paths and hosts out of
committed files and put them only in your local `output/stack/stack.toml`.

For the processes and their channels see the diagrams in the root
[README](../README.md#runtime-processes). The design of each part is in the
[server README](../server/README.md), the
[render description](../server/docs/render-description.md) and
[development environment](development.md).

## Where things live

| Location | Holds | Rules |
| --- | --- | --- |
| Main checkout, on `main` | The code the onsite stack runs | Run the stack from the main checkout. Use a `.worktree/<name>/` checkout only to try a branch, and stop it there afterwards. |
| `<checkout>/output/stack/` | `stack.toml` (your configuration), `<component>.log`, `<component>.pid` | Per checkout and ignored by Git. `stack.py` only sees and stops what its own checkout started. |
| `<checkout>/output/<module>/` | Generated artifacts, such as renderer PNGs in `output/renderer/` | Ignored by Git; delete freely. |
| `<checkout>/output/pitch-studio/` | Studio data when `studio_data_dir` is not set | For development only; set `studio_data_dir` onsite. |
| `<checkout>/target/linux/` | Cargo build cache | Each checkout and worktree has its own. |
| `<checkout>/reference/` | Private reference material | Never stage, upload or embed it. |
| `<asset-root>` (`asset_root`, chr-server `--asset-root`) | Everything chr-server reads at runtime: Fusion offline ZIPs, logo and flag images, GLB models and the studio data directory | Outside the repository. Every file a description names must be an absolute path inside it, otherwise `description.load` rejects it. |
| `<checkout>/asset/ibl/` (`ibl_dir`) | OpenEXR panoramas the studio's 场景灯光 (in 广告位预览) offers for image-based lighting | The `asset` submodule (`git submodule update --init asset` inside `nix develop`); with `asset_root` set, the stack lets chr-server read it too. |
| `<asset-root>/studio` (`studio_data_dir`) | The studio store (layout below) | Inside the asset root, so uploaded files can be sent to the renderer. Back it up; do not edit it by hand while the studio runs. |
| `dir` in `stack.toml` (default `$XDG_RUNTIME_DIR/chr-stack`) | chr-server's control socket `render.sock`, chr-sdi-in's capture sockets `capture.sock` and `capture.sock.audio` (frames and audio as memfds) and the program tap | Kept between runs; a RAM-backed runtime directory is fine. |
| `$XDG_RUNTIME_DIR/chr/` | chr-server's worker sockets (`worker-<pid>/` and `tracking-<pid>/`, two GStreamer unixfd sockets each) and its verified copies of loaded assets | Managed by chr-server and removed on shutdown; a later chr-server removes those of a killed one. |

### Studio store

The studio keeps all its data on the server, never in the browser:

| Path | Content |
| --- | --- |
| `matches.json` | Match library and Fusion placement drafts |
| `package-paths.json` | Offline ZIP path per imported package |
| `lighting.json` | The global lighting presets and the preset each match uses |
| `matches/<match>/local-ads.json` | Onsite placements: pitch-local and point-cloud 2D/3D ads and flags |
| `matches/<match>/session.json` | Imported server paths, point-cloud confirmations, selections |
| `matches/<match>/exports/` | Default folder for the studio's exports |
| `files/<sha256>.<ext>` | Uploaded materials and offline ZIPs, named by content |

Onsite placements are stored per match and apart from the Fusion package:
importing the same match's ZIP again keeps them. Every document carries a
revision; when two pages edit the same data, the later save is refused and the
page reloads the current copy. Files in `files/` are never deleted automatically.

### Asset rules

- chr-server registers an asset by its ID and SHA-256. A known ID with other
  content is rejected as a conflict, so give changed content a new ID or file
  name; do not overwrite a registered file in place.
- Registrations last until chr-server restarts: at most 64 assets and 1 GiB.
  When either is reached, restart the `render` component.
- Keep files where placements reference them. A moved or deleted file can no longer
  be sent; the studio shows the reason in its list.
- Flag artwork and local materials are referenced by server path. A file picked
  from the operator's computer is uploaded into `files/` first.

## Prerequisites

- A Linux host with a hardware GPU driver. On non-NixOS NVIDIA hosts keep
  `nixgl = true`; the stack then runs GPU processes through nixGLNvidia.
- Nix with flakes and `uv` (or Python 3.11 or newer) on the host. Run the stack
  commands outside any Nix shell; the script enters the shells it needs.

### Host checklist

Check every render host before it goes onsite:

| Check | Command | Expected |
| --- | --- | --- |
| Render GPU and driver | `nvidia-smi` | The intended GPU (the RTX 3090 on the development host) with a working driver |
| Resizable BAR enabled (#146) | `sudo lspci -vv -s <gpu-bus-id> \| grep "BAR 1:"` and `grep EnableResizableBar /proc/driver/nvidia/params` | BAR 1 larger than 256MB, `EnableResizableBar: 1` |
| GPU clocks under light load | `nvidia-smi -q -d CLOCK` while the stack runs | Not stuck at idle clocks; otherwise lock the minimum core clock, e.g. `sudo nvidia-smi -pm 1 && sudo nvidia-smi -i 0 -lgc 1500,2100` (`server/README.md`) |
| Room for the frames | `df -h $XDG_RUNTIME_DIR` and `free -h` | More free space than the [Video size](#video-size) table needs |
| SRT port reachable from viewers | `sudo ufw status` | The `srt` UDP port is allowed from the viewers' addresses |
| DeckLink card and driver (`chr-sdi-in --source decklink`, `chr-server --output decklink`, #154) | `lspci \| grep -i blackmagic`, `ls /dev/blackmagic/`, `lsmod \| grep blackmagic` | The card on the bus, its device nodes, and the `blackmagic`/`blackmagic_io` modules of Desktop Video loaded |
| DeckLink API and firmware | `ls /usr/lib/libDeckLinkAPI.so` (or the distribution's path) and `BlackmagicFirmwareUpdater status` | Desktop Video's `libDeckLinkAPI.so` installed in the version the firmware needs |
| GStreamer sees the card | `nix develop -c gst-device-monitor-1.0 Video` | A `DeckLink` source and sink per sub-device (8 on a Duo 2); without them chr fails with "no DeckLink device" |
| Nothing else holds the card | `ps -eo pid,args \| grep -E "MediaExpress\|gst-launch\|decklink"` | No Media Express or other DeckLink pipeline: sub-devices are exclusive |
| Genlock reference | The card's reference input (Desktop Video Setup: "Reference": locked) | Black burst or tri-level from house sync, locked |

**DeckLink.** Verified on a DeckLink Duo 2 at 1080p30 without a reference
input ([GStreamer core](../server/docs/gst-core.md#on-a-card-decklink-duo-2-1080p30));
genlock and the other modes are still to be verified on site (#161). Install
Blackmagic Desktop Video (its kernel modules and `libDeckLinkAPI.so`; on NixOS
the `decklink` module and package), check the rows above, then run the input
and the server with the card:

```sh
chr-sdi-in --source decklink --device 1 --mode auto --connection sdi
chr-server --description <json> --output decklink --device 3 \
  --program-delay-ms 300 --input-loss black ...
```

- **Sub-devices.** `--device` is the card's logical sub-device
  (`device-number`), not the label of its connector, and the numbering can
  change after re-cabling. Find the input with `mode=auto` (chr-sdi-in logs
  the detected format) and the output with a test picture on the monitor.
- **The API library.** The Nix shells' GStreamer has the `decklink` plugin
  (gst-plugins-bad); it loads Desktop Video's `libDeckLinkAPI.so`, which Nix's
  glibc does not look for in the host's `/usr/lib`. The shells and the
  `chr-server` package programs put the flake's `chr-decklink-api` on
  `LD_LIBRARY_PATH`: a link to just `/usr/lib/libDeckLinkAPI.so` (Debian,
  Ubuntu), not the host's whole `/usr/lib`.
- **The source's format.** The card locks only on the mode the source sends;
  `mode=auto` follows it. chr-server opens the output card in the input's
  format (HD and UHD; no SD): it waits for `chr-sdi-in`'s first frame, so the
  output is exactly the input's rate and scan, for a real SDI input as for a
  clip (`chr-sdi-in --video`). `--size` must be the input's extent. The card
  keeps that mode for the whole run: after the source changes its format,
  chr-server logs a warning and converts until it is restarted.
  `--output-rate fixed:<rate>` converts to another mode on purpose. A PC through an
  HDMI-to-SDI converter may send 3G-SDI Level B at 50/60 Hz, which the card
  detects but does not lock: use 30 Hz or a Level A source. The output sends
  1080p50, 1080p59.94 and 1080p60 in Level A, which monitors and converters
  expect.
- **Timing.** The output card's clock times the whole pipeline. Lock its
  reference input to house sync before going on air: a free-running card
  drifts from the source by parts per million, so the delay grows by up to a
  frame before one is dropped. D must exceed 24 ms (the card gets each frame
  20 ms before air).
- **Audio.** Embedded audio passes by default; `--no-audio` on both
  `chr-sdi-in` and `chr-server` leaves it out when the program needs none.
- To verify on site: playout with genlock, the card's start (the core primes
  it with a black frame and silence), RP 188 timecode, captions (line 9) and
  lip sync.

To enable Resizable BAR on an NVIDIA host (needs a reboot; stop the stack first):

```sh
echo "options nvidia NVreg_EnableResizableBar=1" | sudo tee /etc/modprobe.d/nvidia-rebar.conf
sudo update-initramfs -u
sudo reboot
```

If BAR 1 still reports 256MB, enable "Above 4G Decoding" and "Re-Size BAR Support"
in the BIOS (older boards may need a BIOS update). Until it is enabled, run only one
stack or GPU benchmark per GPU.

## Configure

```sh
mkdir -p output/stack
cp server/scripts/stack.example.toml output/stack/stack.toml
```

Edit `output/stack/stack.toml`; the example documents every key.

| Key | Set to |
| --- | --- |
| `scene`, `video` | The tracked-camera manifest and its clip |
| `description` | Optional startup description, such as an empty scene |
| `asset_root` | `<asset-root>`; without it the studio cannot send |
| `studio_data_dir` | `<asset-root>/studio` |
| `ibl_dir` | Optional: another folder of OpenEXR panoramas; default `<checkout>/asset/ibl` |
| `srt` | An interface address, to let players on the LAN receive SRT directly |
| `server_ws`, `monitor_ws`, `webrtc_tcp`, ports | Free local ports; set a page or relay port to 0 to leave it out |
| `size` | The camera's own size, such as `1920x1080`; see [Video size](#video-size) |
| `tracking`, `tracking_cli`, `tracking_worker_args`, `tracking_centre` | The tracking worker; see [Tracking worker](#tracking-worker) |
| `program_delay_ms` | The program delay D: every frame leaves this long after its capture; see [Program delay and output](#program-delay-and-output) |
| `input_loss`, `output_rate`, `audio` | What fills the output while the input is lost, the output's rate, and the clip's audio; see [Program delay and output](#program-delay-and-output) |

An unknown key stops the script ("unknown keys"). This happens when a configuration
from a newer checkout is used with an older `stack.py`: remove the key or update
the checkout. A renamed key is still read with a warning: `tracking_wait_ms` is
now `program_delay_ms` (#154); rename it, since both together are refused.

### Video size

chr-server renders and transmits at `size`, from 1080p up to DCI 4K
(4096x2160, which covers 4K UHD 3840x2160); 1280x720 works too. Onsite the camera
is 1080p. Both sides must be even and have the camera's aspect ratio (the
`width`/`height` of the description and the manifest). Render at the camera's
own size: for the 1080p camera set

```toml
size = "1920x1080"
```

and restart `render`. A smaller size of the same aspect, such as `1280x720`,
scales the composed program down. The SDI port and the program tap carry `size`;
the monitor preview (WebRTC and SRT) is scaled down to at most 1920x1080 and
encoded with NVENC when the GPU offers it (`stack.py` runs it through nixGL), else
with x264. The `render` log names the choice: `monitor preview 1920x1080 of
1920x1080p30 with nvh264enc`.

| Size | Program tap in `dir` | Each captured frame (RGBA memfd) | Frames in flight at D = 800 ms, 30 fps (RGBA + clean feed) |
| --- | --- | --- | --- |
| 1280x720 | about 15 MB | 3.7 MB | about 0.15 GB |
| 1920x1080 | about 33 MB | 8.3 MB | about 0.3 GB |
| 3840x2160 | about 133 MB | 33 MB | about 1.3 GB |

`dir` usually lives in `$XDG_RUNTIME_DIR`, a RAM-backed file system of limited
size (`df -h $XDG_RUNTIME_DIR`); the captured frames are memfds, in RAM but not
in `dir`. 4K is accepted but not yet sustained at 30 fps
with the software camera simulation on the development host (see the
measurements in
[live process implementation](live-process-implementation.md#resolutions-and-performance)).
Run one stack per GPU: while Resizable BAR is off, a second Vulkan process on the
same GPU, such as a test stack or a benchmark, is slowed down by orders of
magnitude in its frame uploads and readbacks (#146; see the
[host checklist](#host-checklist)).

### Tracking worker

With `tracking = "worker"` (the default), chr-server hands every camera frame
to its tracking worker and renders each frame with the `Ti` it returns
([tracking worker](../server/docs/tracking-worker.md)).

- Without `tracking_cli`, the worker is the `chr-tracking-sim-cli` stand-in.
  It replays the manifest's recorded `Ti`.
- `tracking_cli` names another worker that speaks vchao's VchaoFrame contract,
  such as vchao's `pitch-analysis`. Its `tracking_worker_args` give its map and
  settings; `{stream_id}` in them becomes each session's stream ID, for example
  in `--ledger`. It sends no lens, so its `Ti` is rendered with the pinhole camera.
- `tracking_centre` is the station's optical centre `[x, y, z]`. Without it,
  the description's or manifest's reference camera is used.
- The lens follows the tracking data
  ([lens](../server/docs/tracking-worker.md#lens)): a worker's own lens (the
  stand-in sends its manifest's), else `tracking_lens_k`, `tracking_lens_centre`
  and `tracking_lens_scale` (all three, plus an optional `tracking_lens_support`),
  else none (pinhole). The description's or manifest's lens is never applied.
  `tracking_lens = "pinhole"` ignores every lens, for debugging.
- `program_delay_ms` sets how long the worker may take; see
  [Program delay and output](#program-delay-and-output).
- `tracking = "external"` runs no worker: `chr-sdi-in --send-cameras` sends the
  manifest's recorded cameras as `tracking.frame` (testing only).

`stack.py status` names the worker and its current stream, counts the
returned poses (fresh, interpolated, reused, unavailable and overdue) and shows
the camera model of the last rendered frame with its lens source, for example
`pinhole (lens source: none)` for `pitch-analysis` or `radial (lens source:
worker)` for the stand-in, which sends its manifest's lens but not its
per-frame warps. Frames
without a camera, such as while the worker loads its model or restarts, go out
as clean feed: the camera picture without CG, at the same delay.

### Program delay and output

chr-server runs every tracked program on its GStreamer core
([GStreamer core](../server/docs/gst-core.md)): `chr-sdi-in` plays the clip
(`video`) at its own rate, or captures a DeckLink card, and every frame leaves
the program delay after its capture, with its CG if that was ready in time, else
as its clean feed (the camera picture without CG). The output follows the
input's rate.

- `program_delay_ms` (D): the default, three frame periods, suits the stand-in.
  `pitch-analysis` may hold a frame up to its 500 ms overload threshold: set
  about 800 ms. Above 4 and at most 1500 ms. Every frame in flight holds its
  picture in memory ([Video size](#video-size)).
- `input_loss`: what the output shows while the input delivers nothing:
  `"hold"` (the last frame, default), `"black"`, `"slate"` (colour bars) or
  `"slate:/path/to.png"`. The first frame after a loss leaves at its own D.
- `output_rate`: `"follow"` (default) or `"fixed:<rate>"` such as `"fixed:50"`;
  a fixed rate repeats or drops input frames.
- `audio`: `false` drops the clip's audio; by default it leaves with its frame's
  delay.

`stack.py status` shows the core: D, CG / clean-feed / empty slots, the CG ratio
since start and over the last 10 s, the per-slot slack of the CG slots (how long
before its deadline each frame's CG arrived; near 0 means D is too short), the
render budget at submit and the capture-to-output delay, input-loss and
format-change slots. It reports a `PROBLEM` when fewer than half the slots of
the last 10 s had CG.

### Run with vchao tracking

vchao's `pitch-analysis` estimates every frame's `Ti` from a map of the venue
([tracking worker](../server/docs/tracking-worker.md#running-vchaos-pitch-analysis)).

1. Build it once per checkout (about 7 GB in the Nix store; the first build
   downloads PyTorch and CUDA):

   ```sh
   git submodule update --init external/vchao
   nix build ./external/vchao#pitch-analysis --out-link output/vchao/pitch-analysis
   ```

2. Put the map (`manifest.json` with its Parquet files) under `<asset-root>`
   and create a directory for its ledgers, for example `<asset-root>/runs`.
3. In `stack.toml` (relative paths are from the repository root):

   ```toml
   tracking = "worker"
   # On NixOS the wrapper is not needed: name pitch-analysis directly.
   tracking_cli = "server/scripts/with-cuda-driver.sh"
   tracking_worker_args = [
       "output/vchao/pitch-analysis/bin/pitch-analysis",
       "--asset-manifest", "<asset-root>/map/manifest.json",
       "--batch-frames", "8", "--estimate-every", "4", "--interpolation", "logh",
       "--overload-ms", "500", "--ledger", "<asset-root>/runs/{stream_id}.arrow",
   ]
   tracking_centre = [14.52857216404143, 60.620266710663365, -9.91514362632655]
   program_delay_ms = 800
   nixgl = true
   ```

   `tracking_centre` is the station's optical centre of that map; the value
   above is the SC1464 fixture's. The description must match the camera's size
   and rate; its lens is not applied to vchao's `Ti`, which `stack.py status`
   shows as `camera model pinhole (lens source: none)`.
4. `uv run server/scripts/stack.py restart render`. The worker is `ready`
   about 10 s later (model and map loading); until then the output carries
   clean feed. Its log lines, such as `analysis-listening` and, at stop,
   `analysis-complete`, are in `output/stack/render.log`.

The ledgers grow with every frame (about 90 bytes per frame plus about 3 KB
per estimate) and a restarted worker starts a new pair; remove old ones from
time to time.

**The live N16ROT tracker.** vchao's `pitch-track-live` (in the pin since
`209a97b`) speaks the same VchaoFrame contract and estimates frames as they
come, one per batch, on the GPU; it needs an N16ROT map (an official-N16
manifest is refused). It is the worker of the DeckLink run
([GStreamer core](../server/docs/gst-core.md#on-a-card-decklink-duo-2-1080p30):
18.7 fresh `Ti`/s at 1080p30, capture to estimate p95 82 ms, so D 300 ms):

```sh
nix build ./external/vchao#pitch-track-live --out-link output/vchao/pitch-track-live
```

```toml
tracking_cli = "server/scripts/with-cuda-driver.sh"
tracking_worker_args = [
    "output/vchao/pitch-track-live/bin/pitch-track-live",
    "--asset-manifest", "<asset-root>/map/manifest.json",
    "--batch-frames", "1", "--estimate-every", "1", "--interpolation", "logh",
    "--overload-ms", "200", "--ledger", "<asset-root>/runs/{stream_id}.arrow",
]
program_delay_ms = 300
```

## Launch and operate

From the repository root of the main checkout, outside a Nix shell:

```sh
uv run server/scripts/stack.py start      # builds, starts and waits until frames flow
uv run server/scripts/stack.py status     # processes, sockets, health and URLs
uv run server/scripts/stack.py stop
uv run server/scripts/stack.py restart
uv run server/scripts/stack.py restart pitch-studio monitor-page   # only these components
```

The components are `render` (`chr-sdi-in`, the capture input playing the clip;
chr-server with its GStreamer core and both workers; chr-monitor), `srt-relay`,
`pitch-studio` and `monitor-page`.
- **First start:** builds the release binaries and both pages. This takes minutes; later starts reuse the builds.
- **Readiness:** `start` prints `ready: …` as each part comes up, and `PROBLEM: …` for whatever is still wrong.
- **Logs:** in `output/stack/<component>.log`.
- **`status`:** marks a process whose binary was rebuilt since it started as `OLD BUILD`.

Restarting `render` interrupts the program output for about a minute. Restarting
`pitch-studio` or `monitor-page` does not touch the output.

### Update to a new version

```sh
git pull                                                  # in the main checkout
uv run server/scripts/stack.py restart pitch-studio monitor-page   # client-only changes
uv run server/scripts/stack.py restart                    # server changes; interrupts output
```

Both pages are built when they start and served by `vite preview`, so they change
only after a restart. Force-reload the browser afterwards (Ctrl+Shift+R).

## Remote access

The pages, chr-server and chr-monitor listen on loopback. Reach them from another
machine through the SSH command that `stack.py status` prints, for example:

```sh
ssh -N -o ServerAliveInterval=30 -o ServerAliveCountMax=3 \
  -L 4173:127.0.0.1:4173 -L 5173:127.0.0.1:5173 -L 8787:127.0.0.1:8787 \
  -L 8789:127.0.0.1:8789 -L 8790:127.0.0.1:8790 -L 8791:127.0.0.1:8791 \
  -L 8792:127.0.0.1:8792 -L 8793:127.0.0.1:8793 <user>@<host>
```

- **Use a dedicated tunnel:** prefer it to an editor's automatic port forwarding, which can make each new connection slow. Do not forward the same ports twice.
- **Keepalives:** they stop the network from dropping an idle tunnel.
- **SRT:** goes to `srt` directly over UDP. Behind SSH, set `srt_relay_port` and play `tcp://127.0.0.1:<srt_relay_port>`.
- **Loopback only:** the studio's store and file endpoints answer only loopback requests from the studio page itself. Do not expose the pages on a LAN address.

## Operating the studio

**Before the match:** import the Fusion offline ZIP by its server path inside
`<asset-root>`. Its planar ad placements can be prepared and adjusted in advance.

**Onsite:**
1. Import the point cloud and analysis result by server path.
2. Confirm that the point cloud shares the pitch coordinates.
3. Place point-cloud flags and other onsite placements. Flags need the 3D plane, so they can only be placed onsite.
4. Save each placement (保存属性 / 保存旗帜属性). The page warns before leaving with unsaved edits.
5. Send with **发送到渲染器**. It replaces chr-server's whole scene with every sendable placement:
   - Fusion placements need the offline ZIP path;
   - a point-cloud placement needs its cloud confirmed, not loaded.
6. Back up or move the onsite placements with **导出现场广告位** and **导入现场广告位**. Exports are written to a server path, by default `matches/<match>/exports/`.

**What a send does:** the page sends a small description that names each asset by its path and SHA-256. The page and chr-server keep one control connection open, so after the first send of a page, sends take about a second; the status shows the time spent checking materials and sending.
- **Assets new to chr-server:** chr-server reads, verifies and imports them, then replaces its render worker, which reloads every registered asset. This takes one to a few seconds, longer for large models. The output repeats frames meanwhile.
- **Known assets:** reused without reading files or restarting the worker.

## Troubleshooting

| Symptom | Cause and action |
| --- | --- |
| Page reloads by itself or loses unsaved edits | The data is on the server; reload the page. Use the keepalive tunnel above; Chrome may also discard a background tab. |
| 「正在发送」 takes long | The first send of a page opens the connection; later sends reuse it. If the status shows several seconds for 发送, check the tunnel. |
| Send rejected: asset registered with other content | A file changed under a known asset ID. Use a new file or ID, or restart `render`. |
| Send rejected: path outside `--asset-root` | Move the file into `<asset-root>`. |
| Send rejected: capacity | 64 assets or 1 GiB registered; restart `render`. |
| 「另一个页面修改了…」 | Another page saved first; the page reloaded the current copy. Repeat the change. |
| `start` reports a port taken | Another checkout or process holds it; `stack.py status` names it. Stop it from the checkout that started it. |
| Changes not visible after an update | Restart `pitch-studio` / `monitor-page`, then force-reload the browser. |
| `OLD BUILD` in `status` | Restart that component to run the new binary. |
| Many clean-feed frames or deadline misses after raising `size` | The render worker is slower than a frame at that size, or another process uses the GPU. Check `stack.py status`, stop other GPU processes, or use a smaller `size`. |
| A benchmark or test stack is hundreds of times slower than expected | Another Vulkan process uses the same GPU while Resizable BAR is off (#146). Stop the other process or enable Resizable BAR ([host checklist](#host-checklist)). |
| `render.log`: `with-cuda-driver.sh: no libcuda.so.1` or the estimator finds no CUDA device | `pitch-analysis` runs outside nixGL; keep `nixgl = true`, or on NixOS name `pitch-analysis` directly as `tracking_cli`. |
| Many frames without camera, or the tracking worker restarts with "holds N frames" | The worker returns frames later than the program delay. Raise `program_delay_ms` (at most 1500), or check the worker's log in `output/stack/render.log`. |
| Low CG ratio in `status`, slack near 0 | Frames come back from tracking or rendering just before their deadline: raise `program_delay_ms`, or look for a slow render worker (render budget) or another GPU process. |
| `status`: input loss slots growing, `capture from … lost; reconnecting` in `render.log` | `chr-sdi-in` stopped or lost its signal; it is restarted by `run-tracked.sh`. Check its lines in `render.log` (a missing clip, FFmpeg errors). The output keeps its cadence meanwhile (`input_loss`). |
| `stack.py` warns `tracking_wait_ms is now program_delay_ms` | Rename the key in `stack.toml`. |
| `chr-server`: `--tracking-wait-ms was removed`, `--input-ring was removed` or `--pipeline was removed` | A launcher of an older version: use `--program-delay-ms`, and `chr-sdi-in` with `--capture-socket` (#154). |
| `render` log: `NVENC is unavailable …; encoding with x264` | The monitor runs without the NVIDIA driver libraries; keep `nixgl = true`. The preview still works, at more CPU. |
