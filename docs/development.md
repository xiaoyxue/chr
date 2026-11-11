# Development environment and repository layout

## Directory ownership

The root flake is the single Linux development entry point for the TypeScript
client and Rust renderer. The renderer flake has already moved to the root;
do not initialize a second flake or relocate its Cargo workspace. Existing
module boundaries remain unchanged:

```text
flake.nix                 Shared development tools, renderer builds, and checks
flake.lock                Pinned Nix inputs for the whole repository
AGENTS.md                 Repository-wide contributor rules
README.md                 Project overview and quick start
client/src/               TypeScript client sources (currently a placeholder)
render/Cargo.toml         Rust workspace definition
render/Cargo.lock         Locked Rust dependencies
render/rust-toolchain.toml Renderer Rust toolchain selection
render/crates/chr-core/   Shared coordinate math, GPU setup, and rendering
render/native/chr-app/    Native window and headless delivery shell
render/docs/              Renderer-specific contracts
render/scripts/           Renderer-specific development helpers
common/src/               Shared communication protocols (stack undecided)
algorithm/src/            Scene reconstruction algorithms (stack undecided)
docs/                     Repository-wide documentation
reference/                Private, local-only reference materials
output/<module>/          Generated runtime artifacts, including output/render/
.worktree/                Dedicated feature worktrees
```

Keep module sources inside their owning module, not at the repository root.
Use `docs/` for cross-module development and architecture documents, and
`render/docs/` for renderer-specific contracts. Do not move private reference
materials into documentation, source files, fixtures, or Nix build inputs.
No client framework, shared-protocol implementation language, or algorithm
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
| Rust renderer | Rust/Cargo, rustfmt, and Clippy from `render/rust-toolchain.toml` via the locked Rust overlay |
| Build and maintenance | Git, pkg-config, and nixfmt |
| Renderer runtime | Vulkan loader, OpenGL, X11, Wayland, and keyboard/window libraries |

Use Nix for Linux development dependencies, not global npm tool installation.
Node.js and TypeScript come from the existing pinned nixpkgs input. Keep
`flake.lock` and `render/Cargo.lock` tracked, and do not upgrade their revisions
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

`client/src/` is currently a placeholder. The environment supplies its language
tools, not a client application: there is no `package.json`, `tsconfig.json`,
client build command, or client test suite yet. Do not run bare `tsc` or invent
`npm run` scripts as repository validation. Add project configuration with the
actual client implementation, and keep it within `client/`.

Run Cargo commands from `render/` after entering the root shell. When starting
in `render/`, use `nix develop ..`. The shell does not change the working
directory automatically. Windows native renderer setup remains documented in
`render/README.md`; this flake defines Linux outputs for x86_64 and aarch64.
Validate on the actual target rather than assuming an output declaration proves
that platform works.

## Builds and checks

Run these commands from the repository root:

```sh
nix flake check --no-update-lock-file
nix build --no-update-lock-file
nix fmt -- --check flake.nix
```

The default package and named `chr-app` package still build the renderer.
The default application runs it; no placeholder client package is advertised.
`nix flake check` includes:

| Check | Coverage |
| --- | --- |
| `client-toolchain` | Node.js, npm, and TypeScript version commands execute in a Nix build sandbox |
| `fmt` | Rust workspace formatting |
| `clippy` | Rust workspace and all targets, with warnings treated as errors |
| `tests` | Rust workspace CPU tests; hardware GPU tests remain explicitly ignored |

The toolchain smoke check is not a client typecheck or application test.
To run renderer checks directly, enter `nix develop`, then:

```sh
cd render
cargo fmt --all -- --check
cargo clippy --locked --workspace --all-targets -- -D warnings
cargo test --locked --workspace
```

GPU acceptance is separate from build success. From the root, run the packaged
application with `nix run . -- --headless`; non-NixOS systems may need the
host-driver wrapper documented in `render/README.md`. Select real hardware,
inspect the logged adapter, and use the documented GPU pixel test for rendering
changes. SSH supports headless validation, not interactive window acceptance.
Do not treat software rendering or a successful compilation as hardware proof.

## Generated files and private inputs

The root Nix shell sets `CARGO_TARGET_DIR` to the absolute path
`<worktree>/render/target/linux`. Moving between the root and `render/` does not
create a second Cargo cache. Plain Cargo outside the shell uses `render/target/`
by default; keep platform build artifacts separate.

Root `result` / `result-*` symlinks are Nix build outputs. Runtime artifacts
belong under root `output/<module>/`. Renderer PNGs go to
`output/render/triangle.png` when launched from the root or `render/`; outside
a checkout the packaged app uses that path relative to its working directory.
Keep future client runtime artifacts in `output/client/`. These outputs,
`node_modules/`, and TypeScript `.tsbuildinfo` caches are ignored by Git.

The renderer's Nix build source includes only its filtered Cargo workspace and
WGSL shaders, excluding private references, worktrees, caches, and generated
outputs. Adding client development tools does not add client sources or their
future dependencies to the renderer package.

Git-backed flakes see tracked files only. Stage explicit project paths before
validating new files; never stage `reference/`, private assets, credentials,
runtime outputs, or dependency caches. Keep this document, root `README.md`,
and `AGENTS.md` synchronized when changing layout or development commands.
