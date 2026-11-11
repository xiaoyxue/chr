# Repository Guidelines

## Project Structure

- `client/`: Client-side code, primarily written in TypeScript. Source code belongs in `client/src/`.
- `render/`: Rendering code, primarily written in Rust. Source code belongs in `render/src/`.
- `common/`: Shared communication protocols, such as socket protocols. Source code belongs in `common/src/`.
- `algorithm/`: Scene reconstruction algorithms. Language and technology stack are undecided. Source code belongs in `algorithm/src/`; do not choose a stack without confirmation.
- `docs/`: Project documentation.
- `reference/`: Reference materials.

## Feature Development

- Before implementing a new feature, create a dedicated branch and Git worktree under the repository root's `.worktree/` directory.
- Make all changes for that feature in its worktree, not in the primary working tree.
- Run feature-specific commands and validation from that worktree.
- For example, from the repository root:

  ```powershell
  git worktree add -b feature/my-feature .worktree/my-feature main
  ```
