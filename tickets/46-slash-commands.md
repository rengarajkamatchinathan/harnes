# 46 — Slash commands (desktop, CLI, headless)

type: wayfinder:grilling · status: closed · claimed-by: wayfinder session (2026-11-18) · blocked-by: — · related: 05, 17, 21, 22, 33

## Question
How do slash commands work across Desktop Chat, Desktop Code, the CLI and `genie -p`? The spec covers them only as spec assumptions SA-46–SA-48 (06 §9.1, 05 §2.3, 07 §2.3). This ticket turns those into user decisions and fills the gaps.

## Already decided (do not reopen)
- D30: MCP prompts appear as slash commands (`/server:prompt`).
- D37: Markdown commands with `$ARGUMENTS`; `!cmd` only through shell permissions in trusted workspaces; `/skill-name` attaches a skill; user-invoked skills still pass policy.
- D43: project commands load only in trusted workspaces.
- D52: plugin bundles are v2; components are namespaced.

## To decide
- Routes: confirm `client` (opens Genie UI, no model turn) vs `prompt` (sent as a message) — SA-46.
- Unknown `/word`: send it as a message, or warn first?
- Commands vs skills: keep a separate `commands/` folder, or make a command a skill?
- Name collisions and precedence (built-in / project / user / skill / MCP).
- Where a command can appear in the prompt (start only, or anywhere).
- Can the model invoke user commands itself?
- The v1 built-in set beyond 06 §9.1.
- Creating and managing commands in the UI.

## References
- spec/06-agent-runtime.md §9.1; spec/05-desktop-ui.md §2.3 (Slash menu); spec/07-cli.md §2.3; spec/02-protocol.md §6.8 (`command.list`), §8 (`command_not_prompt`)
- spec/11-open-questions.md SA-46, SA-47, SA-48
- tickets/21-skills-commands.md (Slash commands)

## Context found
- Desktop composer (`apps/desktop/src/ui/chat/Composer.tsx`) has no slash handling: typing `/` opens nothing.
- The CLI has its own hardcoded table (`apps/cli/src/tui/commands.rs`).
- The daemon has no `command.list` / `commands.changed` yet, although 06 §9.1 specifies them.

## Resolution
Resolved 2026-11-18. All points are **(user)** unless marked default.

- **Where:** the `/` menu is in **both Chat and Code** composers, same menu; Code adds `/mode /plan /compact /rewind /worktree /agents /init`.
- **Look: like Claude.** A popup above the composer with **one flat list of names** (no group headers, no per-row description); the description shows in a tooltip on hover/keyboard focus. Typing filters. After a pick, the command is an accent-coloured `/token` followed by its grey argument hint (e.g. `/plan [<description>]`). This replaces the grouped list in 05 §2.3.
- **Order:** **recently used first**, then built-ins, then your commands and skills, then connector prompts. While filtering, rank by name match. The recent list is per user, kept with desktop UI state (not settings, D23).
- **Commands in v1:** the **built-ins of 06 §9.1** (no extra Claude-only ones such as `/theme`, `/stats`, `/doctor`), **user/project Markdown commands + `/skill-name`** for enabled skills, and **MCP prompts** `/server:prompt` with their argument form. Confirms SA-46's routes: built-ins open Genie's own UI with no model turn (`client`); commands, skills and MCP prompts are sent as a message (`prompt`).
- **Source: the daemon registry.** Build `command.list` + `commands.changed` in the daemon (06 §9.1, 02 §6.8). The desktop and the CLI both read it; the CLI's hardcoded table becomes only the key bindings joined by name (07 §2.3).
- **Unknown `/word`:** Enter **sends the text unchanged as a message** (as specified; the menu shows "No command matches. Enter sends it as a message.").
- **Position:** commands are recognised **only at the start** of the prompt.
- **`+` menu:** gains a **Skills & commands** submenu listing the same registry entries (minus built-ins — default); picking one inserts the `/token`.
- Default (unchanged from spec): immediate built-ins (`/mcp /skills /memory /permissions`) open on pick, even during a turn; SA-47 (stored expansion) and SA-48 (`command_not_prompt` guard) stand.

Spec updated: 05 §2.3 (Slash menu), 06 §9.1 (SA-46 now a user decision), 11 (SA-46 note).
