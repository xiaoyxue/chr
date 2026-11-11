# Repository Guidelines

## Project Structure

- `client/`: Client-side code, primarily written in TypeScript. Source code belongs in `client/src/`.
- `render/`: Rust rendering workspace. Shared rendering code lives in `render/crates/chr-core/`, and the native application lives in `render/native/chr-app/`. Follow `render/AGENTS.md` and run Cargo commands from `render/`.
- `common/`: Shared communication protocols, such as socket protocols. Source code belongs in `common/src/`.
- `algorithm/`: Scene reconstruction algorithms. Language and technology stack are undecided. Source code belongs in `algorithm/src/`; do not choose a stack without confirmation.
- `docs/`: Project documentation.
- `reference/`: All reference materials belong here at the repository root. These materials are private and local-only; never stage or upload them.
- `output/`: Generated runtime artifacts, ignored by Git. Renderer PNGs belong in `output/render/`.

## Nix and Cargo

- Maintain one `flake.nix` and `flake.lock` at the repository root. Extend its
  existing outputs for future module tooling rather than adding independent flakes.
- Run `nix develop`, `nix build`, and `nix flake check` from the root; run Cargo
  commands from `render/` inside the development shell (`nix develop ..` when
  starting in `render/`).
- Preserve locked dependency revisions unless an update is explicitly required.
- The shell uses an absolute `render/target/linux` Cargo cache. Keep this cache,
  root `result` / `result-*` links, and root `output/` out of Git and build inputs.
- Nix flakes in Git include tracked files only. Stage explicit project paths
  for validation, never private references or runtime artifacts.

## Feature Development

- Before implementing a new feature, create a dedicated branch and Git worktree under the repository root's `.worktree/` directory.
- Make all changes for that feature in its worktree, not in the primary working tree.
- Run feature-specific commands and validation from that worktree.
- For example, from the repository root:

  ```powershell
  git worktree add -b feature/my-feature .worktree/my-feature main
  ```
