# chr

## Project Structure

| Directory | Purpose | Language |
| --- | --- | --- |
| `client/src/` | Client-side code | Primarily TypeScript |
| `render/` | Rendering workspace: shared renderer in `crates/chr-core/`, native application in `native/chr-app/` | Primarily Rust |
| `common/src/` | Shared communication protocols, such as socket protocols | To be decided |
| `algorithm/src/` | Scene reconstruction algorithms | To be decided |
| `flake.nix`, `flake.lock` | Shared, pinned Linux development environment and renderer build/check entry points | Nix |
| `docs/` | Repository-wide documentation, including `docs/development.md`; renderer contracts remain in `render/docs/` | |
| `reference/` | All reference materials (private, local-only, ignored by Git) | |
| `output/<module>/` | Generated runtime artifacts, including renderer images in `output/render/` (local-only, ignored by Git) | |

## Design Documentation

See [product and system design](docs/README.md) for the evolving business
workflow, proposed module boundaries, and open decisions. These documents are
discussion drafts, not implemented interfaces or a frozen protocol.

## Development

Create a dedicated branch and Git worktree under `.worktree/` before implementing a new feature. Make feature changes and run validation in that worktree, not in the primary working tree.

From the repository root:

```powershell
git worktree add -b feature/my-feature .worktree/my-feature main
```

See `AGENTS.md` for contributor guidelines and `docs/development.md` for the
directory layout, toolchain ownership, and complete development workflow.

Run rendering workspace commands from `render/`. See `render/README.md` for setup and usage, and `render/AGENTS.md` for rendering-specific guidelines.

## Linux development and builds

The single root `flake.nix` and `flake.lock` provide Node.js/npm, TypeScript
(`tsc`), the Rust toolchain, and renderer libraries in one development shell.
They also retain the renderer package and checks. Tool versions come from the
existing locked inputs; the Cargo workspace and Rust toolchain file stay in
`render/`. No client framework or algorithm stack is selected.

From the repository root:

```sh
nix develop
node --version
npm --version
tsc --version
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
the default app runs it. Checks include Node.js/npm/TypeScript tool availability,
Rust formatting, Clippy, and CPU tests. `client/src/` is currently a placeholder:
there is no client build/test script or TypeScript project configuration yet.
`nix fmt` formats Nix files. GPU runs need a working hardware driver; see
`render/README.md` for non-NixOS host-driver wrappers.

The Nix shell uses an absolute `render/target/linux` Cargo cache, unchanged by
switching between the root and `render/`. Root `result` / `result-*` links and
`output/` are ignored. Renderer PNGs go to root `output/render/triangle.png`
from either directory. Outside a checkout, the packaged app writes to
`output/render/triangle.png` under its working directory. Build sources include
only the filtered renderer workspace, excluding private references, worktrees,
caches, and generated outputs. `node_modules/` and TypeScript incremental caches
are also ignored. Never stage private files.
