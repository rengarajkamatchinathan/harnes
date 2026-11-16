# Prototype 37: streaming chat (daemon → Tauri relay → React)

> Report for [tickets/37-prototype-streaming-chat.md](../tickets/37-prototype-streaming-chat.md) · decisions D8, D19, D20, D22, D28 in [WAYFINDER.md](../WAYFINDER.md) · background [desktop-shell.md](desktop-shell.md) · 2026-11-16

## Status: BLOCKED on Windows (missing build toolchain). No code built, no numbers measured.

The prototype was not built. The Windows machine cannot link any Rust binary, so neither the `fake-daemon` nor the Tauri app can be compiled. Following the ticket's rules, I stopped instead of attempting system-level installs or switching toolchains.

## Setup found (2026-11-16)

| Item | Found | Needed |
|---|---|---|
| OS | Windows 11 Pro 10.0.26300 | — |
| Rust | 1.99.1, toolchain `stable-x86_64-pc-windows-msvc` (only target installed) | ok |
| Node / pnpm | 22.13.1 / 10.20.0 | ok |
| WebView2 runtime | **157.0.4341.57, installed** | ok |
| MSVC C++ build tools (`cl.exe`, MSVC `link.exe`) | **missing**: no `vswhere.exe`, no `Microsoft Visual Studio` directory under either Program Files | **required** |
| Windows 10/11 SDK (`Windows Kits\10\Lib`: `kernel32.lib`, `ws2_32.lib`, …) | **missing** | **required** |

### Proof

`cargo new hello && cargo build` fails at link time:

```
error: linking with `link.exe` failed: exit code: 1
  = note: link: extra operand '...hello....rcgu.o'
          Try 'link --help' for more information.
note: you may need to install Visual Studio build tools with the "C++ build tools" workload
```

With no MSVC installed, rustc falls back to the first `link.exe` on `PATH`. That is Git for Windows' GNU coreutils `link` (`...\Programs\Git\usr\bin\link.exe`), a hard-link tool, not a linker. `cargo check` doesn't help either: proc-macro crates (serde_derive, tokio-macros, tauri-macros) and build scripts (`tauri-build`, `windows-sys`) must be linked as host DLLs or EXEs.

## How to unblock (needs someone with admin rights; not done by the agent)

1. Install **Visual Studio Build Tools 2022 (or later)** with the **"Desktop development with C++"** workload. This includes MSVC v143+ x64 build tools and a **Windows 11 SDK**. These are the official Tauri Windows prerequisites (https://v2.tauri.app/start/prerequisites/). Unattended:
   ```powershell
   winget install --id Microsoft.VisualStudio.2022.BuildTools --override "--wait --passive --add Microsoft.VisualStudio.Workload.VCTools --includeRecommended"
   ```
   The Build Tools licence terms apply; check that the organisation's Visual Studio licensing covers this use.
2. Open a new shell and verify: `cargo new hello && cd hello && cargo run` prints `Hello, world!`. rustc finds MSVC through `vswhere`, so Git's `link.exe` on `PATH` stops mattering once MSVC is installed.
3. WebView2 is already installed (157); nothing else is needed on Windows.

Not used as a workaround: the `x86_64-pc-windows-gnu` toolchain plus a MinGW gcc. It would need a toolchain install, it isn't the target Genie will ship on Windows, and it would make the cold-start and bundle-size numbers unrepresentative.

## What was built

Nothing. `prototypes/37-streaming-chat/` was not created, because untested Tauri and daemon code would not answer the ticket's question. The planned design is below, so the next run can start straight away.

### Planned design (unchanged from the ticket brief)

- **`fake-daemon`** (Rust, tokio): `\\.\pipe\genie-proto-37` (Windows) or a user-only Unix socket (0600, in `$XDG_RUNTIME_DIR` or a tmp dir), carrying JSON-RPC 2.0 over NDJSON. `health` → `{version}`. `chat.start {scenario}` → `text.delta {seq, t_emit_us, text}` …, then `text.ended`. Scenarios: `normal` ~80 tok/s, `burst` ~1000 tok/s, `long` (pre-fill 200 messages / ~100k tokens, then a normal reply). The corpus is Markdown with tables, lists, fenced code in 5+ languages, KaTeX and Mermaid.
- **Tauri 2 relay**: connect; if that fails, spawn the daemon detached (`DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP` on Windows, `setsid` on Unix), retry with backoff, then `health` handshake. Reader task → `tauri::ipc::Channel<Event>`. On EOF or error, emit `status: reconnecting`, respawn and re-handshake, and record the kill→reconnect time. Daemon emit timestamps use a shared wall clock (`SystemTime` µs) so the webview can compute emit→paint with `performance.timeOrigin + now()`.
- **Renderer**: split the stream into top-level Markdown blocks on blank lines outside fences. Settled blocks are `React.memo` keyed by index and content hash and never re-parsed. Only the tail block re-parses on each rAF-batched flush. Mermaid and KaTeX render only when their block settles. Code highlighting would compare shiki (JS regex engine, fine-grained bundle) with highlight.js on the streaming tail. Expected winner for the *tail*: highlight.js, which is synchronous and has a smaller per-call cost; shiki would be used for settled blocks if its output is preferred. To be confirmed by measurement. The transcript is windowed (`@tanstack/react-virtual` or `content-visibility:auto`).
- **`--bench` / `GENIE_BENCH=1`**: runs all three scenarios and writes `bench-results.json`. It records rAF frame-time p50/p95/p99, `longtask` count and max, emit→paint p50/p95, rendered tok/s, `performance.memory` (Chromium only) plus process RSS from Rust (`sysinfo`), cold start (process start → first paint), installer and `.exe` sizes, and kill→reconnect time.
- **`run-bench.sh` / `run-bench.ps1`** for macOS (WKWebView) and Linux (WebKitGTK).

## Proposed pass criteria

| Scenario | Criterion |
|---|---|
| `normal` (~80 tok/s) | p95 frame ≤ 16.7 ms; no long task > 50 ms; emit→paint p95 ≤ 50 ms |
| `burst` (~1000 tok/s) | no long task > 100 ms; UI stays responsive |
| `long` (200 msgs / ~100k tokens) | scrolling p95 frame ≤ 16.7 ms |
| Daemon crash | kill → reconnected ≤ 2 s |

## Results

| Criterion | Windows / WebView2 | macOS / WKWebView | Linux / WebKitGTK |
|---|---|---|---|
| normal: p95 frame ≤ 16.7 ms | **not run** (blocked) | not run | not run |
| normal: no long task > 50 ms | **not run** | not run | not run |
| normal: emit→paint p95 ≤ 50 ms | **not run** | not run | not run |
| burst: no long task > 100 ms | **not run** | not run | not run |
| long: scroll p95 frame ≤ 16.7 ms | **not run** | not run | not run |
| kill → reconnect ≤ 2 s | **not run** | not run | not run |
| cold start, bundle size, RSS | **not run** | not run | not run |

## Problems hit

1. **No MSVC Build Tools or Windows SDK** on the Windows machine. This is a hard blocker, described above.
2. **`link.exe` name clash**: with MSVC missing, Git for Windows' GNU `link.exe` on `PATH` is picked up and gives a confusing `extra operand` error. It goes away once MSVC is installed (rustc locates MSVC through `vswhere`). Worth a line in the Genie contributor setup docs.

## Recommendations (provisional, not yet backed by measurements)

These come from the research in [desktop-shell.md](desktop-shell.md) §5 and are hypotheses for the prototype to test:
- **Renderer**: a settled/streaming block split, rAF-batched commits (one flush per frame), Mermaid and KaTeX only on settled blocks, and a windowed transcript.
- **IPC batching**: coalesce deltas on the Rust side about every 16 ms (one Channel message per frame) instead of one message per token. At 1000 tok/s that turns about 1000 IPC messages/s into about 60.
- **Highlighting**: highlight.js for the streaming tail and either library for settled blocks, to be confirmed.

## Still to run

- **Windows**: everything, once the MSVC Build Tools and Windows SDK are installed. Then build the prototype, run `--bench` in release, run the kill-recovery test, and record cold start and installer size.
- **macOS (WKWebView)**: the full bench on the oldest supported macOS. The D8 gate, so a failure reopens D8. Also check Mermaid `htmlLabels`/`foreignObject` and the Shiki regex engine. Needs Xcode Command Line Tools.
- **Linux (WebKitGTK)**: the full bench on Ubuntu LTS: Intel/AMD and NVIDIA, X11 and Wayland, with and without `WEBKIT_DISABLE_DMABUF_RENDERER=1`. Needs `libwebkit2gtk-4.1-dev`, `build-essential`, `libxdo-dev`, `libssl-dev`, `libayatana-appindicator3-dev` and `librsvg2-dev`.
