# 47 — Browser tab parity with Claude Code Desktop

type: build · status: in progress · claimed-by: — · blocked-by: — · related: 40, 05 §9, 43

## Goal
Make Genie's Browser tab work like Claude Code Desktop's Browser pane. The feature list and the gap table are in [research/browser.md §2.1](../research/browser.md). Genie keeps what it already does better: take over, hand back, Stop, Pop out and the step log.

## Phases

### Phase 1 — The pane is a real browser the user can drive
- New RPCs, none of which prompts (the user's own navigation never prompts, research §4.2):
  - `browser.navigate {session_id, tab_id?, url? | action: back|forward|reload}`. Launches the browser and opens a tab if none exists.
  - `browser.tab.new {session_id, url?}`
  - `browser.tab.close {session_id, tab_id}`
- **The user can use the page directly while Genie is idle**, with no Take over needed. Input and navigation outside a turn mark the state as changed (the next agent action gets the "user acted" note) but don't set *user in control*. During a turn, input still pauses the agent (D56).
- Panel:
  - Address bar (Enter to go; no scheme means `https://`, `localhost` means `http://`) with back, forward and reload.
  - Tab strip shown whenever a tab exists, with `+` and `×`.
  - Empty state with a URL field.
- ⋮ menu:
  - **Clear browsing data** (existing `browser.data.clear`)
  - **Open links in built-in browser** (desktop setting, default on)
- Chat links: a click on an `http(s)` link opens it in the Browser tab when that setting is on. Cmd/Ctrl-click always opens the system browser.
- **Cmd/Ctrl+Shift+B** opens the Browser tab.

### Phase 2 — Dev servers (Claude's `launch.json`)
- `.genie/launch.json`, with the same fields as Claude's `.claude/launch.json`: `name, runtimeExecutable, runtimeArgs, program, args, port, autoPort, cwd, env, url, autoVerify`. Created automatically on first start.
- Daemon service: start, stop and list servers per session. It runs on `genie-process` with logs, port wait and `autoPort`. Related: 43 (servers must not block the turn).
- Agent tools: `preview_start`, `preview_stop`, `preview_logs`. Starting a server opens it in the Browser tab.
- UI:
  - A server card in chat with an **Open** button.
  - A **Dev servers** menu in the panel header (start, stop, stop all).
  - Localhost and project files need no site approval.

### Phase 3 — Auto-verify
- After file edits in a turn where a dev server is configured, the agent screenshots the result, checks for console and page errors, and confirms before ending. Default on.
- Turned off per project with `"autoVerify": false`, or with the ⋮ menu toggle.
- Needs console capture (`Runtime.consoleAPICalled`, `Log.entryAdded`) as a read tool.

### Phase 4 — Remaining items
- Element select (Cmd+Shift+S): an overlay picks an element and sends its selector, text and a crop to the composer.
- Preview local HTML, PDF and video paths clicked in chat.
- Minimize the pane.
- **Keep cookies** toggle (otherwise cookies are cleared on quit).
- "Pane or system browser?" prompt on the first outside link.
- Admin policy keys: no outside-page tools, no outside navigation (ticket 28).

## Status
- 2026-11-19: **Phase 1 done** on `fix/browser-panel` (not committed).
  - **Daemon:**
    - New RPCs: `browser.navigate`, `browser.tab.new`, `browser.tab.close`.
    - `BrowserHost::turn_active`: input and navigation while idle no longer set *user in control*.
    - Pop-ups from the user's clicks are adopted through `Target.setDiscoverTargets`.
  - **Panel:** address bar with back, forward and reload; tab strip with `+` and `×`; direct use of the page while idle; ⋮ menu (Open links in built-in browser, Clear browsing data).
  - **App:** chat links open in the Browser tab (Cmd/Ctrl-click opens the system browser); Cmd/Ctrl+Shift+B toggles the tab.
  - **Tests:** `user_browsing_needs_no_prompts`, `address_bar_input`, and `real_chrome_user_browsing` (ignored; passes against local Chrome).
- Next: Phase 2 (dev servers).
