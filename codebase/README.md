# Genie codebase

Implementation of [spec/01-architecture.md](../spec/01-architecture.md): the Cargo workspace,
crate graph and dependency rules, the shared daemon with its lifecycle, the client `ensure`
path, the session actor, the cancellation tree, the error taxonomy and log files.

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
  genie-secrets    SecretString re-export
  genie-{providers,catalog,store,config,permissions,tools,mcp,skills,sandbox,browser,markdown}
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

## Known gaps and deviations

- **`genie-daemon` depends on `genie-client`** for the shared transport (endpoint naming, framing,
  Windows SID helpers). The other direction would pull the engine into the Tauri side.
  `xtask check-deps` allows exactly this edge.
- **Subsystem edges are not declared yet.** Cargo 1.99 warns on unused dependencies, so
  `genie-core` gains each subsystem dependency when it first uses it; `xtask check-deps`
  enforces the directions in the meantime.
- **Durable storage is in memory.** `genie-core::journal::Journal` is the store port;
  `MemoryJournal` stands in until `genie-store` (spec 04) implements it on SQLCipher.
- **Windows Job Object race:** a child is assigned to its job right after `spawn`, so anything it
  starts in that gap escapes. Closing it needs a raw `CreateProcessW` with `CREATE_SUSPENDED`
  (planned with the `opencode-pty` port, D45).
- **Windows token file** relies on the per-user `%LOCALAPPDATA%` ACL; no explicit DACL is set.
- **Not in this milestone:** desktop app, `packages/ui` and the Platform-adapter lint rule (spec 05);
  the `-p` headless CLI (spec 07); CI workflows.

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
