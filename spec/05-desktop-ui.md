# 05 — Desktop UI

Implements D5, D8, D19, D28, D29, D31–D34, D36, D39 (consent), D43 (trust dialog), D53 (import offer), D54, D55, D56, D57; C1; tickets 29, 30, 31, 33, 37, 39, 40. The mockup referenced by D54 (claude.ai artifact) is the visual reference; this file describes structure and behaviour.

## 1. App frame

```
┌───────────────┬──────────────────────────────────────┬────────────────────┐
│ Left sidebar  │ Main column                          │ Right panel        │
│ [Chat | Code] │ header: title · model · effort · …   │ tabs (per mode)    │
│ New           │ transcript (virtualised)             │ resizable,         │
│ Search        │                                      │ hideable           │
│ list          │ composer + context ring              │                    │
│ (footer:      │                                      │                    │
│  settings,    │                                      │                    │
│  account)     │                                      │                    │
└───────────────┴──────────────────────────────────────┴────────────────────┘
```

- **Mode switch** at the top of the sidebar: **Chat** and **Code** (Code ships with M6).
- **Right panel** (D54): Chat → **Artifact · Browser**; Code → **Changes · Files · Terminal · Browser**. It can be resized and hidden; its state per session is in `state.json` (D23).
- **Settings** is a full page that replaces the main column (D34).
- Window state, sidebar width, last tab and panel sizes live in `state.json`, never in settings (D23).
- Deep links `genie://session/<id>[#part]`, `genie://settings/<section>[/<item>]` (D34, D42).

## 2. Chat mode

### 2.1 Sidebar (t31)

- **New chat**, **Search** (FTS over titles and content, D24), **Projects** (starred first), **Recents** grouped by date.
- Row actions: rename, pin, delete (with confirm), move to project, export (Markdown/JSON).
- **Incognito**: a toggle on the New chat button; the chat shows a persistent banner "Incognito — not saved to history, memory or search" (t31). Policy `incognito.allowed` can hide it (D38).
- **Recent artifacts** list for the open chat (t39).

### 2.2 Conversation

- Centred column; messages render streaming Markdown, code blocks with copy, LaTeX (KaTeX), Mermaid, tables (t31).
- **Thinking** in a collapsible section ("Thought for 12 s"); redacted thinking shows "omitted".
- **Per message:** copy, edit (user messages) → new branch, regenerate (assistant), **`‹ 2/3 ›`** branch switcher, token + cost on hover (t31, D24).
- **Tool rows** (chat): web search ("Searched 5 sites"), web fetch, MCP calls (server icon, tool name, args/results expandable), memory chip "Memory updated" (view/edit/undo), artifact cards, browser step log (collapsible), code execution outputs.
- **Citations** (D36): inline numbered markers `[1]` with a hover card (title, domain, snippet), a **Sources** row of favicon chips under the answer. Clicks open the **system browser**. Fetched sources say "Fetched". Favicons come through the daemon (no webview network).
- **Compaction divider** (D33): "Earlier messages summarised · View summary" (or "· Summarised by <provider>"). Originals stay visible.
- **Interrupted turn:** a banner "This response was interrupted" with **Resume** (D20).
- **Retry banner:** "Rate limited — retrying in 8 s · Retry now · Stop".
- **Ask cards** (D25, D30): permission prompts render inline above the composer: tool, resource, preview, **Allow once / Always allow / Deny** (+ optional reason field). MCP annotations shown as info only. Elicitation renders a form; sampling shows request text, model, estimated cost and budget left. Subagent label when relevant.
- **Read-only state** when another client drives: banner "Open in <client> · Take over" (D20).

### 2.3 Composer

- Multi-line input; **Enter** sends, **Shift+Enter** newline.
- **Attachments:** drag/drop, paste, picker; images, PDFs, text/code (t31). Chips show size and a warning if they won't fit the context.
  - **Spec assumption (SA-27):** 32 MB per file and 20 files per message, before model limits from the catalog apply. Reason: t31 says limits come from per-model capability but the app also needs an upload cap.
- **Model picker** grouped by provider with badges; **effort selector** next to it with only supported levels (D27).
- **"+" menu:** attach file, attach MCP resource, skills, connectors with per-chat server/tool toggles (D30), web search toggle (greyed out with tooltip when unsupported, D11/D36), code execution toggle (greyed out where unsupported, D35), browser.
- **Context meter ring** (D32): fills with context use; hover shows tokens used/window, cost so far, "compaction at X%"; amber near the trigger.
- **While a turn runs:** the Send button becomes **Stop**; typing and sending delivers as **steer** by default; **Alt+Enter** sends as **queue**; queued items appear as chips above the composer with remove.
  - **Spec assumption (SA-06):** steer by default, queue on Alt+Enter, in both desktop and CLI. Reason: D20/D42 require both deliveries but don't say which is default; steer matches Claude Code's behaviour of folding new input into the running task.
- Slash commands in chat: `/skill-name`, `/server:prompt` (MCP prompts with an argument form, D30).

### 2.4 Projects (t31, D32)

- Project page: name, instructions, knowledge files (upload, list, remove), starred, chats in the project.
- **Capacity bar** shows knowledge size against the window; above 50% a badge "Using retrieval" and indexing progress (D32).

### 2.5 Artifact panel (D28, t39)

- Opens as soon as `artifact_create` starts streaming its input. Documents and code render live; HTML, React, SVG and Mermaid render when the input is complete.
- Toolbar: title, **version switcher `‹ v2/3 ›`** (versions on the active branch path), **Code / Preview** tabs, Copy, Download (`.md`, source file, `.html`, `.svg`, `.png` for diagrams, `.jsx`), Fullscreen, Close.
- Runtime error banner with **Fix with Genie** (sends error + stack as a new user message).
- The sandbox design is in §8.

### 2.6 Browser panel

See §9. In Chat and Code, the Browser tab shows the live screencast, a tab strip, the agent-control banner and confirmation cards.

## 3. Code mode (M6)

D54, D55; t23, t24.

### 3.1 Sidebar

- Sessions **grouped by repo** (workspace). Each row: title, **branch badge** (`main`, `genie/fix-login`), **status dot** (running / waiting for approval / idle / interrupted / error), worktree icon.
- **New session**: folder picker (recent workspaces), agent picker (D46: default, or `--agent`-style choice), option "New session in worktree" (D50). A second running session on the same repo creates a worktree automatically with the banner "Working in an isolated copy · branch genie/<slug>".
- Archive, delete, rename.

### 3.2 Main column

- Header: repo · branch · **mode selector** (default / accept-edits / plan / bypass; bypass hidden if policy disables it) (D25), model, effort.
- **Compact tool rows** (D54): one line each ("Edited src/auth.rs +12 −3", "Ran `cargo test` · 2 failed", "Read 4 files"); expand to show the diff, output (head/tail with "show full"), or arguments.
- **Todo list** (from `todo_write`) pinned at the top of the running turn.
- **Subagent cards** (D47): name, description, status, steps, cost; expand to a read-only transcript.
- **Plan mode:** the final plan renders as a card with **Approve → default / accept-edits** and **Keep planning** (D46).
- **Rewind** action on each message (D49): dialog with Restore files / conversation / both, the file list, external changes and skipped files.
- **Worktree finish** footer (D50): Merge into <base> · Create PR · Keep · Discard.
- **Trust dialog** on first open of a folder (D43): Trust / Trust once / Don't trust, listing what the project can configure. **Definition-hash approvals** for project MCP servers, hooks and setup commands show a diff with Approve / Skip.

### 3.3 Right panel (D54)

| Tab | Content |
|---|---|
| **Changes** | Branch name; **Commit split button**; list of uncommitted files with per-file diffs (unified/split toggle); **checkpoints** list for the session with rewind entry points |
| **Files** | Workspace tree with **M/A/D markers**, ignore rules respected; click → read-only preview with syntax highlighting; "open in editor" via the OS |
| **Terminal** | One or more interactive terminals (§9) |
| **Browser** | Same as Chat (§9) |

**Spec assumption (SA-26):** the Commit split button's options are **Commit**, **Commit & push**, **Create PR** (via `gh`/`az` if found, else "push branch"); the commit message box is pre-filled by the small model from the staged diff and is always editable. Reason: D54 names a "Commit split button" without its options; Create PR reuses the D50 finish logic.

## 4. Settings (D34, t33)

Full page with left section nav, **search over all settings**, Back, deep links. Changes apply live with an inline "Saved" or validation error; settings that need a restart show a banner. Admin-locked values are disabled with a lock and "Managed by <org>". Scope picker (User / Project / Local) only in Code mode for settings that allow it (D23).

| Section | Contents | Built how |
|---|---|---|
| General | Language, launch at login, update channel / check now, default chat model + effort, notifications, **Import** (D53), "Install `genie` command in PATH" (D40) | schema |
| Appearance | Theme (System/Light/Dark), chat font (Default/System/Dyslexia-friendly), font size, density | schema |
| Your Genie | Name, custom instructions, **Memory** (on/pause, list with edit/delete, reset), past-chat search on/off | hand-built memory list |
| Providers & Models | Per provider: labelled credentials (source shown), API key / **Sign in with Microsoft** (PKCE or device code), endpoint, discovered models/deployments, catalog status, per-model **Advanced** (supported sampling, max output, default effort), overrides | hand-built |
| Capabilities | Web search, code execution, artifacts, **browser** (Chrome/Edge detection, profile, site permissions, clear data), skills (list, enable/disable, upload, directory), project retrieval | mixed |
| Connectors | MCP servers (status, tools with per-tool toggles, sign-in, logs), **connector directory** (browse, "see what runs", add), add custom (stdio / URL) | hand-built |
| Permissions | Saved "always" approvals (chat + per project) with revoke; rules (allow/ask/deny) with scope; default mode (Code); bypass availability; trusted folders with revoke (D43) | hand-built |
| Usage & spend | Tokens and cost by day / provider / model / project incl. cache and compaction lines; monthly budget with warnings at 80% and 100% (notification only) | hand-built charts |
| Privacy & data | Retention (admin limit shown), encryption status + **recovery key export**, export all / single chat, delete all data, telemetry and crash-report toggles, checkpoint disk use + clean-up (D49) | mixed |
| Developer | Daemon status (version, uptime, socket, restart), MCP logs, app/daemon logs, open settings files per scope, **doctor** + **Copy diagnostics**, Problems, effective policy (D38), dirty worktrees (D50) | hand-built |
| Account & licence | Licence status placeholder (t41), organisation if managed, About, third-party licences (incl. OpenCode MIT notice) | hand-built |

- **Schema-generated controls** come from `schemars` metadata: title, description, enum labels, `scopes_allowed`, `restart_required`, `advanced` (D23).
- Code-mode settings section and Keyboard shortcuts are **v2** (t33).

## 5. First run (D39, D31, D53, D43)

1. **Welcome** → **Privacy consent**: crash reports (on, can turn off), usage analytics (off, can turn on), "never content" statement. Forced values from policy are shown locked.
2. **Connect a provider**: pick a preset, paste a key or sign in with Microsoft; a test call confirms. Skippable (the app then shows an empty state with "Add a provider").
3. **Import** (if found): "We found settings from Claude Code / Claude Desktop / OpenCode — import?" → preview with per-item diagnostics (D53; 07 §5).
4. **Memory notice**: what memory is, how to view/pause it (D31).
5. **Code mode only**, on the first folder: the trust dialog (D43).
6. If the secrets backend is the passphrase file: create passphrase + optional recovery key (D26, D24).

## 6. Design system and tokens (D19, t29; C1)

- **shadcn/ui + Radix + Tailwind**, themed by CSS variables; light and dark, following the system with a manual override.
- **Fonts:** Inter (UI) — or Geist, decided by design — and a monospace (Geist Mono or JetBrains Mono); all open licences, bundled (no network fonts).
- **Token model** (pick-list #36): seed colours → OKLCH scales (12 steps) → semantic variables.

| Group | Tokens |
|---|---|
| Colour | `--bg`, `--bg-subtle`, `--surface`, `--surface-raised`, `--border`, `--text`, `--text-muted`, `--accent`, `--accent-fg`, `--focus-ring`, `--danger`, `--warning`, `--success`, `--info`, `--diff-add`, `--diff-del`, `--code-bg` |
| Type | `--font-ui`, `--font-mono`, scale `xs 12 / sm 13 / base 14 / md 15 / lg 18 / xl 22 / 2xl 28`, chat body `--chat-font-size` (Appearance) |
| Space & shape | 4-px grid; `--radius-sm 4 / md 8 / lg 12`; density `comfortable | compact` |
| Motion | `--dur-fast 120ms`, `--dur-base 200ms`; `prefers-reduced-motion` disables streaming caret animation and panel slides |

- **Spec assumption (SA-20):** neutral greys from a cool-grey seed and a **placeholder accent** (a teal, `oklch(0.62 0.13 195)`) until the design/brand input arrives; the logo/icon is a placeholder wordmark. Reason: t29 defers the exact accent, token values and logo to the spec phase; a placeholder lets work start without blocking on brand. It must not resemble Anthropic's palette (C1).
- **i18n from day one:** every string through a key; English only in v1; `plural()` helpers (pick-list #37).
- Never: Anthropic names, logos, fonts, illustrations or pixel-identical layouts (C1).

## 7. Renderer (D57, t37)

Confirmed by prototype 37 on Windows; macOS/Linux runs pending (D8 stays conditional).

- **Settled/tail split:** the streaming message is parsed into Markdown blocks; settled blocks are rendered once and memoised; only the unstable tail re-renders. **One commit per animation frame** (rAF).
- **Virtualised transcript** (only visible messages are mounted; `content-visibility: auto` fallback).
- **highlight.js** for code while streaming (p95 0.6 ms vs shiki 3.7 ms); settled blocks may upgrade to a richer highlighter later if needed.
- **~8 ms delta batching** on the Tauri side before IPC.
- **Mermaid and KaTeX pre-loaded at startup** (idle callback) and rendered **off the streaming path**: blocks render when complete, in a worker or idle callback, with the last good render kept while streaming. This removes the 6 long tasks (up to 210 ms) seen on first Mermaid load.
- **Performance gate (CI, all three engines):** emit→paint p95 ≤ 50 ms at 100 tok/s; burst ≥ 500 tok/s with 0 long tasks > 50 ms; frame p95 ≤ idle frame p95 + 1 ms; reconnect ≤ 2 s; 2,000-message timeline scrolls without long tasks. Frame targets are **relative to the display's idle frame time** (D57). Windows measured: emit→paint p95 47.8 ms, burst 984 tok/s, reconnect 0.28–0.32 s, cold start ~1.07 s.
- Markdown parser: a JS incremental parser in the webview for v1 (prototype path). `genie-markdown` (Rust) is for the CLI; compiling it to WASM for the desktop is an option, not a plan (t05).

## 8. Artifact sandbox (D28, t39)

- Served from a **separate custom scheme** `genie-artifact://<artifact>/<version>/` (Windows: `http://genie-artifact.localhost/`) by the Tauri Rust side, never from the app origin. Content comes from the daemon via `artifact.get`.
- Shown in `<iframe sandbox="allow-scripts">` with **no `allow-same-origin`** → opaque origin: no cookies, storage or parent DOM.
- **Tauri capabilities** are not granted to the artifact origin (no IPC, plugins or daemon access). The capability file lists only the app origin.
- **CSP** on every artifact response: `default-src 'none'; script-src genie-artifact: 'unsafe-eval' 'wasm-unsafe-eval'; style-src genie-artifact: 'unsafe-inline'; img-src genie-artifact: data: blob:; font-src genie-artifact: data:; connect-src 'none'; frame-src 'none'; form-action 'none'`.
- **postMessage only**, fixed schema: `ready`, `resize {height}`, `runtime_error {message, stack}`, `request_fullscreen`. The parent validates `event.source === iframe.contentWindow` and the schema; anything else is ignored.
- Blocked: top navigation, popups, downloads, `window.open`; `alert/confirm/prompt` shimmed.
- **React:** a single default-export component transpiled **in the sandbox with esbuild-wasm**; imports resolve only to the **bundled library set** (React, ReactDOM, prebuilt Tailwind CSS, lucide-react, Recharts, d3, lodash, papaparse, mathjs, three.js, Mermaid, KaTeX), served from the scheme. Unknown imports → clear error.
- **SVG** is sanitised (no scripts, no external refs) and shown as an image.
- **Hostile-artifact CI test** loads an artifact that tries IPC calls, `fetch`, WebSocket, `top.location`, `parent.document`, storage, form posts, `window.open`, nested frames and CSP bypasses, and asserts every attempt fails — **on WebView2, WKWebView and WebKitGTK** (D28; 09 §5).

## 9. Browser tab and screencast (D56, D21)

- Chrome/Edge (Genie profile) runs in the background; the daemon streams **CDP `Page.startScreencast`** frames of the active tab; the tab draws them on a `<canvas>` scaled to fit.
- **Agent-control banner** in Genie's window: status pill **"Genie is browsing" / "Paused – you're in control" / "Waiting for your approval"**, buttons **Take over** (also Esc when focused), **Hand back**, **Stop**, and **Pop out** (shows the real window).
- **Take over:** the canvas captures mouse, wheel and keyboard and forwards them with `Input.dispatchMouseEvent` / `Input.dispatchKeyEvent`, mapping coordinates by `device_scale`. Any user input auto-pauses the agent; agent tools then return `user_in_control` until Hand back (research/browser.md §4.6).
- **Tab strip** mirrors CDP targets; the user can switch the viewed tab; only the active tab is screencast.
- **Confirmation cards** for sensitive actions (screenshot of the target + summary + Approve / Take over / Deny) appear in the chat/session and the Browser tab.
- **No browser installed:** an empty state explains and links to install Chrome, Edge or Chromium.

**Spec assumption (SA-23):** screencast at JPEG quality 60, max 1280×800 CSS px × device scale (capped at 2), `everyNthFrame: 1`, ack-based flow control (one frame in flight per client); targets ≥ 15 fps while the page changes and ≤ 150 ms input-to-frame latency on a local machine; when the tab is hidden the screencast stops. Reason: D56 asks the spec to check frame rate/latency but sets no targets.

**Spec assumption (SA-24):** take-over details: text input uses `Input.insertText` for committed text and `Input.imeSetComposition` for IME composition; paste reads Genie's clipboard on Ctrl/Cmd+V and sends `insertText`; copy uses a CDP `Runtime.evaluate` of the selection **only during take-over** and only on user keypress; file pickers opened during take-over are intercepted (`Page.setInterceptFileChooserDialog`) and answered with a native Genie file dialog; downloads go to the quarantine folder; multi-tab: new tabs/popups appear in the tab strip and are not auto-focused for the agent. Reason: D56 lists IME, clipboard, file pickers and multi-tab as risks to check in the spec.

## 10. Terminal (D55)

- **xterm.js** in the webview (with the WebGL renderer where available, canvas fallback) + the **PTY mode of `genie-process`** (portable-pty + libghostty-vt) in the daemon.
- One or more terminals per session, opened in the session's folder or worktree, using the D45 shell choice (PowerShell default on Windows, `shell: "bash"` for Git Bash; login shell on macOS/Linux).
- **Reattach** after a window reload or client switch: `terminal.attach {offset}` returns replay bytes from the 2 MiB ring, or a **VT checkpoint** when truncated (KB ch.16).
- Terminals are user-driven; the agent does not read or type into them in v1 (the agent uses the `shell` tool).

**Spec assumption (SA-25):** at most 8 terminals per session; terminals close when the session is closed or the daemon exits and are not restored after a daemon restart (scrollback is not persisted). Reason: D55 doesn't address limits or persistence; persisting PTY state across daemon restarts isn't possible anyway.

## 11. Accessibility

- Full keyboard navigation (sidebar, transcript, composer, panels, dialogs); visible focus rings; Radix primitives for focus management.
- Screen-reader labels from the schema in Settings (t33); transcript uses `role="log"` with polite live updates **per settled block**, not per token; ask cards announce assertively.
- Contrast meets WCAG AA in both themes (t33); `prefers-reduced-motion` respected; text scales with the font-size setting up to 200% without loss.
- The canvas-based Browser view and the terminal are inherently visual: the browser step log and confirmation cards give a text alternative; xterm.js screen-reader mode is enabled.

**Spec assumption (SA-22):** the target is **WCAG 2.2 level AA** for the whole app (not only contrast), checked with axe in CI and a manual pass with NVDA (Windows), VoiceOver (macOS) and Orca (Linux) before GA. Reason: t33 names WCAG AA contrast and keyboard/screen-reader support but not a full conformance target.

## 12. Platform support

- WebView2 (Windows), WKWebView (macOS), WebKitGTK (Linux) at full support (D9), subject to the D8 gate (D57).
- Linux: set `WEBKIT_DISABLE_DMABUF_RENDERER=1` when an NVIDIA GPU is detected; document `__NV_DISABLE_EXPLICIT_SYNC=1` for Wayland + NVIDIA (research/desktop-shell.md). If WebKitGTK fails the gate, use Tauri 3's Chromium runtime on Linux (D9).
- **Spec assumption (SA-21):** minimum versions: Windows 10 22H2 and Windows 11 (x64, arm64); macOS 13 Ventura or later (universal); Ubuntu 22.04+/Debian 12+/Fedora 40+ with WebKitGTK 4.1 ≥ 2.42. Reason: research/desktop-shell.md Q4 leaves the minimum macOS open; these are the oldest versions still receiving vendor security updates in late 2026 that Tauri 2 supports. The D8 bench runs must use the oldest macOS chosen.
