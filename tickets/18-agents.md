# 18 — Agents (primary agents and modes)

type: wayfinder:grilling · status: closed · claimed-by: charting session (2026-11-16) · blocked-by: 06,13

## Question
What are agents, and which are built in?

## To decide
- Agent definition format (markdown + frontmatter)
- Built-ins: build, plan, general
- Per-agent model, tools and permissions

## References
- OpenCode ch.10: [10-agents-skills-and-commands.md](../knowledge/opencode/10-agents-skills-and-commands.md) — read TL;DR, Architecture, Genie takeaways

## Resolution
Resolved 2026-11-16. User decisions are marked **(user)**; the rest are defaults from KB ch.10 and D25/D31/D32.

### Model (user): Claude style
- **One main Genie agent** per session. Behaviour changes through **permission modes** (default / accept-edits / plan / bypass, D25), not by switching primary agents. No OpenCode-style Tab between build/plan agents.
- **Custom agents are mainly subagents** that the main agent delegates to through the `task` tool (→ 19). A session can also **start as a specific agent**: `genie --agent <name>`, or an agent picker when starting a Code session in the desktop.

### Prompt (user): replace
- An agent's Markdown body **replaces** Genie's base system prompt (same as Claude Code / OpenCode).
- **Always added regardless** (engine-owned, not prompt-owned): tool definitions; the environment block (cwd, OS, shell, date); and the **context baseline** (org/custom instructions, GENIE.md/AGENTS.md, memory, D31/D32), unless the agent sets `context: minimal` (environment only). Permissions, policy and modes are **enforced by the engine** (D25/D38), so a replaced prompt can't weaken them.
- The docs warn that replacing the prompt drops Genie's default tool-usage guidance. The built-in "create an agent" skill (D37) scaffolds prompts that include it.

### Definition format (default; Claude-compatible)
`~/.genie/agents/<name>.md` (user), `.genie/agents/<name>.md` (project, after trust, D43); `.claude/agents` only through the importer (26).
```yaml
---
name: reviewer
description: Reviews diffs for bugs. Use after code changes.   # tells the main agent when to delegate
tools: [read, grep, glob, shell]        # allowlist; omitted = inherit main agent's tools
disallowedTools: [edit, write]
model: inherit | <catalog id>           # D27
effort: inherit | low | … | max
permissionMode: inherit | default | accept-edits | plan   # can only narrow the parent's mode, never widen
maxSteps: 40                            # final step: tools off, answer in text (KB port)
context: full | minimal
skills: [..]                            # preloaded skills (D37)
mcpServers: [..]                        # subset of configured servers (D30)
---
<system prompt body>
```
- Agents **start from the session's permission mode and rules** (not OpenCode's allow-all). `tools`/`permissionMode` can only narrow.

### Built-in agents (default)
| name | purpose | tools | model |
|---|---|---|---|
| `general` | multi-step research/tasks delegated by the main agent | inherit | inherit |
| `explore` | fast read-only codebase search | read, grep, glob, list, read-only shell | catalog small/fast model (D27) |
| `plan` | research + produce a plan for plan mode | read-only set | inherit |
- Hidden internal agents (not user-visible): `title`, `compaction` (D33), `memory` helper if needed.

### Plan mode (default; D25)
- Engine-enforced read-only. Entry/exit **reminders survive compaction** (KB port). Leaving plan mode presents the plan for approval (approve → switches to default or accept-edits).
