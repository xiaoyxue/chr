# chr

## Project Structure

| Directory | Purpose | Language |
| --- | --- | --- |
| `client/src/` | Client-side code | Primarily TypeScript |
| `render/` | Rendering workspace: shared renderer in `crates/rdc-core/`, native application in `native/rdc-app/` | Primarily Rust |
| `common/src/` | Shared communication protocols, such as socket protocols | To be decided |
| `algorithm/src/` | Scene reconstruction algorithms | To be decided |
| `docs/` | Project documentation | |
| `reference/` | All reference materials (private, local-only, ignored by Git) | |

## Development

Create a dedicated branch and Git worktree under `.worktree/` before implementing a new feature. Make feature changes and run validation in that worktree, not in the primary working tree.

From the repository root:

```powershell
git worktree add -b feature/my-feature .worktree/my-feature main
```

See `AGENTS.md` for contributor guidelines.

Run rendering workspace commands from `render/`. See `render/README.md` for setup and usage, and `render/AGENTS.md` for rendering-specific guidelines.
