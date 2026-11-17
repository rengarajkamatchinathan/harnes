# Runbook: cutting a release

Owner: release owner. Spec 09 §5.3, t27. Channels: beta first, then stable (09 §3–4).

1. **Freeze.** Main is green (`pr` + `main` workflows). `cargo xtask ci` passes locally.
   The catalog snapshot is re-checked against vendor pages (03 §8.4).
2. **Version.** Bump `version` in `codebase/Cargo.toml` (one version for the repo, 01 §2.1
   rule 6). If the protocol changed, run `cargo xtask schema --release` to move the
   compatibility baseline (02 §10) and commit it.
3. **Notes.** Add `notices/<version>.md` content to `feeds/notices/index.json` (entry with
   version, date, summary, deprecations).
4. **Tag** `v<version>` → the `release` workflow builds every target, signs (macOS codesign
   + notarytool, Authenticode via Trusted Signing, GPG), writes SBOM, notices, checksums,
   Ed25519 artifact signatures and the update manifests, and uploads to
   `releases/<version>/` + `updates/beta/`. Check the job log: each signing step either ran
   or printed "skipped: <secret> not set". **A release with any skipped signing step must
   not be published to stable.**
5. **Verify what is live** (any machine):
   ```sh
   curl -fsSLO https://<feeds>/updates/beta/linux-x86_64.json{,.sig}
   cargo xtask feed verify linux-x86_64.json --kind updates --pubkey <updates key hex>
   ```
   Install from beta with `install.sh --channel beta` on one machine per OS.
6. **Beta soak** ≥ 48 h. Then dispatch `release` with `channel: stable`: stable manifests
   start at 10%. Continue with [staged-rollout.md](staged-rollout.md).
7. **Package managers** (stable at 100% only): render the Homebrew formula/cask and winget
   templates in `release/packaging/` from the stable manifests and submit; publish the
   .deb/.rpm to the apt/yum repos (signed metadata).

Expedited security release (08 §9): same steps, beta soak may be cut to the time needed to
smoke-test, and stable uses `--expedited` (24 h stages). Record the reason in the release
notes.
