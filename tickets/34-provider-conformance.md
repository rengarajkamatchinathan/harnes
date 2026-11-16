# 34 — Provider conformance suite

type: wayfinder:task · status: closed · claimed-by: charting session (2026-11-16) · blocked-by: 03

## Question
Set up the recorded-fixture conformance tests that every provider adapter must pass (planning only).

## To decide
- Streaming, tool calls, reasoning replay, caching, errors per provider
- Ollama 'reasoning' field check
- Live smoke tests behind env keys

## References
- OpenCode ch.07: [07-ai-package-and-protocols.md](../knowledge/opencode/07-ai-package-and-protocols.md) — read TL;DR, Architecture, Genie takeaways
- OpenCode ch.08: [08-providers-models-and-credentials.md](../knowledge/opencode/08-providers-models-and-credentials.md) — read TL;DR, Architecture, Genie takeaways

## Resolution
Resolved 2026-11-16 (plan only; CI platform deferred → 41).

### Layers
1. **Unit (no network):** each protocol adapter (D26 v1: Anthropic Messages, OpenAI Responses, OpenAI Chat, Gemini) is tested against **recorded cassettes**: raw request bytes + raw streamed response bytes. Recording and replay go through the single HTTP `Executor` (research/provider-layer.md), so there are no network calls in unit CI.
2. **Normalisation golden tests:** canonical `Message/Part` history → exact wire JSON per protocol and route (Anthropic API / Foundry / Vertex; OpenAI / Azure `/openai/v1`; Gemini API / Vertex; OpenAI-compatible + `Compat` flags). Snapshot tests, with diffs reviewed in PRs.
3. **Catalog patch tests:** every `ModelPatch`/quirk in the catalog (D27) has a case showing the resulting request (e.g. sampling settings stripped for `2026-10-21` models, effort mapping per model).
4. **Live smoke (opt-in):** a small matrix run against real endpoints when keys are present in env (`GENIE_TEST_<PROVIDER>_*`). Nightly and before release, never on PRs from forks. It also **re-records cassettes** and flags drift.

### Required cases per protocol (the conformance checklist)
- Text streaming: deltas → `*.ended` reassembly, finish reasons, usage incl. cache read/write.
- Tool calls: single, parallel, streamed arguments, tool result round-trip, tool-history repair, id rewrite.
- Reasoning: thinking/reasoning items streamed; **replay rules** (same protocol family + account, D18); signatures/encrypted content kept byte-exact; account-bound models.
- Caching: breakpoint placement (D32), automatic caching, TTL 5m/1h, Gemini "last breakpoint only".
- Native web search + citations mapping (D36); code execution parts (D35); native compaction (D33).
- Attachments: images, PDFs, too-large errors.
- **Errors:** context overflow (the ~40 patterns from KB ch.07), rate limit vs quota, `retry-after` / `x-should-retry`, 5xx, auth failure (expired Entra token → refresh, D26), mid-stream disconnect, cancellation.
- OpenAI-compatible quirks: **Ollama `reasoning` field**, `max_tokens` field name, missing finish reason, stream usage flag.
- Azure: deployment-name model ids, `api-key` vs Entra bearer, `apim-request-id` logged.

### Rules
- A new provider/route/model quirk merges only with cassettes + golden tests.
- Cassettes are scrubbed (keys, org ids, account ids) by the Executor recorder; a CI check fails on secrets in fixtures.
- **Owner:** a named provider-layer owner (D18 risk 1) reviews the monthly live-drift report.
