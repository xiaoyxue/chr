# Repository Guidelines

## Project Structure

- `client/`: Client-side code, primarily written in TypeScript. Source code belongs in `client/src/`.
- `render/`: Rust rendering workspace. Shared rendering code lives in `render/crates/chr-core/`, and the native application lives in `render/native/chr-app/`. Follow `render/AGENTS.md` and run Cargo commands from `render/`.
- `common/`: Shared communication protocols, such as socket protocols. Source code belongs in `common/src/`.
- `algorithm/`: Scene reconstruction algorithms. Language and technology stack are undecided. Source code belongs in `algorithm/src/`; do not choose a stack without confirmation.
- `docs/`: Repository-wide documentation. `docs/development.md` defines the
  development environment and directory ownership; renderer-specific contracts
  remain in `render/docs/`.
- `reference/`: All reference materials belong here at the repository root. These materials are private and local-only; never stage or upload them.
- `output/`: Generated runtime artifacts, ignored by Git. Renderer PNGs belong in `output/render/`.

## Development Environment

- Maintain one `flake.nix` and `flake.lock` at the repository root. Extend its
  existing outputs for future module tooling rather than adding independent flakes.
- Use the root Nix shell for Linux development tools and dependencies. It provides
  Node.js/npm, TypeScript (`tsc`), the renderer Rust toolchain, Git, pkg-config,
  nixfmt, and rendering libraries. Do not install global npm tools or use an
  unrelated flake template to replace the existing environment.
- Keep client sources in `client/src/`. There is no client framework, package
  manifest, build script, or TypeScript project configuration yet; do not claim
  that the toolchain smoke check builds or tests a client application.
- Run `nix develop`, `nix build`, and `nix flake check` from the root; run Cargo
  commands from `render/` inside the development shell (`nix develop ..` when
  starting in `render/`).
- `nix flake check` covers the Node.js/npm/TypeScript toolchain smoke check and
  renderer formatting, Clippy, and CPU tests. The default package and app remain
  the renderer. GPU and interactive checks are separate; follow `render/AGENTS.md`.
- Preserve locked dependency revisions unless an update is explicitly required.
- The shell uses an absolute `render/target/linux` Cargo cache. Keep this cache,
  root `result` / `result-*` links, and root `output/` out of Git and build inputs.
- Put runtime artifacts in root `output/<module>/`; keep `node_modules/` and
  TypeScript incremental caches out of Git. Do not put generated files in `src/`.
- Nix flakes in Git include tracked files only. Stage explicit project paths
  for validation, never private references or runtime artifacts.
- Keep `AGENTS.md`, `README.md`, and `docs/development.md` consistent when changing
  directory responsibilities, tool availability, or development commands.

## Feature Development

- Before implementing a new feature, create a dedicated branch and Git worktree under the repository root's `.worktree/` directory.
- Make all changes for that feature in its worktree, not in the primary working tree.
- Run feature-specific commands and validation from that worktree.
- For example, from the repository root:

  ```powershell
  git worktree add -b feature/my-feature .worktree/my-feature main
  ```
