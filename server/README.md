# chr server

The live render server for **Windows and Linux**: a Rust + wgpu renderer and the
code that runs in the same process with it. The Cargo workspace is at the
repository root; its server crates live here:

| Directory | Crate | Role |
| --- | --- | --- |
| `renderer/` | `chr-renderer` | Coordinate math, GPU initialization, and rendering; no networking or device dependencies |
| `runtime/` | `chr-server` | Headless render server: JSON-RPC control, render pipeline, simulated SDI output, SDI-to-WebRTC monitor |
| `viewer/` | `chr-app` | Thin winit shell for windows, resize, and presentation, plus headless PNG export |
| `docs/` | | Server contracts, such as `docs/coordinates.md` |
| `scripts/` | | Windows development helpers |

The current slice draws an RGB triangle using the same pipeline in a native
window, an offscreen hardware test, and the render server. No scene
serialization, video input, or calibration pipeline is included yet.

The Linux renderer roadmap is described in
[Simplified renderer scope and design](docs/renderer-design.md)
([Chinese](docs/renderer-design.zh-CN.md)). It distinguishes the initial
pinhole/persistent-scene contract from later camera and lighting extensions;
these are design targets, not implemented features.

## M1: camera, indexed meshes and depth

The shared `MeshRenderer` now draws static scene instances and material-part
colours with camera depth. This is an opaque **unlit diagnostic baseline**, not
PBR lighting, GLB loading, shadows or the interactive socket-controlled demo.
The normal viewer/server triangle paths remain unchanged.

From the repository root, render the synthetic pitch and grounded test models:

```sh
nix develop
cargo run --locked -p chr-app --example mesh_milestone
```

The example saves `output/renderer/milestone-m1.png` and requires hardware GPU
rendering. On a non-NixOS NVIDIA host, use the nixGL wrapper documented below:

```sh
NIXPKGS_ALLOW_UNFREE=1 nix run --impure github:nix-community/nixGL#nixGLNvidia -- \
  cargo run --locked -p chr-app --example mesh_milestone
```

This is an offline core validation artifact, not a new client media stream or
an alternative control channel.

## Render server

`chr-server` renders the triangle offscreen (1280x720p50 by default) and takes
JSON-RPC 2.0 commands (`triangle.start_rotation`, `triangle.stop_rotation`,
`triangle.get_state`, `sdi.get_status`, `webrtc.offer`) over a NativeSocket at
`$XDG_RUNTIME_DIR/chr/render.sock` and a loopback WebSocket at
`ws://127.0.0.1:8787`. The renderer's only product is the SDI program output;
clients receive that output back as WebRTC:

```text
args (JSON-RPC) -> control -> pipeline: render -> SDI frame (UYVY 4:2:2, BT.709 limited)
                                 -> sdi: program output port (own clock, bounded queue,
                                         repeat on underrun)
                                      -> monitor: SDI -> H.264 -> WebRTC -> clients
```

| Module (`runtime/src/`) | Role |
| --- | --- |
| `control.rs` | Control plane: JSON-RPC methods (the args) onto the pipeline and status |
| `pipeline.rs`, `scene.rs` | Render loop in logical frame time; produces SDI frames |
| `sdi.rs` | SDI output port, simulated until hardware is chosen |
| `monitor/` | Encodes what the port transmitted and streams it over WebRTC |

The monitor encodes in software (openh264) after a CPU readback; this is a
development shortcut, not the intended GPU-to-hardware-encoder path. From the
repository root:

```sh
cargo run --locked --release -p chr-server -- --help
cargo run --locked --release -p chr-server -- --webrtc-tcp 127.0.0.1:8788
```

`--webrtc-tcp` adds ICE-TCP candidates on four consecutive loopback ports so a
browser on another machine can connect through an SSH port forward. The browser
viewer and verification scripts are in `client/example/render-harness/`.

Every run also saves a 960x640 sRGB image to repository-root
`output/renderer/triangle.png`, from any directory in the checkout. Outside a
checkout, the packaged app uses `output/renderer/triangle.png` under the working
directory. The image is retained locally and replaced on the next run; root
`output/` is ignored by Git. To export without opening a window (including Linux
SSH), run from the repository root in the development environment:

```sh
cargo run --locked -p chr-app -- --headless
```

## Windows (PowerShell 7, no Nix)

Install Rust with rustup (MSVC host), Visual Studio Build Tools with
**Desktop development with C++** and a Windows SDK, and your vendor GPU driver.
Then, from the repository root:

```powershell
. .\server\scripts\dev-env.ps1
cargo run --locked -p chr-app
```

The helper only prepares the current shell; it never installs tools or changes
global Rust settings. The default wgpu adapter selection requests high performance.
To select a backend explicitly:

```powershell
$env:WGPU_BACKEND = 'dx12' # or 'vulkan'
cargo run --locked -p chr-app
Remove-Item Env:WGPU_BACKEND
```

## Linux

Nix is both a development shell and a build system. The single `flake.nix` and
`flake.lock`, the Cargo workspace (`Cargo.toml`, `Cargo.lock`), and the Rust
toolchain file (`rust-toolchain.toml`) live at the repository root. Preserve the
lockfiles unless intentionally updating inputs. From the repository root:

```sh
nix develop
cargo run --locked -p chr-app
```

Or build, check, and run the packaged application from the repository root:

```sh
nix build
nix flake check
nix run . -- --headless
```

Use `nix run` without `--headless` for the native window. The default package,
named `chr-app` package, default app, Nix formatter, and formatting/Clippy/test
checks remain available.

Nix flakes in Git only include tracked files. Add new project files explicitly
before a build; never add the private root `reference/` directory. The build
source is restricted to the root Cargo manifests and the Cargo sources and `.wgsl`
shaders under `server/` and `common/`; private references, generated outputs,
caches, and worktrees are excluded. The shell sets an absolute `CARGO_TARGET_DIR`
pointing to `target/linux`, so changing directories does not create another
cache. Cargo outside this shell uses `target/` by default. Root Nix `result` /
`result-*` links, root `output/`, and root `target/` are ignored.

Plain Cargo also works without Nix. Install Rust stable, a C/C++ linker,
pkg-config, X11/Wayland development libraries, and a working Vulkan/OpenGL driver.
For example, on Ubuntu:

```sh
sudo apt-get install build-essential pkg-config libx11-dev libxcursor-dev \
  libxi-dev libxrandr-dev libwayland-dev libxkbcommon-dev libvulkan1 libegl1
cargo run --locked -p chr-app
```

Run the window under a graphical desktop session. Linux acceptance runs on a
native Linux GPU host, not WSL; SSH is sufficient for offscreen tests and PNG
export. Inspect the logged adapter rather than accepting a software renderer.
On non-NixOS Linux, Nix may need nixGL to reach host drivers; for NVIDIA,
inside `nix develop`:

```sh
NIXPKGS_ALLOW_UNFREE=1 nix run --impure github:nix-community/nixGL#nixGLNvidia -- \
  cargo run --locked -p chr-app
```

Use the corresponding nixGL wrapper for your GPU. DX12 is Windows-only;
Vulkan is the normal native Linux backend.

For the built package, run the wrapper from the repository root:

```sh
NIXPKGS_ALLOW_UNFREE=1 nix run --impure github:nix-community/nixGL#nixGLNvidia -- \
  ./result/bin/chr-app --headless
```

## Checks and controls

Run Cargo checks from the repository root inside the development shell:

```sh
cargo fmt --all -- --check
cargo clippy --locked --workspace --all-targets -- -D warnings
cargo test --locked --workspace
cargo test --locked -p chr-renderer -- --ignored --nocapture
cargo run --locked -p chr-app -- --smoke-test
```

The GPU tests check real offscreen pixels, RGB orientation, coverage, aspect,
and rotation in landscape and portrait targets, including padded-row readback,
plus the UYVY conversion against BT.709 reference values; they are ignored by
ordinary CPU-only tests. PNG export uses this same readback path and pipeline.
The smoke test exits after three presented frames and requests a resize.
Interactively verify resize, minimize/restore, display scaling, and Escape/close.
Escape or the window close button exits the normal demo.

## Coordinates and contribution rules

See `docs/coordinates.md` and `AGENTS.md` in this directory. World coordinates
are pitch-centred metres with **negative Z upwards**. Forward row-major Ti maps
ground coordinates to SNC, which uses `sqrt(width * height)` and native pixel
centres without a half-pixel shift. The GPU boundary alone flips image Y to
wgpu's Y-up NDC.

The triangle uses synthetic ground geometry and identity Ti. Local `reference`
materials and measurements are private, ignored by Git, and not redistributed.
