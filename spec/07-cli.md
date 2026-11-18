# 07 — CLI

Implements D20 (client), D25 (modes), D42, D43 (`--trust`), D47 (headless waits), D49 (`/rewind`), D50 (`--worktree`), D51, D53; C2; tickets 05, 25, 26. Background: KB ch.14–15, pick-list #32/#35.

The CLI is one binary, `genie`. It is also the daemon (`genie daemon`) and is bundled with the desktop app (D20, D40). Its v1 scope is **Code sessions** plus **`genie chat`** to list, open and continue desktop chats in text (D42). It ships in the Code/CLI track (M5).

## 1. Inline TUI

### 1.1 Model

- **ratatui inline viewport** (`Viewport::Inline(h)` + `insert_before`) on crossterm (D42).
- The conversation goes into the **real terminal scrollback** (append-only, never rewritten). A **live footer** holds the prompt, status line, slash menu, permission prompts and elicitation forms; it grows and shrinks.
- **Streaming:** one active entry is rendered off-screen; only rows of **settled Markdown blocks** are committed to scrollback; the unstable tail stays in the footer region (OpenCode `mini` design, pick-list #35).
- No full-screen mode in CLI v1. No inline images (sixel/kitty) or terminal Mermaid.

### 1.2 Rendering

- **`genie-markdown`:** a Rust port of patchmark (MIT, ~4k lines; C3 header), starting with the heal step; byte offsets instead of UTF-16; explicit block stability (D42).
- Code blocks via `syntect`; diffs as unified with +/− colour; LaTeX as source; Mermaid as a code block + "open in desktop".
- **Themes:** semantic role tokens; a "system" theme from the queried terminal palette with a luminance-based light/dark guess; **truecolor → 256 → ANSI 16** fallback; `NO_COLOR` respected.
- Deltas are batched ~10 ms before redraw (t05).
- **Rich parts** (artifacts, browser views, Mermaid, citations' hover) render as a one-line summary + a link `genie://session/<id>#<part>` that opens the desktop app (D42).

### 1.3 Input and keys

| Key / input | Action |
|---|---|
| Enter / Shift-Enter or Alt-Enter | send / newline |
| Enter while a turn runs | **steer** (SA-06); Alt-Enter → **queue** |
| Esc | interrupt (D22 `interrupt`) |
| Esc Esc on an empty prompt | rewind picker (D49) |
| Shift-Tab | cycle mode default → accept-edits → plan (→ bypass only if started with `--dangerously-skip-permissions` and not disabled by policy) |
| Ctrl-C twice | exit (first press clears the prompt) |
| `@` | file completion (workspace, ignore rules) |
| `/` | slash menu from the **one command registry** that also drives keybindings |
| Up/Down | history |
| paste image/file path | attachment |

**Permission prompts** appear in the footer: **Allow once / Always (scope shown) / Deny (+ reason)** with the subagent label when relevant (D25, D47).

**Status line:** model · effort · mode · context % (D32) · cost · "2 subagents running" · "Waiting for approval".

### 1.4 Connection

`genie-client` to the shared daemon: `ensure` on start with a "Starting engine…" spinner; requires the handshake; on reconnect resumes from the last `seq` (D20, D22). A non-TTY stdin/stdout or `-p` → headless (§4).

## 2. Command line

### 2.1 Interactive

```
genie [prompt]                       start a Code session in the cwd (prompt optional)
genie --continue | -c                continue the most recent session in this folder
genie --resume [id] | -r             resume a session (picker without id)
genie --agent <name>                 start as an agent (D46)
genie --model <id> --effort <level>  model and effort (D27)
genie --permission-mode <mode>       default | accept-edits | plan | bypass
genie --dangerously-skip-permissions bypass mode (still honours deny, policy, D21 confirmations)
genie --worktree [name]              run in a new worktree (D50)
genie --add-dir <path>               additionalDirectories
genie --settings <file>              session-only settings layer (D23 layer 2)
genie --trust                        trust the folder for this run (D43)
```

### 2.2 Subcommands

| Command | Purpose | Source |
|---|---|---|
| `genie chat [list | open <id> | new]` | list, read and continue desktop chats in text; take over the lease | D42 |
| `genie sessions` | list sessions (Code by default; `--all`) | t05 |
| `genie approvals [list | approve <id> | deny <id> [--message]]` | answer pending asks from another terminal | D51 |
| `genie import [claude-code|claude-desktop|opencode] [--project|--user] [--dry-run]` | importer (§5) | D53 |
| `genie export <session> [--sanitize] [--format json|md]` | export | D24 |
| `genie config [get|set|doctor|path]` | config through the daemon; `doctor` shows Problems | D23 |
| `genie doctor` | health checks (01 §8) | D34 |
| `genie update` | self-update for CLI-only installs | D40 |
| `genie daemon [status|stop|restart|logs]` | daemon control; bare `genie daemon` runs it | D20 |
| `genie login <provider> [--device-code]` / `genie logout <provider>` | credentials (Entra PKCE/device code; API key prompt) | D26 |
| `genie mcp [list|add|remove|auth <name>|logs <name>]` | MCP servers | D30 |
| `genie trust [list|revoke <path>]` | workspace trust | D43 |
| `genie hook-adapter claude -- <script>` | shim for imported Claude hooks | D48 |
| `genie --version` | channel, commit, build date | D40 |

**Spec assumption (SA-30):** the `login/logout`, `mcp`, `trust`, `sessions`, `config` and `daemon` subcommands and their verbs. Reason: decisions name `genie chat`, `approvals`, `import`, `export`, `update`, `doctor` and `hook-adapter`, but CLI-only installs (D40) need a way to manage credentials, MCP and trust without the desktop.

### 2.3 Built-in slash commands

`/help`, `/model`, `/effort`, `/mode`, `/compact [focus]`, `/rewind`, `/worktree`, `/init` (create GENIE.md), `/memory`, `/skills`, `/agents`, `/mcp`, `/permissions`, `/cost`, `/context`, `/resume`, `/export`, `/clear` (new session), plus user commands (`~/.genie/commands`, `.genie/commands`), skills (`/skill-name`) and MCP prompts (`/server:prompt`) (D37, D30). The set beyond t21's built-ins follows Claude Code names for familiarity; each is a thin wrapper over a protocol method. These extra built-ins are part of SA-30.

The CLI gets its slash menu from the engine's registry (`command.list`, 06 §9.1), the same list the desktop shows, and refreshes it on `commands.changed`. Its own key table (§1) is joined to that list by command name. `client`-route built-ins run in the footer as pickers, dialogs or a one-line result, never as a message. `prompt`-route commands are sent as `command` input parts. `/plan` and the aliases `/connectors` and `/usage` work here too (SA-46).

## 3. `genie chat`

- `genie chat list` shows desktop chats (title, project, updated). `genie chat open <id>` prints the active branch path and subscribes.
- Sending a prompt takes the driver lease (D20); the desktop shows "Open in CLI · Take over". Hand-off works both ways.
- Text, code, citations (as `[n]` + reference list), tool rows and thinking render; artifacts, browser, Mermaid and images render as one-line summaries with "open in desktop" links.
- Chat sessions keep chat semantics (chat tool registry, `default` mode).

## 4. Headless (`genie -p`)

D51; t25.

### 4.1 Invocation

`genie -p "<prompt>"`, or a prompt on stdin, or any non-TTY → headless. Flags: `--output-format text|json|stream-json`, `--model`, `--effort`, `--agent`, `--permission-mode`, `--allow <rule>` / `--deny <rule>` (D25 syntax), `--max-turns`, `--continue` / `--resume <id>`, `--worktree`, `--trust`, `--settings <file>`, `--ask-timeout <duration>`, `--on-ask wait|deny`, `--standalone`.

### 4.2 Output formats

- **text:** the final assistant text.
- **json:** `{session_id, result, usage, cost_usd, status, num_turns, subagents:[…]}`.
- **stream-json:** NDJSON of the D22 event envelopes, preceded by `{"type":"system/init", "genie_protocol":"…", "session_id", "model", "tools":[…]}` and ended by `{"type":"result", …}` (pick-list #25 adds `init` and `result` lines). Documented as the v1 automation surface with a stability note; there is no public SDK in v1.

### 4.3 Exit codes

`0` success · `1` error · `2` interrupted · `3` permission/approval timeout or `--on-ask deny` denial that ended the run.

### 4.4 Asks in headless

- An ask **pauses the run** and waits; it is durable and raises an OS notification / desktop prompt ("Headless run X needs approval"); answer from the desktop, `genie approvals` or any attached client (D51).
- `--ask-timeout` (default none) → deny + exit 3 on expiry; `--on-ask deny` denies immediately (recommended for CI). When `CI=true` and a pause happens, Genie prints a hint about these flags.
- `--allow`, `--permission-mode`, settings or `--dangerously-skip-permissions` avoid asks (bypass still honours deny, policy and D21 confirmations).

### 4.5 Daemon and lifecycle

- Uses the **shared daemon** by default; `--standalone` starts a private daemon for CI containers that exits on stdin EOF (01 §4.4).
- **Waits for background subagents** to report and the parent's final turn to end before exiting (D47), or until `--max-turns` / a timeout.
- **Trust:** an untrusted folder loads no project config unless `--trust` (or a policy-allowed root); a one-line notice says what was skipped (D43).
- **ACP is v2** (`genie acp`). No public SDK in v1.

## 5. Importer

D53; t26. A **one-time copy**, never live compatibility.

### 5.1 Sources (v1)

| Source | Imported | → Genie |
|---|---|---|
| **Claude Code** | `~/.claude/settings.json`, project `.claude/settings(.local).json`: permissions (same syntax), `defaultMode`, non-secret env, `additionalDirectories` | user / project / local settings |
| | MCP from `~/.claude.json` (user/local) | `mcpServers` (project `.mcp.json` is read natively) |
| | `CLAUDE.md`, `CLAUDE.local.md` | a `GENIE.md` with `@CLAUDE.md` (and `GENIE.local.md` with `@CLAUDE.local.md`); appended after confirmation if GENIE.md exists |
| | skills, agents, commands | `.genie/skills`, `.genie/agents`, `.genie/commands` (`!cmd` flagged for trust) |
| | hooks | Genie hooks wrapping each script in `genie hook-adapter claude` |
| **Claude Desktop** | `claude_desktop_config.json` `mcpServers`; installed extensions | `mcpServers`; extensions → **connector suggestions** matched to the directory |
| **OpenCode** | `opencode.json(c)` global + project: providers/models (keys → keychain), permissions (last-match-wins → D25 classes with diagnostics), MCP, agents, commands, instructions | providers, settings, MCP, agents, commands |
| | `AGENTS.md` | nothing to do (read natively) |

Cursor and VS Code configs are later. **No consumer-subscription credentials** are imported (C2).

### 5.2 Flow

1. **Scan** known locations (`import.scan`).
2. **Preview** (`import.preview`): per item *import / skip / conflict*, with **per-key diagnostics** (unsupported key, semantic difference, secret detected → move to keychain). Nothing is written yet.
3. **Apply** (`import.apply`) the selected items. Secrets go to the keychain (D26), never into settings.
4. Imported project hooks, MCP servers and `!cmd` commands still need **trust + definition-hash approval** (D43).
5. A report is saved to `~/.genie/imports/<date>.json`. Re-running is **idempotent** (already-imported items detected by source path + content hash).

Offered at first run (desktop and CLI), via `genie import` and in Settings → General → Import.

### 5.3 Permission conversion (OpenCode)

OpenCode's `{action, resource}` rules are last-match-wins; Genie evaluates by class (deny → ask → allow). The converter groups rules by class, and when an OpenCode allow followed a deny for an overlapping pattern (an exception that Genie can't express), it emits a diagnostic and keeps the **deny** (safe default, C4).

**Spec assumption (SA-40):** unresolvable OpenCode allow-after-deny exceptions are imported as the deny plus a diagnostic, never as an allow. Reason: D53 says "converted with diagnostics" but not which way a conflict resolves; C4 favours the safe side.
