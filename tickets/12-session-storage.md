# 12 — Sessions and storage (event-sourced SQLite)

type: wayfinder:grilling · status: closed · claimed-by: charting session (2026-11-16) · blocked-by: 01,03

## Question
What are the session data model and storage design?

## To decide
- Event-sourced sessions on SQLite (KB: copy this)
- Schema: projects, sessions, messages, parts, attachments, usage
- Branching from edits, search (FTS5), export, migrations
- Chat sessions vs code sessions (project-scoped)

## References
- OpenCode ch.03: [03-session-and-agent-loop.md](../knowledge/opencode/03-session-and-agent-loop.md) — read TL;DR, Architecture, Genie takeaways
- OpenCode ch.12: [12-storage-snapshots-and-worktrees.md](../knowledge/opencode/12-storage-snapshots-and-worktrees.md) — read TL;DR, Architecture, Genie takeaways

## Resolution
Resolved 2026-11-16. User decisions are marked **(user)**; the rest are defaults from KB ch.03/12, D18, D20 and D22.

### Branching (user)
- **In-place branches, Claude-style.** Editing a message or regenerating creates a **sibling** in a message tree (`parent_id`), and `‹ 2/3 ›` arrows switch versions. Each session stores its `active_leaf_id`; the model context is the path root → active leaf. Forking into a new chat is still available as an explicit action.

### Encryption at rest (user)
- **SQLCipher** (rusqlite `bundled-sqlcipher`, which includes FTS5) for the DB, and **per-file AEAD (XChaCha20-Poly1305)** for blobs on disk. The 256-bit data key is created on first run and stored in the **OS keychain** (→ 14 owns the headless-Linux fallback).
- **Key-loss risk:** losing the keychain entry means the data can't be read. Offer an optional **recovery key export** in Settings → Privacy. Admin policy can require encryption (→ 28). Export (`genie export`) writes plaintext only after an explicit confirmation.

### Retention (user)
- **Chats stay until the user deletes them.** Admins can set a maximum retention in days through managed policy (→ 28); a daily daemon job deletes expired sessions and their blobs. Delete is a real delete: rows, blobs and FTS entries, plus `VACUUM` on a schedule.

### Storage layout (default)
- `~/.genie/genie.db` (channel-specific name, → 27), WAL, `busy_timeout`, 0600; `~/.genie/blobs/<sha256-prefix>/<sha256>` for attachments, knowledge files, images and large tool outputs (content-addressed, encrypted, ref-counted).
- Migrations: a Rust migration runner with a `migration(id, completed_at)` journal; a fresh DB is bootstrapped from the full schema in one transaction; long data migrations are background jobs that can resume, with a cursor in `kv` and progress reported to the UI.

### Schema (default; tables)
- `project` (chat project: name, instructions, settings) · `project_file` (knowledge files → blob) · `workspace` (code project: canonical path, git identity per KB ch.01).
- `session` (id, kind `chat|code`, project_id?, workspace_id?, title, model, active_leaf_id, parent_session_id (subagents), status `idle|running|interrupted`, driver lease, timestamps, archived/pinned).
- `message` (id, session_id, parent_id, role, seq, created, model, provider, usage, cost, finish, error) · `part` (message_id, ordinal, kind `text|reasoning|image|file|tool_call|tool_result|citation|artifact_ref|…`, JSON body, blob_ref?).
- `wire_record` (message_id, provider, protocol, request/response bytes → blob): **the exact provider history required by D18** for thinking-block replay and caching.
- `event` (session_id, seq, type, payload, created): **durable event payloads are persisted** (unlike OpenCode's default), because D22 resume replays them. Ephemeral deltas are never stored. Old events can be compacted once a session is idle, because the projections are canonical.
- `inbox` (steer/queue items) · `execution_claim` (detects an interrupted turn → Resume, D20) · `permission_pending` (durable asks, D22).
- `artifact` + `artifact_version` (→ 39 decides the details) · `usage_daily` (cost roll-ups) · `kv`.
- **Search:** FTS5 over message text and titles (an external-content table kept in sync by triggers), shown as sidebar search.
- **Cascade:** deleting a session deletes messages, parts, wire records, events and artifacts, and decrements blob ref-counts.

### Export / import (default)
- `genie export <session> [--sanitize]` → JSON `{info, messages, parts, artifacts}`; `--sanitize` redacts file contents and secrets for bug reports. Import checks for id conflicts. Single-chat export from the UI (36 v1 list).

### Not here
- File checkpoints for Code mode (shadow git) → 23. Worktrees → 24.
