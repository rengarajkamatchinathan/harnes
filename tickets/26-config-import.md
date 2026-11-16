# 26 — Importing Claude Code and OpenCode configs

type: wayfinder:grilling · status: closed · claimed-by: charting session (2026-11-17) · blocked-by: 08,17,21

## Question
Do we import existing .claude/ and opencode.json setups?

## To decide
- Which keys map across
- Per-key diagnostics (reuse the V1→V2 normaliser idea)
- one-time import vs live compatibility

## References
- OpenCode ch.06: [06-config-permissions-and-policy.md](../knowledge/opencode/06-config-permissions-and-policy.md) — read TL;DR, Architecture, Genie takeaways

## Resolution
Resolved 2026-11-17. User decisions are marked **(user)**; the rest are defaults from KB ch.06 and D23/D25/D30/D31/D37/D43/D46/D48.

### Sources in v1 (user): Claude Code, Claude Desktop, OpenCode
| source | what is imported | → Genie |
|---|---|---|
| **Claude Code** | `~/.claude/settings.json` + project `.claude/settings(.local).json`: permissions (same syntax, D25 → near copy), `defaultMode`, env (non-secret), `additionalDirectories` | user / project / local settings (D23) |
| | MCP: `~/.claude.json` user/local servers, project `.mcp.json` (already read natively, D30) | `mcpServers` (secrets → keychain prompts, D26) |
| | `CLAUDE.md` / `CLAUDE.local.md` | see below |
| | `.claude/skills`, `~/.claude/skills` | `.genie/skills`, `~/.genie/skills` (same format, D37) |
| | `.claude/agents`, `~/.claude/agents` | `.genie/agents` (same format, D46) |
| | `.claude/commands` | `.genie/commands` (`!cmd` flagged for trust, D37) |
| | hooks | Genie hooks wrapping each script in `genie hook-adapter claude` (D48) |
| **Claude Desktop** | `claude_desktop_config.json` `mcpServers`; installed extensions list | `mcpServers`; extensions become **connector suggestions** (matched to the directory, D30) since `.mcpb` is v2 (D52) |
| **OpenCode** | `opencode.json(c)` (global + project): providers/models, `permissions` (converted from `{action, resource}` last-match-wins → D25 classes, with diagnostics where semantics differ), `mcp.servers`, `agents`, `commands`, `instructions` | providers (keys → keychain), settings, MCP, agents, commands |
| | `AGENTS.md` | nothing to do: read natively (D31) |
- Cursor/VS Code MCP configs: not in v1.

### CLAUDE.md (user): GENIE.md that @imports CLAUDE.md
- The importer creates a small `GENIE.md` containing `@CLAUDE.md` (and `GENIE.local.md` → `@CLAUDE.local.md`), so the team keeps **one source of truth** and Claude Code + Genie stay in sync. If a GENIE.md already exists, the import line is appended after confirmation.

### How it works (default)
- **One-time copy, not live compatibility** (KB): Genie never scans `.claude/` live (D31/D37/D46).
- Offered at **first run** ("We found settings from Claude Code / Claude Desktop / OpenCode — import?") and anytime via `genie import [claude-code|claude-desktop|opencode] [--project|--user] [--dry-run]` and Settings → General → Import.
- **Preview first:** a per-item list (import / skip / conflict) with **per-key diagnostics** (unsupported key, semantic difference, secret detected → move to keychain), reusing OpenCode's V1→V2 normaliser idea. Nothing is written until confirmed.
- **Secrets** found in configs (API keys in env/headers) are moved to the keychain (D26), never copied into Genie settings.
- Imported project hooks, MCP servers and `!cmd` commands still go through **trust + hash approval** (D43).
- Re-running the import is idempotent (detects already-imported items); an import report is saved to `~/.genie/imports/<date>.json`.
- **No consumer-subscription credentials** are imported (C2).
