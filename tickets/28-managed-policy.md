# 28 — Managed (enterprise) policy

type: wayfinder:grilling · status: closed · claimed-by: charting session (2026-11-16) · blocked-by: 08,13

## Question
How do admins enforce settings?

## To decide
- Tamper-proof managed layer (OpenCode: only 'policies' is truly tamper-proof)
- Allowed providers, models and endpoints
- MCP allowlist
- Delivery: file, MDM, registry

## References
- OpenCode ch.06: [06-config-permissions-and-policy.md](../knowledge/opencode/06-config-permissions-and-policy.md) — read TL;DR, Architecture, Genie takeaways

## Resolution
Resolved 2026-11-16. User decisions are marked **(user)**; the rest collects what earlier decisions sent here.

### Delivery (user): file + MDM + signed remote URL
- **System file:** `C:\ProgramData\Genie\policy.json` · `/Library/Application Support/Genie/policy.json` · `/etc/genie/policy.json`; admin-writable only (the daemon refuses a policy file that the user can write).
- **MDM:** macOS configuration profile (`com.synergech.genie` domain), Windows Group Policy / Intune via `HKLM\Software\Policies\Genie` (ADMX template shipped), Linux = the system file.
- **Signed remote URL** (`policyUrl` set by file/MDM only): the daemon fetches it at start and every ~1 h. It must be signed (Ed25519; the public key is pinned in the file/MDM). **If a fetch fails, the last good copy is kept**, so policy never silently lifts.
- **Merge:** the sources are combined with **"most restrictive wins"** for allow/deny lists and booleans that disable things. For scalar values, the order is remote URL > MDM > file. The effective policy is shown in Settings → Developer, with the source of each key.

### Tamper-proofing (default)
- Policy is a **separate layer above all settings** (D23 layer 1). Nothing at user/project/local/CLI level can loosen it. **Policy denies are checked first** (D25), before hooks (→ 20). Agent definitions (→ 18) can't override them (the KB found that OpenCode's agent rules can).
- The daemon re-reads policy on change; a policy that is missing or fails to parse after having been valid keeps the last good copy and raises a Problem (D23).

### Policy keys (default; from earlier decisions)
| area | keys |
|---|---|
| Providers & models (D26/D27) | `providers.allowed`, `endpoints.allowed` (e.g. only the company Foundry resource), `models.allowed/blocked`, `auth.methods` (e.g. `["entra"]` → no API keys), `catalog.url` (internal mirror) |
| Permissions (D25) | `permissions.deny/ask` (merged on top), `disableBypassMode`, `defaultMode` floor |
| MCP (D30) | `mcp.allowed` (by name/url/package+checksum), `mcp.directoryOnly`, `mcp.disableLocal`, `mcp.sampling: off/ask` |
| Skills (D37) | `skills.allowed`, `skills.directoryOnly`, `skills.disableUserUpload`, `skills.scriptsAllowed` |
| Capabilities | `webSearch.enabled` + `allowedDomains/blockedDomains` (D36), `webFetch.enabled/domains` (D35), `codeExecution.enabled` (D35), `artifacts.enabled` (D28), `browser.enabled` + `siteAllowlist/Blocklist` (D21), `projectRetrieval.embeddingsProvider` (D32) |
| Memory & data (D24/D31) | `memory.enabled`, `chatSearch.enabled`, `retention.maxDays`, `encryption.required`, `export.allowed`, `incognito.allowed` |
| Telemetry & audit | `telemetry.level` (off / crash-only / usage), `audit.*` (below) |
| Updates (→ 27) | `updates.channel`, `updates.auto`, `updates.minVersion` |
| Org identity | `org.name` (shown in "Managed by <org>"), org instructions (D31: the managed GENIE.md / instructions text) |

### Audit (user): OpenTelemetry + local JSONL
- Events: permission asks/decisions (D25), tool calls (name, resource, outcome, duration; **no arguments' content by default**), policy blocks, sign-ins/credential changes (no secrets), MCP connect/auth, skill installs/runs (with hash), browser site approvals, settings changes, usage/cost per request.
- **Sinks:** an OTLP exporter (`audit.otlp.endpoint`, headers from the keychain) for SIEMs, and/or a **local rotating JSONL** (`~/.genie/audit/`, 0600, size-capped; when policy enables it, in an admin-specified folder).
- **Prompt and response content is excluded** unless `audit.includeContent: true` is set by policy; Genie shows a visible "Content auditing is enabled by <org>" notice in that case.
- Durable session events (D24) remain the per-user record; audit is the org-facing stream.

### What users see (default)
- Locked settings: lock icon + "Managed by <org>" (D34). Blocked actions: "Blocked by <org> policy" with the rule name. Developer → Effective policy (read-only).
