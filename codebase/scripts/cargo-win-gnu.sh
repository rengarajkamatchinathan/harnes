#!/usr/bin/env bash
# Run cargo on Windows machines without MSVC, reusing the prototype-37 gnu toolchain
# workaround (llvm-dlltool + rust-lld shim; see prototypes/37-streaming-chat/README.md).
# Usage: scripts/cargo-win-gnu.sh test --workspace
set -euo pipefail
here="$(cd "$(dirname "$0")" && pwd)"
tools="${GENIE_GNU_TOOLS:-$here/../../prototypes/37-streaming-chat/.tools}"
export RUSTUP_TOOLCHAIN=stable-x86_64-pc-windows-gnu
export PATH="$tools/llvm-mingw:$PATH"
export CARGO_ENCODED_RUSTFLAGS="-Clink-arg=-B$tools/lld-as-ld/"
exec cargo "$@"
