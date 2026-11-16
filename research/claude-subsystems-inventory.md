# Claude Desktop / Claude Code subsystem inventory

> Ticket [36](../tickets/36-feature-inventory.md) · Written 2026-11-16 · The *behaviour* Genie matches (WAYFINDER: "Claude Code/Desktop for behaviour").
> Companion: [opencode-pick-list.md](opencode-pick-list.md) (the *architecture* side).

## How to read this

- **Surface:** Chat (desktop chat), Code (desktop Code mode), CLI (terminal agent), Settings (desktop Settings UI).
- **Ticket:** the Genie ticket that owns the decision. `—` means no ticket owns it yet.
- **Tier**, read against D6 (desktop chat ships first; tools and the agent loop come after):
  - **v1** = the first desktop Chat release, plus the core slice it needs (providers, sessions, streaming).
  - **v2** = the agent milestone (Code mode, CLI, tools, MCP, extensions) and Chat features that need the tool loop. v2 is in the product plan, not a maybe.
  - **never** = out of scope (WAYFINDER "Out of scope", C1/C2), or tied to Anthropic's hosted services or subscription plans.
- *(unverified)* = I could not confirm it in the official docs on 2026-11-16. It is either from memory of earlier versions or only partly described.
- Everything here is a paraphrase. Follow the source links for the exact behaviour.

**Context that changes the picture (2026-11):**
1. The Claude apps have a **new Settings layout**. Help articles give two paths for most controls, for example "Settings > Appearance (older: General)" and "Settings > Your Claude (older: Memory / Capabilities)". There is no single official map of the new layout, so the Settings section below is put together from many articles.
2. **Chat and Cowork are merged** into "one Claude" ([16761823](https://support.claude.com/en/articles/16761823-claude-cowork-and-chat-are-one-claude)). One article says branching from an earlier point is *not* available in the merged experience, while others still describe editing a message to make a branch. Genie should copy the classic edit → branch behaviour.
3. Claude Code now reads **AGENTS.md** as well as CLAUDE.md ([memory](https://code.claude.com/docs/en/memory)). It also has its own **Code Mode** ([code-mode](https://code.claude.com/docs/en/code-mode)), **artifacts** ([artifacts](https://code.claude.com/docs/en/artifacts)) and **"mods"**, which are GUI/interface plugins ([mods](https://code.claude.com/docs/en/plugins/mods/overview)). Those three cut against parts of the KB's guidance; see "Surprises" at the end.

---

## 1. Claude Desktop / claude.ai chat

### 1.1 Sidebar, chat list, conversation

| Feature | What it does | Surface | Ticket | Tier |
|---|---|---|---|---|
| New chat / Recents list | Collapsible left sidebar with new-chat button and recent conversations; sidebar can be toggled but not removed ([8887527](https://support.claude.com/en/articles/8887527-customizing-your-appearance-settings)) | Chat | 31, 29 | v1 |
| Rename / delete chat | Manage individual conversations from the list *(unverified: exact menu)* | Chat | 31, 12 | v1 |
| Star / pin chats and projects | Starred projects appear for quick access ([9519177](https://support.claude.com/en/articles/9519177-how-can-i-create-and-manage-projects)); starring individual chats *(unverified)* | Chat | 31 | v1 |
| Sidebar chat search (by title/content) | Find past chats from the UI (separate from Claude's own past-chat search below) *(unverified: current UI)* | Chat | 31, 12 | v1 |
| Streaming Markdown, code blocks, copy | Rendered responses with syntax-highlighted code and copy buttons *(unverified: no dedicated article)* | Chat | 31, 37 | v1 |
| LaTeX / Mermaid rendering | Math and diagrams rendered inline, or as artifacts *(unverified for inline)* | Chat | 31 | v1 |
| Stop generation | Interrupt a streaming reply | Chat | 31, 02 | v1 |
| Edit earlier message → branch | Editing a past user message creates a new branch; you can switch back to the original version ([17153992](https://support.claude.com/en/articles/17153992-what-are-artifacts-and-how-do-i-use-them)); said to be unavailable in the merged experience | Chat | 31, 12 | v1 |
| Regenerate / retry response | Re-run the last assistant turn, with version arrows *(unverified: no current article found)* | Chat | 31, 12 | v1 |
| Model picker + effort + thinking toggle | Model menu by the send button; "Effort" submenu with a "Thinking/Extended" toggle; hidden where thinking is always on ([10574485](https://support.claude.com/en/articles/10574485-using-extended-thinking)) | Chat | 04, 03, 31 | v1 |
| Thinking display | Reasoning shown in a collapsible section above the answer ([10574485](https://support.claude.com/en/articles/10574485-using-extended-thinking)) | Chat | 31, 03 | v1 |
| Auto model switch on flagged message | Falls back to another model when safeguards trigger; toggle in Settings > Capabilities ([16049681](https://support.claude.com/en/articles/16049681-why-claude-switched-models-in-your-conversation-with-opus-5-or-opus-5-5)) | Chat | — | never (Anthropic-specific safety routing) |
| Usage indicator | Plan usage and limits ring/messages ([11647753](https://support.claude.com/en/articles/11647753-how-do-usage-and-length-limits-work)) → Genie shows **tokens and cost per provider** instead | Chat | 04, 35 | v1 (as cost/usage) |
| Incognito chat | Ghost-icon chat that isn't saved to history or memory and isn't searchable ([12260368](https://support.claude.com/en/articles/12260368-use-incognito-chats)) | Chat | 12 | v2 |
| Per-chat memory off | "+" menu toggle before the first message ([11817273](https://support.claude.com/en/articles/11817273-use-claude-s-chat-search-and-memory-to-build-on-previous-context)) | Chat | 10 | v2 |
| Quick entry (global hotkey) | Floating prompt window from a system shortcut, macOS ([12626668](https://support.claude.com/en/articles/12626668-use-quick-entry-with-claude-desktop-on-mac)) | Chat | 30 | v2 |
| Voice mode / dictation | Speech input *(unverified on desktop chat; CLI has dictation: [voice-dictation](https://code.claude.com/docs/en/voice-dictation))* | Chat | — | v2 |
| Share chat / public link | Shareable snapshot of a chat *(unverified)* | Chat | — | never (no hosted service) |

### 1.2 Projects

| Feature | What it does | Surface | Ticket | Tier |
|---|---|---|---|---|
| Projects | Group chats around shared context ([9517075](https://support.claude.com/en/articles/9517075-what-are-projects)) | Chat | 31, 12 | v1 |
| Project instructions | Instructions applied to every chat in the project ([9519177](https://support.claude.com/en/articles/9519177-how-can-i-create-and-manage-projects)) | Chat | 31, 11 | v1 |
| Project knowledge files | Files shared by all project chats, ≤30 MB each, total must fit context; chats don't share context except through knowledge ([9519177](https://support.claude.com/en/articles/9519177-how-can-i-create-and-manage-projects)) | Chat | 31, 11 | v1 |
| Project RAG mode | Switches to retrieval automatically when knowledge nears the context limit, about 10× capacity ([11473015](https://support.claude.com/en/articles/11473015-retrieval-augmented-generation-rag-for-projects)) | Chat | 11 | v2 |
| Project memory | Each project has its own memory and can opt out of account memory ([11817273](https://support.claude.com/en/articles/11817273-use-claude-s-chat-search-and-memory-to-build-on-previous-context)) | Chat | 10 | v2 |
| Project caching | Project content is prompt-cached; caches expire when idle ([9797557](https://support.claude.com/en/articles/9797557-usage-limit-best-practices)) | Chat | 11 | v1 (Anthropic and OpenAI caching through 03/11) |
| Project Library (beta) | Collects files you add and files Claude produces; rolling out from Claude Code first ([claude-projects](https://code.claude.com/docs/en/claude-projects)) | Chat/Code | — | v2 |
| Sharing projects with a team | Team plans share projects *(unverified detail)* | Chat | — | never (no team sync, WAYFINDER out of scope) |

### 1.3 Attachments and files

| Feature | What it does | Surface | Ticket | Tier |
|---|---|---|---|---|
| Upload files to a chat | Drag/drop, picker or paste; ≤500 MB each, ≤20 per chat, PDFs ≤1000 pages ([8241126](https://support.claude.com/en/articles/8241126-upload-files-to-claude)) | Chat | 31, 03 | v1 (images, PDF, text/code; Genie limits per provider capability) |
| Images in / vision | Image inputs to vision models ([8241126](https://support.claude.com/en/articles/8241126-upload-files-to-claude)) | Chat | 03 | v1 |
| Create and edit files (docx, xlsx, pptx, pdf) | Claude writes downloadable files in a code sandbox ([12111783](https://support.claude.com/en/articles/12111783-create-and-edit-files-with-claude)) | Chat | 09, 06 | v2 |
| GitHub / Drive file pickers | Add repo or Drive files as context ([10167454](https://support.claude.com/en/articles/10167454-use-the-github-integration)) | Chat | 17 | v2 (as MCP connectors) |

### 1.4 Artifacts

| Feature | What it does | Surface | Ticket | Tier |
|---|---|---|---|---|
| Artifacts side panel | Code, documents, HTML/React apps, SVG and diagrams rendered beside the chat ([17153992](https://support.claude.com/en/articles/17153992-what-are-artifacts-and-how-do-i-use-them)) | Chat | 31, 29 | **v2 (decision)**: needs a sandboxed iframe, a CSP and a custom scheme (ch.18) |
| Artifacts tab | Everything made is listed in an Artifacts sidebar tab ([17153992](https://support.claude.com/en/articles/17153992-what-are-artifacts-and-how-do-i-use-them)) | Chat | 31 | v2 |
| Artifact versions | Edits create versions; branching a chat makes its own artifacts ([17153992](https://support.claude.com/en/articles/17153992-what-are-artifacts-and-how-do-i-use-them)) | Chat | 12 | v2 |
| Persistent artifact storage | Artifacts can save data across sessions, about 100 MB / 25k records ([17153992](https://support.claude.com/en/articles/17153992-what-are-artifacts-and-how-do-i-use-them)) | Chat | — | never (hosted) |
| AI-powered artifacts | Artifacts that call Claude themselves ([11649438](https://support.claude.com/en/articles/11649438-prototype-ai-powered-apps-with-claude-artifacts)) | Chat | — | never |
| Publish / share artifact | Public link to an artifact *(unverified detail)* | Chat | — | never |

### 1.5 Web search, research, memory, chat search

| Feature | What it does | Surface | Ticket | Tier |
|---|---|---|---|---|
| Web search | Claude searches the web and cites sources; on by default for individuals, admin-enabled for Team/Enterprise ([10684626](https://support.claude.com/en/articles/10684626-enable-and-use-web-search)) | Chat | 15 | **v1 (decision)**: provider-native search only (Anthropic, OpenAI, Gemini), with citations; hidden for other providers |
| Research | Multi-search agent that returns a cited brief over several minutes; needs web search ([11088861](https://support.claude.com/en/articles/11088861-use-research-on-claude)) | Chat | 15, 19 | v2 |
| Memory | Claude saves topic-based memory entries as you chat; you can view, edit, pause or reset them; sensitive topics are opt-in ([11817273](https://support.claude.com/en/articles/11817273-use-claude-s-chat-search-and-memory-to-build-on-previous-context)) | Chat, Settings | 10 | v2 |
| Search and reference past chats | Claude searches your earlier chats (scoped to non-project chats or one project) as a tool call with citations ([11817273](https://support.claude.com/en/articles/11817273-use-claude-s-chat-search-and-memory-to-build-on-previous-context)) | Chat | 12, 06 | v2 |
| Profile preferences / custom instructions | Account-wide instructions applied to every chat ([10185728](https://support.claude.com/en/articles/10185728-understanding-claude-s-personalization-features)) | Settings | 10, 11 | v1 |
| Styles | Presets and custom styles that change format and tone ([10185728](https://support.claude.com/en/articles/10185728-understanding-claude-s-personalization-features)) | Chat | 11 | v2 |
| Skills in chat | Claude loads skill folders when relevant ([12512180](https://support.claude.com/en/articles/12512180-use-skills-in-claude)) | Chat | 21 | v2 |

### 1.6 Connectors, MCP, extensions

| Feature | What it does | Surface | Ticket | Tier |
|---|---|---|---|---|
| Connectors directory | Browse, connect and manage service connectors with per-tool permissions (Customize > Connectors) ([11176164](https://support.claude.com/en/articles/11176164-use-connectors-to-extend-claude-s-capabilities)) | Chat, Settings | 17 | v2 |
| Custom remote MCP connector | Add a remote MCP server by URL, with OAuth ([11175166](https://support.claude.com/en/articles/11175166-get-started-with-custom-connectors-using-remote-mcp)) | Settings | 17, 14 | v2 |
| Local MCP servers (desktop) | Local stdio servers, status and logs in the desktop app ([10949351](https://support.claude.com/en/articles/10949351-getting-started-with-local-mcp-servers-on-claude-desktop)) | Settings | 17 | v2 |
| Desktop extensions (.mcpb) | One-click packaged local MCP servers; sensitive fields kept in the OS keychain; Extension Developer install ([10949351](https://support.claude.com/en/articles/10949351-getting-started-with-local-mcp-servers-on-claude-desktop)) | Settings | 22 | v2 |
| "+" menu tool toggles | Turn connectors and tools on or off per chat ([10949351](https://support.claude.com/en/articles/10949351-getting-started-with-local-mcp-servers-on-claude-desktop)) | Chat | 17, 31 | v2 (v1 for the web-search toggle) |
| Extension allowlist (admin) | Org allowlist or block of desktop extensions ([12592343](https://support.claude.com/en/articles/12592343-enabling-and-using-the-desktop-extension-allowlist)) | Settings | 28 | v2 |
| Enterprise desktop config | MDM/policy keys for Claude Desktop ([12622667](https://support.claude.com/en/articles/12622667-enterprise-configuration-for-claude-desktop)) | Settings | 28 | v2 |

### 1.7 Other desktop surfaces (for completeness)

| Feature | What it does | Surface | Ticket | Tier |
|---|---|---|---|---|
| Cowork | Agentic desktop tasks with a VM sandbox, merged with chat ([16761823](https://support.claude.com/en/articles/16761823-claude-cowork-and-chat-are-one-claude)) | Chat | — | never (Genie Code mode covers the agentic case) |
| Computer use | Claude controls apps (Cowork/Code; Settings > Capabilities) ([14128542](https://support.claude.com/en/articles/14128542-let-claude-use-your-computer-in-cowork)) | Chat/Code | — | never |
| Claude in Chrome | Browser-extension agent ([chrome](https://code.claude.com/docs/en/chrome)) | — | — | never |
| Office/Docs add-ins, Slack, mobile, Dispatch | Claude inside other apps and on phones ([16951679](https://support.claude.com/en/articles/16951679-use-claude-in-google-docs-sheets-and-slides)) | — | — | never (out of scope) |
| Data export | Export your data from Settings > Privacy ([9450526](https://support.claude.com/en/articles/9450526-export-your-claude-data)) | Settings | 12 | v2 (single-chat Markdown export is v1, see below) |

### 1.8 Settings screens

Sections as named in the **new** layout, with the older name in brackets. *(unverified)* marks a section or control I could not confirm.

| Section | Controls (Claude) | Genie equivalent | Ticket | Tier |
|---|---|---|---|---|
| General | Language, startup options *(unverified)*; the "Only on your computer" option was removed | Language (i18n, en only at first), launch at login, check for updates | 33, 27 | v1 |
| Appearance [General] | Colour mode (Light/System/Dark), chat font (Default/System/Dyslexia-friendly) ([8887527](https://support.claude.com/en/articles/8887527-customizing-your-appearance-settings)) | Same, plus density | 33, 29 | v1 |
| Your Claude [Memory, Capabilities, Profile] | Profile preferences, memory (generate, pause, reset, manage), sensitive topics, search and reference chats ([11817273](https://support.claude.com/en/articles/11817273-use-claude-s-chat-search-and-memory-to-build-on-previous-context)) | v1: name + custom instructions. v2: memory, past-chat search | 33, 10 | v1 / v2 |
| Profile | Name, what Claude should call you, work role *(unverified as a separate section in the new layout)* | Folded into "Your Genie" | 33 | v1 |
| Capabilities | Model switch on flag, computer use, web search defaults, artifact/file-creation toggles *(partly unverified)* ([16049681](https://support.claude.com/en/articles/16049681-why-claude-switched-models-in-your-conversation-with-opus-5-or-opus-5-5)) | Web search on/off (v1); code execution and artifacts (v2) | 33, 15 | v1 |
| — (no Claude equivalent) | — | **Providers & Models**: API keys and cloud credentials, endpoints, default model, model list (C2: keys only) | 33, 14, 04 | v1 |
| Connectors (Customize > Connectors) | Connect, disconnect, per-tool permissions ([11176164](https://support.claude.com/en/articles/11176164-use-connectors-to-extend-claude-s-capabilities)) | MCP servers (remote + local) | 33, 17 | v2 |
| Desktop app [System / Extensions / Developer, under "This computer"] | Quick-entry shortcut, extensions manager, Developer (MCP config file, logs, debug) ([12626668](https://support.claude.com/en/articles/12626668-use-quick-entry-with-claude-desktop-on-mac), [10949351](https://support.claude.com/en/articles/10949351-getting-started-with-local-mcp-servers-on-claude-desktop)) | Shortcuts (v1 minimal); Extensions, Developer/logs (v2) | 33, 22, 30 | v1 / v2 |
| Code / Claude Code | Prompt suggestions, browser tools, allow-bypass mode, worktree location, branch prefix, auto-archive, Remote Control ([desktop](https://code.claude.com/docs/en/desktop)) | Code mode settings, Permissions | 33, 13, 24 | v2 |
| Privacy | Export data, training opt-out, retention *(partly unverified)* ([9450526](https://support.claude.com/en/articles/9450526-export-your-claude-data)) | Telemetry/crash opt-in (no prompt content), clear history, export | 33, 35 | v1 |
| Billing | Plan, usage, credits ([12138966](https://support.claude.com/en/articles/12138966-release-notes)) | Licence/account (35) + per-provider spend view | 33, 35 | v1 (licence only) |
| Keybindings | Not a Claude Desktop section *(unverified)*; CLI has [keybindings](https://code.claude.com/docs/en/keybindings) | Keyboard shortcuts | 33 | v2 |
| Organization settings (admin) | Roles, capabilities, connectors, memory controls ([13930458](https://support.claude.com/en/articles/13930458-set-up-role-based-permissions-on-enterprise-plans)) | Managed policy file/MDM (not a UI) | 28 | v2 |

---

## 2. Claude Desktop Code tab (documented: [desktop](https://code.claude.com/docs/en/desktop))

| Feature | What it does | Surface | Ticket | Tier |
|---|---|---|---|---|
| Session sidebar | Parallel sessions, each with its own folder; group, sort, archive; Ctrl+Tab | Code | 24, 12 | v2 |
| Mode selector | Manual / Accept edits / Plan / Auto / Bypass, per session | Code | 13 | v2 |
| Prompt box "+" | Attach files, skills, connectors, plugins; mid-run corrections | Code | 02, 21 | v2 |
| Suggested next prompt | Tab to accept; toggle in Settings > Code | Code | — | v2 |
| Diff view + line comments | Per-file review, comment on lines, submit to Claude; `/code-review` | Code | 23 | v2 |
| PR monitoring | CI status, auto-fix failing checks, merge when ready (needs `gh`) | Code | — | v2 |
| Preview / browser pane | Run dev servers, auto-verify with screenshots and DOM; `.claude/launch.json` | Code | 30 | v2 (CDP question: 30) |
| Environments | Local, Cloud, SSH, WSL; saved environments | Code | — | v2 (local + WSL); never (cloud) |
| Worktree toggle | Session in an isolated git worktree (`.claude/worktrees/`) | Code | 24 | v2 |
| Workspace panes | Draggable chat/diff/browser/terminal/file/plan/tasks/subagent panes; pop-out windows | Code | 29 | v2 |
| Integrated terminal | Shares the session's cwd and env | Code | 07 | v2 |
| File pane | View/edit files (local only), auto-save | Code | — | v2 |
| Side chat (`/btw`) | Ask with session context without adding to the transcript | Code/CLI | 19 | v2 |
| Background tasks view | Subagents, background shells, workflows | Code | 19 | v2 |
| Cross-session tools | Claude lists, reads, messages and archives other sessions | Code | — | never (v1 of Code at least) |
| Context card + usage ring | Context-window usage; `/compact` | Code | 16 | v2 |
| Transcript view modes | Normal / Thinking / Verbose (Ctrl+O) | Code/CLI | 05 | v2 |
| OS notifications | Notify when a session finishes | Code | 30 | v2 |
| Scheduled tasks | Recurring local tasks ([desktop-scheduled-tasks](https://code.claude.com/docs/en/desktop-scheduled-tasks)) | Code | — | v2 (late) |
| Cloud handoff, Remote Control, Dispatch, computer use, iOS simulator | Hosted/mobile features | Code | — | never |

---

## 3. Claude Code CLI

| Feature | What it does | Surface | Ticket | Tier |
|---|---|---|---|---|
| Agent loop | Gather context → act with tools → verify, until done; interrupt and steer mid-turn ([how-claude-code-works](https://code.claude.com/docs/en/how-claude-code-works), [agent-loop](https://code.claude.com/docs/en/agent-sdk/agent-loop)) | CLI/Code | 01, 18 | v2 |
| Message queueing while working | Typed messages queue and are delivered mid-turn ([interactive-mode](https://code.claude.com/docs/en/interactive-mode)) | CLI/Code | 02 | v2 |
| **Tools: file** | Read, Write, Edit, NotebookEdit, Glob, Grep ([tools-reference](https://code.claude.com/docs/en/tools-reference)) | CLI/Code | 06 | v2 |
| **Tools: shell** | Bash, PowerShell, Monitor (watch background output), TaskOutput/TaskStop | CLI/Code | 07 | v2 |
| **Tools: web** | WebFetch, WebSearch | CLI/Code/Chat | 15 | v2 (v1 provider-native search in Chat) |
| **Tools: planning/interaction** | TodoWrite, TaskCreate/Get/List/Update, AskUserQuestion, EnterPlanMode/ExitPlanMode | CLI/Code | 06, 18 | v2 |
| **Tools: agents** | Agent (subagent), SendMessage, ListAgents, Workflow, Skill | CLI/Code | 19, 21 | v2 |
| **Tools: MCP plumbing** | ToolSearch (deferred tools), ListMcpResources/ReadMcpResource, WaitForMcpServers | CLI/Code | 17 | v2 |
| **Tools: other** | LSP, EnterWorktree/ExitWorktree, Cron*/ScheduleWakeup, PushNotification, Artifact, SendUserFile | CLI/Code | 06, 24 | v2 (LSP, worktree); never (cron, push, hosted artifacts) |
| Permission modes | default (Manual), acceptEdits, plan, auto (classifier), dontAsk, bypassPermissions; Shift+Tab cycles ([permission-modes](https://code.claude.com/docs/en/permission-modes)) | CLI/Code | 13 | v2 (auto mode: v2 late) |
| Permission rules | `allow`/`ask`/`deny` lists of `Tool(specifier)`; deny > ask > allow; lists merge across scopes; "don't ask again" writes local allow rules ([permissions](https://code.claude.com/docs/en/permissions)) | CLI/Code/Settings | 13 | v2 |
| Working directories / add-dir | Extra trusted dirs, block reads/writes outside them ([permissions](https://code.claude.com/docs/en/permissions)) | CLI/Code | 13, 32 | v2 |
| Workspace trust | Project allow rules, env and marketplaces wait until the folder is trusted ([settings](https://code.claude.com/docs/en/settings)) | CLI/Code | 32 | v2 |
| Settings layers | managed > `--settings` CLI > project local > shared project > user; strict JSON, hot reload, `/status` shows sources ([settings](https://code.claude.com/docs/en/settings)) | CLI/Settings | 08 | v1 (the layers and schema are needed for Chat Settings too) |
| CLAUDE.md memory | Managed/user/project/local files, `@imports`, `.claude/rules/*.md` with path globs, AGENTS.md accepted, `/memory`, `/init` ([memory](https://code.claude.com/docs/en/memory)) | CLI/Code | 10 | v2 |
| Auto memory | Claude-written notes per repo, loaded each session (first 200 lines / 25 KB) ([memory](https://code.claude.com/docs/en/memory)) | CLI/Code | 10 | v2 |
| Slash commands | About 120 built-ins (e.g. /model, /effort, /compact, /context, /rewind, /resume, /permissions, /mcp, /hooks, /agents, /skills, /plugin, /config, /status, /cost, /usage, /init, /memory, /review, /btw, /branch, /fork, /export, /add-dir, /sandbox, /statusline, /output-style) ([commands](https://code.claude.com/docs/en/commands)) | CLI/Code | 21, 05 | v2 |
| Custom commands = skills | User/project skills appear as `/name`; chaining several in one message ([skills](https://code.claude.com/docs/en/skills)) | CLI/Code | 21 | v2 |
| Skills | `SKILL.md` folders, model- or user-invoked, `context: fork` to run in a subagent ([skills](https://code.claude.com/docs/en/skills)) | CLI/Code/Chat | 21 | v2 |
| Subagents | Markdown + frontmatter agents (tools, model, permissions); foreground/background ([sub-agents](https://code.claude.com/docs/en/sub-agents)) | CLI/Code | 19, 18 | v2 |
| Agent teams / cross-session messaging / workflows | Orchestrate several sessions ([agent-teams](https://code.claude.com/docs/en/agent-teams), [workflows](https://code.claude.com/docs/en/workflows)) | CLI | 19 | never (for now) |
| Hooks | 33 events: SessionStart/End, Setup, UserPromptSubmit/Expansion, Pre/PostToolUse, PostToolUseFailure, PostToolBatch, PermissionRequest/Denied, Notification, MessageDisplay, Subagent Start/Stop, TaskCreated/Completed, Stop, StopFailure, TeammateIdle, InstructionsLoaded, ConfigChange, CwdChanged, DirectoryAdded, FileChanged, WorktreeCreate/Remove, Pre/PostCompact, Pre/PostModelSwitch, Elicitation, ElicitationResult. Types: command, http, mcp_tool, prompt, agent. Exit 2 blocks ([hooks](https://code.claude.com/docs/en/hooks)) | CLI/Code | 20 | v2 (start with ~10 core events) |
| MCP scopes | local (default, `~/.claude.json`), project (`.mcp.json`), user; plus managed servers and allow/deny lists ([mcp](https://code.claude.com/docs/en/mcp)) | CLI/Code/Settings | 17, 28 | v2 |
| MCP transports + OAuth | HTTP (recommended), stdio, WebSocket, SSE (deprecated); OAuth via `/mcp`; tools named `mcp__server__tool`; tool search on by default ([mcp](https://code.claude.com/docs/en/mcp)) | CLI/Code | 17, 14 | v2 |
| Plugins and marketplaces | Bundles of skills, agents, hooks, MCP servers; git/URL marketplaces; org-managed marketplaces ([plugins](https://code.claude.com/docs/en/plugins/overview)) | CLI/Code | 22 | v2 |
| Mods (interface plugins) | Plugins that draw UI (bands, panels, status line) and react to events ([mods](https://code.claude.com/docs/en/plugins/mods/overview)) | CLI/Code | 22 | never (C4 / KB: no third-party code in the host) |
| Checkpoints / rewind | A checkpoint per prompt from file-tool edits; Esc Esc or `/rewind`: restore code, conversation, or both; summarise from/up to here; 100 per session, ~30-day retention; Bash-only changes not tracked ([checkpointing](https://code.claude.com/docs/en/checkpointing)) | CLI/Code | 23 | v2 |
| Sessions: resume, continue, fork, branch | `--continue`, `--resume`, `/branch`, `--fork-session` ([sessions](https://code.claude.com/docs/en/sessions)) | CLI/Code | 12 | v2 |
| Compaction | Auto-compact near the limit, `/compact [focus]`, `/autocompact` toggle; PreCompact/PostCompact hooks ([context-window](https://code.claude.com/docs/en/context-window), [commands](https://code.claude.com/docs/en/commands)) | CLI/Code | 16 | v2 (Chat needs it for long chats: see decision) |
| Prompt caching | Behaviour and cache invalidation on model switch ([prompt-caching](https://code.claude.com/docs/en/prompt-caching)) | all | 11 | v1 |
| Output styles | Replace the system prompt's style: built-in Explanatory and Learning, plus custom ([output-styles](https://code.claude.com/docs/en/output-styles)) | CLI/Code | 11 | v2 |
| Status line | User script renders a custom status line from session JSON ([statusline](https://code.claude.com/docs/en/statusline)) | CLI | 05 | v2 |
| Model config | `/model`, `/effort`, fallback models, available/denied model lists ([model-config](https://code.claude.com/docs/en/model-config)) | all | 04 | v1 (picker + effort) |
| Headless `-p` | Text / json / stream-json output, `--max-turns`, permission flags ([headless](https://code.claude.com/docs/en/headless)) | CLI | 25 | v2 |
| Agent SDK | TS and Python SDKs around the same loop; sessions, hooks, permissions, structured output ([agent-sdk](https://code.claude.com/docs/en/agent-sdk/overview)) | — | 25, 02 | v2 (generated TS client first) |
| IDE integrations | VS Code extension, JetBrains plugin, `/ide` ([vs-code](https://code.claude.com/docs/en/vs-code), [jetbrains](https://code.claude.com/docs/en/jetbrains)) | — | 25 | never (WAYFINDER out of scope; ACP weighed in 25) |
| Sandboxing | Bash sandbox: Seatbelt (macOS), bubblewrap (Linux/WSL2); native Windows unsandboxed; writes limited to the working dirs; network through a domain-allowlist proxy ([sandboxing](https://code.claude.com/docs/en/sandboxing)) | CLI/Code | 09 | v2 |
| Managed settings | `managed-settings.json`, MDM/OS policy, server-managed from the admin console, SDK host; some restrictive keys win from any scope ([managed-settings](https://code.claude.com/docs/en/managed-settings)) | all | 28 | v2 (file + MDM; no server-managed) |
| Worktrees | `--worktree`, `.claude/worktrees/`, WorktreeCreate/Remove hooks ([worktrees](https://code.claude.com/docs/en/worktrees)) | CLI/Code | 24 | v2 |
| Code Mode | Model writes code that calls tools in one step ([code-mode](https://code.claude.com/docs/en/code-mode)) | CLI | 06 | never (KB #27) |
| Fullscreen / inline rendering, vim mode, keybindings, themes | Terminal UI options ([fullscreen](https://code.claude.com/docs/en/fullscreen), [keybindings](https://code.claude.com/docs/en/keybindings)) | CLI | 05 | v2 |
| Cost / usage / OTel monitoring | `/cost`, `/usage`, OpenTelemetry export ([costs](https://code.claude.com/docs/en/costs), [monitoring](https://code.claude.com/docs/en/monitoring-usage)) | all | 35, 28 | v1 (cost display); v2 (OTel) |
| Goal mode, loop, schedule, routines, channels | Long-running or scheduled automation ([goal](https://code.claude.com/docs/en/goal), [routines](https://code.claude.com/docs/en/routines), [channels](https://code.claude.com/docs/en/channels)) | CLI | 19 | never (for now) |
| Remote Control, teleport, cloud sessions, Slack, GitHub Actions, Code Review service | Hosted surfaces ([remote-control](https://code.claude.com/docs/en/remote-control), [github-actions](https://code.claude.com/docs/en/github-actions)) | — | — | never |
| Gateways / third-party providers | Bedrock, Vertex, Foundry, LLM gateways ([third-party-integrations](https://code.claude.com/docs/en/third-party-integrations)) | all | 03, 14 | v1 (Genie's multi-provider core, D2) |
| Config import | n/a in Claude; Genie imports `.claude/` | — | 26 | v2 |

---

## 4. Proposed v1 Chat cut (input to ticket 31)

**In v1:** sidebar (new chat, recents, rename, delete, pin, title/content search) · projects with instructions and knowledge files (no RAG) · attachments (images, PDF, text/code) · streaming Markdown, code, LaTeX and Mermaid · model picker across providers with effort/thinking toggle and a collapsible thinking display · stop, edit → branch, regenerate · per-chat token and cost display · provider-native web search with citations (where the provider supports it) · custom instructions (profile) · single-chat Markdown export · Settings: General, Appearance, Your Genie (profile + instructions), Providers & Models, Capabilities (web search), Privacy (opt-in telemetry), Account/Licence, minimal shortcuts.

**v2:** artifacts panel, MCP connectors and extensions, memory and past-chat search, styles, skills, research, RAG, incognito, file creation, quick entry, voice, the whole Code tab and CLI.

**never:** hosted/shared surfaces (share links, published artifacts, cloud sessions, Remote Control, Dispatch, mobile, Slack, Chrome, IDE plugins), Cowork, computer use, Anthropic plan/safety-specific behaviour, third-party UI mods.

## 5. Surprises

1. **Claude Code reads AGENTS.md now.** The KB (ch.05) says to avoid it, but ticket 10 lists it. Reading it costs little and helps import.
2. **Claude Code has its own Code Mode, Artifact tool and UI "mods".** The KB's #27 (Code Mode SKIP) and its "no third-party GUI code" rule are still right for Genie (C4), but none of these are OpenCode-only ideas any more.
3. **The Claude Settings layout is in flux** (new "Your Claude" and "Desktop app" sections; Chat merged with Cowork, and branching apparently missing there). Ticket 33 should copy the *structure* (left nav: General, Appearance, personal, capabilities, connectors, app, privacy, account) rather than chase exact names.
4. **Desktop and CLI share one settings stack** in Claude ([settings](https://code.claude.com/docs/en/settings)). That supports making ticket 08's layered config a v1 dependency of Chat Settings.
5. **Claude's checkpoints skip Bash-only changes and most subagent edits** ([checkpointing](https://code.claude.com/docs/en/checkpointing)). This confirms that OpenCode's shadow-git approach (pick-list #12) is the better default.

## Tier counts

134 rows in §1–3, each counted once under the first tier in its Tier cell: **v1 = 34 · v2 = 82 · never = 18**.
Ten rows split tiers (for example Settings "Your Claude" v1/v2, web tools v2 with v1 native search). Each is counted under its first tier.
