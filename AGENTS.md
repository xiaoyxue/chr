# Repository Guidelines

## Project Structure

- `client/`: Client-side code, primarily written in TypeScript. Source code belongs in `client/src/`; `client/example/` holds example clients and verification harnesses, each a self-contained npm package.
- `server/`: The live render server, primarily Rust. The renderer library lives in `server/renderer/` (`chr-renderer`), the headless render server in `server/runtime/` (`chr-server`), and the native window/headless shell in `server/viewer/` (`chr-app`); further server crates are added as `server/<name>/`. Follow `server/AGENTS.md`.
- `Cargo.toml`, `Cargo.lock`, `rust-toolchain.toml`: The single Rust workspace at the repository root. Its members live under `server/` (and Rust parts of `common/`); run Cargo commands from the root.
- `common/`: Shared communication protocols. `common/` is the Rust crate `chr-common` (JSON-RPC 2.0 control messages and the NativeSocket/WebSocket IPC transports) and is the source of truth; TypeScript clients mirror its types.
- `algorithm/`: Scene reconstruction algorithms. Language and technology stack are undecided. Source code belongs in `algorithm/src/`; do not choose a stack without confirmation.
- `docs/`: Repository-wide documentation. `docs/development.md` defines the
  development environment and directory ownership; server-specific contracts
  live in `server/docs/`.
- `reference/`: All reference materials belong here at the repository root. These materials are private and local-only; never stage or upload them.
- `output/`: Generated runtime artifacts, ignored by Git. Renderer PNGs belong in `output/renderer/`.

## Development Environment

- Maintain one `flake.nix` and `flake.lock` at the repository root. Extend its
  existing outputs for future module tooling rather than adding independent flakes.
- Use the root Nix shell for Linux development tools and dependencies. It provides
  Node.js/npm, TypeScript (`tsc`), the renderer Rust toolchain, Git, pkg-config,
  nixfmt, and rendering libraries. Do not install global npm tools or use an
  unrelated flake template to replace the existing environment.
- Keep client sources in `client/src/`, which has no client framework or package
  yet. Example packages under `client/example/` carry their own `package.json` and
  scripts; the root toolchain smoke check does not build or test them.
- Run `nix develop`, `nix build`, `nix flake check`, and Cargo commands from the
  root inside the development shell.
- `nix flake check` covers the Node.js/npm/TypeScript toolchain smoke check and
  renderer formatting, Clippy, and CPU tests. The default package and app remain
  the renderer. GPU and interactive checks are separate; follow `server/AGENTS.md`.
- Preserve locked dependency revisions unless an update is explicitly required.
- The shell uses an absolute `target/linux` Cargo cache. Keep this cache,
  root `result` / `result-*` links, and root `output/` out of Git and build inputs.
- Put runtime artifacts in root `output/<module>/`; keep `node_modules/` and
  TypeScript incremental caches out of Git. Do not put generated files in `src/`.
- Nix flakes in Git include tracked files only. Stage explicit project paths
  for validation, never private references or runtime artifacts.
- Keep `AGENTS.md`, `README.md`, and `docs/development.md` consistent when changing
  directory responsibilities, tool availability, or development commands.

## Implementation Style

- Keep implementations simple, clear, and readable. Avoid excessive defensive
  programming, repeated checks of established invariants, and speculative fallbacks.
- Validate external inputs at clear boundaries; preserve necessary error handling
  and resource-lifetime guarantees without duplicating them throughout internal code.
- Use focused unit tests to verify invariants and realistic edge cases rather than
  adding production branches for hypothetical failures.

## Feature Development

- Before implementing a new feature, create a dedicated branch and Git worktree under the repository root's `.worktree/` directory.
- Make all changes for that feature in its worktree, not in the primary working tree.
- Run feature-specific commands and validation from that worktree.
- For example, from the repository root:

  ```powershell
  git worktree add -b feature/my-feature .worktree/my-feature main
  ```
