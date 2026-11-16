# 20 — Hooks

type: wayfinder:grilling · status: closed · claimed-by: charting session (2026-11-16) · blocked-by: 13

## Question
Which lifecycle hooks do we expose?

## To decide
- Hook points (pre/post tool, prompt submit, session start/stop, compaction)
- The command-hook protocol (subprocess + JSON)
- Deny is evaluated before hooks

## References
- OpenCode ch.11: [11-mcp-and-plugins.md](../knowledge/opencode/11-mcp-and-plugins.md) — read TL;DR, Architecture, Genie takeaways

## Resolution
Resolved 2026-11-16. User decisions are marked **(user)**; the format below is a **proposed Genie design** that the user can revise in the spec phase.

### Scope (user)
- **Code sessions + CLI only in v1.** Desktop Chat doesn't fire hooks.

### Format (user): Genie's own, not Claude-compatible
- **Claude hooks are not read directly.** The importer (26) converts them and wraps each Claude script in a small adapter (`genie hook-adapter claude -- <script>`) that translates the payloads, so existing scripts keep working without being rewritten.

### Proposed Genie hook design
**Events** (dotted names, matching protocol event naming D22):
| event | when | can it change the outcome? |
|---|---|---|
| `tool.before` | before a tool runs (after permission rules, D25) | deny · ask · allow-this-call · **rewrite input** |
| `tool.after` | after a tool returns | add context for the model · mark as failed |
| `prompt.submit` | user prompt admitted | block with message · add context |
| `turn.end` | main turn about to end | **continue** with an instruction (bounded, max 3 per turn) |
| `subagent.end` | a child session reports (D47) | add context |
| `session.start` / `session.end` | session opened/resumed / closed | add context (start only) |
| `compact.before` / `compact.after` | around compaction (D33) | read-only |
| `permission.request` | a permission prompt is about to be shown | answer it (allow/deny), **never overriding a deny** |
| `notification` | Genie wants the user's attention (approval, idle) | read-only (e.g. desktop/Slack notifier) |

**Config** (in settings, D23 scopes; project hooks need D43 approval by hash):
```json
"hooks": [
  { "event": "tool.before", "match": { "tool": "shell", "resource": "git push*" },
    "run": ["./scripts/check-push.sh"], "timeoutMs": 10000, "failClosed": true },
  { "event": "tool.after", "match": { "tool": ["edit", "write"], "path": "**/*.rs" },
    "run": ["cargo", "fmt", "--", "{path}"] }
]
```
- `run` is an **argv array** (no shell string parsing). Use `["sh","-c","…"]` or `["pwsh","-c","…"]` explicitly if a shell is needed.
- `match` is structured (tool name(s), resource glob, path glob, agent), not a regex string.

**Protocol:** the hook gets one JSON object on stdin and replies with one JSON object on stdout:
```json
// stdin
{ "genieHook": 1, "event": "tool.before", "session": "ses_…", "cwd": "…",
  "agent": "main", "mode": "default", "data": { "tool": "shell", "input": { "command": "git push" } } }
// stdout (all fields optional)
{ "decision": "deny" | "ask" | "allow" | "continue",
  "message": "shown to the model (and the user)", "updatedInput": { … }, "addContext": "…" }
```
- **Exit codes aren't semantic.** 0 = read stdout; any non-zero or timeout = **hook error**, which is shown to the user and **ignored unless `failClosed: true`** (then the action is denied).
- Several matching hooks run **in order**; the most restrictive decision wins (deny > ask > allow); `updatedInput` from later hooks applies on top of earlier ones; the rewritten input is **re-checked against permission rules**.

### Invariants (default)
- **Deny rules and policy are checked before hooks** (D25/D38); a hook can never turn a deny into an allow.
- Hooks run as child processes through `genie-process` (D45), with the session's cwd, a minimal environment + `GENIE_*` variables, and kill-tree on timeout. They don't get the provider credentials.
- Hook runs are logged to the session events and audit (D38).
- Policy can disable hooks or allow only managed hooks (`hooks.managedOnly`).
- Internal-only extension points (context rewriting, HTTP interception) are **not** exposed (KB).
