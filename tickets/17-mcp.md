# 17 — MCP client

type: wayfinder:grilling · status: closed · claimed-by: charting session (2026-11-16) · blocked-by: 13,14

## Question
How do we support MCP servers?

## To decide
- rmcp crate
- stdio, Streamable HTTP, OAuth
- Scopes: user / project (.mcp.json) / managed
- Sane tool timeout (not 12 h) and collision-safe tool names
- Self-hosted OAuth client metadata
- MCP connectors in desktop chat

## References
- OpenCode ch.11: [11-mcp-and-plugins.md](../knowledge/opencode/11-mcp-and-plugins.md) — read TL;DR, Architecture, Genie takeaways

## Resolution
Resolved 2026-11-16. User decisions are marked **(user)**; the rest are defaults from KB ch.11, D25, D26 and D29.

### Connector directory (user): remote + pinned local
- A Genie-hosted, **signed** directory JSON (same delivery as the D27 catalog), maintained by Synergech.
- **Remote entries:** URL + OAuth metadata + a description of the tools.
- **Local entries only when vetted and pinned:** an exact package version + **SHA-256 checksum** (npm/PyPI/binary), checked before the first run and on every update. The one-click add dialog **shows exactly what will run** (command, args, env names, network/filesystem notes) before installing.
- Users can still add any server by hand (Settings → Connectors → Add custom). Admin policy (→ 28) can restrict to directory-only, to an allowlist, or disable local servers.

### MCP features in v1 (user): tools + all four extras
- **Resources:** attach through the "+" menu or an `@server:resource` mention; resource content goes in as attachments (size-limited, D27 capabilities). Subscriptions → refresh on `resources/updated`.
- **Prompts:** server prompts appear as **slash commands** (`/server:prompt`), with an argument form.
- **Elicitation:** a server request for input during a call is shown as a form/confirm dialog in the driving client (D22 client request). With no client attached → pause + OS notification (D22). Sensitive-looking fields are flagged; the user can decline.
- **Sampling:** a server's request for a model completion **always needs user approval**, showing the request text, the chosen model and an estimated cost. "Always allow for this server" is available but **off by default**, and admin policy can ban it. Sampling uses the chat's current model and is counted in usage/cost. A per-server token budget is enforced.

### Config (default; Claude-compatible)
- The shape matches Claude: `mcpServers: { "<name>": {command, args, env} | {type: "http", url, headers?} }`. Scopes: user (`~/.genie/settings.json`), project (`<project>/.mcp.json`, after trust → 32), local (`.genie/settings.local.json`), managed (28). **Later scope wins per server name.** Secrets in `env`/`headers` are references to the keychain (D26), never literal values written by the UI.
- Transports: **stdio** and **Streamable HTTP**. A legacy SSE fallback is used only if a server reports it.

### Runtime (default; ported from OpenCode)
- Status: `pending | connected | disabled | failed | needs_auth`, shown in Settings and in the "+" menu.
- Timeouts: startup 30 s, list 30 s, **tool execution 10 min** (configurable). Transient connect failures are retried within the startup deadline. There's a 2 s grace period before the first model call, rather than blocking on slow servers.
- `list_changed` → debounced reload. Protocol-version mismatch → an error naming the supported versions.
- Results: structured content first, JSON-looking text parsed, binary saved as a blob (D24) with a path note. Output goes through the central truncation (→ 11).
- Local servers are child processes in a Job Object / process group (D20), with the user's privileges. Sandboxing → 09.

### Tools to the model (default)
- Named **`mcp__<server>__<tool>`** (sanitised, deterministic, collision-checked). The permission resource is the same string (D25). No Code Mode wrapping.
- **Tool search:** when MCP tool definitions exceed a token threshold (e.g. 10% of context), switch to deferred loading: the model sees a `tool_search` tool and loads definitions on demand.
- Per-chat "+" toggles (D29) enable or disable whole servers or single tools for that chat.

### OAuth (default; → D26)
- Client id order: static client_id → **Genie-hosted client metadata document (CIMD)** → dynamic client registration. Loopback redirect on both `127.0.0.1` and `localhost`. Tokens are kept in the D26 store, keyed by hash(name, url). Refresh 30 s before expiry, and on 401 only if the stored token is unchanged.

### Not in v1
- Packaged one-click extensions (`.mcpb`-style) → 22. MCP Events (draft spec). Pooling servers across sessions beyond what the shared daemon (D20) already does: one process per server config, shared by sessions.
