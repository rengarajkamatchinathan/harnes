# 23 — Checkpoints and undo

type: wayfinder:grilling · status: closed · claimed-by: charting session (2026-11-17) · blocked-by: 12

## Question
How do users rewind file changes?

## To decide
- Shadow git repo with before/after snapshots per step (KB: strong default
- captures Bash changes too)
- Retention and gc
- Rewind the conversation vs the files

## References
- OpenCode ch.12: [12-storage-snapshots-and-worktrees.md](../knowledge/opencode/12-storage-snapshots-and-worktrees.md) — read TL;DR, Architecture, Genie takeaways

## Resolution
Resolved 2026-11-17. User decisions are marked **(user)**; the rest are defaults from KB ch.12 and D24/D45.

### Mechanism (default; KB port)
- **Shadow git:** a private, ref-less git dir per (project, worktree) under `~/.genie/snapshots/<projectId>/<worktreeHash>/` (0700), outside the user's repo. It borrows the user's objects via `alternates` and seeds its index from the user's index, for speed.
- A tree snapshot is taken **at the start and end of every agent step** that can mutate files (edit/write/patch/shell). This **includes changes made through shell commands**, which Claude's file-tool checkpoints miss. The tree ids are recorded on the assistant message (D24).
- **Non-git folders:** a content-addressed blob fallback in the encrypted store (D24) for files touched by tools. Shell side effects in non-git folders are best-effort (a snapshot of the workspace limited by size/ignore rules).
- Untracked files over 2 MiB are skipped; **the UI says so** in the rewind dialog (KB gap).
- Built through the `git` CLI (or `gix` for capture), as a child process via `genie-process` (D45).

### Rewind (user): code, conversation, or both
- Pick a past message → **Restore files**, **Restore conversation**, or **Restore both**. The dialog shows the list of files that will change.
- **Files:** path-level restore from the snapshot through a temporary index; changes made outside Genie since then are detected and listed before overwriting.
- **Conversation:** uses in-place branching (D24): a new branch from that message; the old path stays available via `‹ n/m ›`.
- Two-phase semantics (KB): **stage → clear (undo the rewind) or commit**. The rewind is committed implicitly on the next prompt.
- CLI: `/rewind` (picker) and Esc-Esc on an empty prompt, as in Claude Code; desktop: a "Rewind" action on each message in Code sessions.

### Retention (user): until the session is deleted
- Snapshots are kept **as long as their session exists**. Deleting a session (or retention expiry from policy, D24/D38) drops its refs, and `git gc` prunes unreferenced objects (a ref is kept per retained checkpoint, the KB fix for OpenCode's missing gc).
- Settings → Privacy & data shows **checkpoint disk usage per project** with a "Clean up old checkpoints" action. Policy can set a cap (`checkpoints.maxDays` / `maxSizeMB`).

### Not covered
- Changes to remote systems (deployments, databases, APIs called via shell/MCP) can't be rewound; the dialog says so.
