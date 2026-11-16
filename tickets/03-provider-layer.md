# 03 — Provider layer: canonical model and wire protocols

type: wayfinder:research · status: closed · claimed-by: research-agent (2026-11-16) · blocked-by: —

## Question
Rust has no Vercel AI SDK. Do we adopt a crate (genai, rig, async-openai) or build our own protocol adapters?

## To decide
- Canonical message/part model (text, image, file, tool_call, tool_result, reasoning, citation)
- Protocols: Anthropic Messages, OpenAI Responses + Chat, Azure AI Foundry (OpenAI and non-OpenAI models), Bedrock, Vertex (Anthropic + Gemini), OpenAI-compatible (Ollama, vLLM, OpenRouter)
- Request normalisation (tool-history repair, tool-id rewrite, cache breakpoints)
- per-model quirk flags and effort patches
- Replay opaque reasoning only to the same provider+model
- Error classification (context overflow, quota vs rate limit), retry/backoff
- Usage type with cache read/write
- Output: research/provider-layer.md

## References
- OpenCode ch.07: [07-ai-package-and-protocols.md](../knowledge/opencode/07-ai-package-and-protocols.md) — read TL;DR, Architecture, Genie takeaways
- OpenCode ch.08: [08-providers-models-and-credentials.md](../knowledge/opencode/08-providers-models-and-credentials.md) — read TL;DR, Architecture, Genie takeaways

## Resolution
**Recommendation: build our own adapters; use crates only for plumbing.** Full report: [research/provider-layer.md](../research/provider-layer.md).
- No Rust crate meets the bar, and neither Anthropic nor OpenAI ships an official Rust SDK. `rig-core` 0.46 is closest but is a 0.x agent framework that breaks often. `genai` 0.7.5 has the widest coverage, but its reasoning model is lossy and it has no Azure support or error taxonomy.
- Build `genie-providers` with OpenCode's design (canonical `Part`s, compile step, `Protocol` state machines, data-driven `Compat` quirks, classified errors, retry) and five protocols:
  - Anthropic Messages (Claude API, Bedrock Mantle, Foundry, Vertex);
  - OpenAI Responses (OpenAI, Azure `/openai/v1`);
  - OpenAI Chat (Foundry models, Ollama, vLLM, OpenRouter);
  - Gemini (Gemini API, Vertex);
  - Bedrock ConverseStream.
- Plumbing crates: `reqwest`, our own SSE parser, `aws-config`/`aws-sigv4`/`aws-smithy-eventstream`, `azure_identity` 1.x (Entra scope `https://ai.azure.com/.default`), `google-cloud-auth`.
- Doc findings that change OpenCode's design:
  - Anthropic thinking replay is scoped to the protocol family and account, not the same model ("don't strip on model switch").
  - Preserved-thinking prefix checks mean normalisation must be append-only, so the wire history has to be persisted.
  - Anthropic `2026-10-21` rejects `temperature`, `top_p` and `top_k`.
- Needs the user: approve build vs. adopting rig (Q1), choose the Anthropic API version (Q2), and choose the Bedrock path for v1 (Q3).
- Still to do: add the D-line to WAYFINDER.md after review, then close.
