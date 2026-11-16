# 15 — Web search and fetch

type: wayfinder:grilling · status: closed · claimed-by: charting session (2026-11-16) · blocked-by: 03,06

## Question
How do web search and fetch work across providers?

## To decide
- Provider-native server tools (Anthropic and OpenAI web search) vs our own search backend
- Fetch tool: HTML → markdown, size limits, permissions
- Citations in the UI

## References
- OpenCode ch.09: [09-tools.md](../knowledge/opencode/09-tools.md) — read TL;DR, Architecture, Genie takeaways

## Resolution
Resolved 2026-11-16. User decisions are marked **(user)**; the rest builds on D11, D27 and D35.

### Search backend (D11)
- **Provider-native only in v1:** Anthropic web search (server tool), OpenAI Responses `web_search`, Gemini Google Search grounding. Availability is **per model** from the catalog (`capabilities.web_search_native`, D27), because e.g. some Azure-hosted Claude deployments reject the newer web tools (research/provider-layer.md). Our own search backend (BYO Brave/Tavily/Exa-style) is v2.
- On models without native search the toggle is greyed out, with the tooltip "This model can't search the web. Switch to … or use web fetch with a URL". `web_fetch` (D35) still works everywhere.
- Native results come back as provider-hosted tool parts (`executed=false`) and are stored with their citation data (D24 `part` kind `citation`). They are replayed only to the same provider (D18 replay rules).

### Default (user)
- **On by default** for supported models. Toggle per chat ("+" menu) and globally (Settings → Capabilities, D34). **Admin policy** can disable search or set **allowed/blocked domains** (passed to providers that support domain filters, e.g. Anthropic `allowed_domains`/`blocked_domains`; otherwise search is disabled if a block list is required and can't be enforced).
- A small per-turn limit (e.g. `max_uses` 5) controls cost. Search fees, where the provider charges them, appear in usage/cost (D27, D34).

### Citations (user): inline markers + sources list
- **Numbered inline markers `[1]`** after cited text. Hover shows the title, domain and quoted snippet. A **"Sources" row** of favicon chips sits under the answer. Clicking opens the page in the **system browser** (not the agent browser).
- Mapping per provider: Anthropic `citations` (`web_search_result_location` with cited text) → exact spans; OpenAI `url_citation` annotations (start/end index) → spans; Gemini `groundingMetadata` (segment → chunk indices) → spans. When a provider gives sources without spans, only the Sources row is shown.
- Citations survive copy (the Markdown copy includes `[n]` + a reference list) and export (D24).
- Favicons are fetched through the daemon with caching (never directly from the webview, consistent with the D28 no-network rule for content).

### Web fetch (→ D35)
- Covered in 06. `web_fetch` results can be cited too: the source chip shows "Fetched" instead of "Search".
