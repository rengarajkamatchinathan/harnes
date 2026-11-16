# 32 — Workspace trust

type: wayfinder:grilling · status: closed · claimed-by: charting session (2026-11-16) · blocked-by: 08,13

## Question
When is a folder trusted to load its config, hooks, MCP servers and instructions?

## To decide
- Trust prompt
- what is gated (project hooks, MCP, files above root)
- How trust is stored

## References
- OpenCode ch.05: [05-memory-and-instructions.md](../knowledge/opencode/05-memory-and-instructions.md) — read TL;DR, Architecture, Genie takeaways
- OpenCode ch.10: [10-agents-skills-and-commands.md](../knowledge/opencode/10-agents-skills-and-commands.md) — read TL;DR, Architecture, Genie takeaways

## Resolution
Resolved 2026-11-16. User decisions are marked **(user)**; the rest are defaults.

### Folder trust (user): ask once per folder
- The first time a folder is opened in Code mode / CLI: **"Do you trust the files in this folder?"** (Trust / Trust once / Don't trust), listing what a project can configure.
- **Until trusted, these are NOT loaded:** `.genie/settings.json`, `.genie/settings.local.json`, `.mcp.json` servers, project hooks (→ 20), project skills (`.genie/skills`, `.agents/skills`), project commands with `!cmd`, project agents (→ 18). **GENIE.md/AGENTS.md are still read as plain instructions** (text only, no `@imports` outside the project).
- Trusting a folder trusts its subfolders. Trust is keyed by the canonical path + project identity (git remote hash / root commit, KB ch.01), so a moved clone keeps it and a different repo at the same path doesn't.
- Stored in the user scope (`~/.genie/trust.json`, separate from settings); listed and revocable in Settings → Permissions (D34). Policy (D38) can set `trust.mode: prompt | never` and `trust.allowedRoots`.
- Home directory, `/`, and system folders can't be trusted as a whole.

### Changes after trust (user): re-ask for new/changed servers and hooks
- Folder trust covers settings, instructions, skills and commands. **Each project MCP server and each hook command is approved separately**, and the approval records a **hash of its definition**. A new or changed entry (e.g. after `git pull`) is **not started** until approved: "This project added/changed MCP server X / hook Y → Approve / Skip". The diff is shown.
- Approvals are stored with the trust record; removing an entry drops its approval.

### Headless / CI (default)
- `genie -p` in an untrusted folder doesn't load project config unless `--trust` is passed (or the policy allows the root). It prints a one-line notice saying what was skipped.
