# Desktop shell: Tauri 2 vs Electron (ticket 30)

> Research for [tickets/30-desktop-shell.md](../tickets/30-desktop-shell.md) · decision D1 in [WAYFINDER.md](../WAYFINDER.md) · 2026-11-16
> Anything marked *(unverified)* was not checked against a primary source in this session.

## TL;DR

- **Go on D1 (Tauri 2), with conditions.** No v1 feature (chat, Code mode with diffs and terminal, settings, MCP connectors, artifacts preview) needs Electron. Genie's core is a **Rust daemon**, so Electron's main advantage for OpenCode (running a TypeScript server inside Electron's Node) does not apply to Genie.
- **Why OpenCode switched, in its own words:** (1) **WebKit rendering.** Tauri uses WebKit on macOS and Linux, which rendered their app slower than Chromium and had style inconsistencies. (2) **The bundled CLI sidecar** hurt startup time and "occasionally just failed", notably on Windows. (3) They were moving **Bun → Node** anyway, so running the TS server inside Electron's Node process was attractive. They said the choice was "nothing to do with either Tauri or Electron being innately 'better' or 'faster'" and that Tauri is "still a great fit" for apps whose core is native. Sources below.
- **Risk that applies to Genie:** reason (1). Genie's UI is the same kind as OpenCode's: a long, streaming markdown timeline with code, diffs, Mermaid and KaTeX. WKWebView and especially **WebKitGTK on Linux** are where problems show up. Reason (2) applies partly: Genie also runs a separate daemon process, but it is a small Rust binary rather than a JIT runtime.
- **Feature that is blocked on Tauri 2:** an **agent-controlled browser embedded in the app window** on macOS and Linux. WKWebView and WebKitGTK expose no CDP. Mitigation: the daemon drives a **separate Chromium over CDP** (Chrome for Testing or the system Chrome/Edge), not a webview. This is v2 or later scope.
- **New escape hatch (2026):** Tauri 3.0 alpha ships an official **CEF (Chromium) runtime** (`tauri-runtime-cef`) with native CDP access, on Windows, macOS and Linux. It is alpha and not available in Tauri 2. It gives a **Chromium path that keeps the Rust/Tauri code** if WebKit fails, so a fallback need not mean Electron.
- **Conditions for the go:** (a) the streaming-chat prototype (ticket 37) includes a **rendering performance gate on all three engines**, Linux on NVIDIA included; (b) the React app stays **shell-agnostic** behind one `Platform` adapter; (c) Linux ships as a lower support tier until the gate passes; (d) re-check at Tauri 3 stable.

## 1. Why OpenCode moved from Tauri to Electron

**Primary source:** a post by an OpenCode desktop engineer, "Moving OpenCode Desktop to Electron" (DEV Community, 19 Apr 2026), plus the announcement on X (17 Apr 2026). Quotes are from the post as fetched on 2026-11-16:

| Reason | Quote / paraphrase | Applies to Genie? |
|---|---|---|
| WebKit rendering | "Tauri uses WebKit on macOS and Linux, which not only has worse performance than Chromium when rendering our app", plus minor **style inconsistencies** that made a consistent cross-platform experience harder | **Yes.** Same UI shape (streaming markdown timeline). This is the main risk |
| Bundled CLI as a separate process | The bundled CLI that ran `opencode serve` "impacted startup time, and occasionally just failed" (paraphrase from the post: notably on Windows) | **Partly.** Genie also has a separate daemon (D4). It is a native Rust binary, so it starts faster than a 200 MB Bun binary. Antivirus scanning and spawn failures on Windows still apply |
| Bun → Node | "our desire to move from Bun to Node (for other reasons)" made "running our server code within Electron's built-in Node process … quite appealing" | **No.** Genie's core is Rust. In Electron it would still be a separate process (or a napi addon) |
| Rust gives no speed-up for them | Tauri's Rust side does not help "unless the core logic is also in Rust"; "the server needs to run in a Node/Bun process regardless" | **Inverted.** Genie's core *is* Rust, which is the case the author says suits Tauri |
| Bundle size | Electron bundles are larger: "Yeah, it's a trade-off we're willing to make." | Trade-off, not a blocker |
| Framing | "has nothing to do with either Tauri or Electron being innately 'better' or 'faster'". "If you're building an app with a simpler UI that demands native performance or easy access to system APIs, I think Tauri is still a great fit." | — |

**Supporting evidence from the repo** (now at `github.com/anomalyco/opencode`):
- `.github/release/sign-desktop-macos.sh:5` mentions the "retired Tauri updater feed" (KB ch. 17).
- Issue #58268 (Oct 2026): "Electron version still ~1GB memory" at idle on Windows. About 665 MB is Electron processes and about 390 MB is the CLI, with "no improvement over Tauri wrapper". It was closed as not planned. **So the move did not lower memory use.**
- A search of the issue tracker found **no PR or issue that records the switch decision itself**, and no Tauri-era WebKitGTK issue in the first result pages. The rationale exists only in the blog post and on X.
- Third-party commentary repeats the post: the sidecar caused startup overhead, IPC latency and stability problems, and "Tauri itself is good" *(secondary source, X post)*.
- The browser plugin's use of CDP (`webContents.debugger`, `WebContentsView`) is **not** given as a reason in the post. The KB's guess that CDP drove the switch is **not supported by the sources**. The browser plugin appears to have come after the move *(inferred)*.

## 2. Tauri 2 vs Electron for Genie

Versions as of 2026-11-16: Tauri core **2.12.3** (10 Oct 2026). Tauri **3.0.0-alpha** is in progress with a CEF runtime (alpha.5 seen on crates.io; alpha.6 *(unverified)*). OpenCode is on Electron 44.x and has an open PR for Electron 45.

| Area | Tauri 2 (wry: WebView2 / WKWebView / WebKitGTK) | Electron (bundled Chromium + Node) | Weight for Genie |
|---|---|---|---|
| **Rendering consistency** | Three engines. WebView2 is evergreen Chromium. WKWebView follows the macOS/Safari version. WebKitGTK follows the distro version. Expect CSS and layout differences and per-engine QA | One pinned Chromium on every OS | High. This is OpenCode's reason #1 |
| **Long streaming markdown** | Fine on WebView2. WebKit renders slower than Chromium for this kind of app (OpenCode's experience). One user in a Tauri discussion reported about 40 fps on Ubuntu WebKitGTK even with GPU acceleration, while an Electron build ran much faster | Best-in-class; the same engine as Chrome DevTools profiling | High. Mitigate with a virtualized timeline, incremental markdown parsing in a Web Worker, and batched DOM commits (OpenCode does all three) |
| **Code highlighting** | Shiki/Prism work in WebKit. Shiki's JS regex engine needs modern RegExp features; check WKWebView on the oldest supported macOS *(unverified)* | No issue | Low–Medium |
| **Mermaid / KaTeX** | Both run in WebKit. Mermaid `htmlLabels` uses SVG `foreignObject`, which has known Safari quirks *(unverified)*. KaTeX fonts must be bundled | No issue | Low–Medium |
| **Linux** | WebKitGTK is the weak point. Official Tauri docs list NVIDIA blank or garbled windows (workaround `WEBKIT_DISABLE_DMABUF_RENDERER=1`). On Wayland + NVIDIA the app may not start (`__NV_DISABLE_EXPLICIT_SYNC=1`). WebKit bug 264387 reports scrolling at about 2 fps after a regression. A Tauri maintainer wrote: "if you need good linux support now/soon i can't 100% recommend tauri (for Linux)" and that WebKitGTK was "getting worse/more unstable each release" | Chromium on Linux is mature. Packaging quirks remain (AppImage naming, RPM build-id clashes, a crashpad SIGBUS issue in OpenCode's tracker) | High for Linux users. Mitigate with a Linux support tier and an env-var workaround on startup |
| **Sidecar / daemon process** | `bundle.externalBin` with `<name>-<target-triple>`. Spawned through the shell plugin under a capability ACL. **Tauri does not reliably kill sidecars on exit.** Cmd+Q and force-quit can leave orphans, so Genie must manage this (Windows Job Object, process group, or a daemon that exits itself on idle) | `utilityProcess` / `child_process` with the same orphan problems. OpenCode needed about 1K lines (`Service.ensure`, stage per version, probe) | Medium, and the same on both. Genie's D4 daemon is meant to be **shared and long-lived** anyway, so use a registration file and health probe (OpenCode's pattern), not "child dies with the app" |
| **Daemon auth from the webview** | No `webRequest` header injection. Options: proxy all daemon calls through Rust commands plus a `Channel` for the event stream (the token stays in Rust), or a custom URI-scheme proxy | `session.webRequest.onBeforeSendHeaders` injects auth (OpenCode does this) | Medium. Decide in ticket 02. A Rust-side proxy is the clean option |
| **Auto-update** | `tauri-plugin-updater`. Signature checking with Tauri's own keypair is **mandatory** ("cannot be disabled"). Static JSON or a dynamic endpoint. NSIS/MSI/AppImage/`.app.tar.gz`. Channels through endpoint URLs. **No delta updates** *(unverified)* | `electron-updater`: channels, staged rollouts, differential (blockmap) downloads. Squirrel.Mac needs a signed app | Medium. Tauri covers v1 needs |
| **Code signing** | Windows: built-in signtool, or `signCommand` for **Azure Artifact (Trusted) Signing**, Key Vault and others. Cross-OS signing only through a custom command. macOS: signing and notarisation are built in. **Sidecar binaries must be signed too**, which the docs do not cover clearly *(unverified)* | electron-builder handles both. OpenCode signs inside-out in a separate release job | Equal; ticket 27 |
| **Bundle size** | Installer roughly 5–15 MB plus the Genie daemon binary *(unverified, typical)* | About 80–150 MB installer *(unverified, typical)*. OpenCode accepted this | Low (marketing only) |
| **Memory** | Lower on macOS and Linux (shared system engine). On Windows, WebView2 is Chromium, so the gain is smaller *(unverified)* | Higher. OpenCode's Electron build idles at about 1 GB including its server (#58268) | Low–Medium |
| **Multi-window, tray, global shortcuts, deep links** | All first-party: multi-window in core, tray in core, `global-shortcut`, `deep-link`, `single-instance`, `notification`, `window-state` plugins. Linux deep links need `.desktop` registration (AppImage caveats) *(unverified)*. Global shortcuts on Wayland are limited *(unverified)* | All built in and mature | Low. Both are fine |
| **Agent-controlled browser (CDP)** | **Windows:** WebView2 accepts `--remote-debugging-port` (`additionalBrowserArgs` / `WEBVIEW2_ADDITIONAL_BROWSER_ARGUMENTS`), so Playwright `connectOverCDP` works. **macOS/Linux: no CDP** in WKWebView or WebKitGTK. Multi-webview (a second webview in one window) is behind `unstable`. **Tauri 3 CEF runtime**: native CDP (`send_dev_tools_message`, `on_dev_tools_protocol`), alpha only | `WebContentsView` + `webContents.debugger` gives full CDP inside the app on all OSes. This is what OpenCode's browser plugin uses | **High for a v2 in-window browser agent. Not needed for v1** |
| **Terminal / PTY panes** | PTY in Rust (`portable-pty`, in the daemon); xterm.js in the webview. The xterm.js WebGL renderer on WebKitGTK may need a canvas/DOM fallback *(unverified)* | `node-pty` native module (rebuild per Electron ABI) + xterm.js | Low. Genie's PTY lives in the daemon either way |
| **File watching** | In the daemon (`notify` crate). `tauri-plugin-fs` has an optional `watch` feature | `chokidar`/`fs.watch` in main | Low. Daemon-side on both |
| **Ecosystem maturity** | 2.x stable since Oct 2024. Smaller plugin ecosystem. Single-maintainer areas (Linux) | Over 10 years old, used by VS Code, Slack, Claude Desktop and others *(unverified for some)*. Ships a Chromium security update about every 8 weeks | Medium |
| **Security model** | Rust backend; no Node in the renderer. Capability/permission ACL for each command and plugin; isolation pattern; CSP. Engine security patches come from the OS (WebView2 evergreen; macOS updates; **distro lag for WebKitGTK**). CEF runtime: **no Chromium sandbox on Windows** (alpha) | Strong if configured (contextIsolation, sandbox, fuses), but a misconfiguration gives Node to the renderer. Genie must ship each Chromium update itself | Medium. Tauri's defaults are safer. Electron controls engine patching |
| **Code reuse with Genie's Rust core** | Direct: Tauri commands can link Genie's protocol/client crates | Needs napi-rs or a pure HTTP client in TS. Two runtimes (Node + Rust) | Medium in Tauri's favour |

## 3. Claude Desktop-like features: risk on Tauri 2

| Feature | Release | Rating | Notes |
|---|---|---|---|
| Chat: streaming markdown, code blocks, copy, attachments | v1 | **Risky (perf), not blocked** | Fine on WebView2. WebKit perf must be proven by the ticket 37 gate. Use virtualization, a worker parser and rAF-batched commits |
| Mermaid / KaTeX / syntax highlighting in chat | v1 | **Fine** (QA per engine) | Test Mermaid `htmlLabels` on WKWebView *(unverified)* |
| Settings UI, providers, credentials | v1 | **Fine** | Secrets stay in Rust/daemon (keyring); never in the webview |
| MCP connectors (stdio/HTTP, OAuth) | v1 | **Fine** | Runs in the daemon. OAuth uses the system browser plus a loopback redirect or `deep-link`. On Linux, check deep-link registration for each package format |
| Multi-window, tray, global hotkey ("quick ask"), notifications | v1 | **Fine** | First-party plugins. Wayland global shortcuts are limited *(unverified)* |
| Auto-update, channels, signing | v1 | **Fine** | `tauri-plugin-updater` + Azure Artifact Signing + notarisation. Sign the daemon binary as well |
| Daemon spawn/adopt from the app | v1 | **Risky (engineering)** | Same on both shells. Orphan handling, Windows antivirus, version handshake. Copy OpenCode's registration + probe contract |
| Code mode: diff viewer, file tree, review | v1/v1.x | **Fine** | DOM-heavy diff views need virtualization on WebKit |
| Code mode: terminal panes (xterm.js) | v1.x | **Fine** | PTY in the daemon. Keep a WebGL→canvas renderer fallback for WebKitGTK *(unverified)* |
| Artifacts preview (HTML/SVG/React in a sandboxed iframe) | v1.x | **Fine, with care** | `iframe sandbox` without `allow-same-origin`, a separate custom scheme, CSP, and **no Tauri IPC exposed to that origin** (check the capability `remote` scope). Engine differences show up in user-generated HTML |
| PDF preview | v1.x | **Risky** | Chromium has a built-in PDF viewer. WebKitGTK does not render PDFs inline *(unverified)*. Bundle pdf.js |
| Voice input (mic) | later | **Risky** | `getUserMedia` permission handling differs across WKWebView and WebKitGTK *(unverified)* |
| Agent-controlled browser **in a separate window** (daemon + external Chromium via CDP) | v2 | **Fine** | Does not depend on the shell. Works the same under Electron |
| Agent-controlled browser **embedded in the app window** | v2 | **Blocked on macOS/Linux (Tauri 2)**; possible on Windows (WebView2 CDP) | Needs Tauri 3 CEF, Electron, or the screencast workaround (below) |
| Computer use (screenshots + mouse/keyboard on the OS) | v2+ | **Fine** | Native crates in Rust (screen capture and input injection). Needs OS permissions (macOS Screen Recording/Accessibility) on both shells |

**Verdict: nothing in v1 forces Electron.** The only hard block is an *in-window* agent browser on macOS and Linux, and that has mitigations that do not need Electron.

## 4. Recommendation: GO on D1 (Tauri 2), with conditions

1. **Performance gate in ticket 37.** Before Chat v1 commits to the UI, the prototype must pass on **WKWebView (oldest supported macOS), WebKitGTK (Ubuntu LTS, NVIDIA + Intel/AMD, X11 + Wayland) and WebView2**. Suggested targets, to be confirmed by the user: a 50k-token streamed answer with 30 code blocks, Mermaid and KaTeX; a timeline of 2,000 messages; p95 frame time under 16 ms while streaming at 100 tokens/s; smooth scrolling. **If macOS fails the gate, reopen D1.** If only Linux fails, take the Linux mitigations below.
2. **Shell-agnostic frontend.** All host calls go through one `Platform` interface (OpenCode's pattern, KB ch. 18). No `@tauri-apps/*` imports outside `platform/tauri`. All daemon traffic uses a transport the shell can swap. This keeps a later move to Tauri-CEF or Electron to weeks of work *(estimate)*.
3. **No webview CDP dependency in the roadmap.** The agent browser (v2) is designed as **daemon → external Chromium over CDP**. Any in-app pane is a viewer.
4. **Linux is a lower support tier for v1** (for example "beta") until the gate passes. Set `WEBKIT_DISABLE_DMABUF_RENDERER=1` when NVIDIA is detected, and document `__NV_DISABLE_EXPLICIT_SYNC=1`.
5. **Re-evaluate at Tauri 3 stable.** If the CEF runtime is stable and the Windows sandbox gap is closed, consider CEF on Linux only (or everywhere) for rendering parity and native CDP.

**When to switch (no-go triggers):** macOS WebKit fails the perf gate after the mitigations; or the user makes an **in-window, agent-visible browser on macOS** a v1/v2 must-have *and* Tauri 3 CEF is not stable by then; or a plugin model needs Node in the host (KB ch. 18 already advises against this).

## 5. Mitigations

| Risk | Mitigation |
|---|---|
| WebKit rendering performance | Virtualized timeline; markdown parsed incrementally in a Web Worker; batch token commits with rAF (16 ms flush, as OpenCode does); lazy Mermaid/KaTeX rendering only once a block is complete; `content-visibility:auto`; perf benchmarks in CI on all three engines |
| Style inconsistencies | A design system that avoids cutting-edge CSS; visual regression tests on WebKit (Playwright WebKit as a proxy) and WebView2 |
| Linux WebKitGTK | Support tier; env-var workarounds; minimum WebKitGTK version; later **Tauri 3 + CEF on Linux only** (other projects already plan this) |
| Agent browser on macOS/Linux | **CDP-capable Chromium sidecar used only for browser automation**: the daemon launches Chrome for Testing or the user's Chrome/Edge (`--remote-debugging-port`, isolated profile) and drives it with a Rust CDP client. Show it as its own window, or stream `Page.startScreencast` frames into an app pane and forward input. Windows can optionally use WebView2 CDP |
| Daemon lifecycle and orphans | Registration file + health probe + version handshake (OpenCode's `Service.ensure` contract); Windows Job Object or a daemon idle timeout; stage the daemon binary per version outside the signed bundle |
| Daemon auth from the webview | Rust-side proxy: webview → Tauri command/Channel → daemon. The token never reaches JS (ticket 02) |
| Updater gaps | Tauri updater with endpoint-based channels; restart-into-staged UX; keep a manual download fallback |
| Tauri ecosystem gaps | Budget for small custom Rust plugins (power-save blocker, crash dumps through Sentry/minidump) |
| A full switch becomes necessary | Platform seam + Rust client crate → swap to the Tauri-CEF runtime (keeps the Rust code) before considering Electron |

## 6. Open questions

1. **Linux support level for v1:** full, beta, or deferred? This is the biggest factor in WebKitGTK risk. *(needs user)*
2. **Is an in-window, agent-visible browser a product must-have (v2)?** If yes on macOS, plan for Tauri 3 CEF or Electron. If a separate Chromium window is acceptable, Tauri is fine. *(needs user)*
3. **Daemon topology** (ticket 01/02): a shared per-user daemon or one owned by the app? Note that KB ch. 17/18 "Genie takeaways" assume an *embedded engine*, which conflicts with D4 (client/server daemon). Those takeaways should be read with D4 in mind.
4. The minimum macOS version sets the WKWebView feature level (Shiki regex engine, CSS features). *(ticket 27/31)*
5. Delta updates: does the Tauri updater support them? *(unverified; matters only for a large daemon binary)*
6. Tauri 3 stable timeline: no date found.

## Sources

- OpenCode post: "Moving OpenCode Desktop to Electron" — https://dev.to/brendonovich/moving-opencode-desktop-to-electron-4hip
- Announcement (X, 17 Apr 2026) — https://x.com/brendonovich/status/2045114479244165151
- Secondary summary (X) — https://x.com/vikingmute/status/2045503577377099843
- Secondary coverage of OpenCode 2.0 — https://www.kucoin.com/news/flash/opencode-2-0-major-rewrite-api-overhaul-node-migration-and-electron-desktop-shift
- OpenCode issue #58268 (Electron ~1 GB memory) — https://github.com/anomalyco/opencode/issues/58268
- OpenCode issue search (electron) — https://api.github.com/search/issues?q=repo:anomalyco/opencode+electron+in:title
- KB: [17-desktop-shell.md](../knowledge/opencode/17-desktop-shell.md), [18-app-ui-and-gui-plugins.md](../knowledge/opencode/18-app-ui-and-gui-plugins.md)
- Tauri core release log — https://v2.tauri.app/release/tauri/all-versions/
- Tauri CEF runtime (3.0 alpha) — https://v3.tauri.app/develop/cef/
- tauri-runtime-cef versions — https://crates.io/crates/tauri-runtime-cef/versions
- CEF Windows sandbox PR — https://github.com/tauri-apps/tauri/pull/16431
- Tauri discussion on WebKit instability — https://github.com/orgs/tauri-apps/discussions/8524
- Tauri Linux graphics issues — https://v2.tauri.app/develop/debug/linux-graphics/
- WebKit bug 264387 (slow scrolling) — https://bugs.webkit.org/show_bug.cgi?id=264387
- NVIDIA quirk crate — https://docs.rs/crate/webkit2gtk-nvidia-quirk/latest/source/src/lib.rs
- Tauri sidecar docs — https://v2.tauri.app/develop/sidecar/
- Sidecar orphan issue — https://github.com/tauri-apps/tauri/issues/1626 , https://www.github.com/tauri-apps/tauri/pull/1896
- Tauri updater — https://v2.tauri.app/plugin/updater/
- Tauri Windows signing — https://v2.tauri.app/distribute/sign/windows/
- Playwright WebView2 CDP — https://playwright.dev/docs/webview2
- Example: Linux-only CEF plan in another project — https://github.com/arcforgelabs/openclaw/issues/21
