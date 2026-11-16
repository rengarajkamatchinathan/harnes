# 27 — Distribution, updater and signing

type: wayfinder:grilling · status: closed · claimed-by: charting session (2026-11-16) · blocked-by: 30

## Question
How do we ship and update both clients?

## To decide
- Tauri updater (or Electron's, if 30 flips the decision)
- Windows Authenticode, macOS notarisation, Linux packages
- CLI install and a desktop 'Install CLI' action
- Channels (stable/beta)

## References
- OpenCode ch.14: [14-cli-headless-and-acp.md](../knowledge/opencode/14-cli-headless-and-acp.md) — read TL;DR, Architecture, Genie takeaways
- OpenCode ch.17: [17-desktop-shell.md](../knowledge/opencode/17-desktop-shell.md) — read TL;DR, Architecture, Genie takeaways

## Resolution
Resolved 2026-11-16. User decisions are marked **(user)**; the rest are defaults from KB ch.14/17 and D8/D9/D20/D38.

### Packages in v1 (user): all four groups
| platform | formats | signing |
|---|---|---|
| **Windows** | **MSI** (per-machine, for Intune/GPO; ships the ADMX template, D38) + **MSIX** or per-user installer for individuals + **winget** manifest | Authenticode via **Azure Trusted Signing** |
| **macOS** | **DMG** (universal: arm64 + x64) + **Homebrew cask** | Developer ID, hardened runtime, inside-out signing of app + bundled `genie` binary, one `notarytool` submission, staple, then build the DMG |
| **Linux** | **.deb** + **.rpm** (with Genie apt/yum repos for updates) + **AppImage** | GPG-signed repos and packages; AppImage with an embedded signature + a detached sig file |
| **CLI-only** | `genie` CLI + daemon (one binary) via a **checksum-verified install script** (`install.sh` / `install.ps1`), **brew**, **winget**; for servers/SSH with no desktop | same signing per OS; the install script checks SHA-256 + signature before installing |
- Targets: Windows x64 + arm64, macOS universal, Linux x64 + arm64 (glibc; musl static for the CLI only).
- The desktop app bundles the same `genie` binary (D20). Settings → General gets **"Install `genie` command in PATH"** (symlink/shim; the macOS action mirrors OpenCode's).

### Updates (user): auto-download, restart when ready
- **Desktop:** `tauri-plugin-updater` against Genie's own signed feed (Ed25519 manifest + artifact signatures). Downloads in the background, verified → a **"Restart to update"** badge; if ignored, it's applied on next launch. A failed install rolls back to the previous version.
- **Linux .deb/.rpm:** updates come from the apt/yum repo (the in-app updater only notifies). **AppImage:** in-app updater.
- **CLI-only:** `genie update` (same feed and checks), plus a once-a-day notice in the TUI.
- **Shared daemon (D20):** after an update, the next client handshake sees the version change → the daemon drains (waits for running turns, or offers to stop them) and restarts on the new binary. Pending turns show Resume (D20).
- **Admin control (D38):** `updates.channel`, `updates.auto` (on/off), `updates.minVersion` (forces an upgrade prompt). MSI deployments can turn off the in-app updater entirely (IT pushes versions).

### Channels and hosting (default)
- Channels: **stable**, **beta**, **dev** (internal). Each channel has its own data dir and names (`~/.genie`, `~/.genie-beta`, `genie-beta.db`, socket names), so channels can run side by side.
- **Hosting:** static signed manifests + artifacts on **Azure Storage + CDN** (Synergech's cloud). The same signed-feed infrastructure serves the **model catalog (D27), connector directory (D30), skills directory (D37) and update manifests**: one signing-key hierarchy (custody → 35 action item 5).

### Release pipeline (default; → spec)
- CI builds per target → signing per OS → notarisation (mac) → SBOM + third-party notices (35 item 4) → checksums → publish to the beta channel → staged rollout (10% → 50% → 100% for stable) with crash-rate gates (D39 crash reports).
- Reproducible version metadata (`genie --version` shows channel, commit, build date). Lifecycles (update, relaunch, deep links, window recovery) are pure state machines (KB ch.17).
