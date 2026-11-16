# 37 — Prototype: Tauri + React streaming chat on the daemon

type: wayfinder:prototype · status: blocked (env: no MSVC Build Tools on Windows) · claimed-by: prototype-agent (2026-11-16) · blocked-by: 02,03,30

## Question
Prove token streaming from the daemon into React, with incremental markdown and code highlighting, on Windows, macOS and Linux.

## To decide
- Smooth streaming at long lengths
- Sidecar start, shutdown and crash recovery
- Cold start and bundle size
- The prototype code is thrown away: findings go in the resolution

## References
- OpenCode ch.15: [15-tui-and-rendering.md](../knowledge/opencode/15-tui-and-rendering.md) — read TL;DR, Architecture, Genie takeaways
- OpenCode ch.17: [17-desktop-shell.md](../knowledge/opencode/17-desktop-shell.md) — read TL;DR, Architecture, Genie takeaways
- OpenCode ch.18: [18-app-ui-and-gui-plugins.md](../knowledge/opencode/18-app-ui-and-gui-plugins.md) — read TL;DR, Architecture, Genie takeaways

## Resolution
**Blocked, not resolved (2026-11-16).** Report: [research/prototype-37.md](../research/prototype-37.md).
- The Windows machine has Rust 1.99.1 (MSVC target), Node 22, pnpm 10 and WebView2 157, but **no MSVC C++ Build Tools and no Windows SDK**, so no Rust binary links (`cargo build` of hello-world fails, and Git's GNU `link.exe` is picked up instead).
- No prototype code was built and no numbers were measured. All pass criteria (normal/burst/long frame times, emit→paint, kill→reconnect ≤ 2 s, cold start, bundle size) are **not run** on Windows, macOS or Linux.
- To unblock (admin): install VS Build Tools 2022+ with the "Desktop development with C++" workload, e.g. `winget install --id Microsoft.VisualStudio.2022.BuildTools --override "--wait --passive --add Microsoft.VisualStudio.Workload.VCTools --includeRecommended"`, then re-run this ticket.
- The planned design, pass criteria and macOS/Linux steps are in the report. The D8 gate is still open.
