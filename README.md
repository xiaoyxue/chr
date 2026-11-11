# chr

## Project Structure

| Directory | Purpose | Language |
| --- | --- | --- |
| `client/src/` | Client-side code | Primarily TypeScript |
| `render/` | Rendering workspace: shared renderer in `crates/chr-core/`, native application in `native/chr-app/` | Primarily Rust |
| `common/src/` | Shared communication protocols, such as socket protocols | To be decided |
| `algorithm/src/` | Scene reconstruction algorithms | To be decided |
| `docs/` | Project documentation | |
| `reference/` | All reference materials (private, local-only, ignored by Git) | |
| `output/render/` | Generated renderer images (local-only, ignored by Git) | |

## Development

Create a dedicated branch and Git worktree under `.worktree/` before implementing a new feature. Make feature changes and run validation in that worktree, not in the primary working tree.

From the repository root:

```powershell
git worktree add -b feature/my-feature .worktree/my-feature main
```

See `AGENTS.md` for contributor guidelines.

Run rendering workspace commands from `render/`. See `render/README.md` for setup and usage, and `render/AGENTS.md` for rendering-specific guidelines.

## Linux development and builds

The single root `flake.nix` and `flake.lock` provide the renderer's pinned
toolchain, development shell, package, and checks. Other module tooling can
extend this flake; this setup does not select an algorithm stack or include
TypeScript tools yet. The Cargo workspace and Rust toolchain file stay in `render/`.

From the repository root:

```sh
nix develop
cd render
cargo run --locked -p chr-app -- --headless
```

In a separate shell, from the repository root:

```sh
nix build
nix flake check
nix run . -- --headless
```

The default package and `packages.<system>.chr-app` build the renderer;
the default app runs it. Checks include Rust formatting, Clippy, and CPU tests.
`nix fmt` formats Nix files. GPU runs need a working hardware driver; see
`render/README.md` for non-NixOS host-driver wrappers.

The Nix shell uses an absolute `render/target/linux` Cargo cache, unchanged by
switching between the root and `render/`. Root `result` / `result-*` links and
`output/` are ignored. Renderer PNGs go to root `output/render/triangle.png`
from either directory. Outside a checkout, the packaged app writes to
`output/render/triangle.png` under its working directory. Build sources include
only the filtered renderer workspace, excluding private references, worktrees,
caches, and generated outputs. Never stage private files.
