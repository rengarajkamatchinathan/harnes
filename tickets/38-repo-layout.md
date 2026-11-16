# 38 — Repo layout

type: wayfinder:task · status: closed · claimed-by: charting session (2026-11-16) · blocked-by: 01

## Question
Lay out the monorepo plan (no feature code).

## To decide
- crates/{core, protocol, providers, tools, storage, server, cli}
- apps/desktop (Tauri + React), packages/sdk-ts (generated)

## References
- OpenCode ch.01: [01-architecture.md](../knowledge/opencode/01-architecture.md) — read TL;DR, Architecture, Genie takeaways

## Resolution
Resolved 2026-11-16 (plan only; repo host + CI platform deferred → 41).

### Monorepo: Cargo workspace + pnpm workspace
```
genie/
├─ Cargo.toml                 # workspace; shared lints (clippy unwrap_used, await_holding_lock), deny.toml
├─ crates/
│  ├─ genie-protocol/         # JSON-RPC types, events, errors, config schema types (serde + schemars). NO internal deps (CI check, D20)
│  ├─ genie-core/             # daemon runtime: EngineBuilder, session actor, inbox, bus, compaction, context assembly
│  ├─ genie-providers/        # 4 v1 protocol adapters + Executor + Compat + auth kinds (D18/D26)
│  ├─ genie-catalog/          # model catalog load/refresh/discovery (D27)
│  ├─ genie-store/            # SQLCipher DB, migrations, blobs, FTS, retrieval index (D24/D32)
│  ├─ genie-config/           # layers, policy (D23/D38), watcher, diagnostics
│  ├─ genie-permissions/      # rules, modes, evaluation (D25)
│  ├─ genie-secrets/          # keychain + encrypted-file store (D26)
│  ├─ genie-tools/            # Tool trait + chat tools + code tools (D35)
│  ├─ genie-mcp/              # MCP client, OAuth, directory (D30)
│  ├─ genie-skills/           # skills, commands, directory (D37)
│  ├─ genie-process/          # process runner migrated from opencode-pty (D45)
│  ├─ genie-sandbox/          # v2 sandbox (→ 09, D44)
│  ├─ genie-browser/          # CDP driver for Chrome/Edge (D21)
│  ├─ genie-audit/            # OTel + JSONL audit, telemetry (D38/D39)
│  ├─ genie-daemon/           # socket/pipe transport, handshake, lifecycle, version skew (D20/D22)
│  ├─ genie-client/           # Rust client used by the CLI and the Tauri backend
│  └─ genie-testkit/          # in-memory transport, cassettes, fixtures (34)
├─ apps/
│  ├─ cli/                    # `genie` binary: CLI + `genie daemon` + TUI (→ 05)
│  └─ desktop/
│     ├─ src-tauri/           # Tauri shell: genie-client relay, IPC Channels, artifact scheme (D28), updater (D40)
│     └─ src/                 # React + TS UI (shadcn/Radix/Tailwind, D19)
├─ packages/
│  ├─ protocol-ts/            # TS types generated from genie-protocol JSON Schema (drift check)
│  ├─ ui/                     # design tokens + shared components
│  └─ artifact-runtime/       # sandbox page, esbuild-wasm, bundled libs (D28)
├─ feeds/                     # catalog, connector + skills directory sources, signing scripts (D27/D30/D37)
├─ release/                   # packaging per OS, notices/SBOM, install scripts (D40)
├─ docs/  ·  wayfinder/ (this map, tickets, research, knowledge)
```
### Rules
- Dependency direction: apps → client/daemon → core → subsystems → protocol. Subsystem crates don't depend on each other except via traits in core; `genie-protocol` depends on nothing internal (CI check).
- Generated code (`protocol-ts`, config JSON Schema) is never hand-edited; CI fails on drift.
- One version for the whole repo; channel set at build time (D40).
- Adapted OpenCode code carries an MIT notice header (C3).
