# Contributor and agent guidance

## Feature worktrees

- Before starting a new feature, create a dedicated branch and Git worktree
  under the main checkout's `.worktree/` directory, one worktree per feature.
- Implement and validate the feature inside that worktree, not the main checkout.
- Keep `.worktree/` ignored by Git. Never discard existing uncommitted changes
  when creating or removing a worktree.

## Architecture and scope

- Keep all rendering in `crates/rdc-core`. Delivery shells in `native/` own
  windows, events, surfaces, and presentation, not separate rendering algorithms.
- Use the same wgpu pipeline for Windows, Linux, and offscreen GPU tests.
- Keep generated PNGs in root `output/`, ignored by Git but retained locally.
  PNG encoding/filesystem I/O belongs in the shell; GPU readback belongs in core.
- This first slice is a native RGB triangle. Browser delivery, scene formats,
  video, meshes, and camera calibration are not implemented.
- `reference/` is private, local-only material. Never stage, upload, redistribute,
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
- Keep Cargo.lock and flake.lock under version control. Prefer narrow changes;
  add regression tests for new behavior rather than introducing abstractions early.

## Coordinate contract

`docs/coordinates.md` is the public implementation contract. Local reference
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
  Dot-source `scripts\dev-env.ps1`; do not change global toolchains or install
  packages silently. Use the vendor GPU driver and DX12 or Vulkan.
- Linux: `nix develop` / `nix build` / `nix flake check` are the reproducible
  development/build gates. Direct Cargo is also supported with system window
  libraries and graphics drivers. See README.md.
- Validate Linux on the designated native Linux GPU host over SSH, not WSL.
  Keep machine-specific connection details out of public project files.
- Nix flakes in Git see tracked files only. Stage explicit project paths before
  Nix checks, never `reference/`. Keep platform build artifacts separate.
- Select high-performance hardware and log the adapter. Software rendering does
  not count as hardware acceptance. Non-NixOS NVIDIA systems may need nixGL.
  Do not assume a GPU exists because compilation passed.

## Required checks

Run on both Windows and Linux; report missing environments as unverified, not
passed. Do not duplicate unrelated trd browser/video gates in this smaller project.

```text
cargo fmt --all -- --check
cargo clippy --locked --workspace --all-targets -- -D warnings
cargo test --locked --workspace
cargo test --locked -p rdc-core --test triangle_gpu -- --ignored --nocapture
cargo run --locked -p rdc-app -- --smoke-test
```

Non-GPU checks also run through `nix flake check`. Rendering changes need the
hardware pixel test, plus an interactive check of resize, minimize/restore,
HiDPI behavior, and Escape/close. `--smoke-test` only checks bounded window
startup/presentation, not full UI acceptance. Over Linux SSH, run automated
CPU/GPU tests only and hand off native UI checks to an interactive device.
Never leave test windows or helper processes running.
