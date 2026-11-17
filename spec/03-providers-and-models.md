# 03 — Providers and models

Implements D2, D18, D26, D27, D36 (citations mapping), D41 (conformance); C2; tickets 03, 04, 14, 34. Background: research/provider-layer.md (the type sketch there is the starting point), KB ch.07–08.

## 1. Decisions in force

- **Build our own adapters; use crates only for plumbing** (D18): `reqwest` (rustls), our own SSE parser, `azure_identity` 1.x where it fits, `google-cloud-auth` for ADC, the `oauth2` crate for Entra PKCE/device code, `keyring` for secrets.
- **4 protocol adapters in v1** (D26): Anthropic Messages, OpenAI Responses, OpenAI Chat, Gemini. **Bedrock (Mantle + ConverseStream) is v2.** `AuthKind::SigV4` and `ProtocolKind::BedrockConverse` stay in the enums, unused.
- **Save the exact wire history** with each session; normalisation is append-only (D18; research §"Two doc findings").
- **Reasoning replay scope:** same protocol family + account for Anthropic; per-protocol rules otherwise (D18).
- **Catalog:** Genie-hosted signed catalog + bundled snapshot + ETag refresh, merged with live discovery and user overrides (D27).
- **No consumer subscription logins** (C2).

## 2. Canonical model

The canonical types follow research/provider-layer.md "Type sketch" with these spec-level rules.

```rust
pub struct Message { id: MessageId, role: Role /* System|User|Assistant|Tool */, parts: Vec<Part>, origin: Option<Origin> }

pub enum Part {
    Text(TextPart),            // text + citations + cache hint + optional opaque
    Media(MediaPart),          // image | pdf | file; source = Blob(BlobId) | Url | ProviderFile(id)
    ToolCall(ToolCall),        // id, name, input, provider_executed, opaque
    ToolResult(ToolResult),    // call_id, Text|Json|Rich(parts), is_error
    Reasoning(ReasoningPart),  // readable text (may be empty), redacted, opaque
    Compaction(CompactionPart),// provider checkpoint (opaque) xor our summary text (D33)
    EffortMarker(EffortMarker),
    Unknown(UnknownPart),      // provider block with no canonical meaning, replayed only in scope
}

pub struct Origin { provider: ProviderId, model: Arc<str>, protocol: ProtocolKind, account: Option<AccountTag> }
pub enum ReplayScope { ProtocolFamilySameAccount, ModelFamily, SameModel, Never }
```

Genie-level parts that never reach a provider (artifact refs, memory chips, subagent cards) live in the store's `part` table (04 §3) with `kind` values outside this enum. The compile step maps them to text the model can read (e.g. `artifact_ref` → "Artifact art_x v3: <title>") or drops them.

**AccountTag.** `AccountTag = first 16 bytes of SHA-256(provider_id ‖ credential_id ‖ tenant_or_project ‖ endpoint_host)`. A change of credential or tenant gives a new tag, so account-bound reasoning blocks are not replayed to a different account.

**Spec assumption (SA-13):** this AccountTag derivation. Reason: research Q6 left it open; including the credential id is the conservative choice (a credential switch never replays account-bound blocks).

## 3. Traits

```rust
pub trait Protocol: Send + Sync + 'static {
    type Body: Serialize + Send; type Frame: Send; type State: Default + Send;
    const KIND: ProtocolKind; const FRAMING: Framing; const REPLAY: ReplayScope;
    fn build_body(&self, req: &CompiledRequest, compat: &Compat) -> Result<Self::Body, ProviderError>;
    fn decode(&self, frame: RawFrame) -> Result<Option<Self::Frame>, ProviderError>; // None = unknown → ignored (raw passthrough)
    fn step(&self, st: &mut Self::State, f: Self::Frame, out: &mut Vec<StreamEvent>) -> Result<(), ProviderError>;
    fn on_halt(&self, st: Self::State, out: &mut Vec<StreamEvent>) -> Result<(), ProviderError>; // no terminal event → IncompleteStream
    fn classify(&self, failure: &HttpFailure) -> ProviderError;
}
pub trait Auth: Send + Sync { async fn apply(&self, req: &mut http::request::Parts, body: &[u8]) -> Result<(), ProviderError>; }
pub trait Provider: Send + Sync { fn id(&self) -> &ProviderId; fn route(&self, model: &ModelSpec) -> Result<Route, ProviderError>; }
pub trait LlmClient { fn stream(&self, route: &Route, req: LlmRequest, cancel: CancellationToken) -> BoxStream<'static, Result<StreamEvent, ProviderError>>; }
```

- **One HTTP `Executor`** sends every request. It applies auth, timeouts (headers 5 min, chunk gap 15 min by default), retries (§7) and the cassette recorder/replayer for tests (D41). No adapter talks to `reqwest` directly.
- `StreamEvent` is the research list: step start/finish, text/reasoning start/delta/end, tool input start/delta/end/error, provider tool result, citation, opaque block, model switched, finish with usage, warning, raw.
- **Tolerant decoding everywhere:** unknown event types and fields are ignored or kept as `Raw`; `#[serde(other)]` on every enum we read. We don't depend on `async-openai` types (research).

## 4. The compile step

`compile(history, request, route) -> CompiledRequest { wire_history, request, diagnostics }`:

1. **Path:** take the active branch path root → leaf (D24), apply compaction (the latest compaction message replaces everything before its kept tail; D33).
2. **Reuse sent history byte-for-byte.** For each message already sent on this route, reuse the stored `wire_record` JSON. Only new messages are normalised. This satisfies Anthropic preserved-thinking prefix checks and keeps caches warm (D18).
3. **Replay filter:** for each `Opaque`, compare its `Origin` with the destination's `ReplayScope`. In scope → send the opaque payload. Out of scope → send readable text only (or nothing for empty reasoning). Anthropic → Anthropic sends everything and lets the API drop what the target can't read.
4. **Tool history repair:** orphan tool calls (no result) get a synthetic `is_error` result "interrupted"; orphan results are dropped; `require_assistant_after_tool` inserts an empty assistant turn where a route needs it.
5. **Tool-id rewrite** per `Compat.tool_id_rule` (Anthropic charset, max 40, Mistral 9, omit). The mapping canonical id ↔ wire id is persisted with the wire record.
6. **Schema sanitising** of tool input schemas for Gemini and similar (`schema_sanitizer`).
7. **Cache breakpoints** (D32): see §6.
8. **Effort and sampling:** effort is mapped from the catalog's per-(protocol, model) table (D27). Sampling fields are sent only if the model lists them in `sampling_supported` (D27; handles Anthropic `2026-10-21`).
9. **Attachments:** images/PDFs native where the model's modalities allow; otherwise PDFs → text (pdf extraction in the daemon) and images refused with a clear message (D32).
10. **Diagnostics** (e.g. "dropped 2 reasoning blocks: different account") are logged and shown in the Developer view.

The result is deterministic for the same inputs. Golden tests pin it per route (§9).

## 5. The four v1 adapters and routes

| Protocol | Framing | Replay scope | v1 routes | Auth |
|---|---|---|---|---|
| **Anthropic Messages** | SSE | ProtocolFamilySameAccount | Anthropic API; Claude in Azure AI Foundry; Claude on Vertex (`streamRawPredict`) | `x-api-key`; Foundry `x-api-key` or Entra bearer; Google ADC |
| **OpenAI Responses** | SSE | ModelFamily | OpenAI; Azure `/openai/v1` | Bearer; Azure `api-key` or Entra bearer |
| **OpenAI Chat** | SSE + `[DONE]` | SameModel | Azure Foundry non-OpenAI models; Ollama; vLLM; OpenRouter; generic OpenAI-compatible | `api-key` / Entra / optional bearer / none |
| **Gemini** | SSE (`alt=sse`) | SameModel | Gemini API; Vertex Gemini | API key; Google ADC |

Per-protocol notes (from research/provider-layer.md and t34's checklist):

- **Anthropic Messages:** `anthropic-version` per model from the catalog's `api_version` (D27, resolves research Q2). Thinking/redacted thinking blocks and signatures kept byte-exact. Effort via `output_config.effort` on models that use it (budget tokens return 400 on 5.x). Server tools: `web_search`, `code_execution`, with `allowed_domains/blocked_domains` and `max_uses` (D36). Citations: `web_search_result_location` → spans. Native compaction where `capabilities.native_compaction` (D33). Some Foundry deployments reject newer web tools: availability comes from the catalog per route (t15).
- **OpenAI Responses:** `reasoning.effort` from the catalog levels; `reasoning.summary` opt-in; encrypted reasoning items (`include: ["reasoning.encrypted_content"]`, `store: false`). We do **not** use `previous_response_id` in v1 (research "Defer"). Tools: `web_search`, `code_interpreter`. `url_citation` annotations → spans. `prompt_cache_key` = root session id (D32). Azure deployments that answer "Model not supported" on Responses fall back to Chat when `prefer_responses` is set.
- **OpenAI Chat:** `Compat` flags cover dialects: `max_tokens_field`, `reasoning_field` (Ollama `reasoning`, OpenRouter `reasoning_details`), `require_finish_reason`, `stream_usage` (`stream_options.include_usage`), `cache_format`. Missing finish reason with tool calls → `ToolCalls` (stop reasons are derived, not copied; KB ch.07).
- **Gemini:** `thinkingLevel` or `thinkingBudget` per model; `thoughtSignature` on parts and function calls kept and replayed same-model only; Google Search grounding → `groundingMetadata` → spans (D36); code execution tool; "last breakpoint only" caching via implicit/explicit cache per catalog.

**Provider presets** (what the user picks in Settings, D26): Anthropic, OpenAI, Azure AI Foundry, Google (Gemini API key or Vertex ADC), Ollama (local default `http://127.0.0.1:11434`), OpenRouter, vLLM / custom OpenAI-compatible. Presets are data: a route template + default `Compat`.

**Size:** research estimates 8–12k lines of Rust for five protocols; four adapters plus the shared core is the basis for the estimates in 10 §3 (M1).

## 6. Caching

From D32 and t11:

- Up to **4 breakpoints**: end of tools, end of system block 1 (base prompt), end of system block 2 (context baseline), conversation tail. Placed only where the route honours them (`Compat.max_cache_breakpoints`, catalog `capabilities.prompt_cache`).
- **Baseline placement** (06 §2.1, SA-41): the baseline goes in a second system block (Anthropic system array, OpenAI Responses developer message, Gemini `systemInstruction` parts). Chat dialects without multi-system support fall back to a first user message in `<context>` (`Compat.baseline_as_user`). `<system-update>` lowering per protocol is in 06 §2.6.6; both are covered by conformance tests.
- **TTL** from the catalog: 5 min default; **1 h on the baseline breakpoint when it contains project knowledge** and the model supports it.
- OpenAI: automatic caching + `prompt_cache_key`. Gemini: last breakpoint only. Ollama/vLLM: none.
- **Shared cache key** per root session; subagents reuse it (D47).
- Usage records `cache_read`, `cache_write_5m`, `cache_write_1h` separately and cost uses catalog prices for each (t04).
- Changing effort or thinking mode mid-session invalidates Anthropic's message cache; where `mid_conversation_system` is supported the change is sent as an `EffortMarker` system message instead (research).

## 7. Errors and retry

`ProviderError { kind, status, provider_code, request_ids, retry: RetryHint, message, body_excerpt }`, `kind` as in research (`ContextOverflow`, `PayloadTooLarge`, `ContentPolicy`, `QuotaExceeded`, `Authentication`, `RateLimited`, `Overloaded`, `ProviderInternal`, `InvalidRequest`, `ReplayRejected`, `UnsupportedOnDeployment`, `Transport{phase}`, `Timeout{phase}`, `IncompleteStream`, `InvalidProviderOutput`, `Cancelled`, `Unknown`).

Classification order: protocol-specific codes → `x-should-retry` → status → the data-driven **context-overflow regex corpus (~40 patterns, never on 429)** → body heuristics → `Unknown` (KB ch.07).

**Retry policy** (ported from OpenCode `model-retry`, KB ch.03/07):

| Case | Policy |
|---|---|
| `RateLimited`, `Overloaded`, `ProviderInternal`, `IncompleteStream`, transport before send | Jittered exponential backoff from 2 s, cap 10 s per gap, max 10 retries; `retry-after(-ms)` honoured up to 15 min |
| Network unreachable (DNS, ENETUNREACH) | Separate 1 h budget, gaps up to 30 s; UI shows "Offline — retrying" |
| `Timeout` | Max 3 retries |
| `x-should-retry: false` | Never retried; `true` forces retry |
| `QuotaExceeded`, `Authentication`, `InvalidRequest`, `ContentPolicy`, `Cancelled` | Never retried |
| `Authentication` with Entra/OAuth | One forced token refresh, then a single retry (D26) |
| `ContextOverflow` | Not retried by the executor; the engine compacts and re-sends with the 85→70→50→35% tail steps (D33) |
| `ReplayRejected` | Recompile once with opaque reasoning dropped for that destination; log a diagnostic |
| Failure after output started | Settle the partial part; retry starts a **fresh assistant message** (KB ch.03) |
| One-shot calls (titles, summaries, embeddings) | Only rate limits, 503/529 and not-sent transport errors, max 3 |

Each retry publishes durable `retry.scheduled` (02 §4.1). The user sees a banner with a countdown and **Retry now / Stop**.

## 8. Model catalog

### 8.1 Sources and load order (D27, t04)

1. **Bundled snapshot** in every release (offline floor).
2. **Cached last fetch** (`<data dir>/cache/catalog.json` + signature).
3. **Background refresh** on start and every ~6 h, `If-None-Match` ETag; verify Ed25519 signature against the pinned feed key (D40). A failed refresh keeps the last good copy.
4. **Live discovery** merged on top: Azure Foundry deployments (management API when signed in with Entra; else the user enters deployment name + base model), OpenAI `/v1/models`, Gemini `models.list`, Ollama `/api/tags` + `/api/show`, OpenAI-compatible `/v1/models`. Unknown discovered models get conservative defaults and a **"limited info"** badge.
5. **User overrides** `models.<id>` in settings (context window, price, capabilities).
6. **Admin policy** hides providers/models and can point `catalog.url` at an internal mirror (D38).

Each entry is decoded tolerantly; a bad entry is dropped with a diagnostic, not the whole catalog.

### 8.2 Entry schema

Fields as in t04: `id, provider, protocol, wire_model_id, display_name, family, release_date, deprecated_at?, context_window, max_output, input_modalities[], output_modalities[], capabilities{tools, parallel_tools, vision, pdf, web_search_native, code_execution_native, native_compaction, prompt_cache{auto, breakpoints, ttl_1h}, reasoning{toggle, levels[], budget{min,max}, adaptive}, citations, structured_output, prefers_patch, embeddings?}, sampling_supported[], api_version?, quirks (ModelPatch list), pricing{input, output, cache_read, cache_write_5m, cache_write_1h, web_search_per_call?, per:"1M"}, small_model_for?`.

`code_execution_native` (D35), `prefers_patch` (D35) and `embeddings` (D32) are used by later decisions; they are listed in t04's `capabilities{…}` spirit.

### 8.3 Behaviour driven by the catalog

- **Effort selector** shows only the model's levels (`Off/Low/Medium/High/Max` subset); hidden without reasoning; saved per session; default per model from Settings (D27).
- **Sampling settings** appear only in the per-model **Advanced** panel and only if supported; never sent otherwise.
- **Small model** for titles and compaction summaries: same provider and credential, cheapest model with `small_model_for`, minimal effort; fallback to the primary model at lowest effort; user-overridable (t04).
- **Picker:** grouped by provider, recents first, badges (vision, PDF, reasoning, web search, limited info), price per 1M and context window on hover; recents in `state.json` (D23).
- **Cost** = usage × catalog prices, including cache lines, search fees and compaction (D27, D33, D36).

### 8.4 Licence gate

models.dev data may feed the catalog **only if its licence allows commercial reuse** (t35 action item 3). Until that is confirmed, the catalog is built from provider docs only.

## 9. Embeddings (project retrieval)

D32 uses embeddings when an embedding-capable provider is configured (OpenAI, Azure, Gemini; Anthropic has none). A small `EmbeddingClient` trait sits next to `LlmClient` and goes through the same Executor (cassettes, retry, auth).

**Spec assumption (SA-14):** default embedding models are the catalog entries flagged `embeddings.default` per provider (initially OpenAI/Azure `text-embedding-3-small` and the current Gemini text-embedding model). The provider used is the first configured in the order: the provider of the chat's model → OpenAI → Azure → Gemini; policy `projectRetrieval.embeddingsProvider` overrides (D38). Reason: D32 says "when an embedding provider is available" without choosing one.

## 10. Credentials and auth flows

From D26 and t14.

### 10.1 Store

- Backends: macOS Keychain, Windows Credential Manager, Linux Secret Service (`keyring`); else **`secrets.enc`** (XChaCha20-Poly1305, key from a passphrase via **Argon2id**; passphrase prompted at daemon start in the CLI/desktop unlock dialog, or `GENIE_PASSPHRASE`). Never plaintext. `secrets.backend: "file"` forces the file.
- The same store holds the **DB key** (D24).
- Model: each provider has **multiple labelled credentials** (`key | oauth | external`), one active. The UI shows the source ("Keychain", "Environment: OPENAI_API_KEY").
- **Precedence:** an explicitly selected stored credential beats env vars; env vars are used when nothing is stored.

### 10.2 Flows per provider

| Provider | v1 methods | Notes |
|---|---|---|
| Anthropic | API key | |
| OpenAI | API key | |
| Azure AI Foundry | API key; **Entra sign-in**: auth-code + PKCE in the system browser with a loopback redirect; **device code** for SSH/CLI | Scope `https://ai.azure.com/.default`. Needs the **Genie multi-tenant Entra app** (public client) registered in the Synergech tenant (t35 item 2). Own OAuth client via `oauth2`. |
| Google | Gemini API key, or Vertex via **ADC** (`gcloud auth application-default login` or a service-account JSON path) | `google-cloud-auth` |
| Ollama / vLLM / OpenRouter / custom | optional key or bearer, or none | |

- **Refresh:** single-flight per credential plus a cross-process file lock; re-read under the lock and drop our refresh if another process already rotated the token (pick-list #44).
- **Headless/SSH:** device code (Entra) or the pasted-redirect fallback for other OAuth flows.
- **Not in v1:** Azure CLI/azd reuse, managed/workload identity, AWS (Bedrock).
- **Policy** can restrict providers, endpoints and auth methods (e.g. Entra only) (D38).
- **C2:** no Claude Pro/Max, ChatGPT Plus or Copilot logins; no borrowed client ids.

## 11. Conformance suite

From D41 and t34. Lives in `genie-testkit` + `crates/genie-providers/tests`.

| Layer | What | When |
|---|---|---|
| 1. Cassette unit tests | Raw request bytes + raw streamed response bytes per adapter, replayed through the Executor | Every PR, no network |
| 2. Normalisation golden tests | Canonical history → exact wire JSON per protocol and route (API / Foundry / Vertex; OpenAI / Azure; Gemini / Vertex; compatible + Compat flags) | Every PR; snapshot diffs reviewed |
| 3. Catalog patch tests | Each `ModelPatch`/quirk has a case (e.g. sampling stripped for `2026-10-21` models, effort mapping) | Every PR touching the catalog |
| 4. Live smoke | Small matrix against real endpoints when `GENIE_TEST_<PROVIDER>_*` keys exist; re-records cassettes and flags drift | Nightly + before release; never on fork PRs |

**Checklist per protocol** (t34): streaming reassembly, finish reasons, usage incl. cache; tool calls (single, parallel, streamed args, round-trip, repair, id rewrite); reasoning streaming and replay rules with byte-exact signatures and account-bound models; caching placement and TTLs; native web search + citations; code execution parts; native compaction; attachments incl. too-large errors; **errors** (the ~40 overflow patterns, rate limit vs quota, `retry-after`, `x-should-retry`, 5xx, expired Entra token, mid-stream disconnect, cancellation); OpenAI-compatible quirks (Ollama `reasoning`, `max_tokens` field, missing finish reason, stream usage); Azure (deployment ids, `api-key` vs bearer, `apim-request-id` logged).

**Rules:** a new provider, route or quirk merges only with cassettes and golden tests. The recorder scrubs keys, org and account ids; a CI check fails on secrets in fixtures. **A named provider-layer owner** reviews the monthly drift report and the monthly vendor-docs diff (D18, t34).

## 12. Not in v1

Bedrock (Mantle + Converse) and `AuthKind::SigV4`; Responses WebSocket transport; `previous_response_id`; Gemini Interactions API; image generation and other media clients; Azure CLI/managed identity; a `genai`-backed long-tail adapter (research Q4); a Genie gateway (research Q5, tied to t41).
