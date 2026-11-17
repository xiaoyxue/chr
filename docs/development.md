# Development environment and repository layout

## Directory ownership

The root flake is the single Linux development entry point for the TypeScript
client and Rust server. Both the flake and the Cargo workspace live at the
repository root; do not initialize a second flake or a second Cargo workspace.
Module boundaries:

```text
flake.nix                 Shared development tools, renderer builds, and checks
flake.lock                Pinned Nix inputs for the whole repository
AGENTS.md                 Repository-wide contributor rules
CLAUDE.md                 Imports AGENTS.md for Claude Code (also server/CLAUDE.md)
README.md                 Project overview and quick start
client/src/               TypeScript client sources (currently a placeholder)
client/pitch-studio/      Onsite pitch client (Vite + Three.js), built by the flake
client/example/           Example clients and harnesses, each its own npm package
Cargo.toml                Rust workspace definition (members under server/, monitor/, common/)
Cargo.lock                Locked Rust dependencies
rust-toolchain.toml       Rust toolchain selection
server/                   Live render server; see server/AGENTS.md
server/renderer/          chr-renderer: coordinate math, GPU setup, and rendering
server/tracker/           chr-tracker: tracking logic without processes, GStreamer or async runtime
  src/protocol.rs         vchao's VchaoFrame v1 mirror, chr's lens/frame_warp extensions, validation
  src/pose.rs             A returned pose as its frame's camera and render geometry
  src/lens.rs             Camera-model selection: tracking data's lens > CLI lens > pinhole
  src/join.rs             Frame/camera join by capture sequence until each frame's deadline
  src/replay.rs           The replay stand-in's poses and lens from a calibrated-video-v1 manifest
server/runtime/           chr-runtime: chr-server (render server), its workers chr-renderer-cli (also
                          stills) and chr-tracking-sim-cli (replay stand-in for the tracking module),
                          and chr-sdi-in (the capture input of the GStreamer core, #154; file,
                          test pattern or DeckLink; --send-cameras for recorded cameras)
  src/input/              The --software-video program's in-process MP4 input
  src/calibration/        Calibrated clip lens replay; the renderer's lens and geometry checks
  src/program/            Demo scenes and admitted-frame rendering bridge
  src/sdi.rs              Generated programs' simulated SDI port and internal clock; output status
  src/pipeline.rs         Generated programs' render loop (triangle, synthetic, pitch demo, video)
  src/worker/             Worker transport (GStreamer unixfd), render worker protocol,
                          the worker side and its supervisor
  src/worker/tracking/    Tracking worker supervisor and VchaoFrame on GStreamer buffers
  src/gst_core/           GStreamer core of every tracked program: capture, per-slot selector,
                          CG driver, output, program tap and telemetry; chr-sdi-in's input pipeline
server/viewer/            chr-app: native window and headless delivery shell
server/docs/              Server-specific contracts
server/scripts/           Server-specific development helpers
monitor/                  chr-monitor: program-tap encoding and WebRTC/SRT fan-out
common/                   chr-common: JSON-RPC 2.0 messages and local IPC (Rust)
common/shm/               chr-shm: the shared-memory program tap; the only crate with unsafe
docs/                     Repository-wide documentation
external/vchao/           Git submodule: Hong-Xiang/vchao (soccer video VPP, Python), pinned
asset/                    Git submodule: xiaoyxue/chr-assets (Git LFS), large binary assets
reference/                Private, local-only reference materials
output/<module>/          Generated runtime artifacts, including output/renderer/
.worktree/                Dedicated feature worktrees
```

Keep module sources inside their owning module, not at the repository root.
Use `docs/` for cross-module development and architecture documents, and
`server/docs/` for server-specific contracts. Do not move private reference
materials into documentation, source files, fixtures, or Nix build inputs.
The shared protocol is implemented in Rust (`common/`), with TypeScript
mirrors in clients. No client framework is chosen by this environment setup.

Live tracking runs in a tracking worker that `chr-server` supervises
(`server/runtime/src/worker/tracking/`, [tracking worker](../server/docs/tracking-worker.md)). Its
protocol is vchao's language-neutral VchaoFrame raw-frame contract, so the real
tracking module, vchao's `pitch-analysis`, replaces the `chr-tracking-sim-cli`
replay stand-in without changing the server. chr's tracking logic is the
pure-logic crate `chr-tracker`, the renderer's sibling: the contract, the pose
to camera mapping, the lens selection, the join of frames with cameras by
capture sequence and the replay stand-in's logic. It validates geometry with
the renderer's own checks, which `chr-runtime` supplies, without depending on
the GPU renderer. `chr-runtime` keeps the processes, their GStreamer transport
and the wiring. The `calibration/replay` module is only a startup-loaded lens
replay for a validated clip. Offline scene
modelling (scan video to calibration results) stays outside this repository.
A general public distortion contract and physical SDI adapters are not
implemented by this layout.

### External repositories

`external/` holds other projects' repositories as Git submodules, each pinned to
a commit; today only `external/vchao` (Hong-Xiang/vchao, branch `main`). Its URL in
`.gitmodules` is relative, so it uses the same protocol (HTTPS or SSH) as the chr
clone. Both repositories are private: cloning the submodule needs read access to
vchao.

```sh
git clone --recursive https://github.com/xiaoyxue/chr.git
git submodule update --init external/vchao     # an existing clone or a new worktree
git submodule update --remote external/vchao   # move the pin to vchao's latest main
```

- **No local edits:** never edit files inside `external/<name>` here. Change the
  upstream repository, then move the pin in a chr pull request that commits the new
  submodule commit.
- **Worktrees:** each worktree initializes only the submodules it needs. Git keeps
  their data under the main checkout's `.git/`.
- **Builds stay independent:** builds and checks do not depend on submodules.
  `nix flake check`, Cargo and the pitch-studio checks pass whether or not
  `external/` is initialized, and the Nix build sources exclude it.
- **Privacy:** keep vchao's code, data and models in its own repository. Do not copy
  them into chr fixtures or documentation.
- **Using vchao's programs:** build them from the submodule with vchao's own flake,
  outside chr's checks, for example the tracking worker:
  `nix build ./external/vchao#pitch-analysis --out-link output/vchao/pitch-analysis`
  ([tracking worker](../server/docs/tracking-worker.md#running-vchaos-pitch-analysis)).

### Asset repository

`asset/` is the private repository `xiaoyxue/chr-assets` as a Git submodule (relative
URL, branch `main`). It holds chr's own large binary assets in Git LFS, today
`asset/ibl/`: 4K equirectangular OpenEXR environment maps for image-based lighting,
with their sources and licences in its README. They stay out of chr itself because
Nix copies every tracked file of chr into the flake source on each evaluation; Git
LFS does not prevent that, and `export-ignore` does not either, while submodules are
left out.

```sh
nix develop                                  # git-lfs and its filter
git submodule update --init asset            # about 450 MB
```

The dev shell sets the LFS filter through `GIT_CONFIG_*` variables, so nothing in
your Git configuration depends on `git-lfs`. Run Git commands that read or change
`asset/` inside `nix develop`; outside it they would check out LFS pointers. The
submodule is `ignore = dirty`, so `git status` in chr works anywhere. Do not run
`git lfs install` in chr: it would put LFS hooks into chr's shared `.git/hooks` and
a filter into your global configuration, and Git would then fail outside the shell
wherever `git-lfs` is not installed. To change the assets themselves, run
`git lfs install --local` inside `asset/`.

Builds and checks never need `asset/`. chr-server reads its files at runtime when
`asset/` is one of its asset roots.

`chr-server` creates no GPU device. As decided in #94 and implemented in #97, it
starts and supervises a `chr-renderer-cli worker` process that renders every
frame, and transmits clean feed while that worker is late, failed or being
replaced. #95 moved video monitoring
into the separate `chr-monitor` process: `chr-server` writes every transmitted
SDI slot into a shared-memory program tap that `chr-monitor` encodes for viewers.
`monitor/` is a root crate, not a server crate, because it is an optional process
that may run on another host from an SDI loop-out.

## Start a feature

From the primary checkout, create a dedicated branch and worktree:

```sh
git fetch origin
git worktree add -b feature/my-feature .worktree/my-feature origin/main
cd .worktree/my-feature
nix develop
```

Make feature changes and run validation inside that worktree. Never discard
unrelated work or develop the feature in the primary checkout.

## Shared Linux tools

The root development shell provides:

| Area | Tools |
| --- | --- |
| TypeScript client | Node.js, its bundled npm, and TypeScript (`tsc`) |
| Rust server | Rust/Cargo, rustfmt, and Clippy from `rust-toolchain.toml` via the locked Rust overlay |
| Build and maintenance | Git, Git LFS, pkg-config, and nixfmt |
| Renderer runtime | Vulkan loader, OpenGL, X11, Wayland, and keyboard/window libraries |
| Worker transport | GStreamer core and base libraries and the bad plugins (`unixfdsink`/`unixfdsrc`, 1.28 or later), with `GST_PLUGIN_SYSTEM_PATH_1_0` set; `chr-runtime` links GStreamer and needs them to build, test and run |
| GStreamer core (#154) | `livesync` and `fallbackswitch` copied out of the pinned gst-plugins-rs and `deinterlace` out of gst-plugins-good (the `chr-gst-livesync` derivation, on the same plugin path); the `decklink` elements come with the bad plugins and need Blackmagic Desktop Video on the host to open a card ([operations](operations.md#host-checklist)) |

The optional `nix develop .#media` shell inherits these tools and adds FFmpeg,
the GStreamer ugly plugins, and Python GObject bindings. It runs the video-only
software input helper under `server/scripts/` and builds the opt-in
`gstreamer-media` feature of `chr-runtime` and `chr-monitor`; it does not install
SDI drivers, provide a physical SDI device, or change the renderer protocol. The
bad plugins provide H.264 parsing, MPEG-TS muxing and the direct SRT output of
`chr-monitor`.

Use Nix for Linux development dependencies, not global npm tool installation.
Node.js and TypeScript come from the existing pinned nixpkgs input. Keep
`flake.lock` and `Cargo.lock` tracked, and do not upgrade their revisions
incidentally when changing documentation, directories, or shell packages.

From the repository root:

```sh
nix develop
node --version
npm --version
tsc --version
rustc --version
cargo --version
```

`client/src/` is currently a placeholder with no package or build scripts; do not
run bare `tsc` there or invent `npm run` scripts as repository validation. Example
packages under `client/example/` are self-contained: run `npm install` and their
documented scripts inside each package (see
`client/example/render-harness/README.md`). Keep client configuration within
`client/`.

`client/pitch-studio/` is a self-contained npm package. For development, run
`npm ci` and `npm run dev` (127.0.0.1:5186), `npm run build` or
`npm run test:offline` inside it. Without Fusion, a match can be imported from an
offline Fusion ZIP (`manifest.json` plus `assets/`) in the browser. Every import
(match ZIP, videos, PLY, result folder, ad materials) can be picked locally or
loaded by an absolute path on the onsite server; surrounding whitespace and quotes
(as pasted by "Copy as path") are dropped. The root flake builds it from
`package-lock.json` (see the next section). Its 发送到渲染器 action (on the 广告位预览
and 导入结果与微调 tabs) converts the
placement draft into a render description and loads it into `chr-server` through
the page's `/chr-render` WebSocket proxy (`CHR_RENDER_URL`); see
`server/docs/render-description.md`.

Placement editing in the top view uses pitch-centred world coordinates (+X right,
+Y down), drawn as axes with metre ticks, the pitch length and width and its corner
coordinates. A selected placement shows its vertices and opens locked; it must be
unlocked (button or `L`) before it can be dragged, resized from its corners or
edited numerically: 2D areas by their top-left corner or centre, with an aspect
ratio that can be locked or set (`16:9`, `1.78`), 3D models by centre and one
uniform scale. Outlines are the placement slots; a selected 3D model also shows
its real XY bounding box.
Rotation is read-only. Each target can be enabled or disabled, and every draft
change can be undone with the 撤销 button or `Ctrl/Cmd+Z` until the match is
reloaded.

`uv run server/scripts/stack.py start|stop|restart|status` starts and stops the
whole local development stack (tracked simulation, pitch-studio client, program
monitor page, optional SRT relay) from a local `output/stack/stack.toml`; see
`server/README.md`. It builds both pages and serves them with `vite preview`, so a
dropped SSH tunnel does not reload them; after client changes restart only the pages
(`stack.py restart pitch-studio monitor-page`). Use `npm run dev` for hot reload
while developing a client. The [operations guide](operations.md) covers onsite
launching, remote access and where runtime data lives (asset root, studio store).

The stack's processes and how they connect:

```mermaid
flowchart LR
  subgraph offline["Before the match: offline, outside chr"]
    scan["scan video of the station"]
    build["vchao map building<br/>and centre fit"]
    map[("V/Q map + T_ref<br/>manifest.json")]
    centre[("optical centre C")]
    assets[("asset root: Fusion ZIPs,<br/>logos, flags, GLB models")]
  end
  subgraph camera["Camera: simulated SDI input"]
    sim["chr-sdi-in<br/>+ ffmpeg decoders"]
  end
  subgraph render["render component: run-tracked.sh restarts each process"]
    server["chr-server: GStreamer core<br/>no GPU: per-slot CG or clean feed,<br/>program delay D, output, program tap"]
    tracker["tracking worker<br/>pitch-analysis (vchao, GPU)<br/>or chr-tracking-sim-cli (replay)"]
    renderer["render worker<br/>chr-renderer-cli (GPU)"]
    monitor["chr-monitor<br/>H.264 encoder"]
  end
  studio["pitch-studio page :4173"]
  page["monitor page :5173"]
  relay["SRT relay (srt_relay_port)"]
  player["VLC / ffplay"]

  scan --> build
  build --> map
  build --> centre
  map -- "loaded at start (asset-manifest)" --> tracker
  centre -- "tracking_centre" --> server
  assets -- "files the description names" --> server
  sim -- "frames + audio (unixfd, memfd)" --> server
  server -- "frame N + VchaoFrame (unixfd, memfd)" --> tracker
  tracker -- "same frame N + selected Ti or unavailable (unixfd)" --> server
  server -- "render + same frame N memory + camera (unixfd)" --> renderer
  renderer -- "done + composed SDI frame (unixfd, memfd)" --> server
  server -- "every output slot: program tap (shm)" --> monitor
  studio -- "description.load: ws :8787" --> server
  page -- "status: ws :8787" --> server
  page -- "signalling ws :8789, WebRTC ICE-TCP :8790-8793" --> monitor
  monitor -- "SRT :9001" --> player
  monitor -- "SRT" --> relay
  relay -- "TCP" --> player
```

The same in text, with every channel:

```text
              camera (simulated SDI input; a DeckLink card with --source decklink)
  ┌──────────────────────────────────────────────────────────┐
  │ chr-sdi-in  ◄── ffmpeg decodes the clip at its own rate  │
  └─────┬────────────────────────────────────────────────────┘
  frames│ + audio: unixfd sockets <dir>/capture.sock(.audio), memfd
        ▼
  ┌───────────────────────────────────────────────────────────┐ ws :8787 control
  │ chr-server (no GPU): one GStreamer pipeline               │◄── description.load, status ── pitch-studio :4173
  │ every frame leaves D after capture: its CG, else its own  │    (through its /chr-render proxy)
  │ clean feed; supervises both workers; output (UYVY) on the │◄── status ───────────────────── monitor page :5173
  │ pipeline clock (simulated sink, or a DeckLink card)       │◄── optical centre C (--tracking-centre); files under the asset root
  └───┬─────▲───────────────────┬─────▲─────────────────┬─────┘
      │     │ ①                 │     │ ②               │ ③ program tap (shm)
      ▼     │                   ▼     │                 ▼
  ┌─────────┴────────────┐  ┌─────────┴────────┐  ┌───────────────────────────────────────┐
  │ tracking worker      │  │ render worker    │  │ chr-monitor (≤1080p H.264: NVENC/x264)│── SRT udp :9001 ──► VLC / ffplay
  │ pitch-analysis       │  │ chr-renderer-cli │  └───────────────────┬───────────────────┘   └► stack.py relay tcp ──► players over SSH
  │ (vchao, GPU), or     │  │ (GPU)            │                      ├─ ws :8789 status, WebRTC signalling ──► monitor page
  │ chr-tracking-sim-cli │  │                  │                      └─ WebRTC ICE-TCP :8790-8793, per viewer ─► monitor page
  │ (replay stand-in)    │  │                  │
  └──────────▲───────────┘  └──────────────────┘
             │ ④
  scan video ─► vchao map building (offline, before the match) ─► V/Q map + T_ref (manifest.json), optical centre C

  ① frame N as memfd memory with vchao's VchaoFrame meta ▼; the same frame N with its selected Ti or `unavailable` ▲
  ② `render` carrying the same frame N memory and its camera ▼; `done` carrying the composed SDI frame ▲
  ①② two GStreamer unixfd sockets per worker; each message is JSON in a buffer's GstCustomMeta
  ③ every slot the output sends
  ④ pitch-analysis loads the station's V/Q map at start (--asset-manifest); the replay stand-in reads the clip's manifest instead
```

Each box is one process; the only shared-memory ring left is the program tap
(`chr-shm`), which chr-monitor reads. Frames travel as memfd memory over GStreamer
`unixfdsink`/`unixfdsrc` sockets end to end: chr-sdi-in writes each captured
frame once and chr-server, both workers and the delay queue map that same memory.
chr-server runs one GStreamer pipeline ([GStreamer core](../server/docs/gst-core.md)):
every frame's slot is due its capture time plus the program delay D
(`--program-delay-ms`, `program_delay_ms` in `stack.toml`) and goes out with its CG
if that arrived in time, else with its own clean feed. Every worker message is
JSON in a buffer's `GstCustomMeta`. The tracking hop speaks vchao's VchaoFrame
contract: each frame goes to the tracking worker and comes back with its `Ti`.
Clients such as pitch-studio still talk JSON-RPC to chr-server over WebSocket or
the NativeSocket, unchanged. `chr-tracking-sim-cli` replays the clip's recorded
`Ti` and estimates nothing. vchao's `pitch-analysis` replaces it unchanged
(`chr-server --tracking-cli`, #148); see [tracking worker](../server/docs/tracking-worker.md).
`pitch-analysis` loads a V/Q map built offline for the station
(`--asset-manifest`), and chr-server gets that station's optical centre C
(`--tracking-centre`, `tracking_centre` in `stack.toml`); see
[vchao atlas, V, Ti and T_ref](vchao-atlas.md).
With `tracking = "external"` in `stack.toml`, no tracking worker runs and
`chr-sdi-in --send-cameras` sends each frame's recorded camera as `tracking.frame`
on `<dir>/render.sock`. `run-tracked.sh` restarts chr-sdi-in, chr-server and
chr-monitor independently, so one exiting does not stop the others. Viewers only
ever see what the output sent. Ports are the defaults of
`server/scripts/stack.example.toml`; `stack.py start` and `status` print the ports
each running process actually holds, and `status` the core's D, CG ratio, per-slot
slack and render budget.

Run Cargo commands from the repository root after entering the root shell.
Windows native setup is documented in `server/README.md`; this flake defines Linux outputs for x86_64 and aarch64.
Validate on the actual target rather than assuming an output declaration proves
that platform works.

## Builds and checks

Run these commands from the repository root:

```sh
nix flake check --no-update-lock-file
nix build --no-update-lock-file
nix fmt -- --check flake.nix
```

The default package and named `chr-app` package build the renderer viewer,
`chr-server` builds the headless render server with its workers `chr-renderer-cli`
and `chr-tracking-sim-cli` and the `chr-sdi-in` input (wrapped with the GStreamer
plugin path for the unixfd transport and the GStreamer core), and `chr-monitor` builds the video monitor process. `pitch-studio` builds the onsite client: the static
site in `share/pitch-studio/dist` and a `pitch-studio` launcher. The
default application runs the viewer; `nix run .#pitch-studio` serves the
client.
`nix flake check` includes:

| Check | Coverage |
| --- | --- |
| `client-toolchain` | Node.js, npm, and TypeScript version commands execute in a Nix build sandbox |
| `pitch-studio` | `client/pitch-studio` typecheck, production build and offline-package tests (`npm run test:offline`) from the locked npm dependencies |
| `media-source` | Python tests in `server/scripts/`: synthetic MP4 decoding into a GStreamer appsink, pixel integrity, fractional-rate loop timing and short-input failure; `stack.py` configuration, components and process records |
| `fmt` | Rust workspace formatting |
| `clippy` | Rust workspace and all targets, with warnings treated as errors |
| `tests` | Rust workspace CPU tests, including the GStreamer unixfd worker transport, the tracking worker and the GStreamer core end to end with a solid-frame render worker; hardware GPU tests remain explicitly ignored |

The toolchain smoke check is not a client typecheck or application test. The
`pitch-studio` check typechecks and builds that client and runs its Node
tests, but does not run it in a browser.

The client launcher runs `vite preview` on the built site, including the
read-only Fusion proxy under `/fusion-api/`. Run it from a directory holding the
onsite `.env.local` (see `client/pitch-studio/.env.example`), or set the
variables in its environment; process variables take precedence:

```sh
FUSION_API_URL=https://localhost:50001 FUSION_API_KEY=... \
  nix run .#pitch-studio -- --port 4173
```

It listens on 127.0.0.1 (port 4173 by default). Without both variables the proxy
answers 503. The key is read at runtime only; never put it in `VITE_*` variables
or the Nix store. For a local HTTPS Fusion server, set `NODE_EXTRA_CA_CERTS` to
its certificate first.

Server paths are read by the same process under `/local-files/`, only for
requests to a loopback host (use an SSH port forward from another machine), and
only inside `LOCAL_FILES_ROOTS` (`:`-separated; the home directory by default),
never hidden files or symlinks that leave those folders. It also answers a
file's SHA-256 (`/local-files/sha256`, cached until the file changes), so a send
downloads no material, and writes the studio's JSON exports
(`POST /local-files/write`): absolute `.json` paths only, inside those roots or
`STUDIO_DATA_DIR`, no hidden or relative segments, missing folders created
there, written atomically, at most 64 MB, and an existing file replaced only
after the page asks.

场景灯光 (scene lighting, in the 广告位预览 tab) offers the OpenEXR panoramas of `STUDIO_IBL_DIR` (default
`asset/ibl` of the checkout; the stack's `ibl_dir`), which the page server lists and
hashes through `/local-files/`; `/studio-config` tells the page that folder.

The studio keeps all of its data on the onsite server, not in the browser, in
one directory: `STUDIO_DATA_DIR` (`output/pitch-studio` in the repository for
`npm run dev` / `vite preview`, or under the working directory for the
`pitch-studio` launcher; the stack's `studio_data_dir`). The same process serves
it under `/studio-store/`, for loopback hosts only: `matches.json` (the match
library and Fusion drafts), `package-paths.json` (offline ZIP paths),
`lighting.json` (lighting presets and each match's choice),
`matches/<matchId>/local-ads.json` and `session.json` (placements made onsite and
the operator's session, kept apart from the Fusion package),
`matches/<matchId>/exports/` (the default folder for exports), and
`files/<sha256>.<ext>` (uploaded materials up to 64 MB and offline ZIPs up to 1
GB). Each document carries a revision; a write based on an older one is refused
with 409, so two pages cannot overwrite each other. Every change to the store or an
export write must come from the studio page itself (same-site `Origin`, or
`Sec-Fetch-Site: same-origin`/`same-site`) with an `application/json` body
(`application/octet-stream` for uploads), so another website open in the
operator's browser cannot write through the loopback page server. Keep the directory inside
chr-server's `--asset-root` so the uploaded files can be sent to the renderer.
To run Rust checks directly, enter `nix develop` at the root, then:

```sh
cargo fmt --all -- --check
cargo clippy --locked --workspace --all-targets -- -D warnings
cargo test --locked --workspace
```

GPU acceptance is separate from build success. From the root, run the packaged
application with `nix run . -- --headless`; non-NixOS systems may need the
host-driver wrapper documented in `server/README.md`. Select real hardware,
inspect the logged adapter, and use the documented GPU pixel test for rendering
changes. SSH supports headless validation, not interactive window acceptance.
Do not treat software rendering or a successful compilation as hardware proof.

## Generated files and private inputs

The root Nix shell sets `CARGO_TARGET_DIR` to the absolute path
`<worktree>/target/linux`. Changing directories does not create a second Cargo
cache. Plain Cargo outside the shell uses `target/` by default; keep platform
build artifacts separate.

Root `result` / `result-*` symlinks are Nix build outputs. Runtime artifacts
belong under root `output/<module>/`. Renderer PNGs go to
`output/renderer/triangle.png` when launched from anywhere in the checkout;
outside a checkout the packaged app uses that path relative to its working
directory.
Keep client runtime artifacts in `output/client/` or `output/<harness>/`, such as
`output/render-harness/`. These outputs,
`node_modules/`, and TypeScript `.tsbuildinfo` caches are ignored by Git.

The Rust Nix build source includes only the root Cargo manifests and the
filtered Rust sources and WGSL shaders under `server/`, `monitor/` and `common/`, excluding
private references, worktrees, caches, and generated outputs. Client sources and
their dependencies are not part of the Rust packages.

Git-backed flakes see tracked files only. Stage explicit project paths before
validating new files; never stage `reference/`, private assets, credentials,
runtime outputs, or dependency caches. Keep this document, root `README.md`,
and `AGENTS.md` synchronized when changing layout or development commands.
