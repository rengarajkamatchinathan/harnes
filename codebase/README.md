# Genie codebase

Implementation of [spec/01-architecture.md](../spec/01-architecture.md): the Cargo workspace,
crate graph and dependency rules, the shared daemon with its lifecycle, the client `ensure`
path, the session actor, the cancellation tree, the error taxonomy and log files; and of
[spec/04-storage.md](../spec/04-storage.md) in `genie-store` (see its coverage table and gaps below),
and of [spec/06-agent-runtime.md](../spec/06-agent-runtime.md) §7 (MCP client, `genie-mcp`),
§9 (skills and commands, `genie-skills`) and §13.4 (pipe mode, `genie-process`).

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
                   skew handling, idle exit, clean shutdown, standalone lease;
                   `secrets` (backend choice, unlock / locked phase, DB key) and
                   `providers` (credentials, sign-in, models, catalog, usage, budget)
  genie-client     endpoint layout, NDJSON framing, Connection, ensure (spawn / wait / retry)
  genie-process    kill-trees: Job Object (Windows) / process group (Unix), subreaper; pipe mode
                   (piped stdio, env policy, head/tail/spill capture, deadlines, spawn hook) (06 §13.4)
  genie-audit      tracing → rotating log files (10 × 10 MiB); org audit stream → rotating JSONL
  genie-testkit    in-memory transport
  genie-secrets    secret backends: OS keychain (`keyring`), `secrets.enc` (Argon2id +
                   XChaCha20-Poly1305), memory; credential model + store port + backend
                   store; cross-process file lock (03 §10, 08 §2.5/§8)
  genie-store      SQLCipher DB, migrations, GNB1 blobs, FTS, event log, retrieval index (spec 04)
  genie-providers  canonical model, SSE parser, compile step, 4 protocol adapters, Executor,
                   error classification + retry policy, routes/presets, cassettes (spec 03)
  genie-catalog    model catalog: bundled snapshot, signed cache + ETag refresh, discovery
                   parsers, overrides/policy, effort map, pricing (spec 03 §8)
  genie-config     settings layers + schema/UI metadata, managed policy (sources, signed URL,
                   merge), workspace trust + definition-hash approvals (06 §14, 08 §4–5)
  genie-permissions  rule grammar, evaluator (policy deny → deny → ask → allow → mode),
                   modes + policy floor, taint, "always" saving, read-only/arity tables (06 §5)
  genie-mcp        MCP client: stdio + Streamable HTTP, lifecycle/status machine, tools/resources/
                   prompts, progress, cancellation, elicitation/sampling/OAuth ports (06 §7)
  genie-tools      Tool trait + ports, registry, input repair/validation, output limits + spill,
                   v1 chat tools (`tool_output_read`, `memory_*`) (spec 06 §2.4, §4);
                   `shell` tool + command scanner (06 §13), `web_fetch` / web search (06 §8);
                   Code file tools, shadow-git snapshots, worktrees (`code`, 06 §4.2, D49, D50)
  genie-skills     skills: scanner (built-in / user / trusted project), SKILL.md schema, catalogue
                   (policy, enable state, index, load, skill_read), .zip/.skill import, signed
                   skills directory; Markdown slash commands (06 §9)
  genie-browser    agent browser engine side (06 §10): Chrome/Edge discovery + launch over
                   `--remote-debugging-pipe`, own CDP client, AX snapshot/diff/read, site
                   permissions + D21 gates, the 18 `browser_*` tools, screencast + input
  genie-{sandbox,markdown}
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
| 10.1 | credential model, precedence (stored beats env), store port + in-memory store; persistent `BackendStore` (index + `credential:<id>` accounts, one active per provider, hints only) | `genie-secrets::{credentials, store}` |
| 10.2 | Entra auth code + PKCE (S256, loopback `http://localhost:<port>` on 127.0.0.1, `state` checked), device code, refresh via `oauth2` with our rustls client (no redirects); `OAuthTokenSource` refresh under a cross-process file lock with re-read (drops our refresh when another process rotated the token); Google ADC (`authorized_user`, `service_account` RS256 via `ring`; explicit path → `GOOGLE_APPLICATION_CREDENTIALS` → gcloud well-known file) | `genie-providers::{oauth, adc}`, `genie-secrets::lock` |
| 11 | layer 1 cassettes (`tests/cassettes`, `CassetteTransport`, `RecordingTransport`, header scrubbing, secret check) and layer 2 goldens (`tests/golden`, `GENIE_UPDATE_GOLDEN=1`); layer 3 patch tests | `genie-providers/tests` |

### Spec 06 §5 / 08 §3–6 (permissions, config, trust, policy, audit)

| § | What | Where |
|---|---|---|
| 06 §5.1 | `Tool` / `Tool(spec)` grammar: `Bash`/`PowerShell` (exact, `prefix:*`, `*` glob), `Read`/`Edit`/`Write` globs (`//abs`, `~/`, `/root-rel`, `./cwd-rel`, bare = any depth), `WebFetch(domain:…)`, `WebSearch`, `Browser(site:…/action:…)`, `Artifact(…)`, `mcp__server[__tool|__*]`; Windows case-insensitive paths, normalised separators | `genie-permissions::{rule, path}` |
| 06 §5.2 / 08 §3.1 | policy deny → deny (all layers) → ask → allow → mode default, by class not order; compound shell: every part must pass, any denied part denies; unparseable → ask (deny still checked on the raw text); cwd outside workspace → ask | `genie-permissions::eval::Evaluator::evaluate` |
| 06 §5.3 / 08 §5.2 | modes default / accept-edits / plan / bypass; chat always `default`; plan = engine deny of shell, edits, browser act, non-read-only tools, MCP without an allow rule; bypass keeps deny + policy (incl. policy ask); `disableBypassMode` and `defaultMode` floor | `eval::{effective_mode, ModePolicy}` |
| 06 §5.4 | save suggestions (arity prefix, dir globs, domain, `mcp__s__t`, site), default scope Code → local / Chat → user, `remember` (in-memory + where to persist), re-evaluation of pending asks (`released`); file write via `ConfigStore::add_permission_rule` / `remove_permission_rule` | `eval`, `genie-config::layers` |
| 06 §5.5 (SA-29) | taint downgrade of rule / accept-edits allows to ask for mutating shell, edits outside the workspace, MCP not hinted read-only, new fetch domains, browser act on another origin; none in bypass | `eval::Taint` |
| 06 §13.3 (rule side) | built-in read-only list (bash + PowerShell, disqualifying args, redirection chars), arity table, `ReadOnlyClassifier` hook | `genie-permissions::shell` |
| D23 | layers default < user < project < local < CLI < policy, exact paths, project/local only when trusted; deep merge, arrays concatenated; sources per key; locked keys; per-key diagnostics (unknown → warning, invalid → error, sub-keys checked separately); malformed JSON keeps last good copy; content-hash change suppression (`refresh`, `watched_paths`); `config.set` with validation, 0600 atomic writes; JSON Schema + UI metadata (`config.schema`) | `genie-config::{layers, settings, fsutil}` |
| 08 §4 / 06 §14 | `trust.json` `{canonical_path, identity, decision, decided_at, approvals}`; identity match, subfolder inheritance, deeper decision wins, once in memory, revoke; home, `/`, drive roots, system dirs refused; `trust.mode: never`, `trust.allowedRoots`; headless `--trust`; definition hash (canonical JSON SHA-256), pending list with line diff, approve only with the current hash, prune removed entries, approvals reset on re-trust with a new identity | `genie-config::trust` |
| 08 §5.1 | file source (refuses non-root-owned or group/world-writable on Unix), MDM as a `PolicySource` port, signed remote URL (Ed25519, keys + URL only from file/MDM), last good copy per source + Problems, remote cache re-verified on load; merge: allow lists ∩, deny lists ∪, disabling booleans, ordered levels, min numbers, scalars remote > MDM > file; per-key source | `genie-config::policy` |
| 08 §5.2–5.3 | key table merge rules; `org.name`, `disableBypassMode`, `defaultMode` floor, `audit.includeContent` notice text, "Blocked by <org> policy (rule)", "Managed by <org>" lock errors; `policy.effective` result | `policy::EffectivePolicy`, `Verdict::blocked_message`, `ConfigError::Locked` |
| 08 §6 | record `{audit_schema: 1, ts, install_id, user, org, session_id?, event, attrs}`, event names, content only with `includeContent`, secret-named attrs always dropped, rotating JSONL (0600 files, 0700 dir, size-capped, N files), sink port, failures logged never raised | `genie-audit::audit` |

Tests: `genie-permissions/tests/eval_table.rs` (evaluation order, shell cases, modes, policy floor,
taint, save suggestions, always + release, Windows paths), `genie-config/tests/{layers,policy,trust}.rs`
(merge/sources, trust gating, diagnostics/last good, policy locks, set/always, merge table, signed
remote + tamper + cache, trust decision table, approvals), unit tests in each module.

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
  `import.*`; the CLI wires each one and prints "not available in this
  engine build yet". `session.get` returns an empty `path`, so `genie chat open` replays the
  event log instead of printing the branch.
- **Headless flags:** `--allow/--deny`, `--settings`, `--add-dir` and `--worktree <name>` go
  out on `session.create` (session-only layer, `worktree_name`); the daemon now creates the
  worktree (see "Code mode" below).
- **`no_provider_configured`:** core maps every `TurnError::Provider` to the bare code
  `GENIE-PROV`, so the class is lost on the wire; the CLI words bare `GENIE-PROV` as "provider
  failed / none configured". Suggested core change: put the class in `ErrorInfo.message` or the
  code (`GENIE-PROV:<class>`); `errors::turn_error` already handles both.
- **`--continue`** picks the most recent session of the kind: `SessionSummary.workspace_id` is
  never set and `workspace_path` is not stored, so "in this folder" can't be filtered yet.
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

### Spec 06 §7 (MCP client, `genie-mcp`)

| 06 §7 / D30 | What | Where |
|---|---|---|
| Config | Claude-compatible `mcpServers` entry (`{command,args,env,cwd?}` / `{type:"http",url,headers?}`), scope merge (user < project < local < managed, later wins per name), definition hash (D43 approval, change detection), keychain references via `SecretResolver` port | `config` |
| Transports | stdio over `genie-process` pipe mode (own kill-tree, minimal inherited env + `env`, stderr → log ring, exit status as close reason, 16 MiB lines); Streamable HTTP (POST + JSON or SSE, `Mcp-Session-Id`, `MCP-Protocol-Version`, GET stream, 404 → session expired, DELETE on close) behind the `HttpClient` port with a `reqwest`/rustls implementation | `transport::{stdio,http,sse}` |
| Handshake | `initialize` with MCP **2025-06-18** (accepts 2025-03-26, 2024-11-05), client capabilities `elicitation` + `sampling`, mismatch error naming the supported versions, `notifications/initialized`, server instructions | `client` |
| Requests | id allocation, per-request timeout, caller cancellation → `notifications/cancelled`, `_meta.progressToken` + `notifications/progress` routed to the caller, pagination (`nextCursor`), `ping` both ways, unknown server requests → `-32601` | `client` |
| Runtime | status `pending → connected / failed / needs_auth / disabled` (`McpStatus`, `McpServerInfo`); startup 30 s, list 30 s, tool 10 min, other 60 s (configurable); transient connect retries inside the startup deadline; crash/EOF → `failed` → reconnect with exponential backoff (1 s → 60 s, gives up after 6 until `restart()`); `restart`, `set_enabled`, `stop`; event broadcast | `server` |
| `list_changed` | tools: debounced (300 ms) reload + `ToolsChanged`; resources/prompts: `ResourcesChanged`/`PromptsChanged`; `resources/updated` → `ResourceUpdated` | `server` |
| Shared servers | one supervised server per name in the daemon, shared by sessions; reconcile on config change (start/stop/restart by definition hash); `wait_ready(2 s)` grace before the first model call | `manager` |
| Naming | `mcp__<server>__<tool>`, `[A-Za-z0-9_-]`, ≤ 64 chars (hash suffix when cut), deterministic, cross-server collisions get per-pair hash suffixes; `resolve()` maps back to (server, tool) | `naming`, `manager` |
| Schemas | root forced to an object schema, `$schema`/`$id` dropped, `required` filtered to declared properties, > 64 KiB or > 32 levels → permissive object (warnings in the log ring); descriptions capped at 2048 chars; annotations kept as info only (D25) | `schema`, `server::McpTool` |
| Results | structured content first (duplicate serialized text dropped), JSON-looking text parsed, image/audio/blob decoded for the caller's blob store, embedded resources, resource links, `isError` | `result` |
| Taint (08 §2.2, SA-29) | every tool result, resource read and prompt carries `TaintSource {server, trusted}`; untrusted unless the server is on the trusted allowlist (`ServerOptions.trusted`) | `result::TaintSource` |
| Resources / prompts | `resources/list`, `resources/templates/list`, `resources/read`, `resources/subscribe`; `prompts/list`, `prompts/get` | `server` |
| Elicitation | `ClientHandler::elicit` port; parsed request, sensitive-field flags, `Ask` builder (kind `elicitation`), `AskAnswer` → `accept/decline/cancel`; server `notifications/cancelled` cancels the handler and suppresses the reply | `handler`, `client` |
| Sampling | `ClientHandler::sample` port; policy ban and per-server token budget enforced before the handler (refusal = JSON-RPC `-1`), usage charged after; `Ask` builder with preview, model, estimated cost, budget left | `handler`, `client` |
| OAuth | `AuthProvider` port: bearer token, one refresh on 401 (only if the stored token is unchanged), otherwise `needs_auth`; `authorize()` returns the URL (`mcp.auth`); tokens keyed by hash(name, url) | `auth`, `transport::http` |
| Logs | per-server ring (1000 lines, 4 KiB each): stderr, `notifications/message`, lifecycle, schema warnings (`mcp.logs`) | `logs` |

Tests (`crates/genie-mcp/tests/mcp.rs`): an in-process fake server over `tokio::io::duplex`, the same
fake as a real child process (`genie-mcp-fake-server`, run through `genie-process`), and the same fake
behind a fake `HttpClient` (SSE and JSON response modes; no network). Covered: handshake, paginated
tools, naming and schema sanitising, structured results, taint, progress, cancellation and timeout
(server sees `notifications/cancelled`), resources (text, blob, templates, subscribe → updated),
prompts, `list_changed` reload, elicitation, sampling (budget, policy ban), crash → failed → reconnect
(duplex and real process with exit status), transient retries, giving up until `restart()`, version
mismatch, disable/enable, manager collisions/resolve/reconcile, config errors, spawn failure, HTTP
session expiry → reconnect, 401 → `needs_auth` → restart, 401 → refresh.

### Spec 06 §13.4 (`genie-process` pipe mode)

| 06 §13 / D45 | What | Where |
|---|---|---|
| Pipe mode | `spawn_pipe(PipeSpec)` in its own kill-tree (Job Object / process group); stdin `null` or piped; stdout + stderr piped | `genie-process::pipe` |
| Environment | `Env {base: Empty \| Inherit \| Allowlist, remove, set}`: exact env (opencode-pty semantics), inherit minus daemon internals (`GENIE_*`), or the MCP SDK's minimal allowlist; Windows names case-insensitive | `pipe::Env` |
| Output | combined stdout+stderr → bounded head + tail, byte/line/stderr counts, optional spill file with a cap; `OutputSummary::text()` with an omission note naming the file | `pipe::{OutputCollector, TailBuffer}` |
| Timeouts / background | `collect(deadline)` returns `TimedOut` with the child still running (the shell tool backgrounds it, 13.1) and can be resumed; `run()` kills on timeout | `PipeChild::collect`, `run` |
| Cancel | cancel kills the whole tree and reaps it; descendants that keep the pipes open don't hang `collect` (300 ms drain after the direct child exits) | `PipeChild::collect` |
| Error split | `SpawnError` / `RunError::Spawn` = couldn't start, safe to retry; `RunError::Io` = may have run | `pipe` |
| Spawn hook | `SpawnHook::wrap(&mut PipeSpec)` for the v2 sandbox (D44) | `pipe::SpawnHook` |

### Spec 06 §9 (skills and commands, `genie-skills`)

| 06 § | What | Where |
|---|---|---|
| 9 Format | `SKILL.md` frontmatter (`name`, `description`, `disable-model-invocation`, `allowed-tools` as list or string, `metadata`; other keys kept); name rules (a-z0-9-, ≤ 64), description ≤ 1024 and no `<`/`>` (it is rendered into `<available_skills>`); YAML-subset frontmatter parser | `genie-skills::{manifest, frontmatter}` |
| 9 Locations | built-in < `~/.genie/skills` < `.agents/skills` < `.genie/skills` (project roots only when trusted; `.claude/skills` never); id = folder name; depth-limited scan (3) that stops at `SKILL.md`; diagnostics for invalid/shadowed/renamed skills; fingerprint (folder listings + `SKILL.md` size/mtime) → `refresh()` for hot reload | `genie-skills::scan`, `SkillCatalog::{rescan, refresh}` |
| 9 Use | progressive disclosure: `index()` (enabled, model-invocable `(id, description)` for `context::skills_index`), `load()` (fresh `SKILL.md` body + base path + sampled file list ≤ 50, scripts marked "runs in Code mode" in chat / "run with shell" in code, D44), `read_file()` (traversal, hidden-file and symlink-escape guards, 256 KiB cap, binary refused); `Invoker::Model` refused for `disable-model-invocation`; policy + enabled checked for user invocation too; `LoadedSkill::render()` is also the re-injection text after compaction | `genie-skills::catalog` |
| 9 / 02 §6.8 | `skill.list` → `SkillListResult`, `skill.set_enabled` (persisted via `SkillStateStore`: `JsonStateFile` / `MemoryState`), `skill.upload` → `SkillCatalog::upload`, `uninstall` | `SkillCatalog` |
| 9 Sources | `.zip`/`.skill` import: archive ≤ 20 MiB, ≤ 1000 entries, ≤ 16 MiB/file, ≤ 64 MiB total counted while inflating, ratio cap 200 for files > 1 MiB, no symlinks/encrypted/absolute/`..`/drive/backslash paths, `SKILL.md` at root or in one top folder, frontmatter schema, dotfiles/`__MACOSX` skipped, atomic temp-dir + rename (replace = swap), install marker with archive SHA-256 | `genie-skills::package` |
| 9 Sources | 4 built-in skills compiled in (`create-skill`, `write-spec`, `summarise-meeting`, `review-document`), can be disabled or overridden | `crates/genie-skills/builtin/`, `scan::BUILTIN` |
| 9 Sources / 08 §2 | signed skills directory: Ed25519 detached signature over the feed, pinned keys; `directory.list` with query and details manifest in `extra`; `directory.install` returns the "what will run" manifest, installs only with `confirm`, checks pinned version, size, SHA-256 and the exact file list, entry id = package name; fetching is the `DirectoryTransport` port | `genie-skills::directory` |
| 08 §5.2 | `skills.allowed` (allowlist), `skills.directoryOnly` (built-in + directory only, no upload), `skills.disableUserUpload` | `SkillPolicy` |
| 9 Slash commands | `~/.genie/commands` + trusted `.genie/commands` (`command_dirs`), CLI-compatible names (`dir:name`, later wins) and description fallback; `description`, `model`, `agent`, `argument-hint`, `allowed-tools`; `$ARGUMENTS`, `$1..$N` (highest takes the rest, shell-like quoting), `@file` refs (not `@server:resource`, e-mails or URLs), `` !`cmd` `` extracted, never run (`fill_shell` / `without_shell`); built-ins `/compact /init /memory /skills /model /help` shadow user files | `genie-skills::command` |
| 4.1 | `skill` / `skill_read` names, descriptions, schemas and bodies (`run_skill`, `run_skill_read`) | `genie-skills::tools` |

Tests: `crates/genie-skills/tests/{skills,commands}.rs` on temp fixture folders and generated zips
(precedence and trust, diagnostics, enable state persistence, load/read guards incl. symlink
escape, policy, hot reload, upload layouts and every rejection, zip bomb, signed directory list /
confirm / tampered package / wrong file list / wrong key).

### Spec 06 §13 (`shell` tool, `genie-tools::shell`)

| 06 §13 / D45 | What | Where |
|---|---|---|
| 13.1 hidden shell | each call spawns the session's shell in pipe mode (`genie-process`: own kill-tree, stdin null, env = inherited minus `GENIE_*`, `PAGER`/`GIT_PAGER=cat`, `GIT_TERMINAL_PROMPT=0`) in the session cwd | `shell::tool` |
| 13.1 `cd` persists | trailer (`trap … EXIT` / PowerShell `finally`) writes the final directory to a side file; the next call starts there; variables/functions don't carry over; a vanished cwd resets to the session's initial dir; `[cwd is now …]` note | `shell::tool::script`, `ShellRuntime::{cwd,set_cwd}` |
| 13.1 timeout → background | `timeout_ms` (default 2 min, max 10 min); on timeout the running child moves to a `TaskTracker` task (job `bg_…`), `BackgroundJobs::started` (→ `background_job` row) and, at exit, `BackgroundJobs::finished` with exit code, head+tail and spill ref (→ `background_done` inbox message); the model is told it will be notified and not to poll; `run_in_background: true` starts there | `shell::{BackgroundJobs, JobStarted, JobFinished}`, `ShellTool::background` |
| 13.1 kill | call cancel (stop / interrupt) kills a foreground command's whole tree; `ShellRuntime::end_session`, `kill_job` and the shutdown token kill background jobs | `shell::ShellRuntime` |
| 13.1 output | combined stdout+stderr to a 0700 temp file; the model gets ≤ 400 head + 400 tail lines (20 KiB each) with `[… lines a–b of n omitted; full output: <ref>]`, full text in the `SpillStore` | `shell::tool::render` |
| 13.1 errors | spawn failure → `Failed {may_have_run: false}` (safe to retry); anything later → `may_have_run: true`; exit code / signal noted in the output | `shell::tool` |
| 13.2 shells | Unix: `$SHELL` zsh (`+o nomatch`) or bash, else bash, else `/bin/sh`; `shell: "powershell"` → `pwsh`. Windows: `pwsh` → Windows PowerShell 5.1; `shell: "bash"` → Git Bash (next to `git`, then the install dirs; never WSL bash); PowerShell gets the script via `-EncodedCommand`; the description names the active shell | `shell::select`, `ShellTool::description` |
| 13.3 scanner | bash/zsh: `&&` `\|\|` `;` `&` newlines, pipes, `( )`, `{ ;}`, `$( )`, backticks, `<( )`/`>( )`, substitutions in `"…"` and `${…}`, here-docs, comments, `if/while/until`; PowerShell: `;`, newlines, pipelines, `&&`/`\|\|`, `&`, script blocks, `( )`, `$( )`, `@( )`, `@{ }`, here-strings, `#`/`<# #>`, control keywords, `$x = …`; redirections stay in the part; unparseable (unbalanced, `for`/`case`/functions, `switch`/`function`/`param`, `--%`, deep nesting) → `Err` → ask | `shell::scan` |
| 13.3 permissions | ask with resource `Bash(<cmd>)` / `PowerShell(<cmd>)` + `Command {command, cwd}` preview; `shell::authz_from_request` decodes it into dialect, command, cwd and scanned parts for `genie_permissions::Action::Shell`; hidden in Chat and in `plan` mode | `shell::{permission_resource, authz_from_request}` |

Tests: `genie-tools/tests/shell.rs` (real bash/zsh processes: run, cd persistence, no variable
persistence, subshell `cd`, permission request shape, exit codes + stderr, timeout → background →
finished notification, `run_in_background`, `end_session` / `kill_job`, cancel kills a grandchild,
head/tail + spill, declined never runs, spawn failure, timeout without a jobs port, zsh `nomatch` and
EXIT-trap cwd, availability); unit tables for the bash and PowerShell scanner (`shell::scan`), shell
selection on fake Unix/Windows machines (`shell::select`), scripts and `-EncodedCommand`.

### Spec 06 §8 / 08 §2.1 (web, `genie-tools::web`)

| § | What | Where |
|---|---|---|
| 06 §8 fetch | `web_fetch {url}`: http/https only, credential URLs rejected, ≤ 10 redirects (manual, `Location` resolved per hop), 5 MB body cap with a note, 30 s network budget (time waiting for the user excluded), cancel; HTML → Markdown, text/JSON/XML pass through, binary refused, HTTP errors reported | `web::fetch` |
| 06 §8 / D35 permission | `resource` = host, `Url` preview; asked again for **each new host** on a redirect; `web::fetch_authz` decodes it for `Action::WebFetch` (engine adds `pasted` / `seen`) | `web::fetch`, `web::fetch_authz` |
| 06 §8 SSRF | loopback, private, link-local, CGNAT, unspecified, multicast/broadcast, reserved and cloud metadata (incl. IPv4-mapped/compatible, NAT64, 6to4 forms) blocked; every resolved address must pass; the client is pinned to the checked addresses (`resolve_to_addrs`, no proxy) and the peer address re-checked; checks repeat on every redirect; `SsrfPolicy {allow_private, allow}` (metadata stays blocked) | `web::ssrf`, `WebFetch::{checked_addrs, client}` |
| HTML → Markdown | lenient parser (implied end tags, void/raw-text elements, entities), headings, paragraphs, nested lists, links/images resolved against the page URL, emphasis, code, fenced `pre` with language, blockquotes, tables, `hr`; drops head/script/style/noscript/template/svg/iframe/forms and hidden content (`hidden`, `aria-hidden`, `display:none`, `visibility:hidden`) | `web::html` |
| 08 §2.1 / 06 §2.6.4 | results framed as `<untrusted_web_content origin="…">`, a closing tag inside the page is defused; `TaintPort::tainted(session, turn, origin)` on every page / search result (SA-29) | `web::{frame_untrusted, TaintPort}` |
| 06 §8 search (native) | `native_search` → `Declare {allowed, blocked, max_uses: 5}` (copied into `ServerTools.web_search`) or `Disabled {reason}` from policy, global/per-chat toggles, model capability, and what the protocol can enforce (Anthropic: allow + block; Responses: allow only; Gemini: none) | `web::search::native_search` |
| 06 §8 search (local) | `web_search` over a `SearchBackend` port: policy on/off, allowed = policy ∩ requested, blocked = policy ∪ requested, results filtered again, per-turn `max_uses`, untrusted framing, taint | `web::search::WebSearch` |

Tests: `genie-tools/tests/web.rs` (loopback HTTP server + fake resolver, no live network): framed
Markdown + taint + one ask; SSRF blocks loopback by literal, name, mapped IPv6 and hex forms, private,
mixed public/private answers and metadata with zero server hits; pinning (a name only the fake resolver
knows still connects); redirects re-authorised per new host, same-host redirect not re-asked, redirect
into a private range blocked, declined redirect, redirect loop; bad URLs rejected without asking; size
cap, timeout, JSON, binary refused, 404; cancel; frame breakout defused; `web_search` with a fake
backend (filtering, merged block lists, per-turn budget, taint, policy off). Unit tables for the SSRF
classifier, URL/host parsing, content types, HTML conversion and the native-search decision.

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

### Spec 06 §10 (browser, `genie-browser`)

| § | What | Where |
|---|---|---|
| 06 §10 backend, SA-36 | `BrowserBackend` trait; `SystemChromeBackend`: Chrome / Edge / Chromium discovery per OS (macOS app bundles, Windows Program Files / LocalAppData, Linux `PATH` + snap), version ≥ 120 (`--version`, Windows version dir), separate profile `<data>/browser/profile` (0700) or ephemeral, prefs that turn off password saving / autofill and auto-deny geolocation / camera / mic / notifications, flags (no sync / extensions / first-run, `--headless=new` for the desktop), own kill-tree via `genie-process` | `backend`, `discover` |
| 06 §10 transport | `--remote-debugging-pipe` (no TCP port): fds 3/4 on Unix (`pre_exec` + `dup2`), `--remote-debugging-io-pipes=<r>,<w>` inheritable handles on Windows; NUL-framed messages bridged to a channel `Wire` | `transport` |
| 06 §10 CDP client | own async client: id → response routing, flattened sessions (`sessionId`), event fan-out, per-call timeouts, `Timeout`/`Closed` = result lost → status `unknown`, never retried | `cdp` |
| 06 §10 model input, 08 §2.1 | `Accessibility.getFullAXTree` + `DOMSnapshot.captureSnapshot` → outline with stable per-tab refs (`e12` ↔ `backendNodeId`, reset on navigation); interactive (default) / full / `scope=ref`; ≤ 6k tokens with viewport + one screen first and "N more elements"; diff ≤ 1.5k (changed / added / removed); `browser_read` Markdown ≤ 8k from the `main`/`article` landmark; hidden-content stripping (AX-ignored, `visibility`, ancestor opacity < 0.05, ≤ 1 px or off-screen boxes, fonts < 3 px) with a removed count; password/OTP/card values masked | `snapshot`, `read` |
| 06 §10 tools | all 18 tools (`open, navigate, snapshot, read, find, screenshot, click, type, fill_form, select, press_key, hover, scroll, wait, tabs, upload, downloads, handoff`) as `genie_tools::Tool` via `genie_browser::tools(&browser)`; every result = `status` (`ok | blocked | needs_confirmation | user_in_control | unknown`) + tab + URL + title + diff inside `<untrusted_web_content origin=…>`; screenshot only with `BrowserHost::vision`; upload takes chat attachment ids only; pop-ups opened by our tabs join the tab list unfocused | `tools`, `session` |
| 06 §10 per-site permissions (D21) | key = eTLD+1 with exact-host override; `blocked | ask | read | act`; order admin block → admin allow → user host → category → user site → `ask`; default-blocked categories (banking, payment dashboards, government ID/tax, health, password managers, AI provider consoles / API-key pages, cloud consoles); messaging sites readable but every `act` confirms; first visit asks through `ctx.authorize` with a `Browser(site:…)` save suggestion (`once` = this chat, kept as a session grant); `browser.sites.list/set` data | `site`, `Op::gate` |
| 06 §10 sensitive actions (D21) | deterministic: submit of a form with password / `cc-*` / OTP fields or on a page with a payment iframe, clicks named with commit verbs (en/de/fr/es/pt/it/nl), every upload, every download start (cancelled, confirmed, re-fetched into quarantine), category navigation → `browser_confirm` via `BrowserHost::confirm` (also in bypass); typing into password / OTP / card fields refused (hand off instead); plan mode = navigate/read only | `sensitive`, `Op::{confirm, check_downloads}`, `tools` |
| 06 §10 injection defences, SA-29 | untrusted framing, hidden stripping, model-built URL exfiltration guard (> 256 chars of query+fragment, or a ≥ 12-char value copied from another origin's page → blocked; page links and user-typed URLs exempt), taint reported per turn (`BrowserHost::taint`) + ask before `act` on an origin not read this turn, step/time caps (50 actions or 15 min per turn → check-in confirmation), landing pages re-gated after redirects | `site::exfiltration_check`, `Op::{gate, gate_navigation, count_step, finish}` |
| 06 §10 audit | `browser.action` event per call (tab, tool, origin, decision `allowed:{site,chat,user,policy}` / `confirmed:user` / `blocked:{policy,category,user,mode,scheme,exfiltration}` / `denied:user`, status, thumbnail after `act` steps via `BrowserHost::store_image`) | `Op::conclude` |
| 05 §9, SA-23/24, D56 | `screencast_start/ack/stop` (one CDP screencast per tab, many streams, one frame in flight per client, Chrome acked only when a client can take a frame, newest frame wins, JPEG size → `device_scale`); `input` maps mouse / key / wheel / IME (`imeSetComposition`) / `insertText`, and any input but pointer moves takes control; `takeover / handback / stop`, `tabs`, `tab_activate`, `clear_data`, `status` | `screencast`, `input`, `Browser` |

Tests: unit tests per module; `tests/browser.rs` runs every gate through the tools against
`testing::FakeCdp` replaying `tests/fixtures/checkout.json` (recorded AX/DOMSnapshot/Target
responses): site ask + act upgrade + submit confirmation + audit, password refusal,
exfiltration, category, scheme, plan mode, commit-verb denial, take-over/hand-back note,
step-cap check-in, handoff, lost result → `unknown`, download confirmation, read/find/tabs/
type/fill_form, screenshot, screencast flow control + input. `real_chrome_smoke` (`--ignored`)
drives a local Chrome against a tiny local HTTP page.

### Engine wiring (06 §5, §7–§10, §13; 08 §3–6; 02 §6.4, §6.8, §7)

The subsystems are wired into the turn loop (`genie-core::runtime`) and the daemon
(`genie-daemon::{lib, services}`):

| What | Where |
|---|---|
| Settings/policy/trust hub: one `ConfigStore` per project root (+ user-only for Chat), shared `EffectivePolicy` and `TrustStore`, `HubEvent`s; managed policy from `FilePolicySource::system()` + remote cache, re-read every 60 s; settings files polled every 2 s (content-hash suppressed) | `genie-core::config::ConfigHub`, `genie-daemon::services::Services::start` |
| Permissions in the turn loop: rules folded from every loaded layer + policy `permissions.deny/ask` + session-only CLI layer + remembered "always"; `ModePolicy` from `disableBypassMode` / `defaultMode` floor; tool call → `Action` (`shell` via `shell::authz_from_request`, `web_fetch` via `web::fetch_authz` with `pasted`/`seen`, `mcp__*`, `browser_*` read vs act, `skill`→`Skill(name)`); deny → `Blocked` (`tool.ended.error.code = policy_blocked` + "Blocked by <org> policy (rule)" for policy); ask → durable ask with save suggestions; "always" → `Evaluator::remember` + `ConfigStore::add_permission_rule` in the chosen scope + `permission.resolved.saved_rule` + release of the session's other pending asks (`by: rule`, 02 §7 step 6) | `runtime::permissions::PermissionAuthorizer`, `runtime::bridge::CallGate`, `session::resolve_ask` |
| Taint (SA-29): per-turn cell set by untrusted results (MCP unless trusted, `External` tools, `web::TaintPort`, browser host) → evaluator `Taint` | `runtime::host::TurnTaints`, `AgentRuntime::taint_source` |
| Audit (08 §6): `permission.asked`, `permission.decided`, `policy.blocked`, `settings.changed`, `trust.changed`, `mcp.connected`, `skill.installed` (sha256), `browser.site_approved`; local JSONL in `<data>/audit/` (or `audit.jsonl.dir`) | `PermissionAuthorizer`, `services`, `genie-daemon::build_auditor` |
| `config.get/set/schema/diagnostics`, `policy.effective`, `trust.get/set/pending_definitions/approve_definition` (paths canonicalised); `config.changed` / `policy.changed` on the workspace feed | `genie-daemon::services` |
| MCP: one manager for user + managed servers, one per project root for `.mcp.json` / project-settings servers (trust **and** D43 hash approval; unapproved ones listed as `disabled`) and local servers; reconcile on settings/trust/policy events; policy `mcp.allowed`, `mcp.disableLocal`, `mcp.sampling: off`, `mcp.trusted`; tools exposed per turn as `ToolProvider` tools (2 s grace), authorised through the gate, results tainting; elicitation/sampling → asks of the calling turn (sampling then runs the chat model); `mcp.list/add/remove/set_enabled/logs/tools/resources/prompts/resource.read/auth.start`; `mcp.status` on the feed | `runtime::mcp::McpHub`, `runtime::active::ActiveTurns` |
| Skills: catalog per workspace root (project roots after trust), policy keys, `skills-state.json`, timer refresh, roots/policy reset on trust/policy change; `<available_skills>` + `feature.skills`; `skill` / `skill_read` tools (`Skill(name)` permission first); loaded skills re-injected after compaction; `InputPart::Skill` rendered; `/command args` expanded (file refs → `file_ref` parts); `skill.list/set_enabled/upload`, `directory.list/install` (skills) | `runtime::skills`, `services` |
| Shell (Code) + background jobs (`background_job` rows, keep-alive, synthetic `background_done`), `end_session` on delete/eviction; web (`web_fetch` for Chat + Code; provider-native search declared in `ServerTools.web_search` from policy `webSearch.*`) | `runtime::host::{EngineJobs, SettingsDirs}`, `RuntimeBuilder::{shell, web, web_search}` |
| Browser: one `Browser` (`SystemChromeBackend`, Chrome launched on first use), tools for Chat + Code (hidden in headless sessions unless `enable_browser` or a `Browser(site:…)` allow), `EngineBrowserHost` (confirm → `browser_confirm` ask / `Unavailable` when no attached client renders it, taint, image blobs, typed-URL check, vision, `browser.control` banner events), `browser.*` methods (lease-gated where the table says; `stop` interrupts the turn), sites persisted in `browser.sites` user settings, admin lists from policy, backend shutdown on exit | `runtime::browser`, `services::browser_async` |
| Protocol (additive): `ConfigSource::Cli`, `ConfigScope::Managed`, `session.create.{settings: {allow, deny, settings_path, add_dirs, enable_browser}, worktree_name}`, `tools.list`, `tool.ended.error`, `mcp.add/remove.cwd`, `browser.control` event | `genie-protocol` |
| Paused-ask notification also when the attached clients can't render the ask kind (e.g. headless) | `DaemonState::can_render`, workspace pump |
| CLI: session-only flags on `session.create`, `system/init.tools` from `tools.list`, `mcp add/remove` send the cwd | `apps/cli` |

Tests: `genie-core/tests/turn.rs` `wiring::*` (cassettes, real authorizer: policy block,
"always" + saved rule + release, MCP over an in-memory fake server with taint downgrade,
skills index/tool/commands, `shell` read-only vs denied, `web_fetch` against a loopback
server (ask vs pasted), `browser_navigate` site ask against the fake CDP endpoint);
`genie-daemon/tests/services.rs` (config/policy/trust methods and `config.changed`,
`tools.list`, session-only settings validation, skills methods, the can't-render
notification, headless browser gating + `browser.sites` persistence + `browser.status`);
`config::tests` (hub trust gating and approvals).

### Projects, artifacts, memory, blobs, retrieval (02 §6.9, D28, D31, D32, 06 §2.5, §4.1, §6)

| § | What | Where |
|---|---|---|
| 02 §6.9 | `project.list/create/update/delete`, `project.file.add/remove`, `project.index.status` (`mode`, files, tokens, `capacity_pct`, `indexing`), `artifact.list` (versions on the active path or up to `leaf_id`) / `artifact.get`, `memory.list/update/delete/set_state/reset`, `blob.put.begin/chunk/end` (1 MiB chunks, in order, retried chunk idempotent, `expected_offset` on conflict), `blob.put.path`, ranged `blob.get` (≤ 4 MiB) | `genie-daemon::{projects, blobs}` (hooked first in `Services::dispatch`) |
| 02 §6.2 | `session.search` (`message_fts`, all / project scope, incognito excluded), `session.export` (JSON `{info, messages, parts, artifacts}` with `sanitize`, Markdown; > 1 MiB → blob; policy `export.allowed: false` → `policy_blocked`) | `genie-daemon::projects` |
| D28, t39, 06 §4.1 | `artifact_create {type, title, content, language?}` / `artifact_update {id, old_str, new_str}` (exact, unique) / `artifact_rewrite {id, content}`; six types; always allowed (no ask), Chat only, admin switch `artifacts.enabled`; 4 MiB cap; versions **staged per tool call** and committed with the assistant message that carries the call (`ArtifactConversation` decorator), so branches get their own versions; `artifact.versioned {artifact_id, n, message_id, type, title}`; > 256 KiB content as a blob; `feature.artifacts` guidance with the bundled library list | `genie-core::runtime::artifacts` |
| D31, t10, 06 §6 | memory on/paused (`kv memory.state`, survives restarts; admin `memory.enabled`); paused/incognito → memory tools hidden and memory left out of the prompt; `<memory scope="global|project">` baseline sources (MRU first, ~8 KiB each, ids shown for `memory_update`, pointer to `memory_search` when cut); changes reach running sessions as `<system-update>` | `genie-core::runtime::memory_ctl`, `turn::tool_env` |
| D31 | `chat_search {query, scope: current_project|non_project}` (current chat and incognito chats excluded, hits with chat/message ids), `feature.chat_search`; admin `chatSearch.enabled` | `genie-core::runtime::chat_search` |
| D32, SA-18, 04 §7 | text extraction (UTF-8 text/code/Markdown, PDF via a built-in `FlateDecode` + text-operator extractor), chunking (~800 tokens; Markdown by heading, code by top-level item, PDF by page, plain by paragraph; overlap when a section is cut), file + chunks in one transaction, background embedding job (cached by content hash, `indexing` flag), capacity cap = min(10 × capacity window, 20M) tokens and 500 files; full mode while ≤ 50 % of the current model's window (re-evaluated every turn) → `<project_instructions>` + `<project_knowledge>` in the baseline (1 h TTL), retrieval mode → `<project_excerpts>` (top 5, ≤ 4k tokens) on the user message + `project_search` tool + `feature.project_search`; ranking BM25 (any word) + cosine fused with RRF, BM25 alone without vectors; sessions linked to their project for chat search scopes | `genie-core::runtime::{projects, retrieval, extract}`, `genie-store::retrieval::{bm25_any, project_stats}` |
| 03 §9 | `EmbeddingClient` port; `OpenAiEmbeddings` (OpenAI-compatible `/v1/embeddings` through the `Transport`, errors classified, batches of 64, cancel) wired from `OPENAI_API_KEY`; deterministic offline `HashEmbedder` for tests | `genie-providers::embedding`, `projects::embedder_from_env` |
| 06 §2.5 | attachments: images → media parts (blob-referenced, refcounted via `part.blob_id`), base64 at assembly; no vision → clear note; PDFs → media with extracted text, text for models without PDF input; text files inlined as `<attachment>` (≤ 256 KiB); other types → note; image budget 25 MiB → ≤ 15 MiB in 10 MiB steps | `genie-core::runtime::attachments` |

Tests: `genie-core/tests/knowledge.rs` (cassette turns, real store/engine/Anthropic adapter,
`HashEmbedder`): artifact create → update (v1, v2, `artifact.versioned`, branch sees v1 only,
failed update makes no version), project knowledge in full mode, retrieval mode (excerpts on the
message, `project_search` result, embeddings computed), capacity cap + unreadable files, memory
paused (tools hidden, `<system-update>` removal, nothing saved, state persisted), `chat_search`
across chats, attachments (text, image, PDF fallback, no-vision note); unit tests for the chunker,
PDF extractor, image budget and embedders; `genie-daemon/tests/projects.rs` (in-memory transport):
chunked upload round trip (conflict, retry, early end, ranged reads, `blob.put.path`), project
methods and files, memory methods and state, artifact list/get, `session.search`, `session.export`.

### Spec 06 §4.2, D49, D50, 02 §6.10 (Code mode: file tools, checkpoints, worktrees)

| § | What | Where |
|---|---|---|
| 06 §4.2 `read` | path relative to the workspace (or absolute, `~/`), `offset`/`limit`, `cat -n` numbering, 2,000 lines / 50 KiB / 2,000 chars per line with a "read with offset=N" note, BOM stripped, binary/image/PDF reported (not shown), "similar names" on a miss, pages spilled-output refs; records **mtime + hash** per session | `genie-tools::code::{read, ReadState}` |
| 06 §4.2 `write` | create (folders too) or overwrite **only after read** + staleness check, keeps the file's CRLF/BOM, atomic temp + rename keeping permissions, `Diff` preview ask | `code::write` |
| 06 §4.2 `edit` / `multi_edit` | `old_string/new_string/replace_all` (aliases `old_str`, `file_path` …); 3 tiers exact → Unicode-normalised (quotes/dashes/nbsp folded char for char, mapped back to the original bytes) → trailing-whitespace-tolerant lines; a non-unique match fails unless `replace_all`; "closest lines" hint (Levenshtein) on a miss; LF view preserving CRLF + BOM; read-before-edit + staleness **inside the per-path lock**; ask **after** the match (a miss never asks); empty `old_string` creates a file; `multi_edit` all or nothing | `code::{edit, matcher}` |
| 06 §4.2 `patch` | Codex envelope (Add / Delete / Update + Move to, `@@` context headers, `*** End of File`), fuzz (exact → trim-end → trim), all paths locked in sorted order, one ask per file, atomic write with rollback | `code::patch` |
| 06 §4.2 `grep` / `glob` / `list` | ripgrep's own crates in-process (`ignore` walker: `.gitignore` in repos, `.ignore`, git excludes, hidden skipped; `regex`; `globset`), so every OS behaves alike; grep `content` / `files_with_matches` / `count`, context lines, glob filter, binary and > 20 MiB files skipped; glob newest first; list as an indented tree; 100 results / 500 entries, 30 s, cancellable | `code::search` |
| 06 §4.2 `todo_write` | whole-list replace, kept per session (`TodoStore`) | `code::todo` |
| 06 §4.2 registry / permissions | Code registry only (`RuntimeBuilder::code`); every tool asks the gate with the absolute path (`read/grep/glob/list` → `Action::Read`, editors → `Action::Edit` + `Diff` preview) through the existing `permissions::action_for` names | `runtime::code`, `runtime::permissions` |
| 06 §4.2 "serialised per path" | `PathLocker` port; `EnginePathLocks` on `genie-core::locks::PathLocks`, one instance per daemon (all sessions and clients) | `code::PathLocker`, `runtime::code::EnginePathLocks` |
| 04 §10 / D49 shadow git | `<data>/snapshots/<project or _>/<fnv(work tree)>/` (0700), `GIT_DIR` + `GIT_WORK_TREE`, alternates to the user's objects, index seeded from the user's index, user `info/exclude` synced, untracked files > 2 MiB skipped and reported, non-git folders snapshotted directly (≤ 50,000 untracked files); `git` through `genie-process` (kill-tree, `GIT_DIR`-style overrides removed, no prompts) | `code::{git, snapshot}` |
| D49 per step | before snapshot lazily, just before the first `edit/write/multi_edit/patch/shell` call of a step; after snapshot when the calls joined; a step that changed files → JSONL record + refs `refs/genie/<session>/<n>/{before,after}` + durable `checkpoint.created {step_ref = the step's assistant message id, before_tree, after_tree, skipped}` | `runtime::checkpoint`, hook in `runtime::turn` |
| D49 rewind | `checkpoint.list` (per message on the active path, `files_changed`), `preview` (files with their change relative to now, `external_changes` = touched paths changed outside Genie between checkpoints or since the last, `skipped`), `rewind` files / conversation / both (path-level restore through a temporary index; leaf → the message's parent + `branch.switched`), two-phase: `undo` restores files and leaf, the next turn commits; refused while the session runs (`busy`) | `runtime::checkpoint`, `genie-daemon::code` |
| D49 retention | `session.delete` drops the session's refs and log and runs `git gc --prune=now`; `Checkpoints::disk_usage` per project folder | `Checkpoints::drop_session` |
| 06 §15 / D50 worktrees | `session.create` (Code) with `worktree: new` or `worktree_name` → `../<repo>.genie-worktrees/<slug>/` on `genie/<slug>` from HEAD (numeric suffixes; a session started in a subfolder keeps its relative folder); `auto` → only when another Code session already works in that repo (otherwise, or on failure, the main checkout); `new` in a non-git folder → `invalid_state` | `code::worktree`, `genie-daemon::code` |
| D50 finish | `worktree.finish`: merge (clean worktree, main checkout on the base branch, `--no-ff`, aborted on conflict, then remove + `branch -d`; the session moves back to the main checkout), pr (network-free: returns the push + `gh pr create` commands), keep, discard (remove; the branch is deleted only when nothing is unmerged/unpushed, else kept and said so); `session.delete` removes a clean, fully merged/pushed worktree | `code::worktree::finish`, `genie-daemon::code` |
| 02 §6.10 panels | `git.status` (porcelain v1, branch, added/modified/deleted incl. untracked), `git.diff` (against HEAD, or the index on an unborn branch), `git.commit` (`paths`; `then: push` runs `git push -u origin HEAD`, `then: pr` returns instructions), `files.tree` (one level, ignore rules, hidden files shown except `.git`), `files.read` (text ≤ 2 MiB, else a store blob); paths confined to the session folder (symlink escapes refused) | `genie-daemon::code` |
| Protocol (additive) | `worktree.finish` → `WorktreeFinishResult {removed, branch_deleted, merge_commit?, message}` (was `{}`); `GitCommitResult.note` | `genie-protocol::api::code` |

Tests: `genie-tools/tests/code.rs` (every tool on temp folders: paging, read-before-edit, staleness,
another session's read, CRLF/BOM, matcher tiers, uniqueness, miss hint without an ask, decline,
atomic multi-edit, patch rollback + move/delete, ignore rules, 100-result cap, todo, concurrent edits
of one path; shadow snapshots in a real repo and a plain folder, big-file skip, restore, refs + gc;
worktree create / suffix / merge / PR / discard with and without unpushed commits, non-git refusal)
plus unit tests (matcher, Myers diff, patch parser); `genie-core/tests/code.rs` (cassette turns: read
→ two parallel edits of one file + write in one step → one checkpoint; a shell step's checkpoint;
list / preview / rewind / undo of files, conversation and both; external-change detection; a failed
edit and read-only steps leave none; drop); `genie-daemon/tests/code.rs` (in-memory transport:
plain / auto / named worktree sessions, `files.tree/read` + escape refusal, `git.status/diff/commit`,
merge / keep / discard, checkpoint errors, `new` without git).

### Spec 08 §2.5 / §8, 04 §2, 03 §10, 02 §6.1 / §6.5 (secrets, provider auth, providers)

| § | What | Where |
|---|---|---|
| D26 / 03 §10.1 | backends: macOS Keychain / Windows Credential Manager / Linux Secret Service via `keyring` (service `genie`, `genie-beta`, `genie-dev`), probed at start; else `secrets.enc`; `secrets.backend: "file"` forces the file | `genie-secrets::{keychain, vault}`, `genie-daemon::secrets::Secrets::open` |
| 08 §8 / 04 §1 | `secrets.enc` 0600: Argon2id (m 64 MiB, t 3, p 1, stored with the file, bounds-checked) + XChaCha20-Poly1305, fresh nonce per write, KDF params as AAD; wrong passphrase leaves it locked; `change_passphrase` | `genie-secrets::vault` |
| 04 §2 | data key in the backend through `SecretsKeyStore` (`genie_store::KeyStore`); recovery key flow unchanged (`genie-store::keys`); the old `keystore.placeholder.json` is migrated once (verified, then deleted; a different keychain key is kept as `data-key.replaced`) | `genie-daemon::secrets::{SecretsKeyStore, migrate_placeholder}`, `open_store` |
| 02 §2 / §6.1 | `GENIE_PASSPHRASE` unlocks at start; otherwise a locked phase serves the endpoint with `InitializeResult.locked = {reason: "passphrase_required"}`, answers only `secrets.unlock`, `daemon.status/restart`, `doctor.run`, `$/ping`, `workspace.subscribe` (everything else → `locked` −32006); after `secrets.unlock` → `daemon.restarting` and the full engine starts with the store opened (clients reconnect) | `genie-daemon::secrets::locked_phase`, `providers::ProviderHub::dispatch` |
| 02 §6.5 | `provider.list` (built-in + custom `providers.*`, status, credentials incl. env ones with source label + 4-char hint, endpoint), `provider.credential.add/remove/activate` (keys; Google `external` = ADC path; OAuth only via sign-in; env credentials can't be removed), `provider.login.start/status` (Entra PKCE / device code; the fresh credential becomes active), `provider.test` (authenticated models GET, latency; failure → status `error`), `model.list`, `catalog.status/refresh`, `usage.query` (group by day/provider/model/project, cache + compaction tokens, cost), `usage.budget.set` | `genie-daemon::providers` |
| 02 §3.2 | `provider.status` on every status change (credentials, sign-in, test, settings/policy events), `catalog.updated` after a refresh, budget `notification`s at 80 % and 100 % once per month and level (checked every minute and on set; no hard stop) | `ProviderHub::{refresh_statuses, catalog_refresh, check_budget, start}` |
| 03 §8.1 | catalog = bundled / verified cache → signed refresh (policy `catalog.url` or `GENIE_CATALOG_URL`, `<url>.sig`, ETag, on start + every 6 h) → live discovery (OpenAI / OpenAI-compatible / OpenRouter `/v1/models`, Gemini `models.list`, Ollama `/api/tags`; 10 min TTL; "limited info") → Azure deployments from `providers.azure-foundry.deployments` → user overrides `models.<id>` → policy (`providers.allowed`, `models.allowed/blocked` with `prefix*`); one shared copy read by the resolver | `ProviderHub::{rebuild_catalog, model_list}` |
| 03 §10 / D38 | the engine's model resolution goes through the hub (`ProviderBinder`): API keys (`x-api-key`, `api-key`, `x-goog-api-key`, Bearer), Entra tokens (`TokenAuth` cached per credential: single-flight + forced refresh on 401), Google ADC → Vertex, Ollama / custom endpoints without auth, Settings endpoints (`base_url`, `project`, `location`), `AccountTag` per credential; policy `providers.allowed` and `auth.methods` (`api_key`, `entra`, `adc`) enforced on add, sign-in and resolution | `genie-core::runtime::models::{ProviderBinder, CatalogResolver::{with_binder, with_shared_catalog}}`, `ProviderHub::auth_for` |
| 08 §2.5 / §6 | secrets never in settings, logs or results: `SecretString` / `Zeroizing`, redacted `Debug` on grants, params, backends and stores; `credential.changed` / `auth.signed_in` audit events without secrets | all of the above |

Tests: `genie-secrets` unit tests (memory backend, vault create/unlock/wrong passphrase/tamper/re-key/0600, backend store over memory and a locked vault, file lock); `real_keychain_roundtrip` is `#[ignore]` (run by hand, touches the real keychain). `genie-providers/tests/oauth.rs` against a fake local token endpoint: PKCE loopback (S256 verified by the fake), forged `state`, device code with `authorization_pending`, refresh + persisted rotation, rotation by "another process" under the lock, `TokenAuth` 401 path, ADC authorized-user and service-account (RS256 verified with the test-only key in `tests/fixtures/`). `genie-daemon/tests/providers.rs`: credentials + `provider.status` + redaction + test failure, discovery + overrides + resolver auth, policy, Entra device-code sign-in → bearer on the fake Foundry endpoint, PKCE URL, usage + 80/100 % budget notifications, signed catalog refresh, locked gate + unlock, and the real-endpoint locked phase → unlock → restart → store key in `secrets.enc` + placeholder migration. Automated tests only use the memory backend or `secrets.enc` with cheap KDF parameters.

## Known gaps and deviations

- **`genie-daemon` depends on `genie-client`** for the shared transport (endpoint naming, framing,
  Windows SID helpers). The other direction would pull the engine into the Tauri side.
  `xtask check-deps` allows exactly this edge.
- **Subsystem edges are not declared yet.** Cargo 1.99 warns on unused dependencies, so
  `genie-core` gains each subsystem dependency when it first uses it; `xtask check-deps`
  enforces the directions in the meantime.
- **Store fallback:** if the store can't be opened (e.g. the keychain entry is gone and the
  DB exists) the daemon logs it and runs in memory; the recovery-key / quarantine prompt
  (04 §2) needs UI (spec 05).
- **Windows Job Object race:** a child is assigned to its job right after `spawn`, so anything it
  starts in that gap escapes. Closing it needs a raw `CreateProcessW` with `CREATE_SUSPENDED`
  (planned with the `opencode-pty` port, D45).
- **Windows token file** relies on the per-user `%LOCALAPPDATA%` ACL; no explicit DACL is set.
- **Not in this milestone:** desktop app, `packages/ui` and the Platform-adapter lint rule (spec 05);
  CI workflows.

### Projects, artifacts, memory, retrieval gaps and deviations

- **Artifact language** (`artifact_create.language`) is echoed to the model but not stored (no
  column / wire field); the artifact panel, sandbox and "Fix with Genie" are desktop work.
- **Capacity window** (SA-18) is a config value (`ProjectsConfig::capacity_window`, default
  200k), not computed from the user's enabled models; `project.index.status` reports the mode
  for `status_window` (200k) because it has no session model.
- **PDF extraction** handles uncompressed and Flate content streams with Latin-1/UTF-16BE
  strings; CID fonts with custom CMaps, ASCII85/LZW streams and page-tree ordering are not
  handled (pages = content streams in file order). Images in PDFs are ignored.
- **Embeddings:** only OpenAI (`OPENAI_API_KEY`, `OPENAI_BASE_URL`); SA-14's provider order
  and `projectRetrieval.embeddingsProvider` are not applied; no embedding usage line;
  `OpenAiEmbeddings` does one attempt per batch (no Executor retry loop).
- **Uploads** are buffered in daemon memory until `end` (≤ 512 MiB, 32 concurrent, dropped
  after 1 h idle); `name` in `blob.put.begin` is not stored.
- **Excerpts** are stored with the user message (so they stay in history and in `session.get`);
  `session.search` returns title hits with an empty `message_id`.
- **Memory:** Code sessions use global memory only (no workspace scope yet); `memory.*`
  methods publish no `memory.changed` (that is a session event); sensitive-category filtering
  is left to the prompt guidance. Policy keys `memory.enabled`, `artifacts.enabled`,
  `chatSearch.enabled` are read once at daemon start (our names).
- **Not implemented:** `export.all` (SA-17 zip), session import, custom instructions ("Your
  Genie") as a baseline source, project file update/re-index on change, reading `kchunk` text
  for full mode (file text is re-extracted and cached in memory).

### Engine wiring gaps and deviations

- **Hot reload is a 2 s poll** of the settings files (and 60 s for managed policy), not the
  debounced (100 ms) OS watcher; the remote-policy HTTP fetcher is still a port.
- **Shared MCP servers** are shared per manager: user + managed servers once per daemon,
  project/local servers once per project root (a project server overrides a user server of the
  same name for that project). `mcp.trusted` (array of server names, policy) is our assumption
  for the trusted allowlist. Sampling cost estimate is 0 and sampling usage is not recorded.
  Elicitation/sampling outside a running tool call of that server is declined. MCP tool
  `readOnlyHint` is not passed to the evaluator (taint downgrade always applies).
- **Permission request encoding:** tools report via `resource` + `preview`; tool-name tables in
  `permissions::action_for` map them to actions. Cross-session release after "always" is not
  done (only the session's own pending asks are re-evaluated).
- **Slash commands:** `` !`cmd` `` snippets are replaced by a note (the shell permission path
  is not driven from prompt expansion yet); `@file` refs become `file_ref` parts, which the turn
  still renders as placeholders. The CLI still expands its own user commands with
  `apps/cli/src/tui/commands.rs` before sending, so the engine expansion serves other clients.
- **Skills directory** has no HTTP transport (`directory.list/install` for skills report it as
  unavailable); `skill.upload` reads the blob from the store.
- **Background jobs** are not killed on interrupt-with-stop (`stop_subagents`); a daemon restart
  leaves `background_job` rows without `ended`.
- **Browser:** `browser_screenshot` returns a blob reference (no image part in the tool result
  yet); there is no `--enable-browser` CLI flag (the wire field exists); "always" answers for
  sites are saved as `Browser(site:…)` permission rules, not as `browser.sites` states;
  `attachment_path` is not implemented; `browser.popout` is not routed.
- **`add_dirs`** is stored and fed to the evaluator; worktrees are created (see "Code mode").
- **Audit** covers the events listed in the coverage table; tool calls, usage, sign-ins and
  hook runs are not recorded yet.

### Spec 06 §5 / 08 §3–6 gaps and deviations

- **OS sources are ports only**: macOS configuration-profile and Windows registry readers, the
  ADMX template, the Windows ACL check on `policy.json` (Unix checks root owner + no g/o write),
  the hourly remote refresh loop and its HTTP fetcher, and the OTLP audit exporter.
- **Assumptions** (spec ambiguous): `defaultMode` floor = most permissive mode allowed; policy
  `permissions.ask` still asks in bypass; plan mode blocks MCP unless an allow rule matches (no
  ask); reads outside the workspace + `additionalDirectories` ask; `Read(...)` and `Edit(...)`
  rules don't cover each other (`Write` is an alias of `Edit`); `prefix:*` is word-bounded;
  `*.example.com` also matches the apex; `/path` rules are relative to the project root in
  every layer; `trust.allowedRoots` auto-trusts beneath them (even with `trust.mode: never`);
  a policy file that disappears keeps its last good copy for the daemon's lifetime; settings
  keys locked by policy are the exact settings paths a policy sets.
- **Settings schema** covers permissions and the keys policy can lock; `mcpServers`, `hooks`
  and `providers` are opaque JSON until their specs land. Deprecated-key migration is not built.
- Audit content opt-in drops content attrs, but callers must still route content through
  `Attrs::content`; nothing scans plain attribute values.

### Spec 02 gaps and deviations

- **Methods owned by other specs** (terminal, import/`export.all`,
  `browser.popout`) are typed in the table and generated
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

### Spec 06 gaps and deviations

- **Out of scope here (typed extension points only):** `tool_search`
  deferral (§7), subagents (§11:
  `finish = stop` with running children), hooks (§12: `turn.end` continue, `compact.before/after`),
  and nothing else (artifacts, `chat_search` / `project_search`: see "Projects, artifacts…";
  Code tools and checkpoints: see "Code mode").
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
- **MCP resources, skills and file refs** in prompts become text placeholders (attachments
  are read, see the projects section). `UsageLine` web-search fees are not counted.
- **Wire records** for byte-exact replay are cached in memory per daemon run (the store's
  `wire_record` table stores request/response blobs, not compiled items).
- **Parallel tools:** all calls of a step run concurrently; mutating file tools serialise per
  path through `PathLocks` (shell commands are not path-locked).
- **`session.get`** returns the active path but no `siblings`; `prompt` with `parent_id` (edit →
  branch) is still refused. Session metadata stays in the daemon's in-memory registry.
- **Model resolution in the daemon** goes through `genie-daemon::ProviderHub` (stored
  credentials, env keys, Entra, ADC, Settings endpoints, refreshed catalog); see the secrets /
  providers section.
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
- **Keychain**: the daemon adapts `genie-secrets` backends to the `KeyStore` port
  (`genie-daemon::SecretsKeyStore`). Recovery-key UI/`.txt` download is UI work (spec 05).
- **Blobs**: `put_blob` takes the whole buffer (two passes would be needed for streaming input
  because the address is the plaintext hash); no streaming writer yet.
- **Not implemented:** long resumable data migrations (kv cursor jobs); export-all `.zip` (SA-17),
  session import with id remapping (shadow git §10 lives in
  `genie-tools::code::snapshot` + `genie-core::runtime::checkpoint`, see "Code mode");
  the in-memory `Store` for incognito (§8); the daily scheduler that calls `run_maintenance`.
- **Build:** `rusqlite` uses `bundled-sqlcipher-vendored-openssl` (macOS has no OpenSSL headers);
  it needs Perl and a C compiler. Not yet tried on Windows with `scripts/cargo-win-gnu.sh`.

### Secrets and provider auth gaps and deviations

- **Entra app id:** no Genie multi-tenant Entra app is registered yet (t35 item 2). Builds pick
  it up from `GENIE_ENTRA_CLIENT_ID` at compile time, or `providers.azure-foundry.client_id`
  in settings; without one `provider.login.start` answers `invalid_state`. The redirect is
  `http://localhost` (any port), which the app registration must allow (public client,
  "Mobile and desktop" platform, device-code flow enabled). `authority` / `tenant` can be set
  under `providers.azure-foundry` (https or loopback only).
- **Locked phase restarts the engine in-process:** the unlocking client gets `{ok}`, then
  `daemon.restarting`, and must reconnect (as for `daemon.restart`). No passphrase prompt in
  the CLI yet (spec 07); `GENIE_PASSPHRASE` works. No passphrase change / backend switch
  method (a `FileVault::change_passphrase` exists); switching backends doesn't migrate
  entries; `secrets.backend` is read from the user settings file at start only.
- **Keychain limits:** Windows Credential Manager caps a blob at 2,560 bytes (large OAuth grants
  or many credentials in the index may not fit); macOS may prompt for access on first use
  of an unsigned dev build; the keychain probe can't tell a locked Linux keyring from a missing
  Secret Service (both fall back to the file).
- **Not implemented:** the pasted-redirect fallback for non-Entra OAuth, Azure deployment
  discovery via the management API (manual `deployments` only), Ollama `/api/show` context
  lengths, the catalog feed's default URL and pinned key (D40; only policy `catalog.url` /
  `GENIE_CATALOG_URL`), `endpoints.allowed` policy, per-request usage audit. Budget months
  are UTC like `usage_daily` days. `provider.test` for Vertex only checks that a token can be
  obtained.
- **Settings keys added:** `models` (overrides) and `secrets` are now known top-level settings
  (opaque JSON) so the layer validation keeps them.
- **New external dependencies:** `keyring` 3.6 (MIT/Apache-2.0; `vendored` dbus on Linux),
  `argon2` 0.5, `oauth2` 5 (with our reqwest/rustls client), `ring` (already in the tree via
  rustls) for RS256.

### Spec 03 gaps

- **Google ADC is our own** (`genie-providers::adc`, refresh-token and RS256 service-account
  grants) instead of `google-cloud-auth` (keeps rustls-only HTTP); external-account / workload
  identity files are not supported (not in v1).
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
- Not implemented: Azure / Gemini embedding clients (only the OpenAI-compatible
  `OpenAiEmbeddings` exists, §9), Azure `prefer_responses` fallback to Chat on
  "Model not supported" (the flag and `UnsupportedOnDeployment` exist), `ReplayRejected`
  recompile (engine side), `<system-update>` lowering (06),
  model-family detection is a heuristic on the wire id, Bedrock (v2).
- Catalog body `quirks` with a body merge patch re-serialise the request through `Value`, so
  they must not be used on routes that need byte-exact history replay.

### Spec 06 §7 and §13.4 gaps and deviations

- **Protocol revision is an assumption.** 06 §7 names none; Genie requests 2025-06-18 (first with
  elicitation) and accepts 2025-03-26 / 2024-11-05. 2025-11-25 additions (tasks, URL-mode elicitation,
  icons) and `roots` are not implemented (the `roots` capability isn't declared).
- **Legacy SSE transport** (`type: "sse"`) parses as a config error; the "use it only if a server
  reports it" fallback is not built. Streamable HTTP does not resume SSE streams (`Last-Event-ID`) and
  opens the GET stream once without reconnecting.
- **OAuth is a port only**: client id order (static → CIMD → DCR), loopback redirect, token storage and
  refresh-ahead belong to the `AuthProvider` implementation (with spec 08's keychain).
- **Not in this crate:** reading settings files, `.mcp.json` trust and definition-hash approval (D43),
  the connector directory (signed feed, pinned checksums, "what will run" manifest), policy
  (directory-only/allowlist/disable local), per-chat "+" toggles, `tool_search` deferred loading,
  the 10 % context threshold, central truncation, blob storage of binary parts, `@server:resource` and
  `/server:prompt` UX, asks for first-use permission, elicitation/sampling wiring to asks and the
  chat model, usage accounting. The engine/tools/config crates own these; this crate provides the ports
  and data they need (`Ask` builders, `TaintSource`, `McpEvent`, `definition_hash`).
- **Schema sanitising** is a Genie assumption (the spec only says names are sanitised); per-provider
  lowering stays in `genie-providers`.
- **Sampling budget** default is 100,000 tokens per server per daemon run (not persisted); estimated
  cost is computed by the caller.
- **Windows:** stdio commands like `npx` resolve only if `Command` finds them (`npx.cmd` needs
  `cmd /c` or the full name); the Job Object assignment race from 01 still applies to pipe mode.
- **genie-process:** stdout and stderr are two pipes interleaved at chunk granularity (opencode-pty
  uses one); pipe mode is written against opencode-pty's documented semantics, not copied, so no MIT
  notice is needed yet; PTY mode, constant-time token compare and the Linux blocked-writer fix belong to
  the PTY port (D55). Environment scrubbing beyond `GENIE_*` (e.g. provider keys) is not done because
  user commands may legitimately need them.
- The `genie-mcp-fake-server` test binary is built with the workspace (it is tiny and only used by
  tests).

### Spec 06 §9 (`genie-skills`) gaps and deviations

- **Hot reload is polling-based:** `refresh()` compares a fingerprint; no OS file watcher
  (`notify` is not a dependency). The caller decides when to poll and diffs `index()` into
  `<system-update>`s.
- **Frontmatter is a YAML subset** (no anchors/aliases/tags/multi-doc/complex keys) to avoid
  a YAML dependency; unusual Claude skills using those fail validation with a line number.
- **Assumptions:** precedence built-in < user < `.agents/skills` < `.genie/skills` (later
  wins, with a diagnostic); `name` differing from the folder name is a warning (the folder is
  the id) on disk, while uploads take the id from `name`; package limits (20 MiB archive,
  64 MiB unpacked, 16 MiB/file, 1000 entries, ratio 200) and the 256 KiB `SKILL.md` /
  `skill_read` caps are Genie choices; enable state lives in a JSON file (or a caller-supplied
  `SkillStateStore`) rather than settings.
- **Directory:** `PINNED_KEYS_HEX` is empty until D40's key exists, so the production
  directory refuses every feed; update re-verification/notifications for installed directory
  skills, a details page UI and the HTTP transport are not built.
- **Not implemented:** `/skill-name` argument passing, `skill_run` + sandbox (v2, D44),
  `skills.scriptsAllowed` (v2), `/server:prompt` (MCP), `@file` reading and `` !`cmd` ``
  execution (engine, via tools and shell permissions), fuzzing of zip unpacking (08 §7).
- **New external dependency:** `zip` 2.4 (MIT, `deflate` feature: flate2/miniz_oxide/zopfli).

### Spec 06 §8 and §13 gaps and deviations

- **Permission request encoding.** `PermissionRequest` has no structured field, so the shell tool
  puts the dialect in the resource (`Bash(<cmd>)` / `PowerShell(<cmd>)`) and the cwd in the
  `Command` preview; the authorizer must call `shell::authz_from_request` to get the scanned parts.
  `web_fetch` sends the host as resource. A typed field on `PermissionRequest` would be cleaner but
  changes a struct `genie-core` builds.
- **Scanner location.** The compound-command scanner lives in `genie-tools::shell::scan` (the
  dependency rules forbid `genie-tools → genie-permissions`); `genie-permissions` keeps receiving
  pre-split parts. Redirections stay inside a part, so `Bash(cat:*)` also matches `cat a > b`
  (the read-only list never does). `for`/`case`/`select`/function definitions, arithmetic
  commands and PowerShell `switch`/`function`/`param` blocks are unparseable (→ ask) rather than
  split. Leading `VAR=value` assignments stay in the part (no rule-prefix match → ask).
- **Background jobs vs interrupt.** Interrupting the turn kills a foreground command; a command
  already in the background survives until it ends, `kill_job`, `end_session` or shutdown (our
  reading of "stop / interrupt / session end"). The background job's own `cd` is not persisted.
  Jobs live in daemon memory; a daemon restart kills them (the kill-tree) without a `finished`.
- **Shell details.** The login shell runs with `-c` (no `-l`; rc/profile files beyond `.zshenv`
  aren't read). A command that installs its own `EXIT` trap loses the cwd trailer. PowerShell and
  Git Bash paths are covered by unit tests only (no Windows run yet); PowerShell 5.1 output in a
  non-UTF-8 code page is decoded lossily. Temp side files live in `$TMPDIR/genie-shell-<pid>`.
- **`web_fetch`.** No `prompt`/summarisation by a small model (the page Markdown is returned);
  charset is assumed UTF-8 (lossy); no gzip/brotli (reqwest features off); HTTP→HTTPS isn't
  upgraded; permission is asked before DNS (so a model-built hostname leaks nothing via DNS before
  approval), which means a host that later fails the SSRF check was still asked about. PDF and
  other binary types are refused.
- **Web search.** Native search is a decision function only: declaring it in `ServerTools`,
  replaying results to the same provider, citations, fees and tainting on provider result parts
  are engine/provider work. The local `web_search` tool is for hosts with their own backend and
  must not be registered together with the native declaration (same name).

### Spec 06 §10 (browser) gaps and deviations

- **Generated CDP types** (research §3) are not used: commands and results are `serde_json::Value`
  with a few typed accessors; no `chromiumoxide_cdp` dependency.
- **Windows pipe transport** uses `--remote-debugging-io-pipes` with inheritable handles; it
  type-checks for `x86_64-pc-windows-msvc` but has not been run on Windows. Inheritable handles can
  leak into a child spawned concurrently (no `PROC_THREAD_ATTRIBUTE_HANDLE_LIST` in std).
- **eTLD+1** uses a short built-in multi-label suffix list, not the full Public Suffix List; the
  category list is a curated placeholder until the shipped, updatable list exists. Admin
  allow/blocklist are `SitePolicy::set_admin` inputs; reading them from managed policy is wiring.
- **Images to the model:** `browser_screenshot` stores the JPEG through `BrowserHost::store_image`
  and returns its reference; `ToolOutput` has no image variant, so the engine must lift the
  reference into an image part. Set-of-marks overlays are v1.x.
- **Context hygiene** ("only the latest full snapshot per tab stays in context") is compaction-side
  (core) and not done here.
- **Not implemented:** cross-origin iframe auto-attach (OOPIF content is missing from snapshots and
  their payment fields are only detected through the iframe `src`), `browser_wait` for network idle,
  file-chooser interception during take-over (SA-24; `browser_upload` uses `DOM.setFileInputFiles`),
  copy/paste during take-over (the clipboard lives in the desktop app), pop-out of a headless
  browser (needs a relaunch with a window), the Linux "no keyring → ephemeral + warning" detection
  (`ProfileKind::Ephemeral` exists), enterprise-policy detection of disabled remote debugging, the
  v1.x critic model / injection classifier, the "always allow this action on this site" option
  for non-payment confirmations, and moving downloads out of quarantine.
- **Assumptions:** `browser.stop` = take over + end screencasts (the daemon interrupts the turn);
  any take-over input except plain pointer movement pauses the agent; the step cap counts every
  browser tool call in a turn and resets after an approved check-in; a messaging-site `act` and a
  category navigation always confirm even when the site was allowed; downloads triggered by an agent
  action from an unknown frame are attributed to the acting session; refs keep counting up per tab
  so a stale ref never names a new element.

### Code mode (06 §4.2, D49, D50) gaps and deviations

- **Not implemented:** `ask_user` (needs a `question` ask port for tools); `task` / `task_wait`
  (subagents workstream); the opt-in formatter after edits; images/PDFs in `read` (`ToolOutput`
  has no image part); `prefers_patch` gating (`patch` is offered to every model in Code); the
  worktree setup command (trust + hash approval); `worktrees.dir` / `branchPrefix` from settings
  (the fields exist on `CreateSpec` / `CodeServices`); the Settings → Developer list of dirty
  worktrees; `git worktree list --porcelain` reconciliation at startup; `worktree` table rows
  (worktrees are tracked in daemon memory, like sessions); the Settings clean-up action and the
  policy caps `checkpoints.maxDays/maxSizeMB`; checkpoint ids on `message.snapshot_before/after`
  (records live in the shadow dir's JSONL so `StoredMessage` stays unchanged).
- **grep/glob/list** use ripgrep's crates in-process instead of spawning a system or bundled
  `rg` (same ignore/hidden rules, no binary to ship or pin).
- **Non-git folders** use the same shadow git (nothing to borrow from) instead of the store's
  blob fallback; a tree with more than 50,000 untracked files is not snapshotted (logged).
- **Checkpoints:** only steps whose trees differ are recorded (no empty checkpoints); restoring
  the conversation moves the leaf to the chosen message's **parent** (the message is asked again,
  as in Claude Code); a second rewind commits the first; staged state is in memory (a daemon
  restart commits it). Objects borrowed through alternates can vanish if the user's repo prunes
  them (inherent to the alternates design). A shell command moved to the background is captured
  by the next mutating step, not its own.
- **Edits:** `patch` doesn't require a prior `read` (hunk context guards it) but refuses files
  read and changed since; `write` over a non-UTF-8 file replaces it as text; shell commands are not
  path-locked.
- **Worktree `auto`** treats "running session" as any other Code session in the daemon whose
  folder is inside the repo; slugs come from `worktree_name` or `session` (the title isn't known
  at create). `pr` and `git.commit then: pr` never touch the network (instructions only);
  `then: push` does run `git push`.
