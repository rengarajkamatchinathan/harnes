# 11 — Context assembly and prompt caching

type: wayfinder:grilling · status: closed · claimed-by: charting session (2026-11-16) · blocked-by: 03,10

## Question
How is the system prompt built, and how is it kept cache-stable?

## To decide
- Freeze AGENTS.md, the skills index and the environment into a versioned system part per session
- send later edits as <system-update> messages
- Up to 4 cache breakpoints keyed by the root session, so subagents share the cache
- Central tool-output truncation (2,000 lines / 50 KiB) spilled to a file

## References
- OpenCode ch.04: [04-context-and-compaction.md](../knowledge/opencode/04-context-and-compaction.md) — read TL;DR, Architecture, Genie takeaways

## Resolution
Resolved 2026-11-16. User decisions are marked **(user)**; the rest are defaults from KB ch.04 and D18/D27/D31.

### Big project knowledge (user: "how OpenCode and Claude Desktop handle it")
- **OpenCode:** has no project-knowledge feature; instruction files go in whole with no size limit, and an oversize prompt leads to overflow → compaction. Not a model to copy.
- **Claude Desktop:** files go in full while they fit; near the limit it **switches automatically to retrieval (RAG)**, which raises capacity about 10× (inventory §1.2).
- **Genie follows Claude Desktop → project RAG moves into v1** (this changes D29's "no RAG in v1"):
  - **Full mode:** while the project knowledge fits in **≤ 50% of the active model's window**, every file is included in full, as a cached block.
  - **Retrieval mode:** above that, the project switches automatically (with a badge on the project page). Files are chunked (~800 tokens, overlapping, per file type) and indexed with **FTS5/BM25 locally (always available)**, plus **embeddings when the user has an embedding-capable provider** (OpenAI / Azure / Gemini; Anthropic has none) → hybrid ranking. The model gets a `project_search {query}` tool, and the top chunks for the user's message are pre-injected.
  - The index is kept in the encrypted store (D24). It is rebuilt when files change, and embeddings are cached by content hash. Capacity follows Claude's ~10× guidance, enforced by a per-project cap shown in the capacity bar.
  - Switching models: the mode is re-evaluated per request, against the current model's window.

### Context meter (user)
- A **small ring by the composer** fills with context use. Hover shows tokens used / window, cost so far, and "compaction at X%". It turns amber near the compaction threshold (→ 16). Code/CLI show the same data in more detail.

### Prompt layout (default; most-shared first)
1. **Tools** (definitions; deferred MCP tools via tool search, D30).
2. **System:** Genie base prompt (+ an explicit per-provider tuning table in the provider layer, not plugins).
3. **Context baseline message** (first user/context message, cache-stable, portable to every provider): org instructions → custom instructions → project instructions → project knowledge (full mode) → skills index (→ 21) → memory (D31) → instruction files (code) → date + environment.
4. **Conversation:** active branch path (D24), with `<system-update>` deltas inline when sources change (D31).

### Caching (default; KB port)
- Up to **4 breakpoints:** end of tools, end of system, end of the context baseline, and the conversation tail. Applied only where the route honours them (per-protocol/per-model table from D27 `capabilities.prompt_cache`), including the Gemini "last breakpoint only" and OpenAI automatic-caching cases.
- The cache key per root session is shared by subagents. TTL comes from the catalog: 5 min by default; **1 h for the project-knowledge block** when the model supports it (cheaper for projects that are reused).
- Cache read/write tokens are recorded in usage and cost (D27).

### Token accounting (default)
- Measured (last same-provider/model usage) + estimated (4 chars/token, 15% margin) for everything after that. `max_tokens` is fitted to the remaining room. Measurements from a different provider are not trusted after a model switch.

### Tool output (default)
- Central truncation at **2,000 lines / 50 KiB**; the full output is saved as a blob (D24) with a marker `[showing 1–N of M; full output: <ref>]` that the model can page with a read tool. Kept until the session is deleted (not a 7-day sweep, since the store is encrypted and ref-counted). Configurable with `toolOutput.maxLines/maxBytes`.

### Attachments (default)
- Images/PDFs go in natively where the model supports them (D27 modalities); otherwise PDFs are converted to text, and images are refused with a clear message. Large attachments count against the meter before sending, with a warning if they won't fit.
