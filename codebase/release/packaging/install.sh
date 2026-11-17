#!/bin/sh
# Genie CLI-only installer for macOS and Linux (spec/09 §1, D40).
#
#   curl -fsSL https://<feeds>/install.sh | sh
#   curl -fsSL https://<feeds>/install.sh | sh -s -- --channel beta --prefix "$HOME/.local"
#
# Reads the signed update manifest for this OS/arch, downloads the archive, checks its
# SHA-256 AND its Ed25519 signature (same format as the feeds: base64 of the 64-byte
# signature over the file) before anything is installed. Needs curl, tar and OpenSSL >= 3.0
# (for Ed25519 `pkeyutl -rawin`); refuses to install if it can't verify.
set -eu

FEEDS_URL="${GENIE_FEEDS_URL:-https://feeds.genie.invalid}"
# Online updates-feed public key (SPKI DER, base64). Filled in at release time; see
# release/README.md. An env override exists for mirrors with their own key.
UPDATES_PUBKEY_SPKI="${GENIE_UPDATES_PUBKEY_SPKI:-@UPDATES_PUBKEY_SPKI@}"
CHANNEL=stable
PREFIX="${HOME}/.local"

while [ $# -gt 0 ]; do
  case "$1" in
    --channel) CHANNEL="$2"; shift 2 ;;
    --prefix) PREFIX="$2"; shift 2 ;;
    *) echo "unknown option $1" >&2; exit 2 ;;
  esac
done
case "$CHANNEL" in stable|beta|dev) ;; *) echo "channel must be stable, beta or dev" >&2; exit 2 ;; esac
case "$UPDATES_PUBKEY_SPKI" in @*) echo "this install.sh has no pinned key (unreleased template)" >&2; exit 1 ;; esac

die() { echo "genie install: $*" >&2; exit 1; }
need() { command -v "$1" >/dev/null 2>&1 || die "needs $1"; }
OPENSSL="${OPENSSL:-openssl}"
need curl; need tar; need "$OPENSSL"
"$OPENSSL" pkeyutl -help 2>&1 | grep -q rawin ||
  die "needs OpenSSL >= 3.0 for Ed25519 (macOS ships LibreSSL: use 'brew install genie', or set OPENSSL=/path/to/openssl3)"

os="$(uname -s)"; arch="$(uname -m)"
case "$os" in
  Darwin) target="darwin-universal" ;;
  Linux)
    case "$arch" in
      x86_64|amd64) a=x86_64 ;; aarch64|arm64) a=aarch64 ;; *) die "unsupported arch $arch" ;;
    esac
    # glibc build if glibc is present, else the static musl CLI.
    if ldd --version 2>&1 | grep -qi glibc; then target="linux-$a"; else target="linux-$a-musl"; fi ;;
  *) die "unsupported OS $os (use install.ps1 on Windows)" ;;
esac

tmp="$(mktemp -d)"; trap 'rm -rf "$tmp"' EXIT INT TERM

# Ed25519 public key as PEM (SubjectPublicKeyInfo DER, base64; `cargo xtask feed pubkey --pem`).
printf -- '-----BEGIN PUBLIC KEY-----\n%s\n-----END PUBLIC KEY-----\n' "$UPDATES_PUBKEY_SPKI" > "$tmp/key.pem"

verify() { # file sig_b64
  printf '%s' "$2" | "$OPENSSL" base64 -d -A > "$tmp/sig.bin"
  "$OPENSSL" pkeyutl -verify -pubin -inkey "$tmp/key.pem" -rawin -in "$1" -sigfile "$tmp/sig.bin" >/dev/null 2>&1
}

manifest_url="$FEEDS_URL/updates/$CHANNEL/$target.json"
curl -fsSL "$manifest_url" -o "$tmp/m.json" || die "can't fetch $manifest_url"
curl -fsSL "$manifest_url.sig" -o "$tmp/m.sig" || die "can't fetch the manifest signature"
verify "$tmp/m.json" "$(cat "$tmp/m.sig")" || die "manifest signature does not verify"

# Minimal JSON field extraction (the manifest is machine-written, one artifact for the CLI).
field() { tr -d '\n' < "$tmp/m.json" | sed -n "s/.*\"kind\": *\"cli\"[^}]*\"$1\": *\"\([^\"]*\)\".*/\1/p;s/.*\"$1\": *\"\([^\"]*\)\"[^}]*\"kind\": *\"cli\".*/\1/p" | head -n1; }
url="$(field url)"; sha="$(field sha256)"; sig="$(field signature)"
[ -n "$url" ] && [ -n "$sha" ] && [ -n "$sig" ] || die "manifest has no CLI artifact for $target"
expires="$(tr -d '\n' < "$tmp/m.json" | sed -n 's/.*"expires_at": *"\([^"]*\)".*/\1/p')"
now="$(date -u +%Y-%m-%dT%H:%M:%SZ)"
[ "$now" \< "$expires" ] || die "manifest expired at $expires (stale mirror?)"

curl -fSL --progress-bar "$url" -o "$tmp/genie.tar.gz" || die "download failed"
if command -v sha256sum >/dev/null 2>&1; then got="$(sha256sum "$tmp/genie.tar.gz" | cut -d' ' -f1)"
else got="$(shasum -a 256 "$tmp/genie.tar.gz" | cut -d' ' -f1)"; fi
[ "$got" = "$sha" ] || die "SHA-256 mismatch (expected $sha, got $got)"
verify "$tmp/genie.tar.gz" "$sig" || die "artifact signature does not verify"

tar -xzf "$tmp/genie.tar.gz" -C "$tmp"
mkdir -p "$PREFIX/bin"
install -m 0755 "$tmp"/genie-*/genie "$PREFIX/bin/genie"
echo "installed $("$PREFIX/bin/genie" --version) to $PREFIX/bin/genie"
case ":$PATH:" in *":$PREFIX/bin:"*) ;; *) echo "add $PREFIX/bin to your PATH" ;; esac
