# chr

## Project Structure

| Directory | Purpose | Language |
| --- | --- | --- |
| `client/src/` | Client-side code | Primarily TypeScript |
| `render/src/` | Rendering | Primarily Rust |
| `common/src/` | Shared communication protocols, such as socket protocols | To be decided |
| `algorithm/src/` | Scene reconstruction algorithms | To be decided |
| `docs/` | Project documentation | |
| `reference/` | Reference materials | |

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

See `AGENTS.md` for contributor guidelines.
