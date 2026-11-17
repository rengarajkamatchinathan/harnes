# 40 — Built-in browser (agent-driven)

type: wayfinder:research · status: closed · claimed-by: research-agent (2026-11-16) · blocked-by: —

## Question
D16 needs a Genie browser window the agent can drive on Windows, macOS and Linux. How do we build it on Tauri 2 (D8)?

## Background
Agent control normally goes through CDP (Chrome DevTools Protocol). WebView2 on Windows has it; WKWebView (macOS) and WebKitGTK (Linux) don't (see [research/desktop-shell.md](../research/desktop-shell.md)).

## Options to evaluate
- **A. Bundled Chromium window driven over CDP by the daemon.** CEF, or Chrome for Testing downloaded on first use, styled as a Genie window. Same behaviour on all three OSes. Costs: download/bundle size, updates, signing.
- **B. The user's installed Chrome or Edge over CDP,** with A as the fallback. Small download, but behaviour varies by machine and it may clash with the user's profile.
- **C. Native webview window plus injected-JS automation** (DOM actions via scripts, plus the platform snapshot APIs). No extra download. Weaker: can't handle cross-origin iframes, native dialogs, file uploads, etc.
- **D. Tauri 3 CEF runtime** for the browser window only, once stable.

## Also decide
- **Release:** v1 or v2? (Chat ships first, D6.)
- **Isolation:** a separate browser profile; cookie and session persistence; does the user log in to sites inside it?
- **Safety:** permission prompts per domain or action; blocklist (banking, etc.); prompt-injection defences for page content; the user takes over before sensitive steps (payments, sending messages).
- **Agent tool surface:** navigate, click, type, scroll, read (accessibility tree / DOM to text), screenshot, tabs.
- **UX:** a Browser tab or button in the sidebar; window chrome; an indicator that the agent is in control.

## Output
`research/browser.md` with a recommendation, a cost/size table and a risk list.

## Resolution
Report: [research/browser.md](../research/browser.md) (2026-11-16, pending user review; WAYFINDER.md not yet updated).
- **v1 = Option A on CEF:** a separate signed "Genie Browser" helper built with `cef-rs`, downloaded on first use (+135–175 MB compressed), updated through its own signed component channel. The daemon drives it over CDP via an authenticated pipe (no TCP debug port). The trusted Genie toolbar shows the agent-in-control state and Take over / Hand back / Stop buttons.
- **Fallback = B:** the system Chrome/Edge with an isolated `--user-data-dir` over `--remote-debugging-pipe`, behind the same `BrowserBackend` trait. **Later = D:** re-host on Tauri-CEF once it is stable and sandboxed on Windows. **Rejected:** C (no cross-origin iframes, dialogs or uploads on macOS/Linux) and Chrome for Testing as the shipped engine (testing-only intent, no auto-update, unclear redistribution).
- **Gate:** a 2-week spike must show the Chromium sandbox working on all three OSes (Windows via cef-rs is unproven), a notarised macOS bundle, and CDP over IPC.
- **Safety (v1):** separate Genie profile (persistent or ephemeral), per-site `blocked/ask/read/act` permissions with a category blocklist, deterministic confirmations (payments, sends, uploads, downloads; no agent typing into password fields), untrusted-content framing, hidden-text stripping, a URL exfiltration guard, session taint for other tools, an audit log, and auto-pause on any user input.
- **Tools:** 18 `browser_*` tools. The model input is an accessibility-tree snapshot with refs (≤ 6k tokens) plus markdown read; screenshots only for vision models.
- **Effort:** ≈ 26–33 engineer-weeks, plus ≈ 0.25 FTE ongoing for Chromium security updates. Open questions for the user are in §8 of the report (download vs bundle, default profile, site policy, patch SLA, B as fallback, CLI/headless).

### User decision (2026-11-16) — overrides the research recommendation for v1
- **v1 uses option B only:** Genie drives the user's installed **Chrome or Edge** over CDP, with a **separate, persistent Genie profile**. No bundled engine. Chrome/Edge keep themselves patched, so Genie doesn't take on Chromium security updates in v1.
- If no supported browser is installed (more likely on macOS/Linux), Genie explains this and links to install Chrome/Edge/Chromium.
- **Agent-control UI lives in Genie's main window** (an "Agent is using the browser" banner with Take over / Hand back / Stop), because B can't add Genie chrome to the browser window.
- **New sites:** ask per site (allow once / always), plus an admin allowlist/blocklist (→ 28). Sensitive actions (payments, sending, uploads, downloads) always need user confirmation.
- **v2:** our own CEF "Genie Browser" helper (option A), **downloaded on first use**, behind the same backend interface. Then Tauri 3's CEF runtime (D) once it is stable.
- Deferred to spec: browser tools in the CLI, headless mode, and the profile location/encryption.

### Amendment (D56, 2026-11-17)
- The browser is shown **inside Genie's window** as a Browser tab (Chat and Code). Chrome/Edge (Genie profile) runs in the background; the daemon streams it with **CDP `Page.startScreencast`** frames to the tab and forwards **Take over** input with `Input.dispatchMouseEvent` / `Input.dispatchKeyEvent`. A pop-out opens the real window. Safety rules (per-site asks, sensitive-action confirmations, profile isolation) are unchanged. Risks to check in the spec: frame rate/latency, IME/clipboard/file pickers during take over, multi-tab handling.
