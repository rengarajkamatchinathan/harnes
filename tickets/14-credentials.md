# 14 — Credentials and auth

type: wayfinder:grilling · status: closed · claimed-by: charting session (2026-11-16) · blocked-by: 03

## Question
How are provider credentials stored and obtained?

## To decide
- OS keychain (keyring crate). Not plaintext JSON in SQLite as in OpenCode
- Azure: API key + Entra ID (device code, interactive, managed identity)
- Bedrock: AWS profiles/SSO. Vertex: ADC
- Headless Linux fallback
- MCP OAuth tokens
- C2: no consumer subscription OAuth

## References
- OpenCode ch.08: [08-providers-models-and-credentials.md](../knowledge/opencode/08-providers-models-and-credentials.md) — read TL;DR, Architecture, Genie takeaways
- OpenCode ch.11: [11-mcp-and-plugins.md](../knowledge/opencode/11-mcp-and-plugins.md) — read TL;DR, Architecture, Genie takeaways

## Resolution
Resolved 2026-11-16. User decisions are marked **(user)**; the rest are defaults from KB ch.08/11 and research/provider-layer.md.

### Azure AI Foundry (user): API key + Microsoft sign-in
- **API key:** the `api-key` header (OpenAI-style routes) / `x-api-key` (Claude in Foundry), stored in the keychain.
- **Microsoft sign-in (Entra):** OAuth 2.0 auth-code + PKCE in the system browser, with a loopback redirect; **device-code** for SSH/headless/CLI. Scope `https://ai.azure.com/.default`. Refresh tokens are kept in the keychain, with single-flight refresh under a cross-process lock. `azure_identity` 1.x has no interactive-browser flow, so this is our own OAuth client (e.g. the `oauth2` crate), using `azure_identity` only where it fits.
- **New task:** register a **Genie multi-tenant Entra app** (public client, no secret) and publish an admin-consent guide for customer tenants. The Synergech tenant owns it. → 35.
- Not in v1: Azure CLI/azd credential reuse, managed/workload identity. They're small additions later, since the auth chain is pluggable.

### Bedrock (user): **not in v1**
- No Bedrock (neither Mantle nor Converse) in v1. **This narrows D18 for v1 to 4 protocol adapters:** Anthropic Messages, OpenAI Responses, OpenAI Chat, Gemini. The AWS crates (`aws-config`, `aws-sigv4`, `aws-smithy-eventstream`) are not v1 dependencies. Bedrock (Mantle + Converse) is planned for v2. The `Protocol`/`AuthKind` design stays ready for it (`AuthKind::SigV4`).

### No keychain (user): passphrase-encrypted file
- If no OS keychain is available (headless Linux, a minimal desktop with no Secret Service), secrets go to `~/.genie/secrets.enc` (0600), encrypted with XChaCha20-Poly1305 using a key derived from a passphrase with **Argon2id**. The daemon asks for the passphrase at start: the CLI prompts on the TTY and the desktop shows an unlock dialog. For CI it is read from `GENIE_PASSPHRASE`. Never plaintext.
- The same store holds the **D24 database key** in that case. Detection order: OS keychain → encrypted file. A user can force the file store with `secrets.backend: "file"`.

### Defaults
- **Backends:** macOS Keychain, Windows Credential Manager, Linux Secret Service (the `keyring` crate), else the encrypted file above. Secrets never go in settings files (D23) or the plain DB.
- **Model:** each provider has **multiple labelled credentials** (`key | oauth | external`), one active per provider/profile; the Settings UI shows the source ("Keychain", "Environment: OPENAI_API_KEY").
- **Precedence:** an explicitly selected stored credential beats env vars; env vars are used when nothing is stored (good for CI). It's documented and shown in the UI.
- **Other v1 providers:** Anthropic API key; OpenAI API key; **Vertex/Gemini via Google ADC** (`gcloud auth application-default login` or a service-account JSON path) or a Gemini API key; OpenAI-compatible endpoints use an optional key/bearer.
- **MCP OAuth (→ 17):** tokens use the same store and refresh logic. Genie hosts its own OAuth client metadata (not OpenCode's).
- **Refresh:** single-flight per credential plus a cross-process file lock; re-read under the lock and drop our refresh if another process already rotated it.
- **Headless/SSH login:** device code (Entra) or the pasted-redirect fallback for other OAuth flows.
- **C2 holds:** no consumer subscription logins (Claude Pro/Max, ChatGPT Plus, Copilot) and no copied client ids.
- **Admin policy (→ 28):** can restrict allowed providers, endpoints (e.g. only the company's Foundry resource) and auth methods (e.g. Entra only, no keys).
