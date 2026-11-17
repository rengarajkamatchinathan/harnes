# 04 — Storage

Implements D24, D31 (memory table), D32 (retrieval index, spill), D33 (compaction rows), D28 (artifact tables), D49 (checkpoints), D50 (worktree table); tickets 12, 10, 11, 16, 23, 24, 39. Background: KB ch.03, ch.12.

## 1. Layout on disk

Per channel data dir (D40): `~/.genie`, `~/.genie-beta`, `~/.genie-dev` (Windows: config under `%USERPROFILE%\.genie`, data under `%LOCALAPPDATA%\Genie\<channel>`).

| Path | Content | Protection |
|---|---|---|
| `genie.db` (`genie-beta.db`, …) + `-wal`, `-shm` | SQLCipher DB | 0600; SQLCipher key from the keychain |
| `blobs/<sha256[0..2]>/<sha256>` | content-addressed blobs | per-file AEAD, 0600 |
| `snapshots/<projectId>/<worktreeHash>/` | shadow git repos (D49) | 0700; plain git objects (see §10) |
| `settings.json`, `state.json`, `trust.json` | config, UI state, trust (D23, D43) | 0600; not secrets |
| `secrets.enc` | only when no keychain (D26) | Argon2id + XChaCha20-Poly1305 |
| `run/` | socket, token, lock (01 §1.2) | 0700 |
| `logs/`, `audit/`, `cache/`, `imports/` | logs, audit JSONL, feeds cache, import reports | 0600 |

## 2. Encryption

- **SQLCipher** via `rusqlite` with `bundled-sqlcipher` (includes FTS5) (D24, t12). Page size 4096, `cipher_memory_security = ON`.
- **Data key:** 256-bit random, created on first run, stored in the OS keychain (or `secrets.enc`, D26). It keys the DB and wraps the blob key.
- **Blobs:** XChaCha20-Poly1305 per file with a random nonce; AAD = the blob's SHA-256 (of plaintext). The content address is the plaintext hash, so dedupe still works. Format: `magic "GNB1" | nonce(24) | ciphertext | tag`. Large blobs are encrypted in 1 MiB chunks (STREAM construction) so they can be read by range.
- **Recovery key** (Settings → Privacy & data): an optional export of the data key, wrapped so the user can restore on a new keychain.
  - **Spec assumption (SA-16):** the recovery key is a 256-bit random key shown once as Base32 groups (and as a downloadable `.txt`); it wraps the data key (AEAD) into `recovery.bin` stored next to the DB. Restore asks for it when the keychain entry is missing. Reason: D24 asks for "optional recovery-key export" without a format; wrapping (instead of exporting the raw key) lets the user revoke it by re-wrapping.
- **Key loss:** if the keychain entry is gone and there's no recovery key, the daemon starts with a fresh DB after the user confirms, and moves the old files to `genie.db.unreadable-<date>` (never deletes them silently).
- Policy `encryption.required` (D38) is always true in v1 because encryption can't be turned off; the key exists for the future and for audit.

## 3. Schema

Key columns and indexes only. All ids are ULID text with a type prefix. Times are integer ms since epoch. JSON columns are `TEXT` checked with `json_valid`.

### 3.1 Projects and workspaces

```sql
project (id PK, name, instructions TEXT, settings JSON, starred INT, created, updated)          -- chat projects (t12)
project_file (id PK, project_id FK→project ON DELETE CASCADE, name, mime, blob_id FK→blob, tokens INT, sha256, added)
  INDEX (project_id)
workspace (id PK, canonical_path UNIQUE, identity TEXT,  -- git remote hash | root commit (pick-list #42)
           git_root, created, last_opened)
worktree (id PK, workspace_id FK, session_id FK NULL, path UNIQUE, branch, base_ref, created_by_genie INT,
          setup_hash TEXT NULL, created)                  -- D50
```

### 3.2 Sessions, messages, parts

```sql
session (id PK, kind TEXT CHECK(kind IN ('chat','code')), project_id FK NULL, workspace_id FK NULL, worktree_id FK NULL,
         parent_session_id FK→session ON DELETE CASCADE NULL,   -- subagents (D47)
         agent TEXT NULL, title, model, effort, mode,
         active_leaf_id TEXT NULL, status TEXT CHECK(status IN ('idle','running','awaiting_ask','interrupted')),
         driver_client TEXT NULL, head_seq INT, log_floor_seq INT DEFAULT 0,
         cost_usd REAL, tokens_in INT, tokens_out INT,
         pinned INT, archived INT, created, updated)
  INDEX (kind, archived, updated DESC); INDEX (project_id, updated DESC); INDEX (workspace_id, updated DESC); INDEX (parent_session_id)

message (id PK, session_id FK ON DELETE CASCADE, parent_id TEXT NULL, role, seq INT,   -- seq of message.admitted / first event
         created, model NULL, provider NULL, origin JSON NULL, usage JSON NULL, cost_usd REAL NULL,
         finish TEXT NULL, error JSON NULL,
         kind TEXT DEFAULT 'normal',    -- 'normal' | 'compaction' | 'synthetic' (subagent report, bg job, fix-with-genie)
         snapshot_before TEXT NULL, snapshot_after TEXT NULL)   -- shadow-git tree ids (D49)
  INDEX (session_id, parent_id); INDEX (session_id, created)

part (id PK, message_id FK ON DELETE CASCADE, ordinal INT, kind TEXT,
      -- text|reasoning|media|tool_call|tool_result|citation|compaction|effort_marker|unknown
      -- + Genie parts: artifact_ref|memory_chip|subagent_ref|attachment
      body JSON, blob_id FK→blob NULL, call_id TEXT NULL)
  UNIQUE (message_id, ordinal); INDEX (call_id)

wire_record (id PK, message_id FK ON DELETE CASCADE, step INT, provider, protocol, route, account_tag BLOB,
             request_blob FK→blob, response_blob FK→blob, id_map JSON, created)   -- exact provider history (D18)
  INDEX (message_id)
```

- **Branches are in place** (D24): editing or regenerating creates a sibling (`parent_id` shared). The model context is root → `active_leaf_id`. Sibling lists come from `INDEX (session_id, parent_id)`.
- **Compaction** (D33): a `message.kind = 'compaction'` row in the tree, with a part holding the summary text or the native opaque blob, the kept-tail boundary message id, trigger and token counts.
- **Wire records** store bytes as blobs (they can be large) and are reused byte-for-byte by the compile step (03 §4).

### 3.3 Events, inbox, claims, asks

```sql
event (session_id FK ON DELETE CASCADE, seq INT, type TEXT, payload JSON, created,
       PRIMARY KEY (session_id, seq)) WITHOUT ROWID                 -- durable events (D22)
inbox (id PK, session_id FK ON DELETE CASCADE, delivery TEXT CHECK(delivery IN ('steer','queue')),
       parts JSON, source TEXT, client_msg_id TEXT, state TEXT CHECK(state IN ('pending','admitted','removed')), created)
  UNIQUE (session_id, client_msg_id); INDEX (session_id, state, created)
execution_claim (session_id PK FK ON DELETE CASCADE, turn_id, started, daemon_pid, daemon_boot_id)   -- D20
permission_pending (id PK, session_id FK ON DELETE CASCADE, turn_id, kind, payload JSON, created,
                    state TEXT CHECK(state IN ('open','answered','cancelled')), decision JSON NULL)
  INDEX (state, created)
background_job (id PK, session_id FK ON DELETE CASCADE, kind TEXT /* shell */, command TEXT, cwd,
                started, ended NULL, exit_code NULL, output_blob NULL)                                  -- D45
```

- Event payloads are persisted (unlike OpenCode's default) because resume replays them (t12). Ephemeral deltas are never stored.
- Writes to `event` happen in the same transaction as the projection into `message`/`part`/`session` (01 §3).

### 3.4 Artifacts, memory, usage, browser

```sql
artifact (id PK, session_id FK ON DELETE CASCADE, type TEXT, title, created)                       -- D28
artifact_version (artifact_id FK ON DELETE CASCADE, n INT, message_id FK→message ON DELETE CASCADE,
                  content TEXT NULL, blob_id NULL, created, PRIMARY KEY (artifact_id, n))
  INDEX (message_id)
memory (id PK, scope TEXT /* 'global' | project_id | workspace_id */, text, source_session NULL, source_message NULL,
        created, updated, last_used)                                                               -- D31
  INDEX (scope, last_used DESC)
usage_daily (day TEXT, provider, model, project_id NULL, kind TEXT /* chat|compaction|title|subagent|embedding|sampling|search */,
             tokens_in, tokens_out, cache_read, cache_write, calls, cost_usd,
             PRIMARY KEY (day, provider, model, project_id, kind))                                 -- D34
browser_action (id PK, session_id FK ON DELETE CASCADE, ts, tab_id, tool, origin, args_redacted JSON,
                decision JSON, status, thumbnail_blob NULL)                                        -- D21 audit
kv (key PK, value JSON)   -- migration cursors, feature flags, budget state
migration (id PK, completed_at)
blob (id PK /* sha256 hex */, size INT, mime, refcount INT, created)
```

- **Artifact versions are tied to the message** that produced them, so branches get their own versions; the panel shows versions on the active path (D28).
- Memory is per scope; incognito sessions never touch it (D31).

## 4. Blobs

- Content-addressed by plaintext SHA-256 (t12). Used for attachments, project files, images, large tool outputs (spill, D32), wire records, artifact content above 256 KiB, screenshots/thumbnails, MCP binary results.
- **Ref-counted** in `blob.refcount`; updated in the same transaction as the referencing row. A daily job deletes blobs with refcount 0 older than 1 hour (grace for in-flight uploads).
- Spilled tool output is kept until the session is deleted (not OpenCode's 7-day sweep) (t11).

## 5. Migrations

- A Rust migration runner with the `migration(id, completed_at)` journal (t12). Migrations are numbered SQL files plus optional Rust steps.
- A fresh DB is bootstrapped from the full current schema in one transaction, then all migration ids are marked complete.
- **Long data migrations** run as resumable background jobs with a cursor in `kv` and progress shown in the UI (t12). The session is usable for read-only access while one runs.
- Before any schema migration the daemon writes `genie.db.bak-<version>` (an encrypted copy via `sqlcipher_export`) and keeps the last two. An update rollback (D40) restores from it.
- Downgrades are not supported; an older binary opening a newer DB refuses with "Update Genie to open this data".

## 6. Full-text search

- **FTS5** external-content tables kept in sync by triggers (t12):
  - `message_fts(text, title)` over text parts and session titles: sidebar search and the `chat_search` tool (D31).
  - `memory_fts(text)` for `memory_search`.
  - `kchunk_fts(text)` for retrieval (§7).
- Tokeniser: `unicode61 remove_diacritics 2` plus a trigram index for titles (substring search in the sidebar).
- Incognito sessions are never indexed (they're never stored).
- `chat_search` scopes: current project, or non-project chats (D31).

## 7. Retrieval index (project RAG)

D32 moves project RAG into v1.

```sql
kchunk (id PK, project_id FK ON DELETE CASCADE, file_id FK→project_file ON DELETE CASCADE, ordinal INT,
        start_byte INT, end_byte INT, tokens INT, text TEXT, content_hash BLOB)
  INDEX (project_id); INDEX (file_id, ordinal)
kembedding (content_hash BLOB, model TEXT, dim INT, vector BLOB /* f32 LE */, created,
            PRIMARY KEY (content_hash, model))       -- cache by content hash (D32)
```

- **Mode switch:** **full mode** while project knowledge ≤ 50% of the active model's window; above that **retrieval mode**, re-evaluated per request against the current model (D32).
- **Chunking:** ~800 tokens with overlap, per file type: Markdown by heading, code by top-level symbol where cheap (tree-sitter optional later), PDF by page, plain text by paragraph.
- **Indexing:** a background job on file add/change; progress on the project page; embeddings computed only when an embedding provider is configured (03 §9) and cached by content hash, so re-uploads cost nothing.
- **Ranking:** hybrid = BM25 (FTS5) + cosine similarity, combined with reciprocal rank fusion. BM25 alone when no embeddings.
- **Vector search:** brute-force cosine over the project's vectors in Rust (SIMD), no SQLite vector extension.
  - **Spec assumption (SA-15):** brute force, no vector extension. Reason: it avoids bundling an extension into SQLCipher; at the per-project cap (SA-18) a project has at most ~25k chunks, which scans in a few ms.
- **Capacity cap:** shown as a capacity bar on the project page.
  - **Spec assumption (SA-18):** the cap is 10× the largest context window among the user's enabled models, with a hard ceiling of 20M tokens and 500 files per project. Reason: D32 says "~10× guidance, enforced by a per-project cap" without numbers.
- Use at request time: top chunks for the user's message are pre-injected into the turn (not the cached baseline), and the model has `project_search {query}` (06 §5).

## 8. Retention and deletion

- **Chats are kept until deleted** (D24). Admin `retention.maxDays` (D38) runs a daily job that deletes expired sessions.
- **Delete is a real delete:** cascade from `session` removes messages, parts, wire records, events, inbox, asks, artifacts, browser actions, child sessions; blob refcounts are decremented; FTS rows are removed by triggers; the shadow-git refs for the session are dropped (D49).
- `secure_delete = ON` for the DB so deleted pages are zeroed.
- **VACUUM:** scheduled.
  - **Spec assumption (SA-19):** retention and blob GC run at daemon start and every 24 h; `VACUUM` runs at most weekly, when free pages exceed 20% and no turn is running. Reason: t12 says "VACUUM on a schedule" only.
- **Delete all data** (Settings → Privacy) removes the DB, blobs, snapshots, caches, keychain entries and the recovery key after a typed confirmation.
- **Incognito** sessions live in daemon memory (an in-memory store implementation behind the same `Store` trait) and vanish on close or daemon exit (t31).

## 9. Export and import

- **Single session:** `genie export <session> [--sanitize]` and UI export to **Markdown or JSON** (D24, t31). JSON = `{info, messages, parts, artifacts}` (pick-list #30). `--sanitize` redacts file contents, attachments, tool outputs and secrets for bug reports.
- **Export all:** a plaintext `.zip` written only after explicit confirmation (t12).
  - **Spec assumption (SA-17):** layout `manifest.json` (version, export date, counts) + `sessions/<id>.json` + `projects/<id>.json` + `memory.json` + `blobs/<sha256>` (decrypted) + `artifacts/<id>/v<n>.<ext>`. Reason: D24 asks for export but gives no format for "all".
- **Import** of a session JSON checks id conflicts and assigns new ids on conflict (t12). Policy `export.allowed` can disable export (D38).

## 10. Checkpoints (shadow git)

D49, t23. Code sessions only (M5).

- **Location:** a private, ref-less git dir per (project, worktree) under `snapshots/<projectId>/<worktreeHash>/` (0700), outside the user's repo. It borrows the user's objects through `objects/info/alternates` and seeds its index from the user's index for speed.
- **When:** a tree snapshot at the **start and end of every step that can mutate files** (edit, write, multi_edit, patch, shell). Shell changes are captured too. Tree ids go to `message.snapshot_before/after` and a durable `checkpoint.created` event.
- **Skipped files:** untracked files over 2 MiB are skipped and listed in the rewind dialog.
- **Non-git folders:** a content-addressed blob fallback for files touched by tools; shell side effects best effort (a size/ignore-limited scan).
- **Tooling:** the `git` CLI as a child process via `genie-process` (`gix` may be used for capture later).
- **Rewind:** Restore files, conversation or both (D49):
  - Files: path-level restore through a temporary index; changes made outside Genie since then are detected and listed before overwriting.
  - Conversation: a new in-place branch from the chosen message.
  - Two-phase: stage → undo or commit; committed implicitly on the next prompt.
- **Retention:** snapshots live as long as their session. A ref per retained checkpoint keeps objects alive; session delete drops refs and `git gc --prune` cleans up (the KB fix for OpenCode's missing gc). Settings → Privacy & data shows disk use per project and a clean-up action; policy caps `checkpoints.maxDays` / `maxSizeMB` (D49).
- **Encryption note:** shadow-git objects are plain git objects, like the user's own repo. They hold workspace file content only, never conversation data, so this is consistent with D24's scope (DB + blobs).
- Remote side effects (deployments, APIs) can't be rewound; the dialog says so.

## 11. Concurrency

- WAL mode, `busy_timeout = 5000`, `synchronous = NORMAL`, `foreign_keys = ON`.
- One writer task (01 §3); read pool of 4 connections.
- Every durable event write and its projections commit in one transaction.
- The DB is never opened by clients directly; only the daemon holds it (D23 "daemon is the source of truth").
