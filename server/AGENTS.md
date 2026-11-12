# Server contributor and agent guidance

`server/` is the live render server: the renderer and everything that runs in its
process. Repository-wide rules are in the root `AGENTS.md`.

## Feature worktrees

- Before starting a new feature, create a dedicated branch and Git worktree
  under the main checkout's `.worktree/` directory, one worktree per feature.
- Implement and validate the feature inside that worktree, not the main checkout.
- Keep `.worktree/` ignored by Git. Never discard existing uncommitted changes
  when creating or removing a worktree.

## Architecture and scope

- Layout: `renderer/` (`chr-renderer`) is the GPU rendering library; `runtime/`
  (`chr-runtime`) builds the `chr-server` render server; `viewer/` (`chr-app`) is the
  native window and headless shell. Further server crates sit
  beside them as `server/<name>/`, as members of the root Cargo workspace.
- Keep all rendering in `renderer/`. It has no networking, video-device, or
  output-card dependencies. Shells such as `viewer/` own windows, events,
  surfaces, and presentation, not separate rendering algorithms.
- Start new server functionality as modules of an existing crate; split out a
  crate when it brings heavy dependencies or needs its own lint exception, such
  as hardware FFI.
- Runtime ownership: `src/input/` acquires video; `src/calibration/` consumes
  calibration and associates it with frames; `src/program/` assembles demo
  scenes and bridges admitted frames into rendering; `src/sdi.rs` owns the
  simulated output port; `src/monitor/` encodes transmitted slots and fans them
  out to WebRTC/SRT. Shared GStreamer helpers remain private to the runtime.
  Keep algorithm estimation in `algorithm/`; replay calibration is not a live
  tracking service or a public distortion protocol.
- Use the same wgpu pipeline for Windows, Linux, and offscreen GPU tests.
- Keep generated PNGs in repository-root `output/renderer/`, ignored by Git but retained locally.
  PNG encoding/filesystem I/O belongs in the shell; GPU readback belongs in `renderer/`.
- The renderer's only product is the SDI program output. `runtime/` streams to
  clients exactly what the SDI port transmits (render -> SDI -> encoder ->
  WebRTC); do not add a second, render-direct client stream. The server's roles
  are the control plane (args over JSON-RPC), the render pipeline and its clock,
  the SDI output, and the monitor that encodes the output for clients.
- The default runtime renders an RGB triangle. `--synthetic-program` explicitly
  connects retained scenes and synthetic images to the same simulated SDI port;
  monitor encoding uses software after CPU readback. See `docs/program-output.md`.
  The renderer library separately supports retained meshes, cameras, PBR,
  shadows, IBL, diagnostics and native image composition. Its offscreen examples
  do not establish capture, SDI hardware or hardware-encoder interoperability.
  Keep synthetic integration separate from production-policy acceptance.
- Outputs must not block each other or the render loop: hand frames to their
  own threads through bounded queues and drop instead of waiting.
- The repository root's `reference/` (`../reference/` from `server/`) holds
  all private, local-only reference material. Never stage, upload, redistribute,
  or embed its assets or experimental data in public fixtures. Use synthetic data.

## Rust conventions

- Use Rust 2024, standard rustfmt, snake_case functions/modules, UpperCamelCase
  types, and SCREAMING_SNAKE_CASE constants.
- Prefer small modules, explicit ownership, borrowed inputs, and typed errors.
  Return `Result` for fallible operations; no unchecked input or runtime `unwrap`.
  Tests and proven static invariants may use `expect`/`unwrap`.
- Inherit workspace dependencies and lints in every crate. Unsafe code is
  forbidden. Do not suppress warnings to bypass checks.
- Document public coordinate units, matrix direction/layout, and GPU invariants.
  Comments explain non-obvious reasons, not the code's mechanics.
- Use English for code, comments, and documentation. Keep any existing paired
  translations synchronized if either version changes.
- Keep root `Cargo.lock` and `flake.lock` under version control. Prefer narrow changes;
  add regression tests for new behavior rather than introducing abstractions early.

## Coordinate contract

`server/docs/coordinates.md` is the public implementation contract. Local reference
documents informed it; their private measurements are not part of this project.

- World coordinates are pitch-centred metres: X length, Y width, ground Z=0,
  upwards **negative Z**. Camera height is `-Cz`.
- Ti is forward, row-major, acting on column vectors: ground XY to image SNC.
  Never silently transpose, invert, or renormalize imported coefficients.
- SNC uses `sqrt(width * height)`, not image height. Native pixel centres have
  no added half-pixel offset. Pixel/SNC Y points down.
- Elevated points require a world camera centre and A(C); never discard Z.
  Multiply homogeneous intermediates before the final divide. Reject invalid
  dimensions, nonfinite geometry, singular Ti, and undefined projections.
- Convert SNC to wgpu's Y-up NDC only at the rendering boundary; NDC depth is
  in `[0, 1]`, not world Z. The triangle's constant depth is not a camera model.
- Do not infer frame identity from Ti row order or reconstruct timestamps.

## Platforms

- Windows: PowerShell 7, Rust MSVC x64, Visual Studio C++ tools and Windows SDK.
  Dot-source `server\scripts\dev-env.ps1`; do not change global toolchains or install
  packages silently. Use the vendor GPU driver and DX12 or Vulkan.
- Linux: run `nix develop` / `nix build` / `nix flake check` and Cargo commands
  from the repository root, where the Cargo workspace lives. The shell uses an
  absolute `target/linux` cache regardless of working directory. Direct Cargo is
  also supported with system window libraries and graphics drivers. See
  `server/README.md`.
- Validate Linux on the designated native Linux GPU host over SSH, not WSL.
  Keep machine-specific connection details out of public project files.
- Nix flakes in Git see tracked files only. Stage explicit project paths before
  Nix checks, never the repository root's `reference/`. Keep platform build artifacts separate.
- Select high-performance hardware and log the adapter. Software rendering does
  not count as hardware acceptance. Non-NixOS NVIDIA systems may need nixGL.
  Do not assume a GPU exists because compilation passed.

## Required checks

Run on both Windows and Linux; report missing environments as unverified, not
passed. Do not duplicate unrelated trd browser/video gates in this smaller project.

From the repository root inside the development shell:

```text
cargo fmt --all -- --check
cargo clippy --locked --workspace --all-targets -- -D warnings
cargo test --locked --workspace
cargo test --locked -p chr-renderer -- --ignored --nocapture
cargo run --locked -p chr-app -- --smoke-test
```

Non-GPU checks also run through root `nix flake check`. Rendering changes need the
hardware pixel test, plus an interactive check of resize, minimize/restore,
HiDPI behavior, and Escape/close. `--smoke-test` only checks bounded window
startup/presentation, not full UI acceptance. Over Linux SSH, run automated
CPU/GPU tests only and hand off native UI checks to an interactive device.
Never leave test windows or helper processes running.
