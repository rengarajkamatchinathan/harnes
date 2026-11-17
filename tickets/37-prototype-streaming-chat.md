# 37 — Prototype: Tauri + React streaming chat on the daemon

type: wayfinder:prototype · status: resolved-pending-review (Windows done; macOS/Linux pending) · claimed-by: prototype-agent (2026-11-17) · blocked-by: 02,03,30

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
**Windows done; macOS/Linux pending (2026-11-17).** Report: [research/prototype-37.md](../research/prototype-37.md), section "Run 2 (windows-gnu)". Code: [prototypes/37-streaming-chat/](../prototypes/37-streaming-chat/).
- **Build.** The user-level `stable-x86_64-pc-windows-gnu` toolchain built everything, with no MSVC and no admin rights. It needed three local workarounds, all automated in `run-bench.ps1`:
  - llvm-dlltool from the llvm-mingw release zip, because the bundled GNU dlltool needs `as`;
  - rust-lld via an `ld.exe` shim, because GNU ld mis-links llvm-dlltool import libs (crash `0xc0000005`), and the shim also re-quotes response files for paths with spaces;
  - a `windres --no-preprocess` shim. Tauri resources are still embedded.
- **Windows 11 / WebView2 157 at 60 Hz, 16 ms coalescing.**
  - `burst`: 984 tok/s, 0 long tasks, lag max 9 ms. **PASS.**
  - `normal`: emit→paint p95 47.8 ms. **PASS.**
  - Kill→reconnect: 281–324 ms. **PASS.**
  - `normal`: 6 long tasks, max 210 ms. **FAIL.** They are attributed to Mermaid's first load and render; the cached re-run in `long` had 0.
  - Frame p95 in `normal` (16.9 ms) and in the `long` scroll (17.1 ms) fail the strict 16.7 ms criterion, but idle vsync p95 on this display is already 16.8 ms, so the criterion should be made relative to idle.
  - Other numbers: cold start ~1.07 s to first paint; app exe 5.6 MB (gnu/lld build, not representative of MSVC); app process tree 558 MB RSS; JS heap 38 MB.
- **Recommendations:**
  - Keep the settled/tail block split, rAF-batched commits and the virtualised transcript.
  - Pre-warm Mermaid and render it off the critical path.
  - Use highlight.js for the streaming tail; it is ~6× cheaper than shiki per call.
  - Use ~8 ms Rust coalescing rather than 16 ms; uncoalesced was fine at 1,000 tok/s on WebView2.
- **Still open: the D8 gate.** Someone needs to run `./run-bench.sh` on macOS (oldest supported version) and on Linux (Ubuntu LTS, with and without `WEBKIT_DISABLE_DMABUF_RENDERER=1`, on X11 and Wayland). The step-by-step instructions are in the report. The Unix code paths were not compiled on this machine.
