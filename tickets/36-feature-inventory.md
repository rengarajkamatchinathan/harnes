# 36 — Feature inventory: Claude Desktop + Claude Code + OpenCode

type: wayfinder:research · status: closed · claimed-by: research-agent (2026-11-16) · blocked-by: —

## Question
Produce research/claude-subsystems-inventory.md (linked from the KB but missing) and research/opencode-pick-list.md.

## To decide
- Matrix of Chat, Code, CLI and Settings features
- Mark each v1 / v2 / never
- For OpenCode components: port / adapt / skip

## References
- OpenCode ch.18: [18-app-ui-and-gui-plugins.md](../knowledge/opencode/18-app-ui-and-gui-plugins.md) — read TL;DR, Architecture, Genie takeaways

## Resolution
- Docs: [claude-subsystems-inventory.md](../research/claude-subsystems-inventory.md) (134 Chat/Code/CLI/Settings features: **v1 34 · v2 82 · never 18**) and [opencode-pick-list.md](../research/opencode-pick-list.md) (45 items: **port 23 · adapt 12 · skip 10**; #26 is still the in-process plugin ABI = SKIP).
- **Proposed Chat v1 cut:** sidebar (recents, rename, delete, pin, search) · projects with instructions and knowledge files (no RAG) · attachments (images, PDF, text) · streaming Markdown/code/LaTeX/Mermaid · multi-provider model picker with effort/thinking and a thinking display · stop, edit → branch, regenerate · token and cost display · provider-native web search with citations · custom instructions · single-chat export.
- **Chat v1 Settings:** General, Appearance, Your Genie (profile/instructions), Providers & Models (keys only, C2), Capabilities (web search), Privacy (opt-in telemetry), Account/Licence. Ticket 08's layered config is a v1 dependency.
- **v2:** artifacts panel, MCP connectors/extensions, memory and past-chat search, styles, skills, research/RAG, the Code tab and the CLI. **never:** hosted/shared/mobile surfaces, Cowork, computer use, UI mods, IDE plugins.
- Open for ticket 31 / the user: artifacts in v1 or v2; native web search in v1; MCP in Chat v1; compaction for long chats in v1. Ticket 01 must also square D4's "shared core daemon" with pick-list #43 (skip OpenCode's shared service).
