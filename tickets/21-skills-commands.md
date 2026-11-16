# 21 — Skills and slash commands

type: wayfinder:grilling · status: closed · claimed-by: charting session (2026-11-16) · blocked-by: 10,18 (chat part decided without 18, per D29)

## Question
How do skills and custom commands work?

## To decide
- SKILL.md folders, the index in the system prompt, loaded on demand
- Markdown commands with arguments
- no unchecked !`cmd` substitution (KB)

## References
- OpenCode ch.10: [10-agents-skills-and-commands.md](../knowledge/opencode/10-agents-skills-and-commands.md) — read TL;DR, Architecture, Genie takeaways

## Resolution
Resolved 2026-11-16. User decisions are marked **(user)**; the rest are defaults from KB ch.10 and D25/D31/D32.

### Skill format and discovery (default)
- A folder with `SKILL.md` (frontmatter `name`, `description`, optional `disable-model-invocation`, `allowed-tools`, `metadata`) + optional `references/`, `assets/`, `scripts/`. The id comes from the folder name. The format is Claude-compatible, so skills move between tools.
- **Locations:** user `~/.genie/skills/`; project `.genie/skills/` and `.agents/skills/` (after trust → 32; matches the GENIE.md + AGENTS.md choice in D31). **`.claude/skills` only through the importer (26)**, never scanned live.
- The scanner stops at SKILL.md with a depth limit. Hot reload uses fingerprint + watch.

### How they're used (default)
- An **`<available_skills>` index** (name + description) goes in the context baseline (D32). Changes arrive as diffs, never re-renders (cache-stable).
- The model loads a skill with the **`skill` tool** → `SKILL.md` content + base path + a sampled file list. Reference files are read with a skill-scoped `skill_read`. Users can attach a skill with `/skill-name` or the "+" menu. **User-invoked skills still pass policy** (admin `skill.use` deny, → 28).
- Loaded skills are re-injected after compaction (D33).
- Enable/disable per skill in Settings → Capabilities → Skills (D34).

### Scripts in chat (user): local sandbox — **DEFERRED TO v2 by D44** (v1: instructions + files only; scripts run via shell in Code/CLI)
- In chat, a skill's scripts run through a **`skill_run {skill, script, args, files?}`** tool in a **local sandbox**. The interpreter set for v1 is Python 3 and Node (bundled or detected; → 09).
- **Sandbox requirements for v1** (→ 09 decides how per OS):
  - filesystem: read-only skill dir + a per-run scratch dir; files the user attached to the chat are copied in only when passed in `files`; no access to home or other paths;
  - **network off by default** (a skill can declare required domains in frontmatter → asks per domain, D25 `SkillRun(network:domain)`);
  - CPU/memory/time limits (e.g. 60 s, 1 GB), no child-process escape, killed with the session (D20 Job Object / process group);
  - outputs (files in the scratch dir) come back as attachments/artifacts.
- **Permission:** the first run of each skill's script asks "Allow once / Always for this skill" and shows the script path and a hash; "always" is invalidated if the script hash changes.
- In **Code mode/CLI**, scripts can also run through `shell` under normal permissions; the sandbox is used when the session's sandbox setting is on (→ 07/09).

### Sources in v1 (user): all four
1. **Folders on disk** (above).
2. **Upload `.zip`/`.skill`** in Settings → Skills: validated (size cap, path-traversal check, SKILL.md required, frontmatter schema), unpacked into `~/.genie/skills/`.
3. **Built-in Genie skills** shipped and signed with the app (e.g. *create a skill*, *write a spec*, *summarise a meeting*, *review a document*). They can be disabled, and they're updated with the app.
4. **Skills directory:** signed and curated like the connector directory (D30): pinned version + checksum, a details page showing files and scripts, one-click install into the user scope. Admins can restrict skills to directory-only / an allowlist (→ 28).

### Slash commands (default; Code/CLI, plus chat where relevant)
- Markdown commands in `~/.genie/commands/` and `.genie/commands/`, with frontmatter `description`, `model`, `agent` (→ 18), `argument-hint`. Templates support `$ARGUMENTS`, `$1..$N` (the last takes the rest) and `@file` includes. **`` !`cmd` `` substitution only goes through the shell permission path and only in trusted workspaces** (never unchecked). MCP prompts appear as `/server:prompt` (D30). Built-ins: `/compact`, `/init` (create GENIE.md), `/memory`, `/skills`, `/model`, `/help`.
