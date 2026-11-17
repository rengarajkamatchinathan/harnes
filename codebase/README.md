# Genie codebase

Implementation of [spec/01-architecture.md](../spec/01-architecture.md): the Cargo workspace,
crate graph and dependency rules, the shared daemon with its lifecycle, the client `ensure`
path, the session actor, the cancellation tree, the error taxonomy and log files; and of
[spec/04-storage.md](../spec/04-storage.md) in `genie-store` (see its coverage table and gaps below).

## Layout

```
crates/
  genie-protocol   wire types: JSON-RPC envelope, handshake, events, asks, method table (§6),
                   error kinds/codes, version skew table, JSON Schema export
  genie-core       TaskScope (cancellation tree), EngineBuilder, session actor + inbox,
                   bus (seq, journal, resync), error taxonomy, path locks, DirContexts
  genie-daemon     socket/pipe listener + peer check, token, spawn lock, handshake,
                   skew handling, idle exit, clean shutdown, standalone lease
  genie-client     endpoint layout, NDJSON framing, Connection, ensure (spawn / wait / retry)
  genie-process    kill-trees: Job Object (Windows) / process group (Unix), subreaper
  genie-audit      tracing → rotating log files (10 × 10 MiB)
  genie-testkit    in-memory transport
  genie-secrets    SecretString re-export; credential model + store port (03 §10.1)
  genie-store      SQLCipher DB, migrations, GNB1 blobs, FTS, event log, retrieval index (spec 04)
  genie-providers  canonical model, SSE parser, compile step, 4 protocol adapters, Executor,
                   error classification + retry policy, routes/presets, cassettes (spec 03)
  genie-catalog    model catalog: bundled snapshot, signed cache + ETag refresh, discovery
                   parsers, overrides/policy, effort map, pricing (spec 03 §8)
  genie-{config,permissions,tools,mcp,skills,sandbox,browser,markdown}
                   placeholders: crate exists, implementation is in its own spec
apps/cli           the `genie` binary (spec 07): inline TUI, `genie -p` headless, subcommands,
                   `genie chat`, importer, `hook-adapter`; also runs `genie daemon`
packages/protocol-ts  generated TS types + typed client stub + JSON Schema (never edit by hand)
xtask              `cargo xtask check` = check-deps (01 §2.1 rules 1–3) + check-spawn (rule 4)
                   + schema drift/compat; `cargo xtask schema [--release]` regenerates
```

## Build and test

```sh
cargo test --workspace
cargo clippy --workspace --all-targets     # unwrap/expect/await_holding_lock are errors
cargo xtask check                          # dependency rules + no untracked tokio::spawn
                                           # + protocol schema drift and compatibility
cargo xtask schema                         # regenerate packages/protocol-ts after protocol changes
cargo xtask schema --release               # move the compatibility baseline (when shipping)
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

### Spec 02 (protocol)

| 02 § | What | Where |
|---|---|---|
| 1 | NDJSON JSON-RPC 2.0, bidirectional; daemon → client requests; 16 MiB lines | `genie-protocol::jsonrpc`, `genie-client::framing`, `genie-daemon::server` (one writer task per connection) |
| 2 | handshake, `unauthorized`, `protocol_mismatch`, skew | `genie-protocol::handshake`, `genie-daemon::server` |
| 3.1 | subscribe → replay → `session.synced` → live; gap-free `seq`; `ref_seq` on ephemerals; bounded per-subscription queue (8 MiB / 20,000, SA-10) → `session.resync_required`; below-floor → `{snapshot: true}` | `genie-daemon::server::{session_subscribe, forward}`, `genie-core::bus` |
| 3.1 (client) | resume cursor, dedupe after resubscribe, stale-delta dropping, resync/reload actions | `genie-client::stream::SessionTracker` |
| 3.2 | workspace feed: snapshot, `session.listed`, `notification`; lag → fresh snapshot | `genie-daemon::server::{workspace_subscribe, ensure_pump}` |
| 3.3, 4 | envelope; all durable + ephemeral event types; unknown types decode as `Unknown` | `genie-protocol::event`; core → wire mapping in `genie-core::bus::CoreEvent::to_wire` |
| 5 | driver lease: acquire on first mutating command, `take_over` + `lease.lost`, `release`, 60 s reconnect grace (SA-11), asks not lease-gated | `genie-daemon::lease`, `with_lease` |
| 6 | method table for every area (SA-08), typed `Method` trait, `conn.call::<M>()` | `genie-protocol::{methods, api::*}`, `genie-client::Connection::call` |
| 6.2 | `session.create/list/get/update/delete/prompt/interrupt/resume/take_over/release/fork/inbox.remove`, `client_msg_id` idempotency | `genie-daemon::server`, `genie-daemon::registry` |
| 7 | durable asks: `permission.asked/resolved`, `permission.ask` request to the driver, `permission.pending/respond` from any client, paused + OS notification on the feed, `$/cancelRequest` when answered elsewhere or the lease moves | `genie-core::session::{ask_with, answer}`, `genie-daemon::server` |
| 8 | error kinds + codes (SA-09) | `genie-protocol::error` |
| 10 | semver, unknown fields/events/enum values tolerated (`#[serde(other)]`) | `genie-protocol` |
| 11 | JSON Schema 2020-12 + `methods.json` → TS types + `GenieClient`; drift check; breaking-change check vs `schema/released/` | `genie-protocol::schema`, `xtask::{schema, ts}`, `packages/protocol-ts` |

Contract tests (`crates/genie-daemon/tests/protocol.rs`, in-memory transport, real core, fake runner):
replay/synced/live with gap-free seqs, overflow → resync → resume without loss, take_over /
release / not_driver, reconnect within grace and lapse after it, ask to the driver as a request,
ask with no client → paused + notification + answered by another client, prompt idempotency.

### Spec 04 (storage)

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

### Spec 03 (providers and models)

| 03 § | What | Where |
|---|---|---|
| 2 | canonical `Message`/`Part`/`Opaque`/`Origin`, `ReplayScope`, `AccountTag` (SA-13) | `genie-providers::model` |
| 3 | `Protocol` trait + object-safe `ErasedProtocol`, `StreamEvent`, `Auth`, `Provider`, `LlmClient`; one `Executor` (auth, header 5 min / chunk-gap 15 min timeouts, retries, cancellation); own SSE parser | `protocol`, `event`, `auth`, `route`, `executor`, `sse` |
| 4 | compile: compaction cut, byte-exact reuse of stored `WireRecord`s, replay filter, tool-history repair, tool-id rewrite, Gemini schema sanitiser, cache breakpoints (tail breakpoint never persisted), effort/sampling filtering, attachment checks, diagnostics | `genie-providers::compile` |
| 5 | adapters: Anthropic Messages (API / Foundry / Vertex `streamRawPredict`), OpenAI Responses (OpenAI / Azure v1), OpenAI Chat (+ Ollama, OpenRouter, vLLM dialects via `Compat`), Gemini (API / Vertex); presets | `protocol::{anthropic,openai_responses,openai_chat,gemini}`, `route` |
| 6 | breakpoints (tools, system 1, system 2, tail; ≤ 4; 1 h ordering), `prompt_cache_key`, cache usage lines | `compile`, adapters |
| 7 | `ProviderError`/`ErrorKind`, classification order, 40-pattern overflow corpus, `x-should-retry`, `retry-after(-ms)`, retry policy (backoff, offline budget, timeouts, one-shot), forced token refresh on 401, `RetryScheduled` event | `error`, `overflow_patterns.txt`, `retry`, `executor` |
| 8 | catalog schema, tolerant decode, bundled snapshot, Ed25519-verified cache + ETag refresh, discovery parsers (OpenAI, Gemini, Ollama, Azure manual deployments), overrides (merge patch), policy, effort map, small model, embeddings default (SA-14), cost | `genie-catalog` |
| 10.1 | credential model, precedence (stored beats env), store port + in-memory store | `genie-secrets::credentials` |
| 11 | layer 1 cassettes (`tests/cassettes`, `CassetteTransport`, `RecordingTransport`, header scrubbing, secret check) and layer 2 goldens (`tests/golden`, `GENIE_UPDATE_GOLDEN=1`); layer 3 patch tests | `genie-providers/tests` |

### Spec 07 (CLI)

`apps/cli` is a library (`genie_cli`) plus a thin `main.rs`. The TUI is a pure model
(`tui::model`: state + `update(Msg) -> Vec<Effect>`; `tui::view`: footer lines) driven by
`tui::run`, so keys and rendering are unit-tested without a terminal.

| 07 § | What | Where |
|---|---|---|
| 1.1 | ratatui `Viewport::Inline` + `insert_before`; append-only scrollback; live footer that grows/shrinks (viewport re-created on height change); settled Markdown blocks committed, unstable tail in the footer | `tui::run::redraw`, `tui::model` (`commit_stable`, `live_lines`), `tui::markdown::stable_prefix` |
| 1.2 | Markdown blocks (headings, lists, quotes, fences, inline code/bold/italic/links) with a heal step for unclosed inline markers; diffs coloured +/−; Mermaid as code + "open in desktop"; LaTeX as source; rich parts (media, artifacts) as one line + `genie://session/<id>#<part>`; role-token themes, truecolor → 256 → 16, `NO_COLOR`, light/dark guess (`COLORFGBG`, luminance helper); ~10 ms delta batching | `tui::markdown`, `tui::theme`, `render`, `tui::run` (`BATCH`) |
| 1.3 | Enter send/steer, Alt-Enter queue (newline when idle), Shift-Enter/Ctrl-J newline, Esc interrupt, Esc Esc on empty prompt → rewind, Shift-Tab mode cycle (bypass only with `--dangerously-skip-permissions`; chats stay `default`), Ctrl-C twice, `@` file completion (ignore rules via `ignore`), `/` menu from **one registry** that also holds keybindings, Up/Down history, pasted file path → attachment (`file_ref`); footer permission prompt (Allow once / Always with scope / Deny + reason, subagent label), question/elicitation forms (sensitive fields masked); status line (model · effort · mode · ctx % · cost · N subagents · Waiting for approval) | `tui::model::on_key`, `tui::commands::REGISTRY`, `tui::view` |
| 1.4 | `ensure` with "Starting engine…"; resubscribe from the last `seq` on reconnect (`SessionTracker`, resync/reload); `-p` or non-TTY → headless | `connect`, `tui::run::{reconnect,on_incoming}`, `main::run_session` |
| 2.1 | `genie [prompt]`, `-c`, `-r [id]` (stdin picker), `--agent/--model/--effort/--permission-mode/--dangerously-skip-permissions/--worktree [name]/--add-dir/--settings/--trust` | `args`, `session::open` |
| 2.2 | `chat`, `sessions [--all]`, `approvals [list|approve [--always]|deny [--message]]`, `import`, `export`, `config [get|set|doctor|path]`, `doctor`, `update`, `daemon [status|stop|restart|logs]` (bare runs it), `login [--device-code]`/`logout`, `mcp [list|add|remove|auth|logs]`, `trust [list|revoke]`, `hook-adapter claude -- <script>`, `--version` (channel, commit, build date from `GENIE_COMMIT`/`GENIE_BUILD_DATE`) | `args`, `cmd`, `hook_adapter`, `main` |
| 2.3 | `/help /model /effort /mode /compact /rewind /worktree /init /memory /skills /agents /mcp /permissions /cost /context /resume /export /clear /exit /takeover`, user commands (`~/.genie/commands`, `.genie/commands`, `$ARGUMENTS`, `dir:name`), `/skill-name` and `/server:prompt` sent as a `skill` input part | `tui::commands`, `tui::model::{submit,run_cmd}` |
| 3 | `genie chat list` (title, project, updated), `open <id>` (prints the active branch when `session.get` has a path, else replays the log; subscribes), `new`; sending takes a vacant lease; `lease.lost` → read-only + Ctrl-T take over; citations as `[n]` + references, tool rows, thinking, rich parts as summaries + links | `cmd::chat_list`, `render::transcript`, `tui::run` |
| 4.1 | `-p "<prompt>"`, stdin prompt, any non-TTY; all §4.1 flags parsed | `args::RunArgs`, `main::run_headless` |
| 4.2 | `text` (final assistant text), `json` (`{session_id, result, usage, cost_usd, status, num_turns, subagents, …}`), `stream-json` (first line `system/init` with `genie_protocol`, then D22 envelopes, last line `result`) | `headless::{run,emit}` |
| 4.3 | 0 / 1 / 2 / 3; `no_provider_configured` and other turn errors worded by `errors::turn_error` | `headless::Run::outcome` |
| 4.4 | asks pause and wait (stderr notice with `genie approvals` hint; CI hint when `CI=true`); `--on-ask deny` denies at once; `--ask-timeout` denies, interrupts, exit 3; asks answered from any client end the wait | `headless::Run::apply` |
| 4.5 | shared daemon by default, `--standalone` private daemon (stdin lease); waits for background subagents and follow-up turns (D47) and `--max-turns`; trust notice / `--trust` → `trust.set once` | `connect`, `headless` |
| 5.1 | Claude Code (user/project/local settings: permissions, `defaultMode`, env with secrets split out, `additionalDirectories`; `~/.claude.json` MCP user + this project's local; `CLAUDE(.local).md` → `GENIE(.local).md` `@`-import; skills/agents/commands with `!cmd` flagged; hooks → `genie hook-adapter claude`), Claude Desktop (`mcpServers`; extensions → connector suggestions), OpenCode (`opencode.json(c)` global + project: providers with keys → keychain / `{env:…}` refs, model, permissions, MCP local/remote, agents, commands, instructions; `.opencode/{agent,command}/*.md`) | `import::sources` |
| 5.2 | scan → preview (import / skip / conflict, per-key diagnostics: unsupported, semantic difference, secret detected, needs trust) → apply; secrets only through `SecretSink`; GENIE.md append needs confirmation; report `<genie home>/imports/<date>.json`; idempotent by id (source path) + content hash; deferred items retried | `import::{preview,apply}`, `cmd::import_cmd` |
| 5.3 | OpenCode last-match-wins → classes; allow-after-deny dropped with a diagnostic, deny kept (SA-40); ask-after-deny kept + diagnostic; allow-after-ask diagnostic; key order preserved without `serde_json/preserve_order` | `import::perms` |

Tests: unit tests for the TUI model/view/markdown/theme/commands, importer (fixture dirs only),
hook adapter and args; `apps/cli/tests/headless.rs` runs the real daemon in-process over the
in-memory transport with a fake `TurnRunner` (stream-json framing and gap-free seqs, text/json
output, exit codes 0/1/2/3, `--on-ask deny`, `--ask-timeout`, an ask answered via
`genie approvals`, `--resume`/`--continue`, unimplemented methods).

#### Spec 07 gaps

- **Engine-side pieces the CLI can't supply:** the daemon answers `not_implemented` for
  `config.*`, `trust.*`, `provider.*`, `mcp.*`, `skill.*`, `memory.*`, `model.list`,
  `usage.query`, `checkpoint.*` (`/rewind`), `worktree.finish`, `session.compact`,
  `session.export` and `import.*`; the CLI wires each one and prints "not available in this
  engine build yet". `session.get` returns an empty `path`, so `genie chat open` replays the
  event log instead of printing the branch.
- **Headless flags not applied:** `--allow/--deny`, `--settings` and `--add-dir` need a
  session-only settings layer on the wire (not in `session.create`); they print a one-line
  notice. `--worktree <name>` sends `worktree: new` (no name field). `system/init.tools` is
  empty (no tool-list method).
- **`no_provider_configured`:** core maps every `TurnError::Provider` to the bare code
  `GENIE-PROV`, so the class is lost on the wire; the CLI words bare `GENIE-PROV` as "provider
  failed / none configured". Suggested core change: put the class in `ErrorInfo.message` or the
  code (`GENIE-PROV:<class>`); `errors::turn_error` already handles both.
- **`--continue`** picks the most recent session of the kind: `SessionSummary.workspace_id` is
  never set and `workspace_path` is not stored, so "in this folder" can't be filtered yet.
- **Headless OS notification:** the headless client advertises no ask kinds and answers from
  the event stream; with a client attached the core sets `awaiting_ask` (not `paused`) and so
  raises no OS notification. Needs a daemon rule ("attached, but cannot render this kind →
  notify").
- **Rendering:** `genie-markdown` (patchmark port) and `syntect` highlighting are not done;
  `tui::markdown` is a small block-stable renderer with the heal step. The OSC 11 palette query
  is not sent (light/dark from `COLORFGBG`, default dark). No full-screen mode, images or
  terminal Mermaid (by spec).
- **Importer:** runs in the CLI, not behind `import.scan/preview/apply` (the daemon side can
  call `genie_cli::import` later or the module can move to a crate). No keychain write path from
  the CLI, so secrets are reported (`secret not imported`) and retried on the next run. Preview
  applies all non-skipped items (no per-item picker in the CLI yet). No first-run offer.
- **Not implemented:** `genie update` (prints a notice), the `/rewind` picker UI, `@`-mentions
  sent as plain text (not `file_ref` parts), images pasted as data (only file paths),
  `/resume` picker inside the TUI (lists sessions; `/resume <id>` switches), ACP (v2).
- **Leases:** `--resume`/`--continue`/`-p` take over a session driven elsewhere
  (`session::with_takeover`); the TUI instead shows "take over" and waits for Ctrl-T.
- **Daemon `stop`** uses `daemon.restart` (there is no `daemon.stop`); the daemon exits and the
  next client restarts it.

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
- **Not in this milestone:** desktop app, `packages/ui` and the Platform-adapter lint rule (spec 05);
  CI workflows.

### Spec 02 gaps and deviations

- **Methods owned by other specs** (config, providers, browser, terminal, MCP, projects, blobs,
  Code mode, `session.regenerate/compact/export/search`) are typed in the table and generated
  into TS, but the daemon answers `method_not_found` with `data.reason = "not_implemented"`
  until their subsystem lands.
- **Session metadata is in memory** (`genie-daemon::registry`) and `session.get` returns an
  empty `path`: the message tree is projected by the store (spec 04). `session.fork` creates an
  empty child with the same settings; `prompt` with `parent_id` (edit → branch) is refused.
- **`status` event:** `{state, activity?}`. `state` is the session state machine (01 §5.1);
  `activity` carries the spec's `thinking|retrying|compacting|…` values.
- **`permission.resolved.by`** adds `"system"` for asks cancelled because their turn ended.
- **`turn.settled`** for an interrupted turn is `status: "cancelled"` with
  `error.code = "interrupted"`; the separate `turn.interrupted` follows.
- **Lease vacancy is lazy:** after the 60 s grace the lease is treated as vacant on the next
  check; no `lease.changed {driver: null}` is published for a lapse.
- **No native notifier yet:** with no desktop on the workspace feed, a paused ask is only logged.
- **`genie -p --output-format stream-json`** (02 §10 `system/init` line) belongs to spec 07.

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

### Spec 03 gaps

- **Auth flows are ports only.** `TokenSource` + `TokenAuth` (cache, single-flight refresh, forced
  refresh on 401) exist; Entra PKCE/device code (`oauth2`), Google ADC (`google-cloud-auth`) and
  the cross-process refresh file lock are not implemented. Keychain / `secrets.enc` backends are spec 08.
- **No live HTTP in tests.** `ReqwestTransport` (rustls) is untested against real endpoints; the
  live-smoke layer (11 layer 4), nightly re-recording and the drift report are not built.
  Cassettes are hand-written from the vendors' documented event shapes, not recordings.
- **Catalog → `ModelSpec` conversion and the 6 h refresh loop** belong in `genie-core` (rule 3:
  catalog and providers don't depend on each other); not wired yet. `genie-catalog` mirrors a few
  provider enums (`Protocol`, `EffortWire`, `Quirk`) for the same reason.
- **The feed signing key is not pinned** (`signature::PINNED_KEYS_HEX` is empty until D40's key
  exists), so cached catalogs never verify in production yet and the bundled snapshot is used.
- **The bundled snapshot is provisional**: model ids, limits and prices must be re-checked
  against vendor docs before release (03 §8.4: provider docs only, no models.dev).
- Not implemented: `EmbeddingClient` (§9), Azure `prefer_responses` fallback to Chat on
  "Model not supported" (the flag and `UnsupportedOnDeployment` exist), `ReplayRejected`
  recompile (engine side), PDF text extraction (daemon side), `<system-update>` lowering (06),
  model-family detection is a heuristic on the wire id, Bedrock (v2).
- Catalog body `quirks` with a body merge patch re-serialise the request through `Value`, so
  they must not be used on routes that need byte-exact history replay.
