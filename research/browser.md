# Built-in agent browser (ticket 40)

> Research for [tickets/40-browser.md](../tickets/40-browser.md) · decisions D8, D9, D16, D17 in [WAYFINDER.md](../WAYFINDER.md) · 2026-11-16
> Anything marked *(unverified)* was not checked against a primary source in this session. Sizes and versions were fetched live on 2026-11-16 unless marked otherwise.

## TL;DR

- **Recommendation for v1: Option A, built on CEF.** Ship a separate, signed helper app, **"Genie Browser"**, built with `cef` (the `tauri-apps/cef-rs` bindings, which track CEF stable). It is **downloaded on first use** of the Browser button and kept up to date through its own signed component channel. The **daemon drives it over CDP**, using an **authenticated IPC pipe rather than an open TCP port**. The window's toolbar (tabs, URL, "Genie is driving" indicator, Take over / Stop) is Genie's own trusted UI, kept separate from page content.
- **Why not the others.** **B** (the user's Chrome/Edge) is not reliably present on macOS or Linux, can't show Genie's window chrome, and varies from machine to machine. **C** (native webview + injected JS) can't reach cross-origin iframes, native dialogs or uploads on macOS/Linux, and behaves differently on each OS. **D** (the Tauri 3 CEF runtime) is still alpha (`tauri-runtime-cef 3.0.0-alpha.7`) and **runs Chromium unsandboxed on Windows**, which is not acceptable for a browser that opens arbitrary websites. **Chrome for Testing** is ruled out as the shipped engine: Google says it is "purely for browser automation and testing" and "should only be used to consume trustworthy content", and it does not auto-update.
- **Fallback:** **B**, the system Chrome or Edge with an isolated `--user-data-dir` over `--remote-debugging-pipe`. It sits behind the same `BrowserBackend` trait, so all tools and safety code are shared (both backends speak CDP). **Migration path:** D. Rebuild the helper on Tauri-CEF once it is stable and sandboxed on Windows. If D8 ever moves the main app to CEF, the browser can merge into the main window.
- **Cost:** about **+135–175 MB compressed download** (CEF 156 "minimal" distribution: macOS arm64 135 MB, Windows x64 175 MB; Linux x64 333 MB before stripping). On disk, roughly 350–600 MB *(unverified)*. Effort is about **26–33 engineer-weeks** for v1 (around 2 engineers for 3.5–4 months, including a 2-week spike), plus about **0.25 FTE ongoing** to ship Chromium security updates.
- **Top risks:** (1) Genie becomes a **browser vendor** and must ship Chromium security fixes within days; (2) **prompt injection** from page content can be reduced but not eliminated; (3) **sandboxing and signing on each OS**: Windows CEF sandbox via cef-rs, the Ubuntu AppArmor user-namespace restriction, and signing every CEF helper bundle on macOS.

## 1. Options

### 1.1 Comparison table

| Criterion | **A1. CEF helper (cef-rs)** ✅ | A2. Chrome for Testing on first use | **B. User's Chrome/Edge** (fallback) | C. Native webview + injected JS | D. Tauri 3 CEF runtime |
|---|---|---|---|---|---|
| Same behaviour on all 3 OSes | **Yes.** One pinned Chromium everywhere | Yes | **No.** Edge is on Windows 11. On macOS and Linux Chrome may be missing, and Linux snap/flatpak confinement complicates launching *(unverified)* | **No.** WebView2 has CDP. WKWebView and WebKitGTK only allow injected JS | Yes |
| Size | +135–175 MB compressed download (CEF 156 minimal) | ~190–210 MB zip per platform (CfT 157 stable: win64 210 MB, mac-arm64 197 MB, linux64 203 MB) | 0 MB | 0 MB | Hundreds of MB more for the **whole app** (Tauri docs) |
| First run | "Set up Genie Browser" with a progress bar (about 30–60 s on broadband *(estimate)*). An "offline/enterprise" installer can bundle it | Same, plus Google's servers in the critical path | Instant if Chrome/Edge is found. Otherwise falls back to A | Instant | Part of the app install |
| Engine security patches | **Our job.** Rebuild or re-sign on each CEF stable update. CEF follows Chromium stable, which ships security fixes about weekly *(unverified cadence)* | Our job; CfT "without auto-update" | **Free.** Google and Microsoft auto-update | Free (OS/WebView2), but WebKitGTK lags behind distros | Our job (whole app) |
| Signing / notarisation | Sign every exe/dll (Windows), and every helper `.app`, framework and dylib inside-out with hardened runtime and a JIT entitlement for renderers (macOS); notarise and staple. Linux: sign packages; sandbox needs userns or setuid `chrome-sandbox` | We'd be shipping **Google-signed** binaries. Re-signing or re-hosting them raises licence questions | Nothing to sign | Nothing extra | As A1, for the whole app |
| Licensing | **CEF is BSD-3**; Chromium third-party licences must ship (about:credits). Official builds have **no proprietary codecs (H.264/AAC) and no Widevine** | **Unclear.** No redistribution licence found; Chrome terms apply to Chrome executables; "testing only" intent | User's own licence | N/A | As A1 |
| Rust support | `cef` 156.6.0+156.0.30 (updated 2026-11-16); CDP from Rust via `chromiumoxide` 0.9.1 / `chromey` 2.58.2 protocol types | `chromiumoxide`, `headless_chrome` 1.0.22 (sync), `chromey` | Same CDP crates | Hand-written JS bridge per webview | `tauri-runtime-cef` 3.0.0-alpha.7; `WebviewCefExt` gives CDP |
| Looks like part of Genie | **Full control.** Our toolbar, tabs, agent indicator and take-over button live in a trusted top view | Chrome UI. An "automation" infobar may appear *(unverified)* | Chrome/Edge UI. Needs a Genie companion bar beside the window; can't put buttons inside the browser frame | Full control | Full control (Tauri multi-webview, behind `unstable`) |
| Driving fidelity (iframes, dialogs, uploads, downloads, real input events) | Full CDP | Full CDP | Full CDP (Chrome 136+ requires a non-default `--user-data-dir`) | **Weak:** no cross-origin iframes, no native dialogs, no file chooser; synthetic events (`isTrusted=false`) | Full CDP, except popups CEF opens itself bypass the observer |
| Maturity | CEF: 15+ years, used by Spotify, Steam and others *(unverified list)*. cef-rs: young, small team | Mature | Mature | Mature APIs, immature approach | **Alpha. Windows unsandboxed** |
| Verdict | **v1 primary** | Reject as shipped engine; OK for internal CI | **v1 fallback / "lite" mode** | Reject | **v2 migration target** |

### 1.2 Notes per option

**A1 — CEF helper app (recommended).**
- *Process model:* the daemon (D4) spawns `genie-browser`, a small Rust binary that links CEF through `cef-rs`. The CEF helpers live inside its bundle. One helper process serves one Genie user and all of that user's browser sessions, with one window per session (or tabs inside a window). The helper exposes **no CDP port**. It forwards CDP messages through CEF's DevTools message API (`SendDevToolsMessage` / `AddDevToolsMessageObserver`) over the daemon's authenticated named pipe or Unix socket (D14). This avoids `--remote-debugging-port`, which lets any local process attach.
- *Window:* a top-level window with two views. (1) **Chrome view**, which is trusted: Genie's toolbar, served from resources bundled in the helper (a small React/TS page that reuses Genie's design tokens from ticket 29). It has no access to page content, and pages have no path to it. (2) **Content view(s)**, which are untrusted, one per tab. How the views are composed (CEF Views framework, or a native top bar) is a build-time choice *(unverified which CEF "style", Chrome or Alloy, gives the cleanest custom chrome)*.
- *Updates:* the helper is a **separately versioned component**: a manifest signed with our Ed25519 key (the same scheme as `tauri-plugin-updater`), served from our CDN, with full downloads per version and deltas later. A **staleness rule** applies: if the installed CEF is more than N days behind a Chromium security release, the Browser button refuses to browse untrusted origins until it updates (N is decided by the user, see §8).
- *Sandbox:* macOS keeps the Chromium sandbox. Windows needs either CEF's sandbox bootstrap mechanism or `cef_sandbox` linkage through cef-rs *(unverified that cef-rs supports either today; **must be spiked first**)*. Linux needs unprivileged user namespaces. Ubuntu 23.10+ restricts these with AppArmor, so the `.deb`/`.rpm` installs an AppArmor profile; AppImage users may lose the sandbox *(unverified for CEF specifically; it is a known Electron/Chromium issue)*.

**A2 — Chrome for Testing.** Ruled out for end users. Its intent ("purely for browser automation and testing", "only … trustworthy content"), Google branding and terms, and no-auto-update model don't fit a product browser. It is still the best choice for **Genie's own CI** (conformance tests for the browser tools).

**B — The user's Chrome or Edge.** Launch with `--user-data-dir=<genie profile dir>` and `--remote-debugging-pipe`. Since Chrome 136, the debugging switches are ignored on the default profile, so isolation is forced anyway. Don't pass `--enable-automation` (which shows an infobar *(unverified)*). Without our own frame, the agent indicator and take-over button live in a **Genie companion bar**: a small always-on-top Tauri window docked to the browser, plus a page-level overlay injected through CDP (which pages can see and tamper with, so it is only cosmetic). Enterprise policies can disable remote debugging *(unverified policy name)*. It suits users who decline the download and machines where IT already manages Chrome or Edge.

**C — Native webview + injected JS.** Rejected for v1. It behaves differently on each OS, input events are synthetic (some sites ignore them), and it cannot handle cross-origin iframes (payment widgets, embedded logins), `alert`/`confirm` dialogs, the file chooser or downloads. It also tempts us to grant Tauri IPC to an origin that is browsing the open web.

**D — Tauri 3 CEF runtime.** Same engine as A1 with nicer windowing. Today it is alpha, its API "offers no API stability", it is **unsandboxed on Windows**, and the first build downloads about 1 GB. Treat it as the **v2 re-host** of the helper. Because A1 and D both use CEF and CDP, the daemon side (tools, safety, CDP layer) carries over unchanged.

## 2. How other apps do it (public sources)

| Pattern | Examples (public docs / coverage) | What they do | Takeaway for Genie |
|---|---|---|---|
| **Cloud VM browser** | General-purpose assistant "agent mode" (help-centre docs) | Remote virtual browser streamed to the user. The user can **take over**, for logins and sensitive input. **Watch mode** on sensitive sites requires the user to stay on the tab. Confirms high-impact actions; monitors for prompt injection. During takeover the agent "doesn't capture passwords" the user types | Copy **takeover for logins and secrets** and **watch mode on sensitive sites**. Local rather than cloud is our choice (D3 commercial, no hosting) |
| **Extension in the user's own browser** | Browser-extension agents (vendor announcements and third-party guides) | **Per-site permission** prompt: allow for this chat / always allow / decline. Default-blocked high-risk categories. Confirms before publishing, purchasing or sharing data. Guides note that site permissions **do not stop injection from an approved site** | Copy the three-way site prompt and default category blocks |
| **Agent built into the browser** | A major browser's agentic mode (vendor security blog) | **Separate "alignment critic" model** isolated from page content vets each action. **Origin sets**: read-only vs read-write origins tied to the task. **Deterministic URL restriction**: model-generated URLs are limited to prevent exfiltration. Model has **no access to stored passwords**. Confirmation before sensitive sites, purchases and messages. **Work log**; pause/take over at any time. Parallel injection classifier. "Spotlighting" | Copy read vs act origin scopes, URL restriction, a password-free model, and the work log. Put the critic/classifier in v1.x |
| **Embedded Chromium in an Electron desktop or IDE** | IDE "integrated browser" (WebContentsView driven by Playwright over CDP, one connection per chat session; secondary source); OpenCode's browser plugin (KB ch. 18: server owns tools and permissions, desktop owns CDP, pull-based requests, "no replay", explicit tab IDs, untrusted page data); desktop coding apps' in-app browser with an **isolated profile** that doesn't share the user's personal logins *(secondary coverage)* | Same split we propose: the agent side owns tools and policy, the browser side owns CDP | Adopt OpenCode's rules: explicit `tab_id`, **never replay** a lost action, results are untrusted |
| **CLI-launched Chromium + snapshot/refs** | Playwright MCP; agent-browser CLI (Rust/Node) | **Accessibility snapshot with refs** (`e12`) instead of screenshots; refs go stale after DOM changes; "interactive only" filter; incremental/diff snapshots to cut tokens | Our model-input format (§5) |

Common patterns: **isolated profile**; **user can take over at any time** and must for logins, 2FA and CAPTCHAs; **site-level permissions plus action-level confirmations**; **visible step log**; **page content treated as untrusted**; and layered defences, because none of them claims to solve prompt injection.

### 2.1 Reference target: Claude Code Desktop's browser features (added 2026-11-19)

Taken from the official docs, fetched 2026-11-19. *(unconfirmed)* means the item was not found in the docs. Anthropic ships three separate browser surfaces, and only the first is the same kind of thing as Genie's Browser tab:

| Surface | What it is | Profile |
|---|---|---|
| **A. Desktop Browser pane** | A browser built into the Claude Code Desktop app, mainly for previewing and verifying your dev server | Clean, isolated profile with no personal logins |
| **B. Claude in Chrome** | A Chrome extension that Claude Code drives (`claude --chrome`, `/chrome`) | The user's real Chrome profile and logins |
| **C. Computer use** | Whole-screen control of native apps (macOS CLI; Desktop on macOS and Windows) | n/a |

#### A. Desktop Browser pane ([desktop docs](https://code.claude.com/docs/en/desktop))

**UI**
- Open it with **Cmd+Shift+B** (macOS) or **Ctrl+Shift+B** (Windows), or with the **Browser** button in the session title bar.
- **Preview from chat:** in local sessions, clicking an HTML, PDF or video path in the chat opens it in the pane. HTML also works in SSH and WSL sessions.
- **Server card → Open:** when a dev server starts, its card in the chat has an **Open** button, which brings the pane back if it was closed.
- **Tabs:** docs and issue trackers can sit in tabs next to the running app.
- **Dev servers** menu in the pane header: start or stop each server, or **Stop all**.
- **⋮ menu**, with four items:
  - **Keep cookies**
  - **Clear browsing data**
  - **Open links in built-in browser**
  - **Auto-verify changes** (toggle)
- **Minimize** button in the header (Desktop v4.0.0+, local sessions only).
- **Outside links:**
  - Cmd/Ctrl-click opens a link in the default browser.
  - The first click on an outside link asks whether to use the Browser pane or the default browser.
- **Element select:** **Cmd+Shift+S** selects an element in the Browser. What happens next (notes, sending it to chat) is *(unconfirmed)*.

**What the agent does**
- Starts dev servers automatically after edits, for frontend and backend. It reads server logs and tests API endpoints.
- Takes screenshots, inspects the DOM, clicks elements, fills forms, and fixes what it finds.
- **Auto-verify** (on by default): after edits, Claude takes screenshots, checks for errors, and confirms the change works before it ends the response. It can be turned off per project (`"autoVerify": false`) or in the ⋮ menu.
- **`.claude/launch.json`** is created automatically and configures the preview servers. Fields:

  | Field | Meaning |
  |---|---|
  | `name` | Server name |
  | `runtimeExecutable`, `runtimeArgs` | Command that starts the server |
  | `program`, `args` | Script to run and its arguments |
  | `port` | Port the server listens on |
  | `autoPort` | `true` picks a free port; `false` fails if the port is taken |
  | `cwd`, `env` | Working directory and environment |
  | `url` | Attach to a server that is already running |
  | `autoVerify` | Turn auto-verify on or off for this project |

- *(unconfirmed for the pane)*: reading the console, inspecting network requests, running JS, recording GIFs.

**Permissions and safety**
- Local dev servers and project files need no approval, so auto-verify never prompts.
- **Outside sites need approval per site, including subdomains:**
  - **Allow once**
  - **Always allow** (saved on the device, revocable in Settings)
  - **Deny**
- **Safety classifiers** review write actions (click, type) in every permission mode and prompt when they flag something. Even Bypass mode still prompts on a classifier flag for outside sites.
- Outside Auto and Bypass modes, a domain allowlist check runs before navigating to a new site.
- Claude won't purchase, create accounts or get past CAPTCHAs without the user, even on approved sites.
- **Settings › Code › Browser tools** switches the Browser off entirely.
- Take-over and pause controls for the pane: *(unconfirmed, not documented)*.

**Profile and logins**
- A clean profile, separate from the user's own browser, with no saved logins or history.
- The user can sign in inside the pane, including popup flows such as Google OAuth.
- **Keep cookies** keeps cookies after quitting the app. In SSH sessions, cookies are always lost on quit.
- For acting as the user on sites where they're logged in, Anthropic points to Claude in Chrome (B).

**Admin settings (managed policy)**

| Setting | Effect | Since |
|---|---|---|
| `browserExternalPageTools: "disabled"` | Claude can't read or act on outside pages. The user can still browse | |
| `disableBrowserExternalNavigation: true` | Blocks all outside navigation. Localhost previews still work | Desktop v4.4212.0 |
| `disableRemotePreviewScripts: true` | Turns off scripts in SSH/WSL HTML previews | Desktop v5.1231.0 |

The pane also follows the Claude in Chrome allowlist and blocklist.

#### B. Claude in Chrome from Claude Code ([chrome docs](https://code.claude.com/docs/en/chrome), [support article](https://support.claude.com/en/articles/12012173-getting-started-with-claude-in-chrome))

**Setup and requirements**
- Start with `claude --chrome`, or use `/chrome`. It shows status, manages permissions, reconnects, picks the browser, and can enable it by default (which costs context, because the browser tool instructions are always loaded).
- Needs extension v1.0.36+ and `/login` with a direct Anthropic plan.
  - API-key or `setup-token` auth keeps it off.
  - Not available on Bedrock, Vertex or Foundry, or under Zero Data Retention. Off by default for HIPAA orgs.
- The extension must be signed in to the same claude.ai organisation as Claude Code.
- Works with Chrome, Edge and other Chromium browsers per the Code docs. The support article says Chrome only. This conflict is unresolved, so treat the Code docs as authoritative. Not supported on WSL.

**What the agent can do**
- Navigate, click, type, and read page and DOM state.
- Open, close and switch tabs. Claude's tabs are grouped into a **Chrome tab group tied to the session**.
  - `/clear` closes the group unless work is still running.
  - `/resume`, exit, or `/clear` while work is running closes the group only if it holds nothing but empty new tabs.
- Read console errors and messages. Network requests are listed in the support article but *(unconfirmed)* for Claude Code.
- Fill forms, extract data to CSV or files, and run workflows across several sites.
- **File upload** to a file input:
  - At most 10 MB per upload.
  - No hard-linked files.
  - Refuses credential-named files (`.env`, `.pem`, `.key`, `.ssh`).
  - Needs Read permission on the file.
- Screenshots are saved to disk and reported by path.
- **GIF recording** of browser interactions.
- Tool names seen in the docs: `tabs_context_mcp`, `browser_batch`. The full list (via `/mcp` › `claude-in-chrome` › View tools) is *(unconfirmed)*.
- *(unconfirmed)*: element picking, JS evaluation.
- Side-panel-only extras that may not apply to Claude Code: saved prompt shortcuts, scheduled tasks, image uploads, downloads, workflow recording.

**Control and permissions**
- Site permissions come from the extension's settings.
  - The prompt reads "Claude in Chrome wants to…" and offers to allow all actions on that site for the session.
  - In auto mode, if the classifier approves a call, the extension skips its own per-site check unless a permission rule denies the site.
- In plan mode, Claude asks before recording a GIF or opening a new tab.
- At login pages and CAPTCHAs, Claude pauses and asks the user to handle them.
- A JS `alert`/`confirm`/`prompt` blocks commands until the user dismisses it.
- No stop or take-over button is documented for Claude Code.
- Team/Enterprise admins can set allowlists and blocklists.

**Error states documented:** "Browser extension is not connected", "No tab available", "Receiving end does not exist" (the extension's service worker went idle; reconnect with `/chrome`), "No browsers are connected".

#### C. Computer use ([computer-use docs](https://code.claude.com/docs/en/computer-use))

- **Availability:**
  - CLI: research preview on macOS, Pro/Max/Ultra plans, interactive sessions only.
  - Desktop: macOS and Windows, behind a Settings toggle.
- Enable it per project with `/mcp` › `computer-use`. Needs the macOS Accessibility and Screen Recording permissions.
- **Per-app approval** each session, with extra warnings for terminals and IDEs ("equivalent to shell access"), Finder and System Settings.
- **Control tiers:**
  - Browsers and trading apps: view-only
  - Terminals and IDEs: click-only
  - Everything else: full control
- One session holds the lock at a time. Other apps are hidden while Claude works, and the terminal is left out of screenshots.
- **Esc** stops it from anywhere, and so does Ctrl+C in the terminal. A notification says "press Esc to stop".
- Tool preference order: MCP server, then Bash, then Claude in Chrome, then computer use last.

#### Genie today vs the Desktop Browser pane (A)

Based on reading [BrowserPanel.tsx](../codebase/apps/desktop/src/ui/panel/BrowserPanel.tsx) and the `genie-browser` crate on branch `fix/browser-panel`, 2026-11-19.

| Feature | Claude Desktop | Genie |
|---|---|---|
| Isolated profile | ✅ | ✅ (§4.1) |
| Live view of the page in a side panel | ✅ | ✅ Screencast to canvas |
| Tabs | ✅ | ✅ Tab strip, shown only with more than one tab |
| Pop out to its own window | — *(minimize only)* | ✅ |
| Take over / Hand back / Esc | *(not documented)* | ✅ |
| Stop | *(not documented)* | ✅ |
| Per-site approval: once / always / deny | ✅ | ✅ `browser.sites` (§4.2) |
| Step log | *(not documented)* | ✅ |
| File chooser during take-over, copy/paste | *(not documented)* | ✅ |
| Keyboard shortcut to open the pane | ✅ Cmd/Ctrl+Shift+B | ❌ |
| Start dev servers automatically, plus a `launch.json` equivalent | ✅ | ❌ |
| Dev servers menu (start, stop, stop all) | ✅ | ❌ |
| Auto-verify after edits | ✅ | ❌ |
| Open a local HTML/PDF/video path from chat in the pane | ✅ | ❌ |
| Server card in chat with an **Open** button | ✅ | ❌ |
| Outside link: pane or default browser (first-click prompt, Cmd-click) | ✅ | ❌ |
| ⋮ menu: Keep cookies, Clear browsing data, Open links here, Auto-verify | ✅ | ⚠️ `browser.data.clear` exists in the API but has no panel UI |
| Element select (Cmd+Shift+S) | ✅ | ❌ |
| Minimize the pane | ✅ | ❌ |
| Safety classifier on click/type | ✅ | ⚠️ Deterministic gates only; critic planned for v1.x (§4.4) |
| Admin policy: no outside-page tools, no outside navigation | ✅ | ⚠️ Planned (ticket 28) |
| Reading console and network, running JS | *(unconfirmed)* | ❌ Planned for the preview pane (§5) |

## 3. Architecture (v1)

```
Desktop (Tauri 2)                Daemon (Rust, D4)                              genie-browser (CEF helper)
 Browser button ──RPC──▶ browser service ──spawn/adopt──▶ window: [trusted toolbar view]
 Chat: step log, confirm cards   ├─ BrowserBackend trait                         [content views = tabs]
                                 │   ├─ CefBackend   ◀──CDP over authed pipe──▶ DevTools message API
                                 │   └─ SystemChromeBackend (fallback, --remote-debugging-pipe)
                                 ├─ snapshot/serialize (AX tree → text, refs)
                                 ├─ policy engine (sites, sensitive actions)  → ticket 13
                                 ├─ audit log                                  → ticket 12
                                 └─ browser_* tools                             → ticket 06
```

- The CDP layer is our own thin async client on tokio, reusing the **generated protocol types** from `chromiumoxide_cdp` / `chromey` and our own transport. `chromiumoxide` assumes a WebSocket endpoint; we need a pipe or IPC transport *(unverified whether it is pluggable)*. `playwright` for Rust (0.0.20, last updated 2022) is stale and wraps the Node driver, so we reject it. `headless_chrome` is synchronous and thread-based, so it is a poor fit for tokio.
- Requests are pull-based and keyed by `tab_id` + `action_id`, following OpenCode's pattern. If a result is lost, the action is reported as *unknown*, never retried.
- **CLI:** the same daemon tools work from the CLI and open the same helper window. A headless mode (no window) is an open question (§8).

## 4. Safety model (required for v1, D17)

### 4.1 Profile and data storage
- **Separate Chromium profile owned by Genie**, never the user's browser profile. Location: `<OS app-data>/Genie/browser/profiles/<profile-id>/` (CEF `root_cache_path`).
- **Two profile kinds:** *Persistent* (default; the user logs in to sites themselves during take-over, and cookies persist) and *Ephemeral* (per task, wiped on close; used for untrusted tasks or when policy requires it).
- **Encryption at rest** uses Chromium's OS key store integration: DPAPI on Windows, Keychain on macOS, libsecret/KWallet on Linux. If no Linux keyring exists, Chromium falls back to a weak "basic" store *(unverified for CEF)*. Warn the user and default to Ephemeral.
- **Disabled in v1:** password saving/autofill, payment-card autofill, sync, extensions, notifications, and geolocation/camera/microphone permission prompts (auto-deny; the user can enable them per site during take-over).
- **Downloads** go to a quarantine folder `<app-data>/Genie/browser/downloads/<session>/`. Moving a file to the user's folders requires confirmation. Files are never auto-opened.
- "Clear browsing data" and "Sign out of all sites" are in Settings (ticket 33). Managed policy (ticket 28) can force Ephemeral, disable the browser, or set site lists.

### 4.2 Per-site permissions
- Key: **registrable domain (eTLD+1)**, with an optional exact-origin override.
- **States:** `blocked` · `ask` (default) · `read` (navigate, read, screenshot) · `act` (click, type, submit). This follows the read-only vs read-write origin-set pattern.
- **First visit by the agent** to a new site prompts: *Allow for this chat · Always allow · Decline*. The user's own navigation during take-over never prompts.
- **Built-in default blocklist (category-based, user-editable):** banking and brokerage, payment processors' dashboards, government ID/tax, health portals, password managers, **AI provider consoles and API-key pages** (to protect Genie's own credentials), cloud admin consoles, and email/messaging send actions (allowed for `read`; `act` always confirms). Category data source: a curated list shipped with the component and updated with it *(to build)*.
- **Model-built URLs:** navigating to a *new* origin with a URL the model composed (as opposed to a link on the page or a user-supplied URL) needs `read` permission. It is **denied** if the query string or fragment carries more than a small amount of data (e.g. >256 chars) or matches content from another origin's page. This is a deterministic exfiltration guard, after the browser-vendor design.
- Hooks into ticket 13's rule engine: `browser:<domain>:<read|act>` rules, where deny wins.

### 4.3 Sensitive-action confirmation
These checks are **deterministic triggers, enforced in the daemon and independent of the model**. They pause the action and show a confirmation card in both the chat and the browser toolbar (screenshot of the target + plain-language summary + Approve / Take over / Deny):
- Submitting a form that contains `password`, `cc-*` / payment `autocomplete` fields, or fields inside a payment iframe.
- Clicking an element whose accessible name or nearby text matches purchase/commit verbs (buy, pay, place order, checkout, subscribe, send, post, publish, delete, transfer, confirm), in a localized list.
- **Any file upload.** The agent may only attach files the user explicitly provided to the chat (attachment IDs), never arbitrary paths. Implemented with CDP file-chooser interception plus `DOM.setFileInputFiles`.
- **Any download start**, and navigation to a URL in a blocked/sensitive category.
- Typing into a password, one-time-code or card field: **refused outright**, with a prompt to take over instead.
- The agent can also call `browser_handoff` to ask for take-over (login, 2FA, CAPTCHA).
"Always allow this action on this site" is offered only for non-payment categories.

### 4.4 Prompt-injection defences (layered; none is sufficient alone)
1. **Untrusted framing:** all page-derived text is returned inside a typed tool result marked `untrusted_web_content`, with a source origin, and never inside system or developer prompts. The agent's system prompt states that page text is data and that instructions in it must be ignored ("spotlighting").
2. **Hidden-content stripping:** the serializer drops `aria-hidden`, `display:none`/`visibility:hidden`, zero-size, off-screen, near-transparent and tiny-font text, as well as HTML comments and `<meta>` content. It reports how much was removed.
3. **Deterministic gates (§4.2, §4.3)** don't depend on the model resisting injection.
4. **Session taint:** once a session has read web content, **other tools** (shell, file write, MCP tools that send data, D15) need confirmation for the rest of that turn, even if they are normally auto-allowed. The exact rule is set in ticket 13.
5. **No secrets reach the browser:** the helper process has no access to daemon credentials; the model never sees stored passwords (autofill is off and password fields are redacted in snapshots).
6. **Scope:** the agent stays within the sites it has been allowed for this task. Cross-site navigation needs permission (§4.2).
7. **Step and time caps** per task (e.g. 50 actions or 15 minutes before checking in with the user).
8. **v1.x:** an independent "critic" check (a small model with no page content) on `act` steps, plus a page-level injection classifier. Both are provider-agnostic, so any configured model can be used.

### 4.5 Audit log
- Every action is recorded per session: timestamp, tool, arguments (typed text redacted for sensitive fields), tab, URL/origin, permission decision and who made it (rule, user or policy), confirmation outcome, and result status. A **thumbnail** after each `act` step is optional (setting, default on, kept for 30 days).
- Stored with the session (ticket 12), shown in the chat as a collapsible step log, and exportable. Managed policy can set retention or force logging.
- **No page content or URLs in telemetry** (ticket 35).

### 4.6 Take-over UX
- The toolbar shows a status pill: **"Genie is browsing"** (accent colour, animated) / **"Paused – you're in control"** / **"Waiting for your approval"**. A thin accent border around the content area appears while the agent drives. All of it is drawn in the trusted chrome view, so pages can't fake or hide it.
- Buttons: **Take over** (also `Esc`), **Hand back**, **Stop**.
- **Any user mouse or keyboard input in the content view pauses the agent automatically.** Agent tool calls then return `user_in_control` until the user clicks Hand back.
- On hand-back, the agent receives a fresh snapshot plus a note that "the user acted; state may have changed". Nothing the user typed into sensitive fields is reported.
- The chat composer shows the same status and buttons, so the user can stop the agent from either window.

## 5. Agent tool surface

| Tool | Semantics (one line) | Gate |
|---|---|---|
| `browser_open` | Open (or focus) the Genie Browser window for this session; returns `tab_id` | — |
| `browser_navigate` | Go to a URL in a tab (or `back` / `forward` / `reload`) | site `read`; URL rule |
| `browser_snapshot` | Accessibility-tree snapshot of the tab with element refs; `mode=interactive\|full`, optional `scope=ref` | `read` |
| `browser_read` | Main readable text of the page or a ref subtree as markdown, with a token cap | `read` |
| `browser_find` | Find elements by text/role/label; returns matching refs with context | `read` |
| `browser_screenshot` | PNG/JPEG of the viewport, full page or a ref; downscaled | `read`; vision models only |
| `browser_click` | Click an element by ref (button, modifiers, double-click) | `act`; sensitive check |
| `browser_type` | Type text into a ref (optionally clear first, optionally press Enter) | `act`; refused for sensitive fields |
| `browser_fill_form` | Set several fields in one call (text, checkbox, radio, select) | `act`; sensitive check |
| `browser_select` | Choose option(s) in a select/listbox by ref | `act` |
| `browser_press_key` | Send a key or chord to the focused element | `act` |
| `browser_hover` | Move the pointer over a ref (menus, tooltips) | `act` |
| `browser_scroll` | Scroll the page or a ref by direction/amount, or scroll a ref into view | `read` |
| `browser_wait` | Wait for text, a ref, navigation or network idle, with a timeout | — |
| `browser_tabs` | `list` / `new` / `switch` / `close` tabs | `read` |
| `browser_upload` | Attach chat-provided files (attachment IDs) to a file input | **always confirm** |
| `browser_downloads` | List downloads in this session and their status | — |
| `browser_handoff` | Ask the user to take over, with a reason (login, 2FA, CAPTCHA, ambiguous choice) | — |

Not in v1, behind a developer setting: `browser_evaluate` (arbitrary JS), network/console inspection and cookie access. These belong to the Code mode "preview" pane (inventory row, v2).

**Every action result** returns: status (`ok` / `blocked` / `needs_confirmation` / `user_in_control` / `unknown`), the new URL and title, and a **compact snapshot diff** (changed or new elements near the action), so a separate snapshot call is usually unnecessary.

### 5.1 Page → model input
- **Primary: the accessibility tree**, built from CDP `Accessibility.getFullAXTree` merged with `DOMSnapshot` for bounding boxes and visibility. It is serialized as an indented outline: `- button "Add to cart" [ref=e14]`, `- textbox "Email" [ref=e3] value=""`. Refs map to `backendNodeId` and are invalidated on navigation. Cross-origin iframes are included through CDP's auto-attach. This is the default because it is **model-agnostic** (D2: not every provider's model has vision) and cheap: published estimates are about 200–5,000 tokens per snapshot *(unverified, third-party)*.
- **`browser_read` (DOM → markdown)** for reading tasks, using readability-style main-content extraction.
- **Screenshots only when needed:** canvas or map apps, visual checks, or when the AX tree is poor (few named nodes). Only for models with the catalog flag `vision=true` (ticket 04). Default 1280×800 JPEG q70. On Anthropic's published formula (~w×h/750) that is about 1.4k tokens; other providers differ *(unverified per provider)*. Optional **set-of-marks** overlay (ref labels drawn on the image) in v1.x.
- **Token budget (proposed defaults):** snapshot ≤ 6k tokens (interactive-only, viewport plus one screen first, then "N more elements, use `scope` or `browser_find`"); read ≤ 8k; action diff ≤ 1.5k; screenshot ≤ 1 per step unless asked. **Context hygiene:** only the latest full snapshot per tab stays in context; older ones are replaced with a one-line stub (hooks into compaction, ticket 16). Target cost for a 20-step task: about 60–150k input tokens before caching *(estimate)*.

## 6. Recommendation and effort

**v1: A1 (CEF helper via cef-rs, downloaded on first use), daemon-driven over CDP on an authenticated pipe, with the safety model in §4 and the tools in §5.**
**Fallback: B (system Chrome/Edge, isolated profile, `--remote-debugging-pipe`, companion bar)** behind the same `BrowserBackend` trait. Use it if the CEF spike fails (for example, no Windows sandbox) or for users and organisations that decline the download.
**v2: D**, re-hosting the helper on Tauri-CEF once it is stable and sandboxed on Windows.

**Step 0 (2 weeks, before committing):** spike `genie-browser` on cef-rs on all three OSes. Exit criteria: (a) Chromium sandbox active on Windows, macOS and Linux (Ubuntu 24.04 with AppArmor); (b) notarised macOS bundle with helpers; (c) CDP round trip over our IPC; (d) custom toolbar view plus content view; (e) measure the real download and on-disk size. If (a) fails on Windows, ship B on Windows and A1 on macOS/Linux, or ship B everywhere, and tell the user.

| Work item | Engineer-weeks *(estimate)* |
|---|---|
| Spike (above) | 2 |
| `genie-browser` helper: window, trusted toolbar, tabs, take-over input routing, downloads, file chooser | 6–8 |
| Component packaging: signing on 3 OSes, notarisation, signed manifest, CDN, staleness rule, update UX | 3–4 |
| Daemon CDP layer + `BrowserBackend` (CEF + system Chrome) | 3–4 |
| Snapshot serializer, refs, diffs, read/markdown, screenshots | 3–4 |
| Tools + policy engine (sites, sensitive actions, URL rule, taint) + audit log | 4–5 |
| Desktop UI: Browser button, step log, confirmation cards, settings pages | 2–3 |
| Cross-OS QA, injection red-team suite (sandboxed malicious pages in CI using Chrome for Testing) | 3 |
| **Total** | **≈ 26–33 engineer-weeks** (about 2 engineers × 3.5–4 months) |
| Ongoing: CEF/Chromium security updates, site-category list, regressions | ≈ 0.25 FTE |
| B-only v1 (if chosen instead) | ≈ 14–18 engineer-weeks |

Running cost: CDN egress of about 150 MB per user per update. For example, 10k users × 2 updates per month is about 3 TB/month, which is low on any major CDN *(estimate)*.

## 7. Risks

| # | Risk | Likelihood / impact | Mitigation |
|---|---|---|---|
| 1 | **Security patch lag:** we ship Chromium, so every Chromium CVE is ours, and CEF builds may trail Chrome stable *(unverified lag)* | High / High | Automated CEF update pipeline; patch SLA (e.g. critical issues in ≤ 7 days); staleness kill switch; B fallback |
| 2 | **Prompt injection** leads to unwanted purchases, messages or data exfiltration | Medium / High | §4 layers; deterministic gates; taint; red-team suite; clear user-facing disclosure |
| 3 | **Sandbox gaps:** Windows sandbox via cef-rs unproven; Linux AppArmor userns; AppImage | Medium / High | Spike exit criterion; refuse to run unsandboxed on untrusted origins; B on affected platforms |
| 4 | **macOS signing/notarisation** of CEF helper bundles (inside-out, entitlements) breaks releases | Medium / Medium | Scripted signing in CI (ticket 27); verify with `spctl` in CI |
| 5 | **cef-rs maturity:** small maintainer team, API churn with each CEF major | Medium / Medium | Pin versions; isolate behind the helper boundary; the same bindings power Tauri 3, so effort is shared upstream |
| 6 | **Size and first-run friction** (+135–175 MB) | Certain / Low–Medium | Download on first use with progress; B as an instant option; an "offline" installer for enterprises |
| 7 | **Bot detection and CAPTCHAs** on automated sessions (CDP is detectable) | High / Medium | `browser_handoff`; no stealth or anti-detection features (ToS and ethics) |
| 8 | **No proprietary codecs or Widevine** in official CEF builds: some video and DRM sites fail | Certain / Low | Document; licensed codecs are a v2 decision |
| 9 | **AV/EDR** flags a downloaded Chromium component on Windows | Medium / Medium | Sign everything with the same certificate as the app; submit to AV vendors; install into a stable per-user path |
| 10 | **Site terms of service and liability** for automated actions | Medium / Medium | User-initiated tasks only; confirmations; ToS language (ticket 35) |
| 11 | **Licence compliance** for Chromium third-party notices | Low / Medium | Ship about:credits notices with the component; legal review (ticket 35) |

## 8. Open questions (need the user)

1. **Download on first use (recommended) or bundled in the installer?** Bundling adds about 135–175 MB to every download.
2. **Default profile:** persistent and logged-in (recommended, more useful) or ephemeral per task (safer)?
3. **Default site policy:** *ask per site* (recommended) or *allow all except the blocklist*?
4. **Patch SLA** for Chromium critical security fixes (proposal: ≤ 7 days) and the staleness cut-off (proposal: block untrusted browsing at 21 days behind).
5. **Is B acceptable as the shipped fallback**, given it can't show Genie's window chrome? And should B be offered to users even when A works?
6. **CLI:** should browser tools be available from the CLI in v1, and should there be a headless mode?
7. **Screenshot thumbnails in the audit log:** on by default (proposal) and kept for how long?
8. **`browser_evaluate` (arbitrary JS):** keep it out of Chat v1 (proposal) and move it to the Code mode preview pane in v2?
9. Who owns the CEF update pipeline long term (the ~0.25 FTE)?

Follow-ups for other tickets: **13** (browser rule syntax and session taint), **06** (tool registration), **12** (audit storage), **27** (component signing and CDN), **28** (policy keys: disable browser, site lists, force ephemeral), **33** (Browser settings page), **04** (`vision` capability flag), **16** (snapshot eviction), **35** (licence notices, ToS).

## Sources

- Chrome for Testing announcement ("purely for browser automation and testing", "only … trustworthy content", no auto-update) — https://developer.chrome.com/blog/chrome-for-testing
- Chrome for Testing repo, platforms and JSON endpoints — https://github.com/GoogleChromeLabs/chrome-for-testing
- CfT stable download URLs and sizes (HTTP Content-Length, fetched 2026-11-16) — https://googlechromelabs.github.io/chrome-for-testing/last-known-good-versions-with-downloads.json
- Chrome terms (cover Chrome executables) — https://www.google.com/intl/en/chrome/terms/
- Question on CfT licence (unanswered) — https://groups.google.com/a/chromium.org/g/chromium-discuss/c/KKJLGEzeeWU
- Chrome 136 remote-debugging switch change — https://developer.chrome.com/blog/remote-debugging-port
- CEF build index and distribution sizes (fetched 2026-11-16) — https://cef-builds.spotifycdn.com/index.json
- CEF BSD licence / third-party notices — https://third-party-mirror.googlesource.com/cef/+/8265caad76429c18c91b1cfabe571852a67e0e5f/src/tools/distrib/README.footer.txt
- cef-rs (Rust CEF bindings) — https://github.com/tauri-apps/cef-rs ; crate versions — https://crates.io/crates/cef
- Tauri CEF runtime (alpha, Windows unsandboxed, CDP via `WebviewCefExt`, bundle-size note) — https://v3.tauri.app/develop/cef/
- tauri-runtime-cef versions — https://crates.io/crates/tauri-runtime-cef/versions
- chromiumoxide — https://docs.rs/crate/chromiumoxide/0.9.1/source/README.md ; chromey — https://docs.rs/chromey ; headless_chrome — https://crates.io/crates/headless_chrome ; playwright (Rust) — https://crates.io/crates/playwright
- Agentic browser security architecture (alignment critic, origin sets, URL restriction, work log, takeover) — https://blog.google/security/architecting-security-for-agentic/?hl=en
- Assistant agent mode help (takeover, watch mode, confirmations) — https://help.openai.com/en/articles/11752874-chatgpt-agent
- Site-permission prompt pattern for a browser-extension agent (third-party guide) — https://www.verdent.ai/guides/codex-chrome-prompt-injection-protection
- Browser-extension agent safeguards (secondary coverage) — https://moneyoval.com/article/anthropic-adds-guardrails-against-prompt-injection-in-chrome-sfm1681cuwkxa3pl3zm8xgz3
- Claude Code Desktop Browser pane, `launch.json`, auto-verify, site approvals, managed settings (fetched 2026-11-19) — https://code.claude.com/docs/en/desktop
- Claude in Chrome from Claude Code (fetched 2026-11-19) — https://code.claude.com/docs/en/chrome ; support article — https://support.claude.com/en/articles/12012173-getting-started-with-claude-in-chrome
- Claude Code computer use (fetched 2026-11-19) — https://code.claude.com/docs/en/computer-use
- In-app browser with isolated profile in a desktop coding app (secondary) — https://www.webpronews.com/anthropic-hands-claude-code-its-own-browser-the-end-of-tab-hell-for-developers/
- IDE integrated browser driven via CDP (secondary listing) — https://mcpservers.org/agent-skills/microsoft/integrated-browser
- Browser-level deterministic guardrails (research) — https://arxiv.org/pdf/2512.12594v1
- Playwright MCP accessibility snapshots — https://playwright.dev/mcp/snapshots ; token-use analysis — https://itnext.io/why-does-playwright-mcp-use-so-many-tokens-da56b6dbd2db
- agent-browser snapshots and refs — https://agent-browser.dev/snapshots
- Apple notarisation and hardened runtime — https://developer.apple.com/documentation/security/notarizing-macos-software-before-distribution ; CEF-helper entitlements example — https://github.com/pleaseai/openhwp/pull/20
- Internal: [desktop-shell.md](desktop-shell.md), KB [18-app-ui-and-gui-plugins.md](../knowledge/opencode/18-app-ui-and-gui-plugins.md) (OpenCode browser plugin), pick-list item 40 in [opencode-pick-list.md](opencode-pick-list.md)
