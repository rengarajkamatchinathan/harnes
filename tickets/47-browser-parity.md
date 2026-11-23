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
- 2026-11-19: **Phase 1 done** on `fix/browser-panel` (committed).
  - **Daemon:**
    - New RPCs: `browser.navigate`, `browser.tab.new`, `browser.tab.close`.
    - `BrowserHost::turn_active`: input and navigation while idle no longer set *user in control*.
    - Pop-ups from the user's clicks are adopted through `Target.setDiscoverTargets`.
  - **Panel:** address bar with back, forward and reload; tab strip with `+` and `×`; direct use of the page while idle; ⋮ menu (Open links in built-in browser, Clear browsing data).
  - **App:** chat links open in the Browser tab (Cmd/Ctrl-click opens the system browser); Cmd/Ctrl+Shift+B toggles the tab.
  - **Tests:** `user_browsing_needs_no_prompts`, `address_bar_input`, and `real_chrome_user_browsing` (ignored; passes against local Chrome).
- 2026-11-19: **Phase 2 done** on `fix/browser-panel` (committed, `50c15af`).
  - **Service** (`genie-tools/src/preview/`): `DevServers` reads and writes `.genie/launch.json` (Claude's fields; comments, trailing commas and unknown keys kept). Each server runs in its own `genie-process` kill-tree with `PORT` set, `autoPort`, `cwd` kept inside the project, and a 512 KiB output ring. It is ready when its port accepts connections, or, with no `port`, when the process tree starts listening (the ticket 43 probe). A server that exits before then fails with its last output. `url` alone attaches to a server that is already running.
  - **Agent tools** (Code only): `preview_start` asks like `shell` (`Bash(<command>)`, so the same rules match) and saves inline entries to `launch.json`; `preview_stop`; `preview_logs`. GPT tuning now points servers at `preview_start`.
  - **Daemon:** `preview.list/start/stop/logs`, and `preview.changed` on the workspace feed. Servers outlive turns and actor eviction; session delete and daemon exit kill them.
  - **Site policy:** `localhost`, `*.localhost`, `127.x` and `::1` are allowed without asking. A user or admin block still wins.
  - **UI:** a server card under `preview_start` (live state, Open, Stop or Start); a **Dev servers** menu in the Browser tab bar (Open, Stop or Start per entry, Stop all); a server that comes up in the session on screen opens in its Browser tab. The mock scripts a `preview_start` turn ("start the dev server").
  - **Tests:** `genie-tools/tests/preview.rs` (6, real processes, need `python3`) and the `launch.json` parser unit tests.
  - **Known gaps:** the Dev servers menu is hidden while the "No browser found" state shows. `preview.start` from the menu waits up to 60 s.
- 2026-11-19: **Phase 3 done** on `fix/browser-panel` (committed, `d757f3d`).
  - **Console capture** (`genie-browser/src/console.rs`): each attached tab enables `Runtime` and `Log`. Console calls, uncaught exceptions and browser log entries are kept per tab: 300 entries, cleared when the main frame navigates, favicon 404s dropped. A new read tool, `browser_console {tab_id?, level?}`, returns them as untrusted page text, so there are now 19 browser tools.
  - **Auto-verify** (`genie-core` turn loop): when a Code turn is about to end after a successful file edit with no page check since (screenshot, snapshot, console or read), and a session dev server is running with `autoVerify` on, the engine adds one hidden note (`hidden/auto_verify.md`) asking the agent to check the app. At most once per turn.
  - **Off switch:** `"autoVerify": false` in `launch.json`, for the project or per server, or the ⋮ menu's **Auto-verify changes** (`preview.auto_verify`).
  - **Tests:**
    - `genie-core/tests/auto_verify.rs` (4).
    - Console parser unit tests.
    - `real_chrome_console` (ignored): passes against local Chrome, along with the other three real-Chrome tests when run with `--test-threads=1`. In parallel, `real_chrome_user_browsing` can fail because several Chromes start at once.
- 2026-11-19: **Phase 4 done** on `fix/browser-panel` (not committed).
  - **Element select** (Cmd/Ctrl+Shift+S, or the search icon in the Browser bar): hovering outlines the element under the pointer with its tag and size. A click adds the element's URL, a CSS selector, its text and HTML to the composer, plus a JPEG crop as an attachment. Esc cancels. Backed by `browser.inspect`.
  - **Local file previews:** inline code or relative links in chat that name `.html/.htm/.pdf/.mp4/.webm/.mov/.ogv/.svg` files are clickable and open as `file://` in a new Browser tab (`browser.open_file`, limited to files inside the session's folder).
  - **Minimize:** a chevron in the panel header shrinks the panel to a pill at the bottom right. The pill, or opening any panel tab, restores it.
  - **Keep cookies:** a ⋮ toggle backed by user setting `browser.keepCookies` (default on). Off clears cookies at daemon exit, and at the first browser start after a crash.
  - **First outside link:** asks "Browser tab" or "Default browser" and remembers the answer, which also sets "Open links in built-in browser". Cmd/Ctrl-click still goes to the default browser.
  - **Policy keys:** `browser.agentLocalOnly` and `browser.userLocalOnly` (ADMX/ADML added; merge as true-wins).
  - **Tests:**
    - `local_only_policy_blocks_outside_sites`.
    - `real_chrome_inspect_files_and_local_only` (ignored; passes against local Chrome). `real_chrome_console` now runs under the local-only policy.
    - Desktop `browserParity.test.ts` (5).
  - **Not done:** the macOS `.mobileconfig` template has no browser keys yet (none of the existing browser keys are there either).
