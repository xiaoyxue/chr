# chr

## Project Structure

| Directory | Purpose | Language |
| --- | --- | --- |
| `client/src/` | Client-side code | Primarily TypeScript |
| `client/example/` | Example clients and verification harnesses, such as `render-harness/` | TypeScript |
| `server/` | Live render server: renderer library in `renderer/` (`chr-renderer`), render server runtime in `runtime/` (`chr-runtime`, building `chr-server`), native window/headless shell in `viewer/` (`chr-app`) | Primarily Rust |
| `Cargo.toml`, `Cargo.lock`, `rust-toolchain.toml` | Root Rust workspace for the crates under `server/` and `common/` | Rust |
| `common/` | Shared communication protocols: `chr-common` JSON-RPC 2.0 messages and NativeSocket/WebSocket IPC | Rust (TypeScript mirrors in clients) |
| `algorithm/src/` | Scene reconstruction algorithms | To be decided |
| `flake.nix`, `flake.lock` | Shared, pinned Linux development environment and renderer build/check entry points | Nix |
| `docs/` | Repository-wide documentation, including `docs/development.md`; server contracts live in `server/docs/` | |
| `reference/` | All reference materials (private, local-only, ignored by Git) | |
| `output/<module>/` | Generated runtime artifacts, including renderer images in `output/renderer/` (local-only, ignored by Git) | |

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

Run Cargo commands from the repository root. See `server/README.md` for setup and usage, and `server/AGENTS.md` for server-specific guidelines.

## Linux development and builds

The single root `flake.nix` and `flake.lock` provide Node.js/npm, TypeScript
(`tsc`), the Rust toolchain, and renderer libraries in one development shell.
They also retain the renderer package and checks. Tool versions come from the
existing locked inputs; the Cargo workspace and Rust toolchain file are at the
repository root. No client framework or algorithm stack is selected.

For the video-only FFmpeg-to-GStreamer software input helper and optional
`chr-server --software-video` simulation (`--features gstreamer-media`), use
`nix develop .#media`; see `server/README.md`. This optional shell adds media
tools without changing the default renderer environment or providing SDI hardware.

From the repository root:

```sh
nix develop
node --version
npm --version
tsc --version
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
Rust formatting, Clippy, and CPU tests. `client/src/` is currently a placeholder;
the example harness in `client/example/render-harness/` has its own npm scripts.
`nix fmt` formats Nix files. GPU runs need a working hardware driver; see
`server/README.md` for non-NixOS host-driver wrappers.

The Nix shell uses an absolute `target/linux` Cargo cache, unchanged by
switching directories. Root `result` / `result-*` links, `target/`, and
`output/` are ignored. Renderer PNGs go to root `output/renderer/triangle.png`
from any directory in the checkout. Outside a checkout, the packaged app writes to
`output/renderer/triangle.png` under its working directory. Build sources include
only the root Cargo manifests and the filtered Rust sources and shaders under
`server/` and `common/`, excluding private references, worktrees,
caches, and generated outputs. `node_modules/` and TypeScript incremental caches
are also ignored. Never stage private files.
