# 16 — Compaction

type: wayfinder:grilling · status: closed · claimed-by: charting session (2026-11-16) · blocked-by: 11,12

## Question
When and how do we compact long sessions?

## To decide
- Trigger at window − max(15%, 16k)
- fixed summary template
- keep about 15k recent tokens
- Overflow retry at 85 → 70 → 50 → 35%
- Preserve read-state and approvals

## References
- OpenCode ch.04: [04-context-and-compaction.md](../knowledge/opencode/04-context-and-compaction.md) — read TL;DR, Architecture, Genie takeaways

## Resolution
Resolved 2026-11-16. User decisions are marked **(user)**; the rest are defaults from KB ch.04 and D24/D31/D32.

### UX (user): automatic + visible divider
- Genie compacts automatically. The chat shows a divider: **"Earlier messages summarised · View summary"** (or "· Summarised by <provider>" in native mode, where no readable text exists).
- **All original messages stay visible and searchable for the user** (D24 keeps them); only the model's context changes.
- The context meter (D32) turns amber near the trigger. **Code/CLI** also get a manual `/compact [focus instructions]`.
- Branching from a message **before** a compaction point uses the uncompacted history for that branch.

### Mechanism (user): native where supported, else our summary
- If the model's catalog entry (D27) has `capabilities.native_compaction`, use the provider's compaction (Anthropic / OpenAI). The opaque result is stored as a part with origin = provider + model + account (same replay rule as D18 reasoning).
- **Our summary** is used otherwise, and **lazily when the user switches model/provider** after a native compaction: generate a readable summary from the stored originals on the next turn.
- Our summary uses the **same model** and **same cached prefix** + a summary instruction (a cache hit). It follows a Genie-written Markdown template: *Goal · User preferences & constraints · Decisions · Current state · Open threads · Key references (files, artifacts, links)*. One retry if the template check fails.

### Triggers (default; KB port)
- **Auto:** when estimated context ≥ window − max(15% of window, 16k).
- **Overflow:** provider rejects for context length → compact, retry, shrinking the kept tail 85% → 70% → 50% → 35%.
- **Manual:** `/compact` (Code/CLI).
- The trigger point can be configured with `compaction.buffer` (percent or tokens). Admins can't disable compaction (it prevents hard failures), but users can change the threshold.

### What is kept (default)
- **Verbatim tail:** about 15k tokens of the most recent messages (whole messages, never cut through a tool call/result pair).
- **Re-injected after compaction** (engine state, not transcript, a gap the KB found in OpenCode): the context baseline is rebuilt (D31/D32), and **invoked skills** (→ 21), **artifact references** with their current version ids (D28), **files read / read-state** (Code, → 06), **session permission grants** (D25), the active plan/todo (Code), and MCP resources attached in this chat are carried over.
- Memory isn't summarised; it's re-injected from the store (D31).

### Storage and events (default)
- A `compaction` message in the tree (D24) holds: summary part or native blob, the kept-tail boundary message id, trigger, and token counts before/after. Durable event `session.compacted` (D22). Hooks: read-only `PreCompact` / `PostCompact` (→ 20).
- **Cost:** summary/compaction tokens are shown in usage as "compaction".
