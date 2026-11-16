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
| `<asset-root>/studio` (`studio_data_dir`) | The studio store (layout below) | Inside the asset root, so uploaded files can be sent to the renderer. Back it up; do not edit it by hand while the studio runs. |
| `dir` in `stack.toml` (default `$XDG_RUNTIME_DIR/chr-stack`) | chr-server's control socket `render.sock`, the camera input ring and the program tap | Kept between runs; a RAM-backed runtime directory is fine. |
| `$XDG_RUNTIME_DIR/chr/` | chr-server's worker sockets (`worker-<pid>/` and `tracking-<pid>/`, two GStreamer unixfd sockets each) and its verified copies of loaded assets | Managed by chr-server and removed on shutdown; a later chr-server removes those of a killed one. |

### Studio store

The studio keeps all its data on the server, never in the browser:

| Path | Content |
| --- | --- |
| `matches.json` | Match library and Fusion placement drafts |
| `package-paths.json` | Offline ZIP path per imported package |
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
| `srt` | An interface address, to let players on the LAN receive SRT directly |
| `server_ws`, `monitor_ws`, `webrtc_tcp`, ports | Free local ports; set a page or relay port to 0 to leave it out |
| `size` | The camera's own size, such as `1920x1080`; see [Video size](#video-size) |

An unknown key stops the script ("unknown keys"). This happens when a configuration
from a newer checkout is used with an older `stack.py`: remove the key or update
the checkout.

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

| Size | Camera input ring + program tap in `dir` | Each captured frame (RGBA) |
| --- | --- | --- |
| 1280x720 | about 44 MB | 3.7 MB |
| 1920x1080 | about 100 MB | 8.3 MB |
| 3840x2160 | about 400 MB | 33 MB |

`dir` usually lives in `$XDG_RUNTIME_DIR`, a RAM-backed file system of limited
size (`df -h $XDG_RUNTIME_DIR`). 4K is accepted but not yet sustained at 30 fps
with the software camera simulation on the development host (see the
measurements in
[live process implementation](live-process-implementation.md#resolutions-and-performance)).
Run one stack per GPU: while Resizable BAR is off, a second Vulkan process on the
same GPU, such as a test stack or a benchmark, is slowed down by orders of
magnitude in its frame uploads and readbacks (#144 follow-up).

## Launch and operate

From the repository root of the main checkout, outside a Nix shell:

```sh
uv run server/scripts/stack.py start      # builds, starts and waits until frames flow
uv run server/scripts/stack.py status     # processes, sockets, health and URLs
uv run server/scripts/stack.py stop
uv run server/scripts/stack.py restart
uv run server/scripts/stack.py restart pitch-studio monitor-page   # only these components
```

The components are `render` (chr-server, both workers, the camera simulator and
chr-monitor), `srt-relay`, `pitch-studio` and `monitor-page`.
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
| `render` log: `NVENC is unavailable …; encoding with x264` | The monitor runs without the NVIDIA driver libraries; keep `nixgl = true`. The preview still works, at more CPU. |
