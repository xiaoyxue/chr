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
client/example/           Example clients and harnesses, each its own npm package
Cargo.toml                Rust workspace definition (members under server/, common/)
Cargo.lock                Locked Rust dependencies
rust-toolchain.toml       Rust toolchain selection
server/                   Live render server; see server/AGENTS.md
server/renderer/          chr-renderer: coordinate math, GPU setup, and rendering
server/runtime/           chr-server: headless render server with IPC and video outputs
server/viewer/            chr-app: native window and headless delivery shell
server/docs/              Server-specific contracts
server/scripts/           Server-specific development helpers
common/                   chr-common: JSON-RPC 2.0 messages and local IPC (Rust)
algorithm/src/            Scene reconstruction algorithms (stack undecided)
docs/                     Repository-wide documentation
reference/                Private, local-only reference materials
output/<module>/          Generated runtime artifacts, including output/renderer/
.worktree/                Dedicated feature worktrees
```

Keep module sources inside their owning module, not at the repository root.
Use `docs/` for cross-module development and architecture documents, and
`server/docs/` for server-specific contracts. Do not move private reference
materials into documentation, source files, fixtures, or Nix build inputs.
The shared protocol is implemented in Rust (`common/`), with TypeScript
mirrors in clients. No client framework or algorithm
technology stack is chosen by this environment setup.

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
| Build and maintenance | Git, pkg-config, and nixfmt |
| Renderer runtime | Vulkan loader, OpenGL, X11, Wayland, and keyboard/window libraries |

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

The default package and named `chr-app` package build the renderer viewer, and
`chr-server` builds the headless render server. The default application runs the
viewer; no client package is advertised.
`nix flake check` includes:

| Check | Coverage |
| --- | --- |
| `client-toolchain` | Node.js, npm, and TypeScript version commands execute in a Nix build sandbox |
| `fmt` | Rust workspace formatting |
| `clippy` | Rust workspace and all targets, with warnings treated as errors |
| `tests` | Rust workspace CPU tests; hardware GPU tests remain explicitly ignored |

The toolchain smoke check is not a client typecheck or application test.
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
filtered Rust sources and WGSL shaders under `server/` and `common/`, excluding
private references, worktrees, caches, and generated outputs. Client sources and
their dependencies are not part of the Rust packages.

Git-backed flakes see tracked files only. Stage explicit project paths before
validating new files; never stage `reference/`, private assets, credentials,
runtime outputs, or dependency caches. Keep this document, root `README.md`,
and `AGENTS.md` synchronized when changing layout or development commands.
