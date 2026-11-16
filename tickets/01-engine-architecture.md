# 01 — Engine architecture (core daemon, crates, layering)

type: wayfinder:grilling · status: closed · claimed-by: charting session (2026-11-16) · blocked-by: —

## Question
How is the Rust core structured inside the daemon (D4), so the desktop and CLI are thin clients?

## To decide
- Layering: apps → core → subsystems → protocol
- Async runtime (tokio) and how OpenCode's Effect patterns translate (ch.02)
- Event bus and per-session coordinator (inbox, interrupts, durable claim for crash resume)
- Daemon lifecycle: Tauri sidecar per app vs one shared per-user daemon (KB warns against a fixed port + plaintext password file)
- Cargo workspace crate boundaries

## References
- OpenCode ch.01: [01-architecture.md](../knowledge/opencode/01-architecture.md) — read TL;DR, Architecture, Genie takeaways
- OpenCode ch.02: [02-effect-runtime-patterns.md](../knowledge/opencode/02-effect-runtime-patterns.md) — read TL;DR, Architecture, Genie takeaways
- OpenCode ch.03: [03-session-and-agent-loop.md](../knowledge/opencode/03-session-and-agent-loop.md) — read TL;DR, Architecture, Genie takeaways

## Resolution
Resolved 2026-11-16. User decisions are marked **(user)**; everything else is a default taken from the KB and can be challenged in the spec phase.

### Process model (user)
- **One shared per-user `genie` daemon** (re-confirms D4/D14). The desktop app and the CLI are clients. We considered and rejected an in-process core and engine-per-session.
- **This overrides the KB:** ch.01/02/03 advise "avoid the shared daemon / one engine per session / skip Location multiplexing". For Genie, ignore that advice. We *do* need a per-directory context map (a light version of OpenCode's Location/Instance), but not its DI graph.
- **Session sharing: hand-off (user).** Any client can open any session from the shared store, but only **one client drives a session at a time** (a driver lease held in the daemon; other clients see it read-only with a "Take over" action). Live co-driving is not a v1 goal, although the daemon makes it cheap to add later.
- **Crash or close mid-turn (user):** the turn is marked `interrupted`, and the UI shows a **Resume** button. There is no auto-resume, because a resume can repeat side effects (MCP calls, browser actions). An execution claim is still written, so that restart *detects* the interrupted turn.

### Daemon lifecycle (default)
- A single binary: `genie daemon` is a subcommand of the CLI binary. The desktop app bundles the same binary.
- **Discovery:** a per-user, channel-specific endpoint: a Unix domain socket under `~/.genie/run/` (0700) on macOS/Linux, and a named pipe ACL'd to the current user SID on Windows. No TCP port, so no fixed-port collisions and no plaintext password file (C4). A random token in a 0600 file is required in the handshake.
- **`ensure`:** a client connects. If there's no daemon, it spawns one detached, waits for a health handshake (with timeout and retries), and shows "Starting engine…" or the error in the UI. This addresses OpenCode's "sidecar sometimes failed" problem.
- **Version skew:** the handshake carries the protocol + build version. If the client is newer and the daemon has no running turns, the daemon drains and restarts; otherwise the client is told to wait or warn.
- **Shutdown:** idle exit after N minutes with no clients and no running turns. The daemon survives the desktop app closing while a CLI is attached.
- **Isolation:** each session runs in its own tokio task under `catch_unwind`, so a panic fails that session only. Tools, MCP stdio servers and the browser run as child processes in Windows Job Objects / process groups, so they die with the daemon. If the daemon crashes, clients reconnect, call `ensure` again, and the affected turns show Resume.

### Internal structure (default)
- **tokio**, and an explicit `EngineBuilder` (no DI framework). Process-wide services: store, provider registry, MCP pool, browser manager, auth. Per-directory context: config, instructions, permissions, cwd. Per-session actor.
- **Per-session actor with an inbox and doorbell** (ported from OpenCode): prompts are admitted to a durable inbox (`steer` = at the next step boundary, `queue` = after idle), and one task drains them. Interrupt acknowledges immediately; cleanup runs asynchronously.
- **Events:** **durable** facts (session created, message admitted, tool result, turn settled) are committed to SQLite with a per-session `seq` and projected in the same transaction. **Ephemeral** deltas (tokens, progress) are live-only. Clients resume with `log(after=seq)` → live (→ 02, 12).
- **Tools dispatch while the model is still streaming,** and calls are joined at step end. The permission wait is a oneshot raced against the turn's cancellation token, and "user declined" is a separate result type that is never turned into tool output.
- Cancellation via a `CancellationToken` tree (daemon → session → turn → tool/subagent/hook); `TaskTracker` for spawns. Retry policy ported from OpenCode's `model-retry`.

### Crates (default → 38)
`genie-protocol` (wire types and schemas; **no internal deps**, enforced in CI) · `genie-core` (daemon runtime, session actor, bus) · `genie-providers` (D18) · `genie-store` (SQLite) · `genie-tools` · `genie-mcp` · `genie-browser` (40) · `genie-daemon` (transport, lifecycle) · `genie-client` (Rust client used by CLI + Tauri) · apps: `genie` CLI, `desktop` (Tauri + React; the TS client is generated from `genie-protocol`).
Lint-as-architecture: clippy `unwrap_used` / `await_holding_lock`, `cargo deny`, and no `tokio::spawn` outside `TaskTracker`.

### Handed to other tickets
- Transport and wire format on the socket/pipe (HTTP+SSE vs WebSocket vs framed NDJSON), and the handshake → **02**.
- Driver-lease semantics in the event model, and the interrupted/Resume state → **02, 12**.
