# 05 — CLI/TUI stack and rendering

type: wayfinder:grilling · status: closed · claimed-by: charting session (2026-11-16) · blocked-by: 02

## Question
How is the terminal client built and rendered?

## To decide
- ratatui inline (KB lean) vs full-screen
- Streaming markdown with a settled/streaming split (like patchmark)
- The CLI connects to the shared daemon or embeds it
- Parity plan vs desktop (delivery is desktop-first per D6)

## References
- OpenCode ch.14: [14-cli-headless-and-acp.md](../knowledge/opencode/14-cli-headless-and-acp.md) — read TL;DR, Architecture, Genie takeaways
- OpenCode ch.15: [15-tui-and-rendering.md](../knowledge/opencode/15-tui-and-rendering.md) — read TL;DR, Architecture, Genie takeaways

## Resolution
Resolved 2026-11-16. User decisions are marked **(user)**; the rest are defaults from KB ch.14/15 and D20/D22.

### Style (user): inline, like Claude Code
- **ratatui inline viewport** (`Viewport::Inline(h)` + `insert_before`) on crossterm. The conversation goes into the **real terminal scrollback** (append-only, never rewritten), and a **live footer** (prompt, status line, slash menu, permission prompts, elicitation forms) grows/shrinks as needed.
- **Streaming:** one active entry is rendered off-screen. Only rows belonging to **settled Markdown blocks** are committed to scrollback; the unstable tail stays in the live region (the design of OpenCode's `mini`).
- No full-screen mode in v1 of the CLI. Session browsing uses `genie sessions` (list) + `genie resume <id>` / `--continue` / a picker in the footer.

### Scope (user): Code sessions + read/continue chats
- The CLI is built for **Code sessions** (tools, permissions, modes D25, shell → 07, checkpoints → 23).
- **`genie chat`** lists, opens and continues **desktop chats** in text form through the D20 hand-off (`take_over` / driver lease). Artifacts, browser views, Mermaid and other rich parts render as a one-line summary + **"open in desktop"** link (`genie://session/<id>#part`). Hand-off works both ways.

### Rendering (default)
- **Port `patchmark`** (OpenCode, MIT, ~4K lines, C3 attribution) to Rust as `genie-markdown`, starting with the heal step; byte offsets instead of UTF-16. It exposes block stability explicitly and is **shared with the desktop renderer decision in 37** if that also wants a Rust/WASM parser.
- Code blocks via `syntect` (or tree-sitter highlight); diffs rendered as unified with +/− colouring; LaTeX shown as source; Mermaid as a code block + "open in desktop".
- **Themes:** semantic role tokens (ported design), a "system" theme from the queried terminal palette + luminance-based light/dark guess; **truecolor → 256 → ANSI 16** fallback; `NO_COLOR` respected.

### Input and UX (default)
- Multi-line prompt editor (Shift/Alt-Enter newline), history, `@file` completion, `/` slash menu from **one command registry** that also drives keybindings (D37 commands), paste of images/files (as attachments), Esc to interrupt (D22 `interrupt`), steer vs queue when typing during a turn (D20 inbox), two-press Ctrl-C to exit.
- Permission prompts inline in the footer: Allow once / Always (with scope) / Deny + reason (D25).
- Status line: model · effort · mode · context % (D32 meter) · cost; configurable later.
- Mode switching with Shift-Tab (default → accept-edits → plan), as in Claude Code.

### Connection (default; D20/D22)
- The CLI is a `genie-client` of the **shared daemon**. It runs `ensure` on start (spawn if absent, health handshake) and a "Starting engine…" spinner, and requires the D22 hello. Deltas are batched about every 10 ms before redraw. On reconnect it resumes from the last `seq`.
- A non-TTY or `-p` → headless (→ 25).

### Not in CLI v1
- Full-screen TUI, inline image rendering (sixel/kitty) and a terminal Mermaid renderer → later.
