# Prototype 37: streaming chat (daemon → Tauri relay → React)

Throwaway code for ticket 37. Findings live in `research/prototype-37.md`; this README only explains how to build and run.

```
fake-daemon/        Rust/tokio. JSON-RPC 2.0 over NDJSON on \\.\pipe\genie-proto-37 (Windows, owner-only DACL)
                    or $XDG_RUNTIME_DIR|$TMPDIR/genie-proto-37-$USER.sock (0600). Exits after 120 s with no clients.
app/src-tauri/      Tauri 2 shell: connect-or-spawn (detached), health handshake, reader -> 16 ms coalescer ->
                    tauri::ipc::Channel, EOF -> "reconnecting" -> respawn, kill/RSS/bench commands.
app/src/            React 19 + Vite + Tailwind 4. engine.ts (rAF-batched commits), markdown.ts (block splitter,
                    marked + highlight.js + KaTeX, lazy Mermaid on settled blocks), App.tsx (virtualised
                    transcript), bench.ts (bench driver), metrics.ts (frames / longtasks / lag / latency).
run-bench.ps1       Windows build + bench.   run-bench.sh   macOS / Linux build + bench.
summarize.mjs       Prints a bench-results JSON as a table.
```

## Protocol

- Request `health` → `{version, pid, t_us}`; `chat.start {scenario}` → `{stream_id}`.
- Notifications: `chat.history {messages, tokens}` (only `long`), `text.delta {stream_id, seq, t_emit_us, text}`, `text.ended {stream_id, tokens}`. `t_emit_us` is wall clock (µs) so the webview can compute emit → paint with `Date.now()`.
- Scenarios: `normal` (1 716 tokens at ~80 tok/s, ~21 s), `burst` (3× corpus, 5 152 tokens at ~1 000 tok/s, ~5 s), `long` (200 messages / ~95k tokens of history, then a `normal` stream).

## Bench mode

`genie-proto-37 --bench` (or `GENIE_BENCH=1`) runs: idle baseline → `normal` → `burst` → `long` (history + stream + 300-frame scroll fling up and down) → highlight.js vs shiki micro-bench → 3× daemon kill/reconnect → writes `GENIE_BENCH_OUT` (default `./bench-results.json`) and exits. Env knobs: `GENIE_COALESCE_MS` (default 16; 0 = one IPC message per token), `GENIE_DAEMON_PATH`.

Do not cover or minimise the window during a run (WebView2/WebKit throttle rAF for hidden views; the window is set always-on-top in bench mode).

## Windows

Requirements: Rust stable, Node 22 + pnpm 10, WebView2 runtime. Then `.\run-bench.ps1` (add `-Bundle` for the NSIS installer size).

### Windows without MSVC (what this machine needed)

`run-bench.ps1 -Toolchain gnu` (auto-selected when Visual Studio is absent) uses `stable-x86_64-pc-windows-gnu` without touching the global rustup default (`RUSTUP_TOOLCHAIN` is set for the run only) and applies three user-level workarounds:

1. **dlltool**: `windows-link`/`windows-sys` 0.61 use `raw-dylib`, so rustc needs `dlltool`. The GNU `dlltool.exe` in the toolchain's `self-contained` dir fails (`CreateProcess`) because it shells out to GNU `as`, which isn't shipped. The script extracts `llvm-dlltool`, `llvm-windres`, `llvm-rc` (+ `libLLVM-23.dll`, `libc++.dll`, `libunwind.dll`, `libwinpthread-1.dll`) from the official llvm-mingw 20261104 release zip into `.tools/llvm-mingw/` and prepends it to `PATH` (`dlltool.exe` = copy of llvm-dlltool).
2. **linker**: GNU `ld` 2.44 links llvm-dlltool import libraries wrongly (llvm-dlltool ignores `--temp-prefix`; an import slot ends up outside every import descriptor, `GetModuleHandleA` is never bound, and tauri's build script dies with `0xc0000005`). So we link with the toolchain's own `rust-lld`: `.tools/lld-as-ld/ld.exe` is a tiny shim (`win-gnu-ld-shim/main.rs`) that runs `rust-lld -flavor gnu`, passed to gcc with `-Clink-arg=-B<dir>/` via `CARGO_ENCODED_RUSTFLAGS` (that gcc ignores `-fuse-ld=lld`). The shim also re-quotes gcc's `@response` files for lld, which otherwise breaks on paths with spaces (`lld: error: unknown argument: -\`).
3. **resources**: tauri-build compiles `resource.rc` (icon, manifest, version) with `windres`. llvm-windres needs clang to preprocess; the generated `.rc` needs no preprocessing, so `.tools/llvm-mingw/windres.exe` is a shim (`win-gnu-ld-shim/windres.rs`) that adds `--no-preprocess`. Resources stay embedded (icon + common-controls manifest).

No admin rights, no MSVC, no MinGW install; nothing outside this folder is changed. `-Bundle` (NSIS) was not tried on gnu.

Debug knob: `GENIE_BENCH_SCENARIOS=long` runs a subset.

## macOS (WKWebView)

```bash
xcode-select --install                  # once
curl https://sh.rustup.rs -sSf | sh     # if Rust is missing
corepack enable && corepack prepare pnpm@10 --activate   # or: npm i -g pnpm@10
./run-bench.sh                          # BUNDLE=1 ./run-bench.sh to also record the .dmg size
```

Expect `longtasks: n/a` and no JS heap (WebKit doesn't expose `longtask` or `performance.memory`); use the frame-time and event-loop-lag figures instead. On ProMotion displays frames are ~8.3 ms. Note the macOS version and Mac model.

## Linux (WebKitGTK)

```bash
sudo apt install libwebkit2gtk-4.1-dev build-essential curl wget file libxdo-dev libssl-dev \
  libayatana-appindicator3-dev librsvg2-dev
./run-bench.sh
WEBKIT_DISABLE_DMABUF_RENDERER=1 ./run-bench.sh     # second run, written to *-nodmabuf.json
```

Run on X11 and Wayland, and on Intel/AMD and NVIDIA if available; note distro, GPU, driver and session type.

Send back the `bench-results-*.json` files and the console summary.
