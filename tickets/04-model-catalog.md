# 04 — Model catalog

type: wayfinder:grilling · status: closed · claimed-by: charting session (2026-11-16) · blocked-by: 03

## Question
Where does model metadata (context window, pricing, capabilities) come from?

## To decide
- models.dev vs our own catalog
- Bundled snapshot as the boot floor + cached fetch + ETag refresh (KB proposal)
- Model variants and effort levels
- Model picker data for the desktop

## References
- OpenCode ch.08: [08-providers-models-and-credentials.md](../knowledge/opencode/08-providers-models-and-credentials.md) — read TL;DR, Architecture, Genie takeaways

## Resolution
Resolved 2026-11-16. User decisions are marked **(user)**; the rest are defaults from KB ch.08 and research/provider-layer.md.

### Source (user): Genie-hosted catalog + live discovery
- **Genie catalog:** our own schema, maintained by us from provider docs. models.dev data may be used as an input **only if its data licence allows commercial reuse** (to be checked; → 35). It is served as a signed JSON document by Genie's update service (→ 27).
- **Loading order:** bundled snapshot (offline floor, ships with each release) → cached last fetch → background refresh (ETag, about every 6 h and on start). Each model entry is decoded tolerantly, so one bad entry is dropped, not the whole catalog. A failed refresh keeps the last good copy.
- **Live discovery** merged on top: Azure Foundry deployments (Entra/management API when signed in; otherwise the user adds a deployment name + base model), OpenAI `/v1/models`, Gemini `models.list`, Ollama `/api/tags` + `/api/show`, OpenAI-compatible `/v1/models`. A discovered model with no catalog entry gets conservative defaults and a "limited info" badge.
- **User overrides** in settings (`models.<id>`: context window, price, capabilities) for custom endpoints. **Admin policy** can hide models or providers (→ 28).
- Air-gapped installs: `catalog.url` can point at an internal mirror, or the bundled snapshot is used only.

### Sampling settings (user)
- **Hidden in Chat.** Settings → Providers & Models → a model's **Advanced** panel shows only the settings that model supports (temperature, top_p, top_k, max output, stop), driven by catalog capability flags. Unsupported settings are never sent. This handles the Anthropic `2026-10-21` behaviour (it rejects sampling settings, per 03) without special cases.

### Effort UI (user)
- A **per-model effort selector** next to the model picker shows only the levels the model supports (`Off / Low / Medium / High / Max` subset), hidden for models without reasoning.
- The mapping is a per-(protocol, model) table from catalog data, e.g. `{toggle, levels[], budget{min,max}, adaptive}`: Claude → effort / adaptive or budgeted thinking; OpenAI → `reasoning.effort`; Gemini → `thinkingLevel` or `thinkingBudget`. `Off` appears only where the model allows it.
- The selected effort is saved per session; the default per model comes from Settings.

### Catalog entry (default; fields)
`id, provider, protocol, wire_model_id, display_name, family, release_date, deprecated_at?, context_window, max_output, input_modalities[], output_modalities[], capabilities{tools, parallel_tools, vision, pdf, web_search_native, prompt_cache{auto, breakpoints}, reasoning{…}, citations, structured_output}, sampling_supported[], api_version?, quirks{…}` (the D18 per-model quirk flags) `, pricing{input, output, cache_read, cache_write_5m, cache_write_1h, per unit}, small_model_for?`.
- **API version per model** (`api_version`), so Anthropic's `2026-10-21` (and later versions) apply per model. **Resolves the open item from 03.**

### Small-model rule (default)
- Titles and compaction summaries use the catalog's `small_model_for` flag: same provider + same credential, cheapest flagged model, minimal effort. If none is flagged, fall back to the primary model at its lowest effort. Users can override this in Settings.

### Picker data (default)
- The model picker groups by provider, then shows recent models first, with badges for vision, PDF, reasoning, web search and "limited info". It shows the price per 1M tokens and the context window on hover. The ordering and recents live in `state.json` (D23).
