# Provider layer: adopt a Rust crate or build our own adapters?

> Ticket: [03 — Provider layer](../tickets/03-provider-layer.md) · Researched 2026-11-16 · Status: resolved-pending-review
> Inputs: [OpenCode ch.07](../knowledge/opencode/07-ai-package-and-protocols.md), [OpenCode ch.08](../knowledge/opencode/08-providers-models-and-credentials.md), crates.io/GitHub metadata pulled on 2026-11-16, crate sources read at the versions below, and the official vendor docs listed under Sources.

## TL;DR

- **Recommendation: build our own thin protocol adapters** in a `genie-providers` crate. Use crates only for **auth and transport plumbing**: `reqwest`, `aws-sigv4` + `aws-config` + `aws-smithy-eventstream`, `azure_identity` 1.x, and `google-cloud-auth`. **Don't adopt** `genai`, `rig-core` or `async-openai` as the provider layer.
- **Why not adopt?** No Rust crate meets the OpenCode bar on all three of: (1) block-bound opaque reasoning replay with an explicit replay scope, (2) an error taxonomy that tells quota from rate limit, catches context overflow and honours `x-should-retry`, and (3) Azure Foundry with Entra token refresh. `rig-core` comes closest, but it is a whole agent framework at 0.x. Its changelog has 182 `breaking` entries, and adopting it would make its canonical model ours. `genai` has the best adapter coverage, but it is mostly one maintainer (987 of about 1,100 commits). It keeps reasoning as a plain string with a separate signature part, has no Azure adapter, and by design leaves error classification to the caller.
- **There is still no official Rust SDK** from Anthropic or OpenAI. Anthropic ships SDKs in 7 languages and OpenAI in 6, and Rust is in neither list. The crates.io `anthropic` crate is an unrelated community crate, v0.0.8 from 2024.
- **Five wire protocols cover every v1 target**, which is less than it looks:
  1. **Anthropic Messages** (SSE) also covers Bedrock Mantle, Claude in Foundry, Claude on Vertex (`streamRawPredict`) and the Anthropic-managed Google Cloud endpoint.
  2. **OpenAI Responses** covers OpenAI and Azure `/openai/v1`.
  3. **OpenAI Chat** covers non-OpenAI models on Azure Foundry, Ollama, vLLM and OpenRouter.
  4. **Gemini `streamGenerateContent`** covers the Gemini API and Vertex Gemini.
  5. **Bedrock ConverseStream** (AWS binary event-stream) covers non-Claude Bedrock models and the Claude models that only run on `bedrock-runtime`.
- **Two doc findings change the design OpenCode gives us:**
  - **Anthropic API version `2026-10-21`** returns 400 if a request sends `temperature`, `top_p` or `top_k`, citations in user messages, or `document` blocks in assistant messages.
  - **Anthropic "preserved thinking"** runs prefix checks. If anything before a thinking block changes, the request can fail with a 400. Anthropic also says *don't strip thinking blocks when you switch models*, because the API drops the ones the target model can't read. So request normalisation has to be **idempotent and append-only** over history that was already sent, and the replay scope for Anthropic is **(protocol family, account)**, not "same model" as OpenCode does it.

## Candidates surveyed (as of 2026-11-16)

Metadata comes from the crates.io and GitHub APIs on 2026-11-16. "Recent DL" is crates.io's 90-day download count.

| Crate | Latest | Released | Licence | Maintainers (top committers) | Recent DL | Shape |
|---|---|---|---|---|---|---|
| `genai` | 0.7.5 | 2026-11-14 | MIT OR Apache-2.0 | jeremychone (987 commits), then ≤34 each | 304k | Multi-provider chat client, about 40 adapters |
| `rig-core` (+`rig-bedrock`, `rig-vertexai`) | 0.46.0 | 2026-10-31 | MIT | 0xPlaygrounds org: gold-silver-copper 760, joshua-mo-143 249, cvauclair 160 | 2.2M | Agent framework (completion, tools, RAG, agent harness, catalog) |
| `async-openai` | 0.43.0 | 2026-11-01 | MIT | 64bit (263 commits), others ≤5 | 3.5M | OpenAI API client from the OpenAPI spec; `*-types` features usable alone |
| `aisdk` (lazy-hq) | 0.6.8 | 2026-11-16 | MIT | 2 main devs | 82k | "Inspired by the Vercel AI SDK" port, young |
| `liter-llm` | 2.7.0 | 2026-11-06 | MIT | Goldziher (1,799 commits) | 48k | LiteLLM-style universal client, OpenAI-shaped types, "174 providers" |
| `misanthropic` | 1.0.0-alpha.25 | 2026-10-18 | MIT (crate) / none on the repo | mdegans (307 commits) | 5.6k | Anthropic-only, alpha |
| `llm` (graniet) | 1.3.8 | 2026-04-19 | MIT | graniet (376 commits) | 21k | Multi-backend, agent/chain extras; no release in 7 months |
| `openai-api-rs` | 10.0.2 | 2026-10-03 | MIT | dongri | 138k | OpenAI client |
| `anthropic` / `anthropic-sdk` / `anthropic-ai-sdk` / `anthropic-sdk-rust` / `clust` | ≤0.2.27 | 2024 to Jan 2026 | MIT | individuals | <6k | Stale or small community Anthropic clients |
| `aws-sdk-bedrockruntime` | 1.155.0 | 2026-11-10 | Apache-2.0 | AWS (awslabs/smithy-rs) | 2.5M | **Official** Bedrock runtime SDK (Converse, ConverseStream, InvokeModel*) |
| `aws-sigv4` / `aws-config` / `aws-smithy-eventstream` | 1.6.5 / 1.13.2 / 0.61.6 | Nov 2026 | Apache-2.0 | AWS | 53M / 47M / 35M | **Official** signing, credential chain, event-stream codec |
| `azure_identity` / `azure_core` | 1.0.0 GA (2026-05-12); 1.1.0-beta.2 (2026-11-06) | Nov 2026 | MIT | Microsoft | 9.9M | **Official** Entra credentials (`DeveloperToolsCredential`, `AzureCliCredential`, `ManagedIdentityCredential`, `WorkloadIdentityCredential`, `ClientSecret/Certificate/AssertionCredential`). There is **no `DefaultAzureCredential`** |
| `azure_ai_openai`, `azure_ai_inference` | 0.0.1 | 2025-03-01 | MIT | Microsoft | ~5 | Name placeholders only. No usable Azure OpenAI or Foundry inference client for Rust |
| `google-cloud-auth` / `google-cloud-aiplatform-v1` | 1.19.0 | 2026-11-11 | Apache-2.0 | Google | 19M / 349k | **Official** ADC auth; Vertex gRPC/REST client |
| `gcp_auth` | 0.12.7 | 2026-06-22 | MIT | djc | 6.8M | Community ADC token provider |
| `reqwest-eventsource` / `eventsource-stream` | 0.6.0 / 0.2.3 | 2024 / 2022 | MIT OR Apache-2.0 | jpopesculian | 2.5M / 17M | SSE helpers, unmaintained since 2024 |

**Official Rust SDKs:**
- The Anthropic docs list Python, TypeScript, C#, Go, Java, PHP and Ruby. There is no Rust SDK, and `github.com/anthropics/anthropic-sdk-rust` returns 404.
- The OpenAI docs list TypeScript, Python, .NET, Java, Go and Ruby. They name `async-openai` only as a community library and say OpenAI does not verify community libraries.

## Scoring against the protocols Genie needs

Legend:
- ✓ = implemented (seen in the source at the version listed)
- ◐ = partial, or only reachable by workaround
- ✗ = absent
- ? = not determined

This scoring comes from **reading and grepping the crate source, not from running anything**. Treat every cell as *(unverified at runtime)*.

| Capability | genai 0.7.5 | rig-core 0.46 | async-openai 0.43 | aisdk 0.6.8 | liter-llm 2.7 | misanthropic α25 | llm 1.3.8 |
|---|---|---|---|---|---|---|---|
| **Anthropic Messages:** streaming, tool use | ✓ | ✓ | ✗ | ✓ | ✓ | ✓ | ✓ |
| Anthropic: `cache_control` placement | ◐ (tools, some blocks) | ✓ (plus final-breakpoint tests) | ✗ | ✗ | ◐ | ✓ | ◐ |
| Anthropic: thinking with `signature_delta`, `redacted_thinking` | ◐ (reasoning is a plain `String`; signature is a separate `ThoughtSignature` part) | ✓ (`Reasoning{redacted, native}` keeps the provider item plus a fingerprint) | ✗ | ◐ | ◐ | ✓ | ✗ |
| Anthropic: citations (`citations_delta`) | ◐ | ✓ | ✗ | ◐ | ✗ | ✓ | ✗ |
| **OpenAI Responses:** reasoning items with `encrypted_content` | ✓ | ✓ | ✓ (types and client) | ◐ | ✗ | ✗ | ◐ |
| **OpenAI Chat Completions** | ✓ | ✓ | ✓ | ✓ | ✓ | ✗ | ✓ |
| **Azure Foundry:** `/openai/v1` + Claude `/anthropic/v1` + Entra refresh | ✗ (no adapter; a custom endpoint and auth resolver *(unverified)*) | ◐ (legacy `api-version` URL, static `AZURE_API_KEY`/`AZURE_TOKEN`, no Entra refresh) | ◐ (`AzureConfig` = api-key + `api-version`; v1 through the base URL; no Entra) | ✗ | ◐ | ✗ | ◐ |
| **Bedrock:** Converse/InvokeModel for Anthropic | ✓ (Converse; SigV4 and bearer; replays signed and redacted reasoning) | ◐ (`rig-bedrock` sits on `aws-sdk-bedrockruntime`) | ✗ | ◐ | ✓ | ✗ | ◐ |
| Bedrock Mantle (`/anthropic/v1/messages`) | ? | ? | ✗ | ? | ? | ✗ | ✗ |
| **Vertex:** Anthropic `streamRawPredict` + Gemini | ✓ (both; `vertex-2023-10-16`) | ◐ (`rig-vertexai` on `google-cloud-aiplatform-v1`; Claude support ?) | ✗ | ✗ | ✓ | ✗ | ✗ |
| **Gemini API:** thought signatures | ✓ (+ `gemini_ix` Interactions) | ✓ (+ Interactions API) | ✗ | ✓ | ✓ | ✗ | ? |
| **OpenAI-compatible:** Ollama, vLLM, OpenRouter | ✓ | ✓ | ✓ (via base URL, no quirk flags) | ✓ | ✓ | ✗ | ✓ |
| Per-model quirk flags (OpenCode `Compatibility`) | ◐ (per-adapter code) | ◐ (per-provider "dialects") | ✗ | ✗ | ◐ | ✗ | ✗ |
| Cancellation | ◐ (drop the stream) | ◐ (drop the stream) | ◐ (drop the stream) | ? | ? | ? | ? |
| Retries and backoff | ✗ by design (exposes `status()` + `headers()` for `retry-after(-ms)` / `x-should-retry`) | ◐ (`retry_after()`, `is_context_overflow()` with an overflow list borrowed from pi-mono; policy is external) | ◐ (`SimpleRetryPolicy` tower middleware) | ? | ✓ (tower) | ? | ✓ (`resilient_llm`) |
| Error taxonomy (overflow, quota vs rate limit) | ✗ | ◐ (overflow yes; quota vs rate limit ?) | ✗ | ✗ | ◐ | ✗ | ✗ |
| Raw-event access | ◐ (`captured_raw_body`, non-stream) | ✓ (`provider_response` / native item) | ◐ (typed, **closed** `ResponseStreamEvent` enum with no `#[serde(other)]`, so a new event type fails to decode) | ? | ? | ? | ✗ |
| Extensibility (add a protocol without forking) | ◐ (`AdapterKind` is a crate enum; a `custom` adapter exists) | ✓ (traits), but a big framework surface | ◐ (`byot` feature) | ◐ | ◐ | ✗ | ◐ |
| API stability | 157 releases, 0.x | 67 releases, 0.x, 182 `breaking` changelog entries | 127 releases, 0.x | young 0.x | 2.x | alpha | 1.x, stale |

**Reading the table:**
- No candidate is a drop-in replacement for OpenCode's `packages/ai`.
- The two serious options are `rig-core` (closest semantics, heaviest) and `genai` (widest adapter coverage, thinnest semantics).
- Both fail on Azure Foundry with Entra and on error classification. Those are D2 must-haves, and they are exactly the parts we would end up writing ourselves anyway.

## Protocol notes (checked against official docs, 2026-11-16)

### Anthropic Messages (Claude API)
- **Version.** `anthropic-version: 2026-10-21` is current. Compared with `2023-06-01`:
  - A request that sends `temperature`, `top_p` or `top_k` gets a 400, *even at the default value*.
  - `citations` on `text` blocks in **user** messages get a 400.
  - `document` blocks placed directly in **assistant** messages get a 400.
  - The SDKs now send the key as `Authorization: Bearer`. The API accepts either that or `X-Api-Key`.
  - **Implication:** the anthropic-version must be a per-route quirk. The sampling controls in the UI must be hidden for Anthropic routes on `2026-10-21`.
- **Stream events:**
  - Order: `message_start`, then for each block `content_block_start`, `content_block_delta`… and `content_block_stop`, then one or more `message_delta`, then `message_stop`.
  - `ping` can appear anywhere. An in-stream `error` event carries a type such as `overloaded_error` (HTTP 529 equivalent).
  - The docs say: "new event types may be added, and your code should handle unknown event types gracefully".
  - `message_delta.usage` is **cumulative**.
  - Some blocks arrive whole, with no deltas: `redacted_thinking`, server tool results (e.g. `web_search_tool_result`), and a `fallback` block at each model boundary of a server-side fallback.
  - Delta types: `text_delta`, `input_json_delta` (partial JSON; parse at `content_block_stop`), `thinking_delta`, and `signature_delta`, which is sent just before `content_block_stop`. `citations_delta` is not on the streaming page *(unverified on the page; seen in crate sources)*.
- **Thinking:**
  - On Claude 5+ thinking is adaptive by default and `display` defaults to `"omitted"`. The block streams an empty `thinking_delta` and then one `signature_delta`.
  - Effort goes in `output_config.effort`. `budget_tokens` returns 400 on 5.x models.
  - Sonnet 5.5's lowest setting is `thinking: {type: "between_tools"}`.
  - Progress-update `thinking` blocks (each with its own signature) can appear before tool calls on some 5.x models.
- **Replay rules:**
  - Pass every `thinking` and `redacted_thinking` block back **unchanged and in order**. Within the latest assistant turn you can't reorder, edit or partially drop them. This is mandatory with tool use.
  - Filtering on `type == "thinking"` alone silently drops `redacted_thinking` and breaks the protocol.
  - **Preserved-thinking models** (Fable/Mythos 5.1–5.2, Opus 5.5, Sonnet 5.5, Haiku 5.5) run a **prefix check**. If you change `system`, `tools` or an earlier message, the thinking blocks that follow fail the check, and the request can get a 400.
  - When you **switch models**, Anthropic says to keep sending the blocks: "Don't strip thinking blocks when you switch. The API drops unreadable ones without billing them". Stripping also loses fallback credit.
  - Signatures are portable across the Claude API, Bedrock Mantle and Google-managed Vertex. The exception is Sonnet 5.5, Haiku 5.5, Fable 5.2 and Mythos 5.2, whose blocks are **account-bound** (they work only in the producing or a linked account).
  - **Conclusion:** Anthropic replay scope = *Anthropic protocol family + account linkage*. It is not "same model", which is what OpenCode uses.
- **Prompt caching:**
  - Breakpoints go in `cache_control: {type:"ephemeral", ttl?: "5m"|"1h"}`. Placement:
    - on tool definitions;
    - on `system` blocks;
    - on message content blocks (text, image, document, `tool_use`, `tool_result`);
    - not on thinking blocks, empty text or citation sub-blocks.
  - **At most 4** explicit breakpoints.
  - A **top-level `cache_control`** turns on automatic caching. It uses one of the 4 slots and returns 400 on Bedrock `bedrock-runtime`. 1h entries must come before 5m entries.
  - Usage fields: `input_tokens` (after the last breakpoint), `cache_read_input_tokens`, `cache_creation_input_tokens`, and `cache_creation.{ephemeral_5m_input_tokens, ephemeral_1h_input_tokens}`.
  - What invalidates the cache:
    - **Tools:** any change invalidates everything.
    - **Thinking mode/budget and `output_config.effort`:** invalidate messages.
    - **Mid-conversation `{"role":"system"}` messages:** do **not** invalidate the cache on listed models. This is the per-message effort/instructions path OpenCode uses.
  - Minimum cacheable prompt: 512 to 4,096 tokens depending on the model.

### Anthropic on clouds (all use the Messages protocol)

| Route | Endpoint | Auth | Differences |
|---|---|---|---|
| Bedrock **Mantle** | `https://bedrock-mantle.{region}.api.aws/anthropic/v1/messages` | SigV4 (service `bedrock-mantle`) or bearer token in `x-api-key`; `AWS_BEARER_TOKEN_BEDROCK` beats the credential chain | Standard SSE. Model ids are `anthropic.claude-…`. No server tools, Files, batches or `fallbacks`. Fable 5.2 and Mythos 5.2 are **not** on Mantle (runtime only) |
| Bedrock **runtime** | `/model/{id}/converse-stream` or `invoke-with-response-stream` | SigV4 | AWS binary event-stream, not SSE. Automatic caching returns 400 |
| Claude in **Microsoft Foundry** | `https://{resource}.services.ai.azure.com/anthropic/v1/messages` | `api-key` or `x-api-key`, or `Authorization: Bearer` with an Entra token for scope `https://ai.azure.com/.default` (Mythos models need Entra) | `model` = **deployment name**. No `anthropic-ratelimit-*` headers. Log `apim-request-id`. "Hosted on Azure" deployments return 400 for code execution, newer web tools, Skills, PTC and Files |
| Claude on **Vertex** (Google-managed) | `https://aiplatform.googleapis.com/v1/projects/{p}/locations/{global\|us\|eu\|region}/publishers/anthropic/models/{m}:streamRawPredict` | ADC OAuth bearer | `model` goes in the URL, not the body. `anthropic_version` goes in the **body** (`vertex-2023-10-16` or `2026-10-21`). Regional endpoints only serve Sonnet 4.6 and earlier. 30 MB payload cap. Older ids use `@date` |
| Claude on Google Cloud (**Anthropic-managed**) | `https://claude.googleapis.com/v1/projects/{p}/locations/global/workspaces/{w}/invoke/v1/messages` | ADC bearer + `anthropic-version` header | A separate offering. Plain Messages API |

### OpenAI Responses and Chat Completions
- **Responses replay:**
  - With `store: false` (or under ZDR), reasoning items carry `encrypted_content` **by default**. `include: ["reasoning.encrypted_content"]` is now a "legacy value" that is accepted but not required.
  - Replay all output items in their original order.
  - "Persisted reasoning can be reused only within the same model family", and the API omits incompatible reasoning after a family switch.
  - Changing the history before a reasoning item can make the API drop it.
- **Effort.** `reasoning.effort` ∈ `none|minimal|low|medium|high|xhigh|max`, depending on the model. `reasoning.summary` ∈ `auto|concise|detailed`, opt-in only.
- **Stream events** (wire names, from the spec-tracking `async-openai` types):
  - Lifecycle and errors: `response.created`, `response.in_progress`, `response.queued`, `response.completed`, `response.incomplete`, `response.failed`, `error`.
  - Items and content: `response.output_item.added|done`, `response.content_part.added|done`, `response.output_text.delta|done`, `response.output_text.annotation.added`, `response.refusal.delta|done`.
  - Tool arguments: `response.function_call_arguments.delta|done`, `response.custom_tool_call_input.delta|done`.
  - Reasoning: `response.reasoning_summary_part.added|done`, `response.reasoning_summary_text.delta|done`, `response.reasoning_text.delta|done`.
  - Compaction: `response.compaction.compacting`.
  - Hosted tools: `response.{web_search,file_search,code_interpreter,mcp,image_generation}_call.*`, `response.shell_call_*`.
  - Terminal events: `response.completed` and `response.incomplete` (clean), `response.failed` and `error` (hard). The official streaming page names only four events *(the rest are from the SDK types; unverified against the API reference)*.
- **Chat Completions.** Same as OpenCode ch.07 §7: `stream_options.include_usage`, `max_tokens` vs `max_completion_tokens`, and the reasoning-field dialects. Not re-verified here.

### Azure AI Foundry (non-Anthropic)
- **v1 API (GA since Aug 2025).**
  - Base URL is `https://{resource}.openai.azure.com/openai/v1/` or `https://{resource}.services.ai.azure.com/openai/v1/`. There is **no `api-version`**.
  - `model` = deployment name.
  - Auth is `api-key: …`, `Authorization: Bearer <key>`, or `Authorization: Bearer <Entra token>` with scope `https://ai.azure.com/.default`. The role needed is Cognitive Services OpenAI User / Foundry User.
  - **Implication:** Azure is not a separate protocol. It is the Responses and Chat adapters with a different endpoint and auth.
- **Non-OpenAI Foundry models** (DeepSeek, Grok, Llama, MAI…) speak v1 **Chat Completions**. Some also speak Responses. A deployment that doesn't returns `400 Model not supported`, so we need a per-deployment `prefer_responses` flag with a fallback to Chat.
- **Preview features** are opted into with per-feature headers, not API versions. The docs advise parsing only the response objects you need, because new ones get added.
- **Rust auth.** `azure_identity` 1.0 GA gives `DeveloperToolsCredential` (Azure CLI / azd), `ManagedIdentityCredential`, `WorkloadIdentityCredential` and `ClientSecretCredential`. There is no `DefaultAzureCredential`, so Genie composes its own chain.

### Bedrock ConverseStream (`bedrock-runtime`)
- `POST /model/{modelId}/converse-stream` with SigV4. The model id can be a base model, an inference profile or an ARN.
- **Stream members:**
  - Content: `messageStart{role}`, `contentBlockStart{contentBlockIndex,start}`, `contentBlockDelta{contentBlockIndex,delta}`, `contentBlockStop`.
  - End: `messageStop{stopReason, additionalModelResponseFields}`, then `metadata{usage{inputTokens, outputTokens, totalTokens, cacheReadInputTokens, cacheWriteInputTokens, cacheDetails[{inputTokens, ttl}]}, metrics, trace}`.
  - In-stream exceptions: `internalServerException` (500), `modelStreamErrorException` (424), `serviceUnavailableException` (503), `throttlingException` (429), `validationException` (400).
- The delta union includes text, toolUse input, and `reasoningContent{text, signature | redactedContent}` *(delta union members per ch.07; not re-read on the API page)*.
- Request carries `additionalModelRequestFields` (thinking and effort for Claude), `system[]`, `toolConfig`, `cachePoint` blocks *(unverified: cachePoint placement)*, `serviceTier` and `performanceConfig`.

### Gemini API and Vertex Gemini
- `generateContent` / `streamGenerateContent?alt=sse`. On Vertex the same body goes to `…/publishers/google/models/{m}:streamGenerateContent` with an ADC bearer *(Vertex path unverified)*.
- **Signatures.** For `generateContent`, signatures "can be attached to any part, such as living inside `functionCall` parts or the final part of a response". The new **Interactions API** keeps signatures on `thought` steps only. In stateless mode you "MUST always resend all `thought` blocks exactly as they were received".
- **Thinking control** is `thinking_level` (`minimal|low|medium|high|xhigh`).
- These rules come from OpenCode and are *(unverified against current Google docs)*:
  - only the first parallel call needs a signature;
  - the `skip_thought_signature_validator` sentinel;
  - no ids for models below 3.

### OpenAI-compatible (Ollama, vLLM, OpenRouter)
- These are Chat Completions plus OpenCode's `Compatibility` flags: `reasoningField`, `maxTokensField`, `requireFinishReason`, `requireAssistantAfterTool`, `streamUsage`, `cacheFormat`.
- The framing must tolerate `data: null`, keepalive comments and tool deltas without an `index`.
- OpenRouter's `reasoning_details` is replayed verbatim.
- Not re-verified here; ch.07 is the reference.

## Recommendation

**Build `genie-providers` as our own canonical model plus protocol adapters, ported in design from OpenCode `packages/ai`. Use crates for plumbing only.**

| Layer | Choice | Why |
|---|---|---|
| Canonical types, compile/normalise, protocol state machines, error classification, retry policy | **Our code** | This is the product's core. Every candidate crate either lacks it or imposes its own model |
| HTTP | `reqwest` (rustls) behind **one** `Executor`, the single choke point for recording, hooks and timeouts | Mirrors OpenCode's "no HTTP outside `RequestExecutor`" rule. It also makes ticket 34's cassettes possible |
| SSE framing | Our own ~150-line parser | `eventsource-stream` and `reqwest-eventsource` have been unmaintained since 2022–24. We need `data: null`, keepalive and `[DONE]` handling plus raw passthrough anyway |
| AWS | `aws-config` (credential chain, profiles, SSO), `aws-sigv4` (sign `bedrock` and `bedrock-mantle`), `aws-smithy-eventstream` (decode frames, check CRC) | Official and Apache-2.0. Keeps Bedrock on our executor. **Fallback:** `aws-sdk-bedrockruntime` for ConverseStream if hand-signing proves fragile; it is official and released weekly |
| Azure | `azure_identity` 1.x (+`azure_core`) for Entra tokens with scope `https://ai.azure.com/.default` | Official and GA. Our `Auth::Entra` caches and refreshes tokens |
| Google | `google-cloud-auth` (official) for ADC | Vertex Gemini and Vertex/Google-Cloud Claude |
| OpenAI wire types | **Don't depend on `async-openai`** | Its closed enums fail on new events. Write lenient `serde` structs with `#[serde(other)]` / `Unknown(Value)` for the fields we read, and keep the raw JSON |
| Reference material | Read `genai` (Apache/MIT) and `rig-core` (MIT) as **references** for tricky cases (Bedrock reasoning replay, Gemini signatures, Responses replay), the same way we use OpenCode | Their code can be adapted with attribution under C3-style rules. Their API is not taken as a dependency |

**Why not adopt `rig-core`?**
- It is the closest in semantics: it has a `Native` item plus a fingerprint for replay, overflow detection and `retry_after`.
- But it is an agent framework whose canonical types, catalog and harness compete with Genie's own (tickets 04, 06, 18).
- It is 0.x with a breaking change in almost every minor release.
- Its Azure support is legacy-URL and static-token only.
- We would wrap it, fight its abstractions, and still write classification, Azure and Mantle ourselves.

**Why not adopt `genai`?**
- It has the best coverage of target routes and is cleanly licensed.
- But its reasoning model (`ReasoningContent(String)` plus a separate `ThoughtSignature(String)`) can't express Anthropic's ordered, block-bound thinking/redacted sequence with account-scoped replay.
- It has no error taxonomy, no Azure support, and a bus factor of one.
- It would be a fine **escape hatch for the long tail** behind our `Protocol` trait later (see Open questions). It is not the core.

**Scope for v1, desktop chat (D6):** five protocols:
- `anthropic_messages`, with routes for the API, Mantle, Foundry, Vertex and Google Cloud;
- `openai_responses`, with routes for OpenAI and Azure v1;
- `openai_chat`, with routes for Azure Foundry models, Ollama, vLLM, OpenRouter and generic endpoints;
- `gemini`, with routes for the Gemini API and Vertex;
- `bedrock_converse`.

Defer for v1: the Responses WebSocket transport, `previous_response_id` continuation, Gemini Interactions, and the media and embedding clients. The size estimate is *(inferred)*: OpenCode's equivalent is about 9k lines of TypeScript for these five plus shared utilities. Expect about 8 to 12k lines of Rust, split into request, stream and usage modules per protocol.

## Type sketch (signatures only)

```rust
// ---------- identity & replay ----------
pub struct ProviderId(pub Arc<str>);          // "anthropic", "azure-foundry", "bedrock", …
pub struct ModelRef { pub provider: ProviderId, pub catalog_id: Arc<str>, pub wire_id: Arc<str> }
pub enum ProtocolKind { AnthropicMessages, OpenAiResponses, OpenAiChat, Gemini, BedrockConverse }

/// Who produced a part and therefore who may receive its opaque payload back.
pub struct Origin {
    pub provider: ProviderId,
    pub model: Arc<str>,            // wire id actually served (fallbacks may differ from requested)
    pub protocol: ProtocolKind,
    pub account: Option<AccountTag>, // hash of credential/tenant/project; Anthropic blocks may be account-bound
}
pub struct AccountTag(pub [u8; 16]);

/// Per-protocol rule deciding whether `Opaque` data is replayed to a destination.
pub enum ReplayScope {
    ProtocolFamilySameAccount, // Anthropic: send all; API drops unreadable; account-bound models need same account
    ModelFamily,               // OpenAI Responses: same model family
    SameModel,                 // Gemini, Bedrock non-Claude (conservative default)
    Never,
}

// ---------- canonical content ----------
pub enum Role { System, User, Assistant, Tool }

pub struct Message {
    pub id: MessageId,
    pub role: Role,
    pub parts: Vec<Part>,
    pub origin: Option<Origin>,      // set on assistant messages
}

pub enum Part {
    Text(TextPart),
    Media(MediaPart),               // image / pdf / file; bytes | file ref | url
    ToolCall(ToolCall),
    ToolResult(ToolResult),
    Reasoning(ReasoningPart),
    Compaction(CompactionPart),     // provider checkpoint: encrypted xor summary text
    EffortMarker(EffortMarker),     // "effort changed here" → mid-conversation system msg where supported
    Unknown(UnknownPart),           // provider block with no canonical meaning (e.g. Anthropic `fallback`), replayed only in-scope
}

pub struct TextPart { pub text: String, pub citations: Vec<Citation>, pub cache: Option<CacheHint>, pub opaque: Option<Opaque> }
pub struct Citation { pub kind: CitationKind, pub cited_text: Option<String>, pub source: CitationSource, pub opaque: Option<Opaque> }
pub enum CitationKind { CharRange, PageRange, ContentBlock, SearchResult, WebUrl, Annotation }
pub struct MediaPart { pub mime: Mime, pub source: MediaSource, pub cache: Option<CacheHint> }

pub struct ToolCall {
    pub id: ToolCallId,             // canonical id; wire ids are derived per destination and persisted
    pub name: String,
    pub input: serde_json::Value,
    pub provider_executed: bool,    // server tools (web_search, code exec…)
    pub opaque: Option<Opaque>,     // e.g. Gemini thoughtSignature on functionCall
}
pub struct ToolResult { pub call_id: ToolCallId, pub content: ToolOutput, pub is_error: bool, pub cache: Option<CacheHint> }
pub enum ToolOutput { Text(String), Json(serde_json::Value), Rich(Vec<Part>) }

pub struct ReasoningPart {
    pub text: String,               // readable summary; may be empty ("omitted" display)
    pub redacted: bool,
    pub opaque: Option<Opaque>,
}

pub struct Opaque { pub origin: Origin, pub data: OpaqueData }
pub enum OpaqueData {
    AnthropicSignature(String),
    AnthropicRedacted { data: String },
    ResponsesReasoning { item_id: String, encrypted_content: Option<String>, summary: Vec<String> },
    GeminiThoughtSignature(String),
    BedrockReasoning { signature: Option<String>, redacted: Option<Bytes> },
    ChatReasoningDetails(serde_json::Value), // OpenRouter / Kimi dialects
    Raw(serde_json::Value),
}

pub struct CacheHint { pub ttl: CacheTtl }
pub enum CacheTtl { FiveMinutes, OneHour }

// ---------- request ----------
pub struct LlmRequest {
    pub model: ModelRef,
    pub system: Vec<Part>,
    pub messages: Vec<Message>,
    pub tools: Vec<ToolDef>,
    pub tool_choice: ToolChoice,
    pub effort: Option<EffortLevel>,          // mapped per (protocol, model) from catalog data
    pub max_output_tokens: Option<u32>,
    pub cache: CachePolicy,                    // Auto | None | Explicit { … }
    pub prompt_cache_key: Option<String>,
    pub include_raw: bool,
    pub timeouts: Timeouts,                    // headers 5 min, chunk gap 15 min defaults
}
pub enum CachePolicy { Auto, None, Explicit { tools: bool, system: bool, tail: TailRule, ttl: CacheTtl } }

/// Output of the compile step; idempotent and append-only over history already sent.
pub struct CompiledRequest { pub wire_history: Vec<WireMessage>, pub request: LlmRequest, pub diagnostics: Vec<CompileNote> }

// ---------- stream ----------
pub enum StreamEvent {
    StepStart { step: u32 },
    TextStart { part: PartIdx }, TextDelta { part: PartIdx, delta: String }, TextEnd { part: PartIdx, citations: Vec<Citation> },
    ReasoningStart { part: PartIdx }, ReasoningDelta { part: PartIdx, delta: String },
    ReasoningEnd { part: PartIdx, final_text: Option<String>, opaque: Option<Opaque> },
    ToolInputStart { part: PartIdx, call_id: ToolCallId, name: String },
    ToolInputDelta { part: PartIdx, json_fragment: String },
    ToolInputEnd { part: PartIdx, call: ToolCall },
    ToolInputError { part: PartIdx, call_id: ToolCallId, raw: String, error: String },
    ProviderToolResult { call_id: ToolCallId, result: ToolResult },
    Citation { part: PartIdx, citation: Citation },
    OpaqueBlock { part: PartIdx, part_value: Part },            // whole-arriving blocks (redacted_thinking, fallback)
    ModelSwitched { from: Arc<str>, to: Arc<str> },             // server-side fallback boundary
    StepFinish { reason: FinishReason, details: FinishDetails, usage: Usage },
    Finish { reason: FinishReason, usage: Usage },
    Warning(ProviderWarning),
    Raw(RawEvent),                                              // only when include_raw
}
pub enum FinishReason { Stop, Length, ToolCalls, ContentFilter, Refusal, Paused, Error, Unknown }
pub struct FinishDetails { pub raw: Option<String>, pub category: Option<String>, pub explanation: Option<String> }

pub struct Usage {
    pub input_total: u64,          // = non_cached + cache_read + cache_write_5m + cache_write_1h
    pub non_cached_input: u64,
    pub cache_read: u64,
    pub cache_write_5m: u64,
    pub cache_write_1h: u64,
    pub output_total: u64,
    pub reasoning: Option<u64>,    // ≤ output_total
    pub context_tokens: Option<u64>, // latest call only; not summed
    pub cost: Option<Cost>,        // computed from catalog; Some(0) is authoritative
}

// ---------- protocol / route / provider ----------
pub trait Protocol: Send + Sync + 'static {
    type Body: serde::Serialize + Send;
    type Frame: Send;              // SSE event / AWS event-stream message / JSON line
    type State: Default + Send;
    const KIND: ProtocolKind;
    const FRAMING: Framing;
    const REPLAY: ReplayScope;

    fn build_body(&self, req: &CompiledRequest, compat: &Compat) -> Result<Self::Body, ProviderError>;
    fn decode(&self, frame: RawFrame) -> Result<Option<Self::Frame>, ProviderError>; // None = ignorable/unknown
    fn step(&self, state: &mut Self::State, frame: Self::Frame, out: &mut Vec<StreamEvent>) -> Result<(), ProviderError>;
    fn on_halt(&self, state: Self::State, out: &mut Vec<StreamEvent>) -> Result<(), ProviderError>; // flush; error if no terminal
    fn classify(&self, failure: &HttpFailure) -> ProviderError;   // protocol-specific codes before generic rules
}
pub enum Framing { Sse, SseWithDone, AwsEventStream, JsonLines }

#[async_trait::async_trait]
pub trait Auth: Send + Sync {
    async fn apply(&self, req: &mut http::request::Parts, body: &[u8]) -> Result<(), ProviderError>; // SigV4 needs body
}
pub enum AuthKind { ApiKeyHeader { header: HeaderName }, Bearer, SigV4 { service: &'static str, region: String }, Entra { scope: String }, GoogleAdc, None }

pub struct Endpoint { pub base: Url, pub path: PathTemplate, pub model_in: ModelPlacement /* Body | Path */, pub version: VersionPlacement /* Header | Body | None */ }

pub struct Route { pub protocol: Arc<dyn ErasedProtocol>, pub endpoint: Endpoint, pub auth: Arc<dyn Auth>, pub compat: Compat, pub headers: HeaderMap }

/// Thin: binds a model to a Route. Discovery/catalog lives above (ticket 04).
pub trait Provider: Send + Sync {
    fn id(&self) -> &ProviderId;
    fn route(&self, model: &ModelSpec) -> Result<Route, ProviderError>; // protocol chosen per model
}

pub trait LlmClient {
    fn stream(&self, route: &Route, req: LlmRequest, cancel: tokio_util::sync::CancellationToken)
        -> Pin<Box<dyn Stream<Item = Result<StreamEvent, ProviderError>> + Send>>;
}

// ---------- quirks ----------
#[derive(Default, Clone, serde::Deserialize)]
pub struct Compat {
    pub anthropic_version: Option<&'static str>,      // "2026-10-21" | "2023-06-01" | "vertex-2023-10-16"
    pub sampling_params_allowed: bool,                // false on Anthropic 2026-10-21
    pub max_tokens_field: MaxTokensField,
    pub reasoning_field: Option<ReasoningField>,
    pub require_finish_reason: bool,
    pub require_assistant_after_tool: bool,
    pub require_thinking_signature: bool,
    pub stream_usage: bool,
    pub cache_format: Option<CacheFormat>,            // Anthropic | Alibaba | None
    pub automatic_cache_supported: bool,              // false on bedrock-runtime
    pub max_cache_breakpoints: u8,
    pub tool_id_rule: ToolIdRule,                     // AnthropicCharset | Max40 | Mistral9 | Omit
    pub schema_sanitizer: Option<SchemaSanitizer>,    // Gemini | Moonshot | ObjectRoot
    pub prefer_responses: bool,                       // Azure deployment: fall back to Chat on "Model not supported"
    pub mid_conversation_system: bool,                // effort/instructions without cache bust
    pub rate_limit_headers: bool,                     // false on Foundry
}
/// Data-driven per-model patches from the catalog / config (ticket 04, 08), applied in order.
pub struct ModelPatch { pub matcher: ModelMatcher, pub compat: CompatOverlay, pub body: Option<JsonMergePatch>, pub headers: HeaderMap }
pub enum ModelMatcher { Exact(Arc<str>), Prefix(Arc<str>), Family(Arc<str>), Regex(regex::Regex) }

// ---------- errors & retry ----------
pub struct ProviderError {
    pub kind: ErrorKind,
    pub status: Option<u16>,
    pub provider_code: Option<String>,
    pub request_ids: SmallVec<[(HeaderName, String); 2]>, // request-id, apim-request-id, x-amzn-requestid
    pub retry: RetryHint,
    pub message: String,
    pub body_excerpt: Option<String>,
}
pub enum ErrorKind {
    ContextOverflow,          // data-driven regex corpus (≈40 patterns), never on 429
    PayloadTooLarge,
    ContentPolicy { category: Option<String> },
    QuotaExceeded,            // 402 / insufficient_quota: never retried
    Authentication,           // 401/403/invalid key
    RateLimited,              // 429, Bedrock ThrottlingException
    Overloaded,               // 529 / overloaded_error / 503
    ProviderInternal,         // 5xx, 408, 409, modelStreamErrorException
    InvalidRequest,
    ReplayRejected,           // thinking prefix-check / "bound to a different conversation"
    UnsupportedOnDeployment,  // Foundry "hosted on Azure" 400s, Azure "Model not supported"
    Transport { phase: TransportPhase },   // Connect | Send | Headers | Body
    Timeout { phase: TimeoutPhase },       // Headers | ChunkGap | Total
    IncompleteStream,         // no terminal event → retryable
    InvalidProviderOutput,
    Cancelled,
    Unknown,
}
pub struct RetryHint { pub retryable: bool, pub after: Option<Duration>, pub forced: Option<bool> /* x-should-retry */ }
pub struct RetryPolicy { pub base: Duration, pub max_gap: Duration, pub max_retries: u8, pub honor_retry_after_up_to: Duration, pub offline_budget: Duration, pub max_timeout_retries: u8 }
```

**Design notes on the sketch:**
- **Quirk flags** live in `Compat`, which is plain data. A route sets defaults, and `ModelPatch`es from the catalog or user config overlay them. Protocols read `Compat` and never switch on model names. Name-based rules (Claude version, Gemini < 3) are expressed as catalog `ModelPatch`es so the conformance suite (ticket 34) can test them.
- **The compile step is append-only.** Tool-id rewriting, history repair and cache-breakpoint placement produce `wire_history`, which is **persisted with the session** (ticket 12). On the next turn, earlier wire messages are reused byte-for-byte. This satisfies Anthropic's prefix checks and keeps the prompt cache warm.
- **The replay filter** runs in compile. For each `Opaque`, the destination's `ReplayScope` is compared against `Origin`. For Anthropic → Anthropic, Genie sends everything and lets the API drop what it can't read, as the docs ask. For anything else, the readable `text` is sent and the `opaque` payload is dropped.
- **Cancellation.** Dropping the stream or firing the `CancellationToken` aborts the request. The executor emits `Cancelled` and never retries it.

## Risks

1. **Maintenance load of owning five protocols.** Vendors change wire formats every month. Examples: Anthropic `2026-10-21`, new Responses events, Gemini moving to the Interactions API. Mitigations:
   - tolerate unknown events and fields everywhere;
   - pass a raw event through;
   - record cassettes (ticket 34);
   - assign a named owner and run a monthly docs diff.
2. **Getting replay and caching wrong costs money and breaks sessions.** Preserved-thinking prefix checks turn any non-idempotent normalisation into 400s or silent reasoning loss. Account-bound blocks (Sonnet 5.5, Haiku 5.5, Fable/Mythos 5.2) break when the user changes credentials mid-session. This needs the persisted `wire_history`, plus `AccountTag` in `Origin`, from day one.
3. **Cloud auth surface.** SigV4 hand-signing for two Bedrock services, Entra chains without `DefaultAzureCredential`, and ADC on desktop. These are security-sensitive and differ by platform. Mitigations:
   - use only the official crates for signing and tokens;
   - keep `aws-sdk-bedrockruntime` as a fallback;
   - keep secrets out of logs (`secrecy`);
   - include them in ticket 14's review.
4. **Lower-severity risks:**
   - Some protocol rules in this report come from OpenCode or crate sources and are not confirmed in current docs: Gemini's parallel-signature rule and sentinel, Bedrock `cachePoint` placement, and the Responses event list. They are marked *(unverified)*.
   - The OpenCode KB's model ids come from a fork and need checking (ch.08 open questions).
   - Adapting `genai` or `rig` code requires keeping their MIT/Apache notices (same handling as C3).

## Open questions

- **Q1 (needs the user):** Approve "build, with plumbing crates only" rather than adopting `rig-core`. This is a build-vs-buy decision with a staffing cost of about 8 to 12k lines *(inferred)* plus ongoing upkeep.
- **Q2 (needs the user):** Target Anthropic `2026-10-21` from day one? If yes, Anthropic routes lose temperature, top_p and top_k, and the Settings UI must hide them (ticket 33). If no, we pin `2023-06-01` and migrate later.
- **Q3:** For Bedrock in v1: Mantle (Messages, SSE, simpler) only, or Mantle plus `bedrock-runtime` Converse? Converse is needed for non-Claude models, Fable/Mythos 5.2, and customers whose IAM only allows `bedrock:InvokeModel*`.
- **Q4:** Wrap `genai` later as a "long-tail" adapter behind `Protocol` for the providers we don't support natively? Weigh it against two canonical models and the bus factor.
- **Q5:** Does the Genie Gateway (if any; ch.07 open question) speak Messages or Responses? That decides which adapter gets the most investment.
- **Q6:** Where does `AccountTag` come from? A hash of the credential id or of the tenant/project? Coordinate with tickets 12 and 14.
- **Q7:** Which Azure Entra chain should desktop use (CLI → azd → device code?). `azure_identity` has no interactive browser credential in the list we saw *(unverified)*.

## Sources

**Vendor docs (fetched 2026-11-16):**
- Anthropic: SDK list: https://platform.claude.com/docs/en/cli-sdks-libraries/overview (redirected from https://docs.anthropic.com/en/api/client-sdks)
- Anthropic: Migrating to 2026-10-21: https://platform.claude.com/docs/en/api/migration-2026-10-21
- Anthropic: Streaming: https://platform.claude.com/docs/en/build-with-claude/streaming
- Anthropic: Thinking (replay, preservation, encryption, redacted): https://platform.claude.com/docs/en/build-with-claude/thinking
- Anthropic: Prompt caching: https://platform.claude.com/docs/en/build-with-claude/prompt-caching
- Anthropic: Claude in Amazon Bedrock (Mantle): https://platform.claude.com/docs/en/build-with-claude/claude-in-amazon-bedrock
- Anthropic: Claude in Microsoft Foundry: https://platform.claude.com/docs/en/build-with-claude/claude-in-microsoft-foundry
- Anthropic: Claude on Google Cloud (Google managed): https://platform.claude.com/docs/en/build-with-claude/claude-models-on-google-cloud/google-managed
- OpenAI: Libraries: https://developers.openai.com/api/docs/libraries
- OpenAI: Reasoning: https://developers.openai.com/api/docs/guides/reasoning
- OpenAI: Streaming responses: https://developers.openai.com/api/docs/guides/streaming-responses
- Microsoft: Foundry Models endpoints: https://learn.microsoft.com/en-us/azure/foundry/foundry-models/concepts/endpoints
- Microsoft: Azure OpenAI v1 API: https://learn.microsoft.com/azure/foundry/openai/api-version-lifecycle
- Microsoft: Configure Entra ID (Foundry): https://learn.microsoft.com/azure/foundry/foundry-models/how-to/configure-entra-id
- AWS: ConverseStream API reference: https://docs.aws.amazon.com/bedrock/latest/APIReference/API_runtime_ConverseStream.html
- Google: Thought signatures: https://aistudio.google.com/docs/thought-signatures
- Google: Thinking: https://aistudio.google.com/docs/thinking

**Crates and repos** (crates.io API and GitHub API on 2026-11-16; sources downloaded from static.crates.io and read locally):
- https://crates.io/crates/genai · https://github.com/jeremychone/rust-genai
- https://crates.io/crates/rig-core · https://github.com/0xPlaygrounds/rig · https://crates.io/crates/rig-bedrock · https://crates.io/crates/rig-vertexai
- https://crates.io/crates/async-openai · https://github.com/64bit/async-openai
- https://crates.io/crates/aisdk · https://github.com/lazy-hq/aisdk
- https://crates.io/crates/liter-llm · https://github.com/xberg-io/liter-llm
- https://crates.io/crates/misanthropic · https://github.com/mdegans/misanthropic
- https://crates.io/crates/llm · https://github.com/graniet/llm
- https://crates.io/crates/anthropic · https://crates.io/crates/anthropic-ai-sdk · https://crates.io/crates/anthropic-sdk-rust · https://crates.io/crates/clust
- https://crates.io/crates/aws-sdk-bedrockruntime · https://crates.io/crates/aws-sigv4 · https://crates.io/crates/aws-config · https://crates.io/crates/aws-smithy-eventstream
- https://crates.io/crates/azure_identity · https://docs.rs/azure_identity/1.0.0/azure_identity/ · https://crates.io/crates/azure_ai_openai · https://github.com/azure/azure-sdk-for-rust
- https://crates.io/crates/google-cloud-auth · https://crates.io/crates/gcp_auth
- https://crates.io/crates/eventsource-stream · https://crates.io/crates/reqwest-eventsource

**Internal:** [knowledge/opencode/07-ai-package-and-protocols.md](../knowledge/opencode/07-ai-package-and-protocols.md), [knowledge/opencode/08-providers-models-and-credentials.md](../knowledge/opencode/08-providers-models-and-credentials.md)
