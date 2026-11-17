# Genie codebase

Implementation of [spec/01-architecture.md](../spec/01-architecture.md): the Cargo workspace,
crate graph and dependency rules, the shared daemon with its lifecycle, the client `ensure`
path, the session actor, the cancellation tree, the error taxonomy and log files.

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
  genie-secrets    SecretString re-export; credential model + store port (03 §10.1)
  genie-providers  canonical model, SSE parser, compile step, 4 protocol adapters, Executor,
                   error classification + retry policy, routes/presets, cassettes (spec 03)
  genie-catalog    model catalog: bundled snapshot, signed cache + ETag refresh, discovery
                   parsers, overrides/policy, effort map, pricing (spec 03 §8)
  genie-{store,config,permissions,tools,mcp,skills,sandbox,browser,markdown}
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
- **Durable storage is in memory.** `genie-core::journal::Journal` is the store port;
  `MemoryJournal` stands in until `genie-store` (spec 04) implements it on SQLCipher.
- **Windows Job Object race:** a child is assigned to its job right after `spawn`, so anything it
  starts in that gap escapes. Closing it needs a raw `CreateProcessW` with `CREATE_SUSPENDED`
  (planned with the `opencode-pty` port, D45).
- **Windows token file** relies on the per-user `%LOCALAPPDATA%` ACL; no explicit DACL is set.
- **Not in this milestone:** desktop app, `packages/*` and the Platform-adapter lint rule (spec 05);
  protocol session methods and TS codegen (spec 02); the `-p` headless CLI (spec 07); CI workflows.

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
- `genie-client/src/ensure.rs:197` has a `collapsible_if` clippy warning that predates this work
  (newer clippy); left for the spec 02 owners.
