# 31 — Desktop Chat v1 scope

type: wayfinder:grilling · status: closed · claimed-by: charting session (2026-11-16) · blocked-by: 36,29

## Question
Delivery is chat-first (D6). What exactly is in Chat v1?

## To decide
- Chat list + search, projects with instructions and knowledge files
- Attachments (images, PDF), markdown/code/LaTeX/mermaid rendering
- Artifacts panel?
- Model picker, thinking toggle, edit/regenerate/stop, usage and cost
- Web search (15), MCP connectors (17), memory (10): v1 or later

## References
- OpenCode ch.18: [18-app-ui-and-gui-plugins.md](../knowledge/opencode/18-app-ui-and-gui-plugins.md) — read TL;DR, Architecture, Genie takeaways

## Resolution
Resolved 2026-11-16. This is the **authoritative Chat v1 scope**. Where it differs from [research/claude-subsystems-inventory.md](../research/claude-subsystems-inventory.md), this list wins.

### Chat v1: IN
**Conversation**
- Sidebar: new chat, recents, rename, delete, pin, **sidebar search** (FTS, D24).
- Streaming Markdown, code blocks with copy, LaTeX, Mermaid; thinking shown as a collapsible section.
- Stop · edit → **in-place branch `‹ 2/3 ›`** · regenerate (D24) · Resume after an interrupt (D20).
- Model picker across providers + **per-model effort selector**; sampling settings hidden (D27).
- Token and **cost** display per message and per chat (D27 pricing).
- **Incognito chat** (pulled into v1 by the user): not saved to history, memory or search. Kept in daemon memory only and discarded when closed; the UI shows a clear incognito banner.
- Single-chat export (Markdown/JSON) (D24).

**Projects**
- Projects with instructions + knowledge files (whole files in context, prompt-cached; **automatic RAG above the limit, per D32**). Starred projects.

**Attachments**
- Images, PDF, text/code: drag/drop, picker, paste. Limits come from per-model capability (D27).

**Artifacts** (D28)
- Documents, code, SVG, Mermaid, HTML, React, in the sandboxed panel, with versions.

**Tools in chat** (D15 tool loop; full list in 06; also **web_fetch** and **provider-native code execution**, D35)
- **Web search:** provider-native only, with citations; toggle greyed out where unsupported (D11).
- **MCP connectors: "like Claude and OpenCode"** (user): local stdio servers, **remote by URL + OAuth**, **per-chat "+" tool toggles**, per-tool permission prompts (D25), server status and logs in Settings, plus a **curated connector directory** (Genie-maintained list with one-click add). Packaged one-click extensions (`.mcpb`-style) stay v2 (→ 22).
- **Built-in browser** via the user's Chrome/Edge (D21).
- **Memory** (pulled into v1 by the user): Genie saves memory entries across chats; the user can view, edit, pause and reset them in Settings; memory is per project + global; incognito chats never write memory (→ 10).
- **Search past chats as a tool** (pulled into v1 by the user): the model can search earlier chats (scoped to non-project chats, or the current project) and cites them (→ 12 FTS, 06/10).
- **Skills in chat** (pulled into v1 by the user): SKILL.md folders, an index in the system prompt, loaded on demand (→ 21). **Scripts in chat are v2 (D44)**; v1 = instructions + files.
- **Basic compaction** for long chats (D13 → 16).

**Settings** (→ 33)
- General, Appearance, "Your Genie" (name, custom instructions, **memory**), Providers & Models (keys, Entra sign-in, Advanced per model), Capabilities (web search, artifacts, browser, skills), Connectors/MCP, Permissions (saved approvals), Privacy (retention, encryption recovery key, export), Licence/Account.

**Platforms:** Windows, macOS, Linux at full support (D9). **Providers:** Anthropic, OpenAI, Azure Foundry (key + Entra), Vertex/Gemini, OpenAI-compatible (D26).

### Chat v1: OUT (v2 or later)
~~Project RAG~~ (moved to v1 by D32) · styles · research mode · voice · quick-entry hotkey · file creation (docx/xlsx/pptx) · packaged extensions · global Artifacts tab · Bedrock · Genie's own browser engine · Code tab / CLI (after chat, D6) · **never:** share links, hosted artifacts, AI-calling artifacts, Anthropic-specific safety routing, computer use.

### Suggested milestone order inside v1 (default; can be changed in the spec phase)
1. **M1, core chat:** daemon + protocol + store + 4 provider adapters + catalog + credentials; sidebar, streaming, branching, projects, attachments, settings basics. Ticket 37 prototype first.
2. **M2, tools:** tool loop + permissions, web search, MCP (local, remote, toggles), artifacts (documents/code/SVG/Mermaid/HTML), compaction.
3. **M3, intelligence:** memory, past-chat search, skills, incognito, React artifacts, connector directory.
4. **M4, browser + hardening:** Chrome/Edge browser agent, the hostile-artifact test, encryption recovery, packaging and signing on all 3 OSes.

### Scope warning
v1 is now large: roughly Claude Desktop's chat feature set **plus** a browser agent, on 3 OSes and 5 provider families. The milestones above let each step ship internally. The spec phase should estimate effort and decide whether v1 = M1–M4 or v1 = M1–M2 with later points.
