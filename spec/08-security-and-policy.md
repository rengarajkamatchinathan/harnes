# 08 — Security and policy

Implements C2, C4, D14, D20–D26, D28, D30, D35–D39, D43, D48, D52; tickets 13, 14, 28, 32, 35, 39, 40. Background: research/browser.md §4, research/sandbox.md, KB "should not copy" list.

## 1. Assets and trust boundaries

**Assets:** provider credentials and OAuth tokens; the DB key; conversation content, memory, project knowledge; the user's files and repos; the user's browser sessions in the Genie profile; the update and feed signing keys; org policy integrity.

**Boundaries:**

| Boundary | Trusted side | Untrusted side |
|---|---|---|
| Daemon socket/pipe | the daemon | any local process (other users; same-user malware is out of scope, see §2.9) |
| Webview ↔ Tauri | Tauri Rust side | webview JS (treated as less trusted; no token) |
| App origin ↔ artifact origin | app UI | artifact content (model-written) |
| Engine ↔ model | engine (policy, permissions) | model output (may be steered by injected content) |
| Engine ↔ content sources | engine | web pages, fetched documents, MCP results, repo files, skills, attachments |
| Engine ↔ child processes | engine | MCP servers, hooks, shell commands, Chrome |
| Genie ↔ network feeds | pinned public keys | CDN, mirrors, policy URL transport |

## 2. Threat model

### 2.1 Prompt injection via web content (search, fetch, browser)

*Threat:* a page tells the model to exfiltrate data, run commands or buy things.
*Mitigations:*
- Untrusted framing: page text returned as `untrusted_web_content` with origin; the system prompt says page text is data (research §4.4).
- Hidden-content stripping in browser snapshots.
- **Deterministic gates in the daemon, independent of the model:** per-site `blocked/ask/read/act` (D21), sensitive-action confirmations (payments, sends, uploads, downloads; no typing into password/OTP/card fields), model-built URL exfiltration guard, step/time caps.
- **Taint** downgrades auto-allows to asks after untrusted content (06 §5.5, SA-29).
- `web_fetch`: SSRF guard, permission per redirect, credential URLs rejected (D35).
- Citations and links open in the **system browser**, not the agent browser (D36).
- Bypass mode still honours deny rules, policy and D21 confirmations (D25).
- Residual risk is accepted and disclosed: injection can be reduced, not eliminated (research risk 2). A red-team suite runs in CI (09 §5).

### 2.2 Prompt injection via MCP servers and tool results

*Threat:* a server returns instructions or a malicious server exfiltrates.
*Mitigations:* every MCP tool asks on first use (once/always per tool/deny); annotations are info only (D25); sampling is always approved by default with cost and budgets (D30); elicitation sensitive fields are flagged; taint applies to untrusted servers; output passes through truncation; the connector directory pins versions and checksums and shows exactly what will run (D30); policy can restrict to directory-only or an allowlist (D38). Local servers run with user privileges (no sandbox in v1; D44) — the add dialog says so.

### 2.3 Malicious repositories

*Threat:* opening a cloned repo runs code via project settings, `.mcp.json`, hooks, skills, agents, `!cmd` commands or a worktree setup command.
*Mitigations:*
- **Workspace trust** (D43): nothing executable loads until the folder is trusted; GENIE.md/AGENTS.md are read as text only (and `@imports` stay inside the project).
- **Definition-hash approval** for each project MCP server, hook and setup command, even in trusted folders; changes after `git pull` show a diff and need re-approval.
- Trust is keyed by path + project identity, so a different repo at the same path is not trusted.
- Home, `/` and system folders can't be trusted as a whole.
- Headless needs `--trust` or a policy root.
- Shell commands still pass permissions; unparseable commands ask (D45).
- Instruction files are prompt text, so they are a prompt-injection vector: they can't grant permissions (only settings can, and project settings need trust).

### 2.4 Artifact escape

*Threat:* model-written HTML/React reaches the app, the daemon or the network.
*Mitigations (D28):* separate custom scheme origin; `iframe sandbox="allow-scripts"` without same-origin; strict CSP with `connect-src 'none'`; no Tauri capabilities on the artifact origin; postMessage-only with a validated schema; no top navigation, popups, downloads or modals; bundled libraries only; SVG sanitised. **Hostile-artifact CI test on all three webview engines** (09 §5). WebKit and WebView2 differ in custom-scheme and sandbox details, which is why the test runs per engine.

### 2.5 Credential theft

*Threat:* keys leak through logs, settings files, the webview, crash reports, child processes or the browser.
*Mitigations:* OS keychain or Argon2id-encrypted file, never plaintext (D26); secrets never in settings files or the plain DB (D23); `secrecy` types with redacted `Debug`; cassette scrubbing and a CI secret scan (t34); the daemon token never reaches JS; hooks and MCP children get a minimal env without provider credentials (D48); the browser category blocklist covers AI provider consoles and API-key pages; crash reports scrubbed on device (D39); `--sanitize` export (D24). OAuth tokens refresh single-flight under a cross-process lock (D26). C2: no consumer subscription tokens.

### 2.6 Local attackers and the daemon endpoint

*Threat:* another user on the machine, or a process exploiting a TCP port, drives the daemon.
*Mitigations:* no TCP port (D14, D20); user-only socket dir (0700) / pipe DACL; peer UID/SID check; random token per daemon start in a user-only file; constant-time compare; `FILE_FLAG_FIRST_PIPE_INSTANCE` against squatting; no HTTP surface, so no CSRF/DNS-rebinding (t02). Chrome is driven over `--remote-debugging-pipe`, not a debug port.

### 2.7 Supply chain

*Threat:* compromised crates/npm packages, a malicious directory entry, a tampered update or feed.
*Mitigations:* `cargo deny` (licences, advisories, sources) and `pnpm audit` in CI; lockfiles committed; SBOM per release (D40); **all feeds signed with Ed25519** and verified against pinned keys (updates, catalog, connector and skills directories, policy URL) (D27, D30, D37, D38, D40); directory entries pinned by version + SHA-256 and re-verified on update; bundled ripgrep checksum-pinned (D35); artifact libraries bundled and pinned (D28); OS code signing and notarisation (D40); plugins are v2 and will be commit-pinned with hash approvals (D52).

### 2.8 Data at rest and data leaving the device

- SQLCipher DB + AEAD blobs; key in the keychain; recovery key optional (D24).
- Telemetry never contains content (D39); audit excludes content unless policy opts in with a visible notice (D38).
- Provider-native code execution uploads files only when used, with a first-use notice (D35).
- Artifacts can't make network requests; favicons go through the daemon (D28, D36).

### 2.9 Out of scope

Malware already running as the same user (it can read the keychain session, attach debuggers, or read the user's repos) is out of scope, as for any desktop app. Genie still avoids making it easier (no plaintext secrets, no open ports).

## 3. Permissions summary

Rules, evaluation order, modes, saving, and taint are in 06 §5. Security-relevant invariants:
1. Policy deny → deny rules → ask → allow → mode default; deny is checked before hooks; nothing converts a deny to an allow (D25, D48).
2. "User declined" is never turned into tool output (D20).
3. Asks are durable and never time out on their own (D22).
4. The engine enforces modes; an agent's prompt can't weaken them (D46).
5. Rewritten hook input is re-checked (D48).

## 4. Workspace trust

See 06 §14 for behaviour. Storage: `~/.genie/trust.json` (user scope, not settings), entries `{canonical_path, identity, decision, decided_at, approvals: [{kind, name, hash}]}`. Policy `trust.mode: prompt | never` and `trust.allowedRoots` (D43, D38).

## 5. Managed policy

D38; t28.

### 5.1 Delivery and merge

- **System file:** `C:\ProgramData\Genie\policy.json`, `/Library/Application Support/Genie/policy.json`, `/etc/genie/policy.json`; must be admin-writable only (the daemon refuses a user-writable policy file).
- **MDM:** macOS configuration profile (domain `com.synergech.genie`); Windows `HKLM\Software\Policies\Genie` via GPO/Intune with a shipped **ADMX** template; Linux = the system file.
- **Signed remote URL** (`policyUrl`, set only by file/MDM): fetched at start and ~hourly; Ed25519 signature with the public key pinned in file/MDM; **last good copy kept** if a fetch fails.
- **Merge:** most-restrictive-wins for allow/deny lists and disabling booleans; scalars by remote > MDM > file. Effective policy with per-key source in Settings → Developer.
- Policy is a **tamper-proof layer above all settings**; a missing or unparsable policy after a valid one keeps the last good copy and raises a Problem.

### 5.2 Keys

| Area | Keys |
|---|---|
| Providers & models | `providers.allowed`, `endpoints.allowed`, `models.allowed/blocked`, `auth.methods`, `catalog.url` |
| Permissions | `permissions.deny/ask`, `disableBypassMode`, `defaultMode` floor |
| MCP | `mcp.allowed` (name/url/package+checksum), `mcp.directoryOnly`, `mcp.disableLocal`, `mcp.sampling: off|ask` |
| Skills | `skills.allowed`, `skills.directoryOnly`, `skills.disableUserUpload`, `skills.scriptsAllowed` (v2) |
| Capabilities | `webSearch.enabled` + `allowedDomains/blockedDomains`, `webFetch.enabled/domains`, `codeExecution.enabled`, `artifacts.enabled`, `browser.enabled` + `siteAllowlist/Blocklist` + `browser.profile: persistent|ephemeral`, `projectRetrieval.embeddingsProvider` |
| Memory & data | `memory.enabled`, `chatSearch.enabled`, `retention.maxDays`, `encryption.required`, `export.allowed`, `incognito.allowed` |
| Trust, hooks, agents | `trust.mode`, `trust.allowedRoots`, `hooks.enabled`, `hooks.managedOnly`, `subagents.maxDepth` |
| Telemetry & audit | `telemetry.level: off|crash-only|usage`, `audit.otlp.endpoint`, `audit.jsonl.dir`, `audit.includeContent` |
| Updates | `updates.channel`, `updates.auto`, `updates.minVersion` |
| Org | `org.name`, org instructions (managed GENIE.md / text) |

`browser.profile`, `hooks.enabled` and `subagents.maxDepth` come from research/browser.md §4.1 ("managed policy can force Ephemeral"), t20 ("policy can disable hooks") and D47 ("policy-capped"); they complete the t28 table.

### 5.3 What users see

Locked settings show a lock and "Managed by <org>"; blocked actions say "Blocked by <org> policy" with the rule; a "Content auditing is enabled by <org>" notice when `audit.includeContent` is on.

## 6. Audit

D38; t28.

- **Events:** permission asks and decisions; tool calls (name, resource, outcome, duration; **no argument content by default**); policy blocks; sign-ins and credential changes (no secrets); MCP connect/auth; skill installs (with hash); browser site approvals and actions; settings changes; usage/cost per request; hook runs.
- **Sinks:** OTLP exporter (`audit.otlp.endpoint`, headers from the keychain) and/or **local rotating JSONL** (`<data dir>/audit/`, 0600, size-capped; or an admin folder).
- **Content** excluded unless `audit.includeContent: true` by policy (with the visible notice).
- Schema: each record `{ts, install_id, user (OS account), org, session_id?, event, attrs}`; versioned (`audit_schema: 1`).
- Durable session events remain the per-user record; audit is the org stream.

## 7. Telemetry

D39; t35.

- **Crash reports: on by default (opt-out).** Stack traces, versions, OS, anonymised module path. No prompts, responses, file contents, paths in user dirs or credentials; scrubbed on device.
- **Usage analytics: opt-in.** Feature counts and timings ("artifact created", "MCP server added"); never content or model inputs.
- First-run consent; Settings → Privacy & data; admin `telemetry.level` can force either off. Random, rotatable install id, not tied to identity.
- Endpoints configurable for air-gapped orgs (or `off`).

**Spec assumption (SA-32):** crash reports use the Sentry protocol (minidumps for native crashes via the `sentry` crate with on-device scrubbing; JS errors via the browser SDK in the webview) to a **self-hosted Sentry-compatible endpoint** on Synergech's Azure; usage analytics go to the same backend as events. Reason: t35 leaves "self-hosted Sentry-compatible vs vendor" to the spec; self-hosting keeps data in Synergech's cloud and the protocol keeps a vendor option open.

## 8. Encryption

D24, D26; details in 04 §2 and 03 §10.

- DB: SQLCipher; blobs: XChaCha20-Poly1305; key: OS keychain or the passphrase file; optional recovery key (SA-16).
- Secrets: never plaintext; Argon2id parameters `m=64 MiB, t=3, p=1` (re-tuned per release; stored with the file).
- Transport: rustls with the OS trust store (`rustls-platform-verifier`) so corporate TLS inspection roots work; no certificate pinning for provider APIs; feeds are protected by signatures, not TLS alone.
- Exports are plaintext only after explicit confirmation (D24).

## 9. Security engineering practices

- Fuzzing (cargo-fuzz) for: the NDJSON/JSON-RPC parser, SSE parser, each protocol decoder, `genie-markdown`, the shell command scanner, the permission rule parser, skill zip unpacking.
- Threat-model review at the end of M2 and M4; an external penetration test before GA covering the daemon endpoint, artifact sandbox, browser gates and updater.
- `unsafe` is forbidden outside `genie-process` (PTY/Job Object FFI), `genie-secrets` (platform APIs) and the Tauri side; each `unsafe` block needs a `// SAFETY:` note and review.
- Security fixes ship through the beta → stable channel with an expedited path that skips the staged-rollout soak for critical issues.
