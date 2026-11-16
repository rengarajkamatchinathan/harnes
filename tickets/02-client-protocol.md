# 02 — Client/daemon protocol and event stream

type: wayfinder:grilling · status: closed · claimed-by: charting session (2026-11-16) · blocked-by: 01

## Question
What wire protocol do the desktop and CLI use to talk to the daemon?

## To decide
- Transport: HTTP + SSE vs WebSocket vs local socket/named pipe
- Per-session event seq and a resume cursor (OpenCode's SSE can't be resumed: avoid that)
- A steer command for user input mid-turn
- Permission prompts that survive reconnects
- Generated TS + Rust clients with a drift check in CI
- in-memory transport for tests
- localhost auth token

## References
- OpenCode ch.13: [13-server-protocol-and-sdk.md](../knowledge/opencode/13-server-protocol-and-sdk.md) — read TL;DR, Architecture, Genie takeaways
- OpenCode ch.18: [18-app-ui-and-gui-plugins.md](../knowledge/opencode/18-app-ui-and-gui-plugins.md) — read TL;DR, Architecture, Genie takeaways

## Resolution
Resolved 2026-11-16. User decisions are marked **(user)**; the rest are defaults from the KB (ch.13) and D20.

### Wire format (user)
- **JSON-RPC 2.0, NDJSON-framed** (one JSON object per line), **bidirectional** on the D20 Unix socket / named pipe. Clients send requests; the daemon sends responses plus `event` notifications, and can also send *requests* to the client (e.g. `permission.ask`), in the LSP/MCP style.
- The same message types are reused for `genie -p --output-format stream-json` (25) and can later be carried over WebSocket for remote/web.
- **Desktop path:** the webview can't open a socket. The Tauri Rust side uses `genie-client` to hold the connection and relays to React over Tauri IPC Channels, behind the D8 shell-adapter layer.

### API status (user)
- **Internal in v1, public later.** It is versioned (`protocol` semver) and schema-generated from day one, but only documented and promised stable from v2.

### No client attached (user)
- If a turn needs approval and no client is driving the session, the **turn pauses**. The daemon sends an **OS notification** ("Genie needs your approval") through the desktop app if it's running, or a native notifier otherwise. The ask is **durable**: stored, listed by `permission.pending`, and shown by whichever client opens the session next. It never times out on its own; the user can Stop.

### Defaults
- **Handshake:** `initialize {protocol, client_kind, client_version, token}` → `{engine_version, protocol, capabilities}`. If the major version differs, the client is refused and the D20 version-skew path runs.
- **Subscribe and resume:** `session.subscribe {session, after_seq?}` → replay durable events after the cursor, then a `synced` marker, then live. Every durable event carries a per-session `seq`. Ephemeral deltas carry the `seq` of the message they belong to, so they can be ignored once the durable `*.ended` event arrives. **No lossy streams:** if a slow client overflows, the daemon sends `resync_required {from_seq}` instead of silently dropping events.
- **Global feed:** `workspace.subscribe` for session list, title, status and settings changes.
- **Commands:** `session.create | prompt {delivery: steer|queue} | interrupt | resume | take_over | release | fork | revert`; `permission.respond {id, decision: once|always|reject, message?}` (the `message` tells the model why it was rejected); `config.*`, `provider.*`, `mcp.*`, `browser.*` as subsystems need them.
- **Driver lease (D20):** `take_over` moves the lease. The old driver gets `lease.lost` and becomes read-only. Non-drivers can subscribe but their `prompt` is rejected with `not_driver`.
- **Errors:** JSON-RPC error codes with typed `data.kind` (`not_driver`, `protocol_mismatch`, `provider_error{class}`, `permission_denied`, …). Internal defects are mapped to `internal_error` and never shown to the model.
- **Codegen:** Rust serde types in `genie-protocol` → JSON Schema (`schemars`) → generated TS types. CI fails on drift. Contract tests use an **in-memory transport** that carries the same NDJSON lines.
- **Auth:** the D20 socket/pipe permissions plus a 0600 token file read by `initialize`. There's no HTTP surface, so no CSRF or cookies.
