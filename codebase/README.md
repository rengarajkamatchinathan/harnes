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
                   bus (seq, journal, resync), error taxonomy, path locks, DirContexts;
                   `runtime`: turn loop, context assembly, prompts (`prompts/`), compaction,
                   tool wiring, catalog → provider resolver (spec 06 §1–§4)
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
  genie-tools      Tool trait + ports, registry, input repair/validation, output limits + spill,
                   v1 chat tools (`tool_output_read`, `memory_*`) (spec 06 §2.4, §4)
  genie-{config,permissions,mcp,skills,sandbox,browser,markdown}
                   placeholders: crate exists, implementation is in its own spec
apps/cli           the `genie` binary: `genie daemon`, `genie status [--standalone]`, `genie doctor`
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

## Known gaps and deviations

- **`genie-daemon` depends on `genie-client`** for the shared transport (endpoint naming, framing,
  Windows SID helpers). The other direction would pull the engine into the Tauri side.
  `xtask check-deps` allows exactly this edge.
- **Subsystem edges are not declared yet.** Cargo 1.99 warns on unused dependencies, so
  `genie-core` gains each subsystem dependency when it first uses it; `xtask check-deps`
  enforces the directions in the meantime.
- **Durable storage is wired with a placeholder keychain.** The daemon opens
  `genie{-channel}.db` in the channel data dir with `StoreJournal` and the store-backed
  conversation, but the data key sits in `keystore.placeholder.json` (0600) via
  `genie-daemon::PlaceholderKeyStore` — **TODO spec 08**: OS keychain / `secrets.enc`. If the
  store can't be opened the daemon logs it and runs in memory.
- **Windows Job Object race:** a child is assigned to its job right after `spawn`, so anything it
  starts in that gap escapes. Closing it needs a raw `CreateProcessW` with `CREATE_SUSPENDED`
  (planned with the `opencode-pty` port, D45).
- **Windows token file** relies on the per-user `%LOCALAPPDATA%` ACL; no explicit DACL is set.
- **Not in this milestone:** desktop app, `packages/ui` and the Platform-adapter lint rule (spec 05);
  the `-p` headless CLI (spec 07); CI workflows.

### Spec 02 gaps and deviations

- **Methods owned by other specs** (config, providers, browser, terminal, MCP, projects, blobs,
  Code mode, `session.export/search`) are typed in the table and generated
  into TS, but the daemon answers `method_not_found` with `data.reason = "not_implemented"`
  until their subsystem lands.
- **Session metadata is in memory** (`genie-daemon::registry`); `session.get` returns the
  active path from the runtime's conversation store (no `siblings` yet). `session.fork` creates an
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

### Spec 06 (turn loop, context, compaction, tools)

| 06 § | What | Where |
|---|---|---|
| 1 | turn loop: admit → steps → settle; deltas as ephemeral `part.delta`; `step.started/ended`, `part.started/ended` (part ids `{message_id}.p{n}`), `tool.started/ended`, `retry.scheduled` (+ `status` activity `retrying`), `usage.updated`; tool calls dispatched as their input ends (D20) and joined at step end; steer injected at the boundary; `finish=length` → `continue_smaller` (≤ 2 in a row); `maxSteps` (SA-07: 50 chat / 200 code) → final step with `tool_choice: none` + `max_steps` note; interrupt keeps partial text and cancels tool calls; resume fails unsettled calls + `interrupted_resume`; regenerate branches from the user message (D24); title after the first reply (D27); usage per step + session totals + `usage_daily` with catalog cost | `genie-core::runtime::{turn, mod}` |
| 2.1 | layout: tools → system 1 (base prompt + tuning) → system 2 (frozen baseline) → conversation; `prompt_cache_key` = session; `baseline_long_ttl` with project knowledge | `runtime::turn::assemble` |
| 2.2 | `InstructionSource {key, rank, hash, text}`, most-shared-first (SA-42), frozen baseline, changes as frozen `<system-update>` messages (date → "Today's date is now …"), rebuilt by compaction | `runtime::context`, `runtime::ports::InstructionProvider` |
| 2.3 | measured usage from the last same-model step + 4 chars/token + 15 % estimate; `max_tokens` fitted to the remaining room; context meter in `usage.updated` | `runtime::context`, `turn::assemble` |
| 2.4 | 2,000 lines / 50 KiB truncation, full output spilled (store blob), marker `[showing 1–N of M; full output: <ref>]`, `tool.ended.spill_ref` | `genie-tools::output`, `runtime::bridge::BlobSpill` |
| 2.6 | prompt fragments as versioned files (`crates/genie-core/prompts/`), template engine, base prompt per session kind, per-tool/feature guidance only when available, per-model tuning appendix (SA-43), all §2.6.4 source formats, all §2.6.7 hidden prompts, `<system-update>` lowering per protocol (SA-45), golden snapshots | `runtime::prompts`, `runtime::context`, `tests/prompts.rs` + `tests/golden/prompts/` |
| 3 | auto threshold `window − max(15 %, 16k)` (`compaction.buffer`), overflow → compact with tail 85/70/50/35 % → retry step, manual `session.compact {focus}`; same model + cached prefix + template; one retry on template failure; ~15k-token verbatim tail of whole messages never starting at a tool result; `compaction` message + `session.compacted` + usage line `compaction`; branches from before a compaction see the full history | `runtime::turn::compact`, `runtime::context::effective_history` |
| 4 | `Tool` trait (name, description(env), schema via schemars, kind, availability, `authorizes`, execute), ports (`PermissionGate`, `SpillStore`, `MemoryPort`, `EventSink`), registry hides unavailable tools, alias + camelCase repair, ≤ 5 input issues, `Declined` → only the user's words; `ToolAuthorizer` port (allow / deny / ask) with `DefaultAuthorizer` (asks for non-read-only tools) | `genie-tools`, `runtime::ports`, `runtime::bridge::CallGate` |
| 4.1 | `tool_output_read` (SA-39), `memory_save/update/delete/search` (store-backed, `memory.changed`, off when incognito) | `genie-tools::chat` |
| 01 §5 / 02 §6.2 | `session.compact`, `session.regenerate` (queued as their own turns), `session.get` returns the active path | `genie-core::session::{compact, regenerate}`, `genie-daemon::server` |

Tests (`crates/genie-core/tests/turn.rs`, cassettes only, real engine/bus/Executor/Anthropic
adapter): streamed text turn with usage and cost; tool call → result → second step; ask for a
mutating tool, decline fed back to the model; steer injected mid-turn; retry →
`retry.scheduled`; non-retryable error with fix link; spill; manual, auto and overflow
compaction; `length` continuation; max steps; interrupt; regenerate; title; baseline
`<system-update>`; store-backed turn (messages, memory, `usage_daily`, FTS).

### Spec 06 gaps and deviations

- **Out of scope here (typed extension points only):** shell and code tools (§4.2, §13),
  MCP + `tool_search` (§7), web search/fetch (§8), skills (§9), browser (§10), subagents (§11:
  `finish = stop` with running children), hooks (§12: `turn.end` continue, `compact.before/after`),
  checkpoints (D49), artifacts (D28), `chat_search` / `project_search` tools.
- **Permissions:** `ToolAuthorizer` is the seam for `genie-permissions`; no rules, modes or
  "always" saving are evaluated here. `DefaultAuthorizer` asks per call for every tool that
  authorizes (non-read-only, except memory which §4.1 always allows).
- **Native compaction** (`capabilities.native_compaction`) is not used; we always write our own
  summary. Re-injection after compaction covers the rebuilt baseline only (skills, artifact
  refs, read-state, grants, todo, MCP resources come with their subsystems). If the summary
  still fails the template after one retry it is accepted as long as it is non-empty.
- **Overflow tail percentages** scale the ~15k verbatim tail (our reading of "tail 85 → 70 →
  50 → 35 %").
- **`<system-update>` lowering** is done in `genie-core` at assembly (system/developer
  message for Responses and Chat with `mid_conversation_system`, appended to the next user turn
  otherwise), not in the adapters; updates are only computed at turn start.
- **Per-model tuning table** lives in `genie-core/prompts/tuning/` (not `genie-providers`) so
  all prompt text is in one versioned place. Eval set (§2.6.8) not built (needs live layer).
- **Attachments (§2.5)**, MCP resources, skills and file refs in prompts become text
  placeholders; no image budget yet. `UsageLine` web-search fees are not counted.
- **Wire records** for byte-exact replay are cached in memory per daemon run (the store's
  `wire_record` table stores request/response blobs, not compiled items).
- **Parallel tools:** all calls of a step run concurrently; per-path serialisation of mutating
  file tools arrives with the code tools (`PathLocks` exists).
- **`session.get`** returns the active path but no `siblings`; `prompt` with `parent_id` (edit →
  branch) is still refused. Session metadata stays in the daemon's in-memory registry.
- **Model resolution in the daemon:** bundled catalog + preset routes + credentials from the
  environment (`ANTHROPIC_API_KEY`, `OPENAI_API_KEY`, `GEMINI_API_KEY`, …); provider settings,
  Azure/Vertex/custom endpoints and the catalog refresh loop are not wired.
- **Engine-side ToolError:** `genie-tools` mirrors `genie_core::ToolError` (rule 3) and adds
  `Cancelled`; core converts.
- `turn.settled.error.message` now carries the engine error text and `link` the fix link
  (`CoreEvent::TurnSettled` gained two optional fields).

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
