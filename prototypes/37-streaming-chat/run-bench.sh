#!/usr/bin/env bash
# Prototype 37 bench for macOS (WKWebView) and Linux (WebKitGTK).
# Usage: ./run-bench.sh            build (release) + bench
#        SKIP_BUILD=1 ./run-bench.sh   bench only
#        BUNDLE=1 ./run-bench.sh       also build an installer (dmg / deb+appimage) to record its size
#        GENIE_COALESCE_MS=0 ./run-bench.sh   no Rust-side coalescing (compare)
set -euo pipefail
cd "$(dirname "$0")"
OS=$(uname -s | tr '[:upper:]' '[:lower:]')
OUT="${GENIE_BENCH_OUT:-$PWD/bench-results-$OS$( [ -n "${WEBKIT_DISABLE_DMABUF_RENDERER:-}" ] && echo -nodmabuf ).json}"

if [ -z "${SKIP_BUILD:-}" ]; then
  cargo build --release -p fake-daemon
  pushd app >/dev/null
  pnpm install
  if [ -n "${BUNDLE:-}" ]; then
    if [ "$OS" = darwin ]; then pnpm tauri build --bundles dmg; else pnpm tauri build --bundles deb,appimage; fi
  else
    pnpm tauri build --no-bundle
  fi
  popd >/dev/null
fi

pkill -f 'target/release/fake-daemon' 2>/dev/null || true   # cold start: let the app spawn the daemon
sleep 0.5
echo "running bench (a window opens for ~2 minutes; do not cover or minimise it)…"
GENIE_BENCH=1 GENIE_BENCH_OUT="$OUT" ./target/release/genie-proto-37 --bench
node summarize.mjs "$OUT"
echo "--- sizes"
ls -l target/release/genie-proto-37 target/release/fake-daemon
du -sh app/dist
find target/release/bundle -maxdepth 2 -type f \( -name '*.dmg' -o -name '*.deb' -o -name '*.AppImage' \) -exec ls -l {} \; 2>/dev/null || true
echo "results: $OUT"
