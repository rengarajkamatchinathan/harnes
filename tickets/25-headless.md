# 25 — Headless mode and ACP

type: wayfinder:grilling · status: closed · claimed-by: charting session (2026-11-17) · blocked-by: 02,05

## Question
What does non-interactive use look like?

## To decide
- `genie -p` with text / JSON / NDJSON output for CI
- ACP server for editor integration (v1 or later?)

## References
- OpenCode ch.14: [14-cli-headless-and-acp.md](../knowledge/opencode/14-cli-headless-and-acp.md) — read TL;DR, Architecture, Genie takeaways

## Resolution
Resolved 2026-11-17. User decisions are marked **(user)**; the rest are defaults from KB ch.14 and D20/D22/D25/D43/D47.

### Headless runs (default)
- `genie -p "<prompt>"` (or prompt on stdin), or any non-TTY stdin/stdout → headless. Flags: `--output-format text|json|stream-json` (stream-json = the D22 event messages as NDJSON), `--model`, `--effort`, `--agent` (D46), `--permission-mode`, `--allow/--deny <rule>` (D25 syntax), `--max-turns`, `--continue` / `--resume <id>`, `--worktree` (D50), `--trust` (D43), `--settings <file>` (D23 layer 2).
- `json` output: final result + usage/cost + session id + exit status. Exit codes: 0 success, 1 error, 2 interrupted, 3 permission/approval timeout.
- Runs through the **shared daemon** (D20) like any client; `--standalone` starts a private daemon for CI containers (exits on stdin EOF, KB lease pattern).
- Waits for background subagents to report before exiting (D47).

### Permission asks in headless (user): pause and wait for approval
- An ask **pauses the run** and waits; the ask is durable (D22) and raises an **OS notification / desktop prompt** ("Headless run X needs approval"), and can be answered from the desktop app, `genie approvals` (list/approve/deny from another terminal), or any client attached to that session.
- To avoid hanging CI forever: **`--ask-timeout <duration>`** (default: none = wait indefinitely; when it expires → deny, exit code 3), and **`--on-ask deny`** to deny immediately (recommended for CI). Genie prints a hint about these flags when it detects a CI environment (`CI=true`) and has to pause.
- Pre-allowing via `--allow`, `--permission-mode` or settings avoids asks entirely. `--dangerously-skip-permissions` still honours deny rules, policy and D21 confirmations (D25).

### ACP (user): later (v2)
- No ACP server in CLI v1. In v2, `genie acp` translates Genie events into Agent Client Protocol over stdio (OpenCode's adapter folds its events the same way), once D22 is public.

### SDK (default)
- No public SDK in v1 (D22 is internal). The stream-json output is documented as the v1 automation surface, with a stability note.
