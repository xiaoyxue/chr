# Repository Guidelines

## Project Structure

- `client/`: Primarily TypeScript. Source code belongs in `client/src/`.
- `render/`: Primarily Rust. Source code belongs in `render/src/`.
- `algorithm/`: Language and technology stack are undecided. Source code belongs in `algorithm/src/`; do not choose a stack without confirmation.
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
