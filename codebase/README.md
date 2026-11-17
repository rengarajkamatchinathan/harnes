# Genie codebase

Implementation of [spec/01-architecture.md](../spec/01-architecture.md): the Cargo workspace,
crate graph and dependency rules, the shared daemon with its lifecycle, the client `ensure`
path, the session actor, the cancellation tree, the error taxonomy and log files; and of
[spec/04-storage.md](../spec/04-storage.md) in `genie-store` (see its coverage table and gaps below).

## Layout

```
crates/
  genie-protocol   wire types: JSON-RPC envelope, handshake, error kinds/codes, version skew table
  genie-core       TaskScope (cancellation tree), EngineBuilder, session actor + inbox,
                   bus (seq, journal, resync), error taxonomy, path locks, DirContexts
  genie-daemon     socket/pipe listener + peer check, token, spawn lock, handshake,
                   skew handling, idle exit, clean shutdown, standalone lease
  genie-client     endpoint layout, NDJSON framing, Connection, ensure (spawn / wait / retry)
  genie-process    kill-trees: Job Object (Windows) / process group (Unix), subreaper
  genie-audit      tracing → rotating log files (10 × 10 MiB)
  genie-testkit    in-memory transport
  genie-secrets    SecretString re-export
  genie-store      SQLCipher DB, migrations, GNB1 blobs, FTS, event log, retrieval index (spec 04)
  genie-{providers,catalog,config,permissions,tools,mcp,skills,sandbox,browser,markdown}
                   placeholders: crate exists, implementation is in its own spec
apps/cli           the `genie` binary: `genie daemon`, `genie status [--standalone]`, `genie doctor`
xtask              `cargo xtask check` = check-deps (01 §2.1 rules 1–3) + check-spawn (rule 4)
```

## Build and test

```sh
cargo test --workspace
cargo clippy --workspace --all-targets     # unwrap/expect/await_holding_lock are errors
cargo xtask check                          # dependency rules + no untracked tokio::spawn
cargo deny check                           # licences/advisories (needs cargo-deny)
```

On Windows without Visual Studio Build Tools, use `scripts/cargo-win-gnu.sh <args>`. It reuses the
gnu-toolchain workaround from `prototypes/37-streaming-chat/.tools` (see that README).

Try it:

```sh
genie status --standalone       # private daemon on a temp endpoint, exits with the CLI
genie status                    # starts the shared daemon if needed ("Starting engine…")
genie doctor
```

`GENIE_CHANNEL` at build time sets the channel (default `stable`); `GENIE_CHANNEL_OVERRIDE=dev`
overrides it at run time for development. `GENIE_LOG` sets the log filter.

## Spec coverage

| 01 § | What | Where |
|---|---|---|
| 1.1 | children bound to the daemon | `genie-process` (`spawn_tree`, `become_subreaper`) |
| 1.2 | endpoint, peer check, token, per-channel dirs | `genie-client::endpoint`, `genie-client::win`, `genie-daemon::{listener,token}` |
| 2 / 2.1 | 19 crates, dependency rules, lints, cargo-deny | `Cargo.toml`, `clippy.toml`, `deny.toml`, `xtask` |
| 3 | EngineBuilder, DirContext, actor per session, bus, path locks | `genie-core` |
| 4.1 | ensure: connect / spawn / wait 50 ms→1 s / 15 s / one retry, stale socket, lock race | `genie-client::ensure`, `genie-daemon::acquire_lock` |
| 4.2 | version skew table, `restart_pending`, degraded, `daemon.restarting` | `genie-protocol::version`, `genie-daemon::server` |
| 4.3 | idle exit (10 min), clean shutdown, crash → `turn.interrupted`, panic isolation | `genie-daemon`, `genie-core::{engine,session}` |
| 4.4 | standalone daemon with stdin lease | `genie daemon --standalone`, `genie status --standalone` |
| 5 | state machine, steer/queue inbox, interrupt, resume, asks/paused, eviction (15 min) | `genie-core::session`, `genie-core::engine` |
| 6 | cancellation tree | `genie-core::task::TaskScope` |
| 7 | ToolError / TurnError / Defect / RPC kinds, stable codes, excerpt scrubbing | `genie-core::error`, `genie-protocol::error` |
| 8 | rotating logs, `genie doctor` (daemon, git, rg) | `genie-audit`, `apps/cli` |

| 04 § | What | Where |
|---|---|---|
| 1 | per-file 0600 / dir 0700, `blobs/<sha[0..2]>/<sha>`, `recovery.bin` | `genie-store::{blobs,keys,store}` |
| 2 | SQLCipher (raw 256-bit key, page 4096, memory security), data key + `KeyStore` port, blob key wrapped by the data key, GNB1 XChaCha20-Poly1305 chunked STREAM with range reads, recovery key (SA-16), key-loss quarantine | `genie-store::{store,keys,crypto,blobs}` |
| 3 | full schema incl. §7 tables; JSON `CHECK(json_valid)`; typed repos for sessions, messages/parts/wire records, events, inbox, claims, projects/files, workspaces/worktrees, artifacts, memory, usage, kv | `migrations/0001_init.sql`, `genie-store::{sessions,messages,events,projects,artifacts,memory,kv}` |
| 3.2 | in-place branches: siblings, root→leaf path, active leaf, latest leaf | `genie-store::messages` |
| 3.3 / 02 §3, §9 | gap-free seq (`head_seq + 1` enforced), `log_floor_seq`, `BelowFloor`, 30-day idle pruning up to last `turn.settled` (SA-12) | `genie-store::events` |
| 4 | content-addressed dedupe, refcounts kept by triggers (cascades included), GC with 1 h grace | `genie-store::{blobs,ops}`, schema triggers |
| 5 | migration runner + journal, bootstrap from current schema, `sqlcipher_export` backup (last two kept), newer DB refused | `genie-store::migrate`, `Store::open` |
| 6 | `message_fts` (parts + titles, scoped `chat_search`), trigram title search, `memory_fts`, `kchunk_fts`; FTS query quoting | `genie-store::{search,memory,retrieval}` |
| 7 | chunk + embedding cache tables, BM25, brute-force cosine (SA-15), RRF hybrid | `genie-store::retrieval` |
| 8 | real delete with cascades, `secure_delete`, retention job, VACUUM policy (SA-19) + FTS rebuild, delete all data | `genie-store::{sessions,ops}` |
| 9 | single-session JSON (`--sanitize`) and Markdown export | `genie-store::export` |
| 11 | WAL, busy_timeout 5000, synchronous NORMAL, foreign_keys; one writer + 4 readers; `Store::write` = one IMMEDIATE transaction | `genie-store::store` |
| 01 §3 | `Journal` on the store | `genie-core::journal::StoreJournal` |

## Known gaps and deviations

- **`genie-daemon` depends on `genie-client`** for the shared transport (endpoint naming, framing,
  Windows SID helpers). The other direction would pull the engine into the Tauri side.
  `xtask check-deps` allows exactly this edge.
- **Subsystem edges are not declared yet.** Cargo 1.99 warns on unused dependencies, so
  `genie-core` gains each subsystem dependency when it first uses it; `xtask check-deps`
  enforces the directions in the meantime.
- **Durable storage is not wired into the daemon yet.** `genie-core::journal::StoreJournal`
  implements the `Journal` port on `genie-store`; the daemon still builds `MemoryJournal` until
  `EngineBuilder` gets the store and a keychain-backed `KeyStore` (spec 08).
- **Windows Job Object race:** a child is assigned to its job right after `spawn`, so anything it
  starts in that gap escapes. Closing it needs a raw `CreateProcessW` with `CREATE_SUSPENDED`
  (planned with the `opencode-pty` port, D45).
- **Windows token file** relies on the per-user `%LOCALAPPDATA%` ACL; no explicit DACL is set.
- **Not in this milestone:** desktop app, `packages/*` and the Platform-adapter lint rule (spec 05);
  protocol session methods and TS codegen (spec 02); the `-p` headless CLI (spec 07); CI workflows.

### Spec 04 (storage) gaps and deviations

- **Deviations:** `execution_claim` has an extra `finish` column (settling keeps the row so
  `Journal::last_turn` survives restarts); `usage_daily.project_id` is `''` rather than NULL
  outside a project (NULLs never collide in a primary key, which breaks the upsert);
  `session.project_id` is `ON DELETE SET NULL` (project chats survive project deletion);
  `message_fts` stores its own copy of the text (it indexes parts and titles) instead of being
  external-content; `session.worktree_id` is not a declared FK (circular with `worktree.session_id`).
- **API is synchronous** with a writer mutex + reader pool; the dedicated writer task with a
  bounded queue (01 §3) belongs in the daemon wiring (`spawn_blocking` until then).
- **`StoreJournal` sessions**: rows are created on first event as bare chat sessions, and
  `events_after` returns retained events only; the daemon must check `events::head` for the floor
  and send `resync_required {snapshot: true}` (02 §3.1).
- **Keychain**: only the `KeyStore` port + `MemoryKeyStore`; the OS keychain and `secrets.enc`
  (Argon2id) land with spec 08. Recovery-key UI/`.txt` download is UI work (spec 05).
- **Blobs**: `put_blob` takes the whole buffer (two passes would be needed for streaming input
  because the address is the plaintext hash); no streaming writer yet.
- **Not implemented:** long resumable data migrations (kv cursor jobs); retrieval chunking,
  indexing job, full/retrieval mode switch and capacity cap (SA-18); export-all `.zip` (SA-17),
  session import with id remapping, `export.allowed` policy; checkpoints / shadow git (§10, M5);
  the in-memory `Store` for incognito (§8); the daily scheduler that calls `run_maintenance`.
- **Build:** `rusqlite` uses `bundled-sqlcipher-vendored-openssl` (macOS has no OpenSSL headers);
  it needs Perl and a C compiler. Not yet tried on Windows with `scripts/cargo-win-gnu.sh`.
