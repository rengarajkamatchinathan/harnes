# release/

Packaging, channels, notices and runbooks for [spec 09](../../spec/09-distribution-and-ops.md)
(D40, t27, t38). The logic lives in `cargo xtask`; the GitHub Actions workflows in
`../../.github/workflows/` only call it, so the CI platform can change (ticket 41).

```
channels/<stable|beta|dev>.json   names per channel: app name, bundle id, data-dir suffix,
                                  DB name, upgrade code, package/cask/winget ids, rollout rules
packaging/install.sh, install.ps1 CLI-only installers (checksum + signature before install)
packaging/nfpm.yaml               CLI .deb/.rpm (GPG-signed)
packaging/homebrew/, winget/      formula, cask and winget templates (@FIELD@ placeholders)
notices/extra.md                  non-crate components for THIRD-PARTY-NOTICES
opencode-derived.txt              files that must carry the OpenCode MIT header (C3)
secret-scan-allow.txt             paths the secret scan skips (with reasons)
runbooks/                         release, staged rollout, rollback, incident response,
                                  key rotation, feeds (09 §6)
```

## Commands

| Command | What |
|---|---|
| `cargo xtask ci [step…]` | PR gates locally: fmt, clippy, test, check, unsafe, headers, secrets, feeds, deny, frontend |
| `cargo xtask nightly <suite>` | main/nightly suites (daemon, process; the desktop/fuzz/live ones once they exist) |
| `cargo xtask dist --target T --channel C` | build + sign + archive + SBOM + notices + checksums + manifest draft + Tauri overlay |
| `cargo xtask sbom` / `notices` / `checksums` | the pieces on their own |
| `cargo xtask sign <macos\|windows\|linux> <files…>` | OS code signing; skipped when the identity env vars are unset |
| `cargo xtask feed build --out D [--updates-dir dist] [--previous-dir live]` | stamp, sign, verify every feed |
| `cargo xtask feed verify F --pubkey HEX [--previous F] [--artifacts-dir D]` | verify like a client |
| `cargo xtask feed rollout M (--next\|--percent N\|--pause\|--resume)` | staged rollout (09 §4.1) |
| `cargo xtask feed gate --candidate % --baseline % --soak-hours H [--expedited]` | SA-33 gate |
| `cargo xtask feed keygen --out F` / `pubkey [--pem]` | key handling (dev/test; production keys per key-rotation.md) |

## Desktop bundling (hooks only)

`apps/desktop` is built by another workstream; nothing here edits it. `cargo xtask dist`
writes `dist/tauri.<channel>.conf.json`, an overlay passed to `tauri build --config` that
sets `productName`, `identifier`, the updater endpoint and public key, the WiX upgrade code
and the WebKitGTK 4.1 deb dependency per channel. `cargo xtask dist --desktop` runs
`pnpm tauri build --config <overlay>` in `apps/desktop` once it exists. Tauri's own updater
signature (`TAURI_SIGNING_PRIVATE_KEY`, minisign) is separate from the Ed25519 feed key; the
desktop must verify the feed manifest `.sig` with the pinned feed key before handing the
artifact to `tauri-plugin-updater` (see Known gaps in `../README.md`).

## Secrets and accounts a human must provide

None of these exist yet; every workflow step that needs one is skipped when it is absent.

| Secret / variable | For | Owner |
|---|---|---|
| Offline **root Ed25519 key** (hardware, two custodians) | signs `feeds/keys/keylist.json` (SA-31) | release owner + security |
| `GENIE_FEED_KEY_UPDATES` / `_CATALOG` / `_CONNECTORS` / `_SKILLS` / `_NOTICES` | online feed keys (32-byte seed, hex or base64) | release owner / feeds owner |
| `vars.GENIE_FEED_ROOT_PUBKEY` | verify the key list before publishing | release owner |
| `APPLE_CERTIFICATE_P12`, `APPLE_CERTIFICATE_PASSWORD`, `APPLE_SIGNING_IDENTITY` | Developer ID Application signing | Apple Developer account holder |
| `APPLE_API_KEY_P8`, `APPLE_API_KEY_ID`, `APPLE_API_ISSUER` | `notarytool` | Apple Developer account holder |
| `AZURE_CLIENT_ID`, `AZURE_TENANT_ID`, `AZURE_SUBSCRIPTION_ID` | OIDC login (federated credential for this repo) | Synergech Azure admin |
| `TRUSTED_SIGNING_ENDPOINT`, `_ACCOUNT`, `_PROFILE` | Authenticode via Azure Trusted Signing | Synergech Azure admin |
| `GPG_PRIVATE_KEY`, `GPG_KEY_ID` | .deb/.rpm, apt/yum repo and archive signatures | release owner |
| `GENIE_TAURI_UPDATER_PUBKEY`, `TAURI_SIGNING_PRIVATE_KEY(_PASSWORD)` | desktop updater artifacts | release owner |
| `AZURE_STORAGE_ACCOUNT`, `AZURE_STORAGE_CONTAINER` | feed/CDN origin | Synergech Azure admin |
| `ANTHROPIC_API_KEY`, `OPENAI_API_KEY`, `GEMINI_API_KEY`, `AZURE_OPENAI_API_KEY` + `vars.GENIE_LIVE_SMOKE` | nightly live provider smoke (environment `provider-keys`) | provider-layer owner |

GitHub environments `release` (required reviewers: release owner) and `provider-keys`
hold these. Runners (SA-34): hosted Windows/macOS/Linux, plus the self-hosted NVIDIA Linux
box, an Intel/AMD Linux box and an oldest-supported-macOS machine for the perf gate.

## Owners (09 §6)

- **Release owner:** signing identities, releases, staged rollout, rollback.
- **Feeds owner:** catalog (`crates/genie-catalog/catalog/bundled.json`) and directory
  curation (`feeds/`), kill switches.
- **Provider-layer owner:** nightly drift report (live smoke), monthly vendor-docs diff.
