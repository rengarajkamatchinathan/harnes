# 30 — Desktop shell: confirm Tauri

type: wayfinder:research · status: closed · claimed-by: research-agent (2026-11-16) · blocked-by: —

## Question
OpenCode moved from Tauri to Electron (reason unrecorded). Does any v1 feature need Electron?

## To decide
- WebView differences: WebView2 / WKWebView / WebKitGTK rendering and performance
- Agent-driven browser needs CDP (not available in WKWebView)
- Sidecar daemon packaging
- bundle size
- Output: research/desktop-shell.md with a go/no-go on D1

## References
- OpenCode ch.17: [17-desktop-shell.md](../knowledge/opencode/17-desktop-shell.md) — read TL;DR, Architecture, Genie takeaways
- OpenCode ch.18: [18-app-ui-and-gui-plugins.md](../knowledge/opencode/18-app-ui-and-gui-plugins.md) — read TL;DR, Architecture, Genie takeaways

## Resolution
**GO on D1 (Tauri 2), conditional.** Full report: [research/desktop-shell.md](../research/desktop-shell.md).
- No v1 feature (chat, Code mode diffs/terminal, settings, MCP, artifacts) needs Electron. Genie's core is Rust, so OpenCode's Node-in-main advantage does not apply.
- OpenCode's stated reasons (blog, Apr 2026): WebKit rendering was slower and less consistent than Chromium on macOS/Linux; the bundled CLI sidecar hurt startup and sometimes failed; they wanted the TS server in Electron's Node. CDP was not given as a reason.
- Main risk: WebKit (WKWebView, especially WebKitGTK) performance on the long streaming-markdown timeline. Blocked: an in-window agent browser on macOS/Linux (no CDP).
- Conditions: ticket 37 adds a rendering performance gate on all three engines; the frontend stays shell-agnostic behind a `Platform` adapter; Linux ships as a lower support tier for v1; re-check at Tauri 3 stable.
- Mitigations: the daemon drives a separate Chromium over CDP for browser automation; Tauri 3 CEF runtime (alpha) is the Chromium fallback that keeps the Rust code, before Electron.
- Needs user: Linux support tier for v1; whether an in-window agent browser on macOS is a must-have.
- WAYFINDER.md line not added (left for the reviewer).
