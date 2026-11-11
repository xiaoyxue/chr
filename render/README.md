# chr renderer

A Rust + wgpu renderer core for **Windows and Linux**. The first vertical slice
draws an RGB hello-world triangle using the same pipeline in a native window
and an offscreen hardware test.

Every run also saves a 960x640 sRGB image to `output/triangle.png`, relative to
the working directory. Run from `render/` to keep it in `render/output/`.
The image is retained locally and replaced on the next run; all of `output/`
is ignored by Git. To export without opening a window (including Linux SSH):

```sh
cargo run --locked -p chr-app -- --headless
```

`crates/chr-core` owns coordinate math, GPU initialization, and rendering.
`native/chr-app` is a thin winit shell for windows, resize, and presentation.
No browser, video, scene serialization, or calibration pipeline is included yet.

## Windows (PowerShell 7, no Nix)

Install Rust with rustup (MSVC host), Visual Studio Build Tools with
**Desktop development with C++** and a Windows SDK, and your vendor GPU driver.
Then, from the checkout's `render/` directory:

```powershell
. .\scripts\dev-env.ps1
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

Like the reference project's setup, Nix is both a development shell and a build
system. Commit/review the generated flake.lock when updating Nix inputs.
Run these commands from `render/`.

```sh
nix develop
cargo run --locked -p chr-app
# Or build and run the packaged application:
nix build
nix run
nix flake check
```

Nix flakes in Git only include tracked files. Add new project files explicitly
before a build; never add the private `reference` directory. The shell places
Cargo build artifacts in `target/linux`.

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

## Checks and controls

```sh
cargo fmt --all -- --check
cargo clippy --locked --workspace --all-targets -- -D warnings
cargo test --locked --workspace
cargo test --locked -p chr-core --test triangle_gpu -- --ignored --nocapture
cargo run --locked -p chr-app -- --smoke-test
```

The GPU test checks real offscreen pixels, RGB orientation, coverage, and aspect
in landscape and portrait targets, including padded-row readback; it is ignored
by ordinary CPU-only tests. PNG export uses this same readback path and pipeline.
The smoke test exits after three presented frames and requests a resize.
Interactively verify resize, minimize/restore, display scaling, and Escape/close.
Escape or the window close button exits the normal demo.

## Coordinates and contribution rules

See `docs/coordinates.md` and `AGENTS.md`. World coordinates are pitch-centred
metres with **negative Z upwards**. Forward row-major Ti maps ground coordinates
to SNC, which uses `sqrt(width * height)` and native pixel centres without a
half-pixel shift. The GPU boundary alone flips image Y to wgpu's Y-up NDC.

The triangle uses synthetic ground geometry and identity Ti. Local `reference`
materials and measurements are private, ignored by Git, and not redistributed.
