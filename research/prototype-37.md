# Prototype 37: streaming chat (daemon → Tauri relay → React)

> Report for [tickets/37-prototype-streaming-chat.md](../tickets/37-prototype-streaming-chat.md) · decisions D8, D19, D20, D22, D28 in [WAYFINDER.md](../WAYFINDER.md) · background [desktop-shell.md](desktop-shell.md) · 2026-11-16

## Status (2026-11-17): Windows/WebView2 measured in Run 2 (windows-gnu toolchain); macOS and Linux still to run.

Run 1 (2026-11-16) was blocked by the missing MSVC toolchain; its sections below are kept as history. See **Run 2 (windows-gnu)** at the end for the build, numbers, pass/fail and the macOS/Linux steps.

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

## Results (Run 1, superseded by Run 2)

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

---

## Run 2 (windows-gnu, 2026-11-17)

Code: [prototypes/37-streaming-chat/](../prototypes/37-streaming-chat/). The README there has the layout and protocol. Raw results are in `prototypes/37-streaming-chat/bench-results-windows-c16.json` (16 ms Rust coalescing, the default) and `bench-results-windows-c0.json` (no coalescing). Both files are git-ignored, so copy them if they need keeping.

### Machine

- Windows 11 Pro 10.0.26300, WebView2 157.0 (Edg/157).
- 60 Hz display at 125 % scaling; 1100×850 window.
- Rust 1.100.0 `stable-x86_64-pc-windows-gnu`. The global default stays `stable-x86_64-pc-windows-msvc`.
- Node 22.13.1, pnpm 10.20.0.
- Tauri 2.12.6 / @tauri-apps/cli 2.12.5, React 19.3, Vite 8.3, Tailwind 4.3, marked 18, highlight.js 11.12, KaTeX 0.19, Mermaid 12.3, @tanstack/react-virtual 3.14, shiki 4.6.

### Toolchain notes and workarounds

No admin rights, no MSVC and no MinGW install were used, and nothing outside the prototype folder was changed. `run-bench.ps1` automates everything below. It picks `-Toolchain gnu` automatically when Visual Studio is absent.

1. **`RUSTUP_TOOLCHAIN=stable-x86_64-pc-windows-gnu`, set per run.** There is no `rust-toolchain.toml`. On this host, `channel = "stable"` resolves to MSVC, and pinning the gnu triple would break macOS/Linux.
2. **dlltool.**
   - Problem: `windows-link`/`windows-sys` 0.61 (pulled in by tokio, mio and tauri) use `raw-dylib`, so rustc calls `dlltool`. The GNU `dlltool.exe` in the toolchain's `self-contained` dir fails with `CreateProcess`, because it shells out to GNU `as`, which isn't shipped.
   - Fix: `llvm-dlltool` from the official **llvm-mingw 20261104** ucrt-x86_64 release zip (GitHub, mstorsjo/llvm-mingw). Only 7 files (~80 MB, including `libLLVM-23.dll`) are extracted into `.tools/llvm-mingw/` and put on `PATH`, with llvm-dlltool copied to `dlltool.exe`.
3. **Linker.**
   - Problem: with llvm-dlltool import libraries, GNU `ld` 2.44 produced binaries that crash at start (`STATUS_ACCESS_VIOLATION` in tauri's build script).
   - Diagnosis, from a small ctypes debugger and a PE dump: execution jumped to `0x1227a50`, the hint/name RVA of `GetModuleHandleA`. Its IAT slot sat between two import descriptors, so the loader never bound it. llvm-dlltool ignores rustc's `--temp-prefix`, so import members collide across crates.
   - Fix: link with the toolchain's own `rust-lld`. The bundled gcc ignores `-fuse-ld=lld`, so `.tools/lld-as-ld/ld.exe` is a tiny shim (`win-gnu-ld-shim/main.rs`) that execs `rust-lld -flavor gnu`. It is passed as `-Clink-arg=-B<dir>/` via `CARGO_ENCODED_RUSTFLAGS`.
   - The shim also re-quotes gcc's `@response` files from GNU to Windows quoting. Without that, lld fails on the spaces in this OneDrive path (`lld: error: unknown argument: -\`). Building from the 8.3 short path did not help, because cargo canonicalises the path.
4. **Windows resources (the known `tauri-build` risk).**
   - Problem: tauri-winres runs `windres`, and `llvm-windres` needs clang to preprocess (`llvm-rc: Unable to find clang for preprocessing`).
   - Fix: the generated `resource.rc` has no `#include`/`#define`, so `windres.exe` is a second shim (`win-gnu-ld-shim/windres.rs`) that adds `--no-preprocess`.
   - **Resource embedding was kept** (icon, common-controls v6 manifest, version info). It did not need to be disabled.
5. **Effect on the numbers.** Exe size and cold start from this gnu+lld build are not representative of the MSVC build Genie would ship. Webview runtime numbers are unaffected, because WebView2 does the rendering.

Also fixed while building: the sysinfo 0.40 API change (`System::new()` now returns a `Result`), and a race in the kill test (status events arriving before the waiter was registered).

### What was built (as planned)

- **fake-daemon** (tokio)
  - Transport: `\\.\pipe\genie-proto-37`, with a protected owner-only DACL (`D:P(A;;GA;;;OW)`) and remote clients rejected. On Unix, a 0600 socket.
  - Protocol: JSON-RPC 2.0 over NDJSON. `health`, `chat.start`, `chat.history`, `text.delta {seq, t_emit_us}` and `text.ended`.
  - Scenarios: `normal` (1,716 tokens at 80 tok/s), `burst` (5,152 tokens at 1,000 tok/s) and `long` (200 messages / ~95k tokens of history, then `normal`).
  - Corpus: ~6 KB of Markdown with paragraphs, lists, a table, Rust/TS/Python/Go/SQL/Bash fences, inline and display KaTeX, and 2 Mermaid diagrams.
  - Exits after 120 s with no clients.
- **Tauri relay**
  - Connects, or spawns the daemon detached (`DETACHED_PROCESS|CREATE_NEW_PROCESS_GROUP` on Windows, `process_group(0)` on Unix).
  - Retries with 5→50 ms backoff, then does a `health` handshake.
  - Reader task → 16 ms coalescer (`GENIE_COALESCE_MS`) → `tauri::ipc::Channel`.
  - On EOF: shows `reconnecting`, respawns the daemon, then shows `connected` with the kill→reconnect time in the status bar.
  - Reports process-tree RSS via `sysinfo`.
- **Renderer**
  - An incremental block splitter: a blank line outside fences or `$$` ends a block.
  - Settled blocks are `React.memo`'d, with their HTML cached by source.
  - Only the tail block is re-parsed (`marked` + highlight.js), and KaTeX is skipped in the tail.
  - Mermaid is lazy-loaded and renders only when its block settles. Renders are serialised and the SVG is cached.
  - One `flushSync` commit per `requestAnimationFrame`.
  - Virtualised transcript (`@tanstack/react-virtual`).
- **Bench** (`--bench`)
  - Order: idle baseline → normal → burst → long (history, stream, then a 300-frame fling of 400 px/frame up and back down) → highlight.js vs shiki → 3× kill/reconnect → a chat on the respawned daemon → `bench-results.json`.
  - Emit→paint compares the daemon's wall-clock emit time with `Date.now()` at the rAF after the commit. This overstates latency by up to one frame.

### Results (Windows 11 / WebView2 157, 60 Hz)

| Metric | c16 (default) | c0 (no Rust coalescing) |
|---|---|---|
| idle frame p50 / p95 (the vsync baseline) | 16.7 / 16.8 ms | 16.7 / 16.9 ms |
| **normal** frame p50 / p95 / p99 / max | 16.7 / **16.9** / 17.4 / 233 ms | 16.7 / 16.9 / 33.3 / 300 ms |
| normal long tasks: count, max | **6, max 210 ms** | 7, max 227 ms |
| normal emit→paint p50 / p95 | 34.8 / **47.8 ms** | 26.3 / 66.5 ms |
| normal rendered tok/s | 79.9 | 79.9 |
| **burst** frame p95 / max | 16.8 / 17.2 ms | 16.8 / 17.2 ms |
| burst long tasks; event-loop lag max | **0**; 9.2 ms | 0; 11.3 ms |
| burst emit→paint p50 / p95; rendered tok/s | 34.5 / 45.7 ms; 984 | 26.0 / 33.5 ms; 983 |
| long: first render of 200-message history | one 50 ms frame (one 52 ms long task) | 50 ms |
| long: stream frame p95; emit→paint p95 | 16.8 ms; 45.1 ms | 16.8 ms; 32.1 ms |
| **long scroll** up: frame p50 / p95 / p99 (frames > 25 ms) | 16.7 / **17.1** / 33.4 (10/300) | 16.7 / 16.8 / 33.3 (8/300) |
| long scroll down: frame p95 (frames > 25 ms) | 16.9 (7/300) | 16.8 (9/300) |
| **kill → reconnected**, 3 runs | **324 / 281 / 302 ms** (EOF detected after 267 / 221 / 234 ms) | 346 / 248 / 240 ms |
| chat on the respawned daemon | ok | ok |
| cold start: process start → first paint | 1,065 ms (→ webview navigation 855 ms; → daemon connected 1,071 ms, with the daemon spawned by the app) | 1,191 ms |
| RSS at end: shell / whole app tree (7 processes) / daemon | 31.6 MB / 558 MB / 6.2 MB | — |
| JS heap at end (`performance.memory`) | 38.4 MB | — |

Sizes:
- App exe: 5.64 MB, with the frontend embedded. `dist` is 7.1 MB uncompressed, mostly lazy Mermaid/shiki chunks.
- Daemon: 0.44 MB.
- Zip of both: 3.95 MB.
- NSIS installer: not built (gnu toolchain).

Highlighter micro-bench (6 corpus code blocks, same WebView2):

| | highlight.js | shiki 4.6 (JS regex engine, 6 langs + 1 theme) |
|---|---|---|
| init | 0 (synchronous, bundled) | 92 ms (async) |
| all 6 blocks, full highlight | 2.7 ms | 66 ms |
| streaming tail (re-highlight each 16-char prefix, 167 calls) p50 / p95 / max | 0.2 / 0.6 / 1.3 ms | 1.1 / 3.7 / 4.7 ms |

### Pass / fail (Windows / WebView2)

| Criterion | Result | Note |
|---|---|---|
| normal: p95 frame ≤ 16.7 ms | **FAIL as written; pass against the baseline** | p95 was 16.9 ms, but idle p95 on this 60 Hz display is already 16.8 ms: vsync jitter alone exceeds 16.7. Restate the criterion as "p95 ≤ idle p95 + 1 ms" and "≤ 1 % of frames > 25 ms". On that basis `normal` passes (7 of 1,249 frames > 25 ms). |
| normal: no long task > 50 ms | **FAIL** | 6 long tasks, max 210 ms; the worst frame was 233 ms. They all fall in `normal`, the first stream to hit Mermaid. The `long` stream re-renders the same corpus with the Mermaid SVGs cached and has **0** long tasks. That points to Mermaid's first lazy load and first render of each diagram (plus first-use KaTeX/hljs warm-up). This is inferred, not profiled. |
| normal: emit→paint p95 ≤ 50 ms | **PASS** | 47.8 ms with 16 ms coalescing. Without coalescing p95 was 66.5 ms, though p50 dropped from 35 to 26 ms. |
| burst: no long task > 100 ms | **PASS** | 0 long tasks, 984 tok/s rendered, frame p95 16.8 ms. |
| burst: UI responsive | **PASS** | event-loop lag max 9 ms. |
| long: scroll p95 ≤ 16.7 ms | **FAIL as written; pass against the baseline** | 17.1 / 16.9 ms (c0: 16.8 / 16.8) against an idle baseline of 16.8 ms. About 3 % of frames are 33 ms (one dropped frame) as new messages mount. |
| kill → reconnect ≤ 2 s | **PASS** | 281–324 ms. About 85 % of that is EOF detection after `TerminateProcess`; respawn plus handshake takes about 60 ms. |

Measurement artefact: the 1.65 s long task in the `kill` bucket is not the kill. It is the synchronous shiki micro-bench that runs just before it (20 × 66 ms plus the tail loop), which the PerformanceObserver delivers late.

### Recommendations

- **Renderer: keep the settled/tail split, rAF-batched commits and the virtualised transcript.**
  - On WebView2 this holds 60 fps at 1,000 tok/s and with 200 messages / ~95k tokens of history.
  - The tail re-parse costs less than 1 ms per frame.
  - **Fix Mermaid before the D8 gate counts as met:**
    - pre-warm the module when idle;
    - render each diagram in an idle callback behind a placeholder (or off the main thread);
    - keep caching SVGs by source.

  KaTeX is cheap. Its warm-up cost should also be absorbed by pre-warming.
- **Highlighter: use highlight.js, at least for the streaming tail.** It is about 6× cheaper per call (p95 0.6 ms vs 3.7 ms) and synchronous. shiki makes sense only for settled blocks, and only if its output quality is wanted, at a cost of 92 ms init and 66 ms for 6 blocks.
- **Coalescing interval: about 8 ms (or flush on every N deltas), not 16 ms.**
  - The webview already commits once per frame, so Rust-side coalescing mainly cuts the IPC message count.
  - At 1,000 tok/s, uncoalesced IPC was still fine on WebView2: 0 long tasks, and emit→paint p50 of 26 ms vs 35 ms.
  - The 16 ms setting adds ~9 ms of median latency but gave a better p95 in `normal` (47.8 vs 66.5 ms). That comparison is noisy, because the Mermaid stalls inflate it.
  - Re-check on WKWebView/WebKitGTK, where IPC costs more; if those are fine at 8 ms, use 8 ms.
- **Crash recovery: no change needed.** ~0.3 s is far under the 2 s target.
- **Frame criteria: make them relative to the idle vsync baseline**, as above, so they work on 60/120/144 Hz displays.

### Still to run: step-by-step instructions for the teammate

**Caveat:** the prototype's Unix code paths (Unix socket, `process_group`) only compile on macOS/Linux, and they **were not compiled here** because no Unix target is installed on this machine. If the first build fails there, the fix should be small and confined to the `#[cfg(unix)]` modules in `fake-daemon/src/main.rs` and `app/src-tauri/src/main.rs`.

**macOS (WKWebView): the D8 gate. Use the oldest macOS version Genie will support.**
1. Install the toolchain:
   - `xcode-select --install`
   - Rust: `curl https://sh.rustup.rs -sSf | sh`
   - Node 22 and pnpm 10: `corepack enable && corepack prepare pnpm@10 --activate`
2. Run `cd prototypes/37-streaming-chat && ./run-bench.sh`.
   - The script builds the daemon and runs `pnpm tauri build --no-bundle`. It then kills any stale daemon and runs `--bench`, which takes about 2 minutes.
   - **Keep the window visible.**
3. Run `BUNDLE=1 ./run-bench.sh` once to record the `.dmg` size.
4. Optional, for comparison: `GENIE_COALESCE_MS=0 SKIP_BUILD=1 GENIE_BENCH_OUT=$PWD/bench-results-darwin-c0.json ./run-bench.sh`.
5. Check by eye that the Mermaid diagrams render (WKWebView `foreignObject` labels) and that KaTeX looks right.
6. Send back:
   - the `bench-results-darwin*.json` files and the console summary;
   - the macOS version, the Mac model and the display refresh rate.

   WebKit has no `longtask` API and no `performance.memory`, so judge jank from `frames.max`, `janky_over_25ms` and `event_loop_lag_ms`.

**Linux (WebKitGTK), on Ubuntu LTS**
1. Install the packages: `sudo apt install libwebkit2gtk-4.1-dev build-essential curl wget file libxdo-dev libssl-dev libayatana-appindicator3-dev librsvg2-dev`. Then install Rust, Node 22 and pnpm 10 as for macOS.
2. Run `cd prototypes/37-streaming-chat && ./run-bench.sh`.
3. Run `WEBKIT_DISABLE_DMABUF_RENDERER=1 SKIP_BUILD=1 ./run-bench.sh`, which writes `bench-results-linux-nodmabuf.json`.
4. Repeat on X11 and on Wayland, and on Intel/AMD and NVIDIA GPUs where available. Optionally run with `BUNDLE=1` to record the `.deb`/AppImage sizes.
5. Send back:
   - all the JSON files and the console summaries;
   - the distro, GPU, driver and session type.

**Windows: optional re-run with MSVC.** Once VS Build Tools are installed, `.\run-bench.ps1 -Toolchain msvc -Bundle` gives a representative exe size, installer size and cold start.
