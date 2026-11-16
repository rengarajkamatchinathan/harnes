# 35 — Commercial model, licensing and telemetry

type: wayfinder:grilling · status: closed · claimed-by: charting session (2026-11-16) · blocked-by: 29

## Question
How is Genie sold and measured?

## To decide
- Seat licence vs BYO key vs open-core
- licence checks
- Telemetry and crash reporting: opt-in, no prompt content
- Third-party licence compliance (OpenCode MIT attribution: C3)

## References
- (none in the OpenCode knowledge base)

## Resolution
Resolved 2026-11-16 (partly; the rest is deferred by the user).

### Deferred (user): "for later, not now" → ticket [41](41-business-model-deferred.md)
- Business model (seat subscription / free + enterprise / bundled credits).
- Licence enforcement (online activation / signed file / none).
- Source licensing (closed / source-available / open-core).
- **Effect on v1 design:** keep a `licence` module boundary in the daemon and the Settings → **Account & licence** section (D34), but for now it shows only About, version and third-party licences. Nothing else in the map depends on these answers.

### Telemetry (user): crash reports on, usage opt-in
- **Crash reports: on by default (opt-out)**: stack traces, app/daemon version, OS, and the anonymised module path. **No prompts, responses, file contents, file paths in user dirs, or credentials.** Scrubbed on the device before upload.
- **Usage analytics: opt-in**: feature counts and timings (e.g. "artifact created", "MCP server added"), never content or model inputs.
- First-run consent screen; Settings → Privacy & data toggles (D34); **admins can force either off** with `telemetry.level` (D38). A random install id, rotatable, never tied to identity unless the licence module (41) later needs that.
- Backend choice (self-hosted Sentry-compatible vs vendor) → spec phase; endpoints are configurable for air-gapped orgs (or `off`).

### Action items (tasks, not decisions; owners to be assigned in the spec phase)
1. **Trademark check for "Genie"** (D19): a crowded name in AI products.
2. **Register the Genie multi-tenant Entra app** (public client) in the Synergech tenant + an admin-consent guide (D26).
3. **Check the models.dev data licence** before using it as catalog input (D27).
4. **Third-party notices** (C3): OpenCode MIT notice for any adapted code; generated notices for crates/npm/bundled artifact libraries (D28) and ripgrep (D35); shown in Account & licence.
5. **Signing keys:** catalog, connector/skills directory, policy URL, updates (D27/D30/D37/D38/27); key custody and rotation plan.
6. **Chromium-based browser terms** (D21 uses the user's Chrome/Edge via CDP, with no redistribution); re-check before the v2 CEF browser.
