# 24 — Worktrees and parallel sessions

type: wayfinder:grilling · status: closed · claimed-by: charting session (2026-11-17) · blocked-by: 23

## Question
Do we isolate parallel code sessions in git worktrees?

## To decide
- Worktree lifecycle and cleanup
- Desktop Code mode with several sessions at once

## References
- OpenCode ch.12: [12-storage-snapshots-and-worktrees.md](../knowledge/opencode/12-storage-snapshots-and-worktrees.md) — read TL;DR, Architecture, Genie takeaways

## Resolution
Resolved 2026-11-17. User decisions are marked **(user)**; the rest are defaults from KB ch.12 and D23/D43/D49.

### When (user)
- **Desktop Code:** starting a session on a repo that already has a **running** Code session automatically creates a worktree for the new session (banner: "Working in an isolated copy · branch genie/<slug>"). A single session works in the main checkout. Users can also choose "New session in worktree" explicitly.
- **CLI:** opt-in with `genie --worktree [name]` or `/worktree`.
- Non-git folders: no worktrees; a second session on the same folder gets a warning about concurrent edits.

### Location (user): sibling folder
- `../<repo>.genie-worktrees/<slug>/` next to the repo. It stays outside the repo, so watchers, builds and search don't see duplicates. Configurable (`worktrees.dir`).

### Lifecycle (default; KB port + adapt)
- **Named branch** `genie/<slug>` (prefix configurable, `worktrees.branchPrefix`), created from the current HEAD (not `--detach`, so PR flows work). Slugs come from the session title with numeric suffixes.
- Tracked in the store (`worktree` table: project, path, branch, session, created). **Remove only what Genie created**; `git worktree list --porcelain` discovery reconciles state at startup.
- An optional per-project **setup command** runs after creation (e.g. `pnpm install`), with `GENIE_WORKTREE_BASE/PATH` env; it is a project setting, so it needs trust and hash approval like hooks (D43).
- Trust (D43) and checkpoints (D49) apply per worktree; it inherits the main repo's trust record.
- **Finishing:** the session footer offers **Merge into <base>**, **Create PR** (via `gh`/`az` CLI if present; otherwise "push branch"), **Keep**, or **Discard**. Discard removes the worktree and deletes the branch only if it has no unpushed commits (otherwise asks).
- **Auto-cleanup:** when a session is archived/deleted, its clean worktree (no uncommitted changes, branch merged or pushed) is removed; dirty ones are listed in Settings → Developer for manual handling.
