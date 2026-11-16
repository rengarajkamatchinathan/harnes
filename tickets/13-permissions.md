# 13 — Permission model

type: wayfinder:grilling · status: closed · claimed-by: charting session (2026-11-16) · blocked-by: 08

## Question
How are tool actions allowed, asked about or denied?

## To decide
- Safe defaults (OpenCode starts at allow-all: don't copy that)
- Plan mode cannot run the shell
- Rule syntax and precedence (deny wins, not last-match-wins)
- Deny is checked before hooks (KB, matches the Genie rule)
- Modes: default / accept-edits / plan / bypass

## References
- OpenCode ch.06: [06-config-permissions-and-policy.md](../knowledge/opencode/06-config-permissions-and-policy.md) — read TL;DR, Architecture, Genie takeaways
- OpenCode ch.09: [09-tools.md](../knowledge/opencode/09-tools.md) — read TL;DR, Architecture, Genie takeaways

## Resolution
Resolved 2026-11-16. User decisions are marked **(user)**; the rest are defaults from KB ch.06/09, D21, D22 and D23.

### MCP tools in Chat (user)
- The **first call to each MCP tool asks "Allow once / Always allow / Deny (+ optional reason)"**. "Always" is saved per tool per server (`mcp__<server>__<tool>`). Server `readOnlyHint` and similar annotations are shown in the prompt as information only; they never skip the ask.

### Modes (user): Claude Code's set, enforced by the engine
| mode | behaviour |
|---|---|
| `default` | Rules apply; anything unmatched and not read-only → **ask** |
| `accept-edits` | File edits inside the workspace are auto-allowed; shell, MCP, browser, network still follow the rules |
| `plan` | **Engine-enforced read-only:** no writes, **no shell**, no MCP tools without read-only allow rules, no browser actions beyond navigate/read |
| `bypass` | No prompts, **but deny rules, managed policy and D21's sensitive-browser-action confirmations still apply**. An admin can disable it (`disableBypassMode`); the CLI flag is `--dangerously-skip-permissions` |
- The mode is a session property set over the protocol (D22), so the CLI, desktop and headless clients behave the same. Chat sessions are always in `default`.

### Rule syntax (user): Claude-compatible
- `Tool` or `Tool(specifier)`, e.g. `Bash(git commit:*)`, `Read(./src/**)`, `Edit(/docs/**)`, `WebFetch(domain:example.com)`, `mcp__github` / `mcp__github__create_issue`.
- Genie extensions in the same grammar: `Browser(site:*.example.com)`, `Browser(action:submit)`, `Artifact(network)`, `WebSearch`.
- Settings keys mirror Claude's shape: `permissions: {allow: [], ask: [], deny: [], defaultMode, additionalDirectories}`. This makes the 26 importer close to a copy.

### Evaluation (default)
1. **Managed policy deny** (28) → blocked: "Blocked by <org> policy".
2. **Deny rules** (all layers) → blocked. **Deny is checked before hooks and plugins** (20); nothing can turn a deny into an allow.
3. **Ask rules** → ask (even in `accept-edits`; in `bypass` → allowed).
4. **Allow rules** (config + saved approvals) → allowed.
5. **Mode default** → read-only tools are allowed; everything else asks (or follows the mode).
- Evaluation is by rule **class**, not file order (not last-match-wins). Path matching is case-insensitive on Windows, with separators normalised.
- **Shell (→ 07):** a compound command is split into its sub-commands, and each must pass on its own. `always` saves a prefix pattern (e.g. `git commit:*`) from a table of command arities.

### The prompt flow (default)
- A request `{id, tool, resource, preview, save_suggestions[]}` is written to `permission_pending` (D24) and sent to the driving client as a JSON-RPC request (D22). If no client is attached → pause + OS notification (D22).
- Replies: `once | always | reject {message?}`. A reject message goes back to the model as feedback; a reject without a message ends that tool call. **No reject cascade:** other pending asks stay pending.
- After `always`, the other pending asks are re-evaluated; any that now match are released automatically.
- "User declined" is its own result type, never turned into normal tool output (D20).

### Where "always" is saved (default)
- **Code sessions:** `<project>/.genie/settings.local.json` (D23).
- **Chat sessions** (no project dir): `~/.genie/settings.json` in the user scope.
- Both can be seen and revoked in Settings → Permissions (33).

### Audit (default)
- Every deny, ask and decision is recorded as a durable session event (D24). It is also exportable via OpenTelemetry when the admin enables it (→ 28/35).
