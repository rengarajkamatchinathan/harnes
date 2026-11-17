# 09 — Distribution and operations

Implements D8/D9 (platforms), D27/D30/D37 (feeds), D28 (hostile-artifact test), D39 (crash gates), D40, D41 (conformance, repo layout); tickets 27, 34, 35, 38, 41.

**Deferred:** the repo host and **CI platform are not decided** (D41 → ticket 41). This file is written CI-agnostic: it lists jobs, runners and secrets, not workflow syntax. Signing integration, secrets for live provider tests and the drift checks all depend on that choice, so ticket 41's CI part should be decided **before M1 week 4** (10 §8).

## 1. Packages

| Platform | Formats | Signing | Targets |
|---|---|---|---|
| Windows | **MSI** per-machine (Intune/GPO; ships the **ADMX** template, D38) · **MSIX** or per-user installer · **winget** manifest | Authenticode via **Azure Trusted Signing** | x64, arm64 |
| macOS | **universal DMG** · **Homebrew cask** | Developer ID, hardened runtime, inside-out signing of the app and the bundled `genie` binary, one `notarytool` submission, staple, then build the DMG | universal (arm64 + x64) |
| Linux | **.deb** + **.rpm** with signed **apt/yum repos** · **AppImage** | GPG-signed repos and packages; AppImage with embedded signature + detached `.sig` | x64, arm64 (glibc) |
| CLI-only | `genie` (CLI + daemon) via **install.sh / install.ps1** (SHA-256 + signature checked before install), **brew**, **winget** | same per-OS signing | as above + **musl static** Linux CLI |

- The desktop bundles the same `genie` binary (D20). Settings → General → **Install `genie` command in PATH** (symlink on macOS/Linux, shim on Windows).
- Each release ships an SBOM (CycloneDX) and generated **third-party notices** (crates, npm, bundled artifact libraries, ripgrep, OpenCode MIT for adapted code; t35 item 4).
- Linux packages declare WebKitGTK 4.1 and set the NVIDIA DMABUF workaround in the launcher when an NVIDIA GPU is detected (05 §12).

## 2. Updates

- **Desktop:** `tauri-plugin-updater` against Genie's signed feed (Ed25519 manifest + artifact signatures). Background download → verified → **"Restart to update"** badge; applied on next launch if ignored; failed install **rolls back** to the previous version (D40).
- **Linux .deb/.rpm:** updates through apt/yum; the in-app updater only notifies. **AppImage:** in-app updater.
- **CLI-only:** `genie update` (same feed and checks) and a once-a-day notice in the TUI.
- **Shared daemon:** the next handshake after an update sees the new client version → the daemon drains (waits for running turns or offers to stop them) and restarts on the new binary; interrupted turns show Resume (D40, 01 §4.2).
- **Admin control:** `updates.channel`, `updates.auto`, `updates.minVersion` (forces an upgrade prompt). MSI deployments can turn the in-app updater off (IT pushes versions).
- **DB safety:** a pre-migration encrypted backup is kept for rollback (04 §5).

## 3. Channels

`stable`, `beta`, `dev` (internal). Each has its own data dir, DB name, socket/pipe names, bundle id and app name suffix ("Genie Beta"), so channels run side by side (D40). `genie --version` shows channel, commit and build date.

## 4. Signed-feed infrastructure

One infrastructure on **Azure Storage + CDN** (Synergech's cloud) for (D40):

| Feed | Content | Refresh |
|---|---|---|
| `updates/<channel>/<os>-<arch>.json` | version, notes, artifact URLs + signatures, `min_version`, rollout % | on start + every 6 h |
| `catalog/v1/catalog.json` | model catalog (D27) | on start + ~6 h, ETag |
| `directory/connectors/v1.json` | connector directory (D30) | daily + on open |
| `directory/skills/v1.json` | skills directory (D37) | daily + on open |
| `notices/` | release notes, deprecations | with updates |

- Feeds are static JSON with a detached Ed25519 signature (`.sig`) and a `{version, issued_at, expires_at}` header; clients reject expired or rolled-back feeds (a lower `version` than the cached one).
- Air-gapped orgs mirror the feeds and point `catalog.url`, update URLs and directory URLs at the mirror via policy (D27, D38).
- `feeds/` in the repo holds the source data and the signing scripts (t38).

**Spec assumption (SA-31):** key hierarchy: an **offline root Ed25519 key** (hardware-backed, two custodians) signs a short **key list** naming one online signing key per feed type (updates, catalog, directories, policy-signing helper for customers is customer-owned); clients pin the root key; online keys rotate yearly or on compromise by publishing a new root-signed key list. Release binaries are signed separately with the OS code-signing identities. Reason: t35 action item 5 asks for "key custody and rotation plan" but no design exists; one pinned root lets keys rotate without an app update.

### 4.1 Staged rollout

Stable releases go 10% → 50% → 100% with crash-rate gates from D39 crash reports (D40). Beta first.

**Spec assumption (SA-33):** each stage soaks at least 48 h and advances only if the crash-free session rate is ≥ 99.5% and not more than 0.2 points below the previous stable; a failed gate pauses the rollout automatically. Critical security fixes may use an expedited 24 h path. Reason: D40 requires crash-rate gates but sets no thresholds.

## 5. CI

Repo layout per t38 (Cargo + pnpm monorepo). Jobs, by trigger:

### 5.1 Every PR

| Check | Purpose | Source |
|---|---|---|
| `cargo fmt`, `clippy` with workspace lints (`unwrap_used`, `await_holding_lock`, no stray `tokio::spawn`) | lint-as-architecture | t01, t38 |
| `cargo deny` (licences, advisories, bans, sources) + `pnpm audit` | supply chain | t01 |
| **`genie-protocol` has no internal deps** | layering | D20 |
| **Protocol + config schema drift:** regenerate, `git diff --exit-code` | generated code never hand-edited | D22, t38 |
| **Protocol compatibility** vs last release schema | semver discipline | 02 §10 |
| Unit tests (Rust, TS), contract tests over the in-memory transport | correctness | t02 |
| **Provider conformance layers 1–3:** cassettes, golden wire tests, catalog patch tests | provider correctness | D41, t34 |
| **Secret scan** of fixtures and cassettes | credential safety | t34 |
| OpenCode-derived files carry the MIT header (path list check) | C3 | t38 |
| Frontend: typecheck, lint (no `@tauri-apps/*` outside `platform/tauri`), component tests, axe checks | D8, SA-22 | |

### 5.2 Merge to main / nightly

| Check | Notes |
|---|---|
| Build all targets (Windows x64/arm64, macOS universal, Linux x64/arm64, musl CLI) | unsigned for nightlies on `dev` |
| **Hostile-artifact test** on WebView2, WKWebView, WebKitGTK | D28; fails the build on any successful IPC/fetch/navigation attempt |
| **Renderer performance gate** (prototype-37 harness) on all three engines, incl. Linux NVIDIA + X11/Wayland | D8, D57; relative frame targets (05 §7) |
| End-to-end desktop tests (WebDriver via `tauri-driver` on Windows/Linux; macOS via a native harness) | smoke flows: first run, chat stream, artifact, ask |
| Cross-OS daemon tests: socket/pipe security, ensure race, version skew, crash → Resume | 01 §4 |
| Shell and process tests per OS (PowerShell, Git Bash, zsh; kill-tree) | M5 |
| **Browser injection red-team suite** with sandboxed malicious pages (Chrome for Testing in CI only) | research §6 |
| Fuzzers (time-boxed) | 08 §9 |
| **Live provider smoke (conformance layer 4)** with keys from CI secrets; re-records cassettes; drift report | nightly + pre-release; never on fork PRs (t34) |

### 5.3 Release pipeline

Build per target → OS signing → notarisation (macOS) → SBOM + notices → checksums → publish to **beta** → staged rollout for **stable** (§4.1) (t27). Lifecycles (update, relaunch, deep links, window recovery) are pure state machines with unit tests (KB ch.17).

**Spec assumption (SA-34):** CI needs, whatever the platform: hosted Windows, macOS (Apple silicon) and Linux runners; **one self-hosted Linux box with an NVIDIA GPU** (X11 + Wayland sessions) and one Intel/AMD Linux box for the WebKitGTK performance gate; **one macOS machine on the oldest supported macOS** (SA-21) for WKWebView perf; secure secret storage for signing (Azure Trusted Signing identity, Apple notarisation key, GPG key) and provider test keys. Reason: ticket 41 defers the platform, but the hardware needs follow from D8/D9 and must be budgeted now.

## 6. Operations

- **Owners:** a named **provider-layer owner** (D18, t34) for the nightly drift report and monthly vendor-docs diff; a **feeds owner** for catalog and directory curation; a **release owner** for signing and rollout.
- **Ongoing load** (after GA): provider upkeep ~0.25–0.5 FTE (4 protocols, monthly vendor changes); catalog + directory curation ~0.1–0.2 FTE; Chrome/Edge are patched by their vendors in v1, so no browser patch load until the v2 CEF browser (≈0.25 FTE then, research/browser.md).
- **Support diagnostics:** Settings → Developer → Copy diagnostics (sanitised); `genie doctor`; `genie export --sanitize`.
- **Incident response:** feed kill switch (publish a feed that disables a broken catalog entry or directory item); update rollback by re-publishing the previous version with a higher feed `version`; `updates.minVersion` to force upgrades past a vulnerable build.
