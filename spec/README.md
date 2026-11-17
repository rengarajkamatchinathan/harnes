# Genie engineering spec

> Wayfinder step: **to-spec**. Written 2026-11-17 from [WAYFINDER.md](../WAYFINDER.md) (D1–D57, C1–C4), the 41 tickets in [tickets/](../tickets/) and the reports in [research/](../research/).
> Status: draft for engineering review. **No production code.** The next Wayfinder step is *to-tickets* (build tickets cut from [10-plan-and-estimates.md](10-plan-and-estimates.md)).

## 1. One-page summary

Genie is a commercial, multi-provider AI harness from Synergech. It has two thin clients on one shared Rust engine (D4, D14, D20):

- **Desktop app** (Tauri 2 + React/TypeScript; D1, D5, D8): **Chat** and **Code** modes, a full-page **Settings** UI, and a right-side panel (Artifact, Browser, Changes, Files, Terminal; D54).
- **CLI** `genie` (Rust; D42): an inline terminal coding agent with Claude Code-level features, a headless mode `genie -p` (D51) and an importer (D53).
- **Daemon** `genie daemon`: one per user and per release channel. It is reached over a user-only Unix socket or Windows named pipe plus a token. It has no TCP port (D14, D20). It owns all state: sessions, the encrypted store, providers, tools, MCP servers, the browser and policy.

What makes the engine work:

| Area | Design in one line | Decisions |
|---|---|---|
| Protocol | JSON-RPC 2.0 over NDJSON, bidirectional; durable events with a per-session `seq`, resumable with no lossy streams | D22 |
| Providers | Our own adapters on plumbing crates; **4 protocols in v1** (Anthropic Messages, OpenAI Responses, OpenAI Chat, Gemini); Bedrock is v2 | D18, D26 |
| Models | Genie-hosted signed catalog + bundled snapshot + live discovery | D27 |
| Storage | One SQLCipher DB + AEAD-encrypted content-addressed blobs; message tree with in-place branches | D24 |
| Agent | Per-session actor with a durable inbox (steer/queue); tools run while the model streams; engine-enforced permissions and modes | D20, D25, D35 |
| Context | Cache-stable baseline + `<system-update>` deltas; 4 cache breakpoints; project RAG above 50% of the window | D31, D32 |
| Safety | Workspace trust, definition-hash approvals, managed policy above all settings, sandboxed artifacts, per-site browser rules | D21, D25, D28, D38, D43 |
| Shipping | Signed packages on Windows/macOS/Linux, signed feeds on Azure Storage + CDN, 3 channels | D40 |

**Release shape.** Chat v1 ships first (D6), in four milestones (D29, ticket 31): **M1 core chat → M2 tools → M3 intelligence → M4 browser + hardening**. The **Code/CLI track** (M5 engine + CLI, M6 desktop Code tab) follows. Bedrock, skill scripts in a sandbox, the CEF browser, plugins and ACP are v2.

**Size.** Chat v1 (M1–M4) is about **178–249 engineer-weeks**. The Code/CLI track adds about **66–89**. With 4 engineers, Chat v1 takes about **12–17 months** of calendar time. See [10-plan-and-estimates.md](10-plan-and-estimates.md) for the breakdown, the timelines for 3–5 engineers, the risk register and the recommendation on v1 scope.

## 2. Files

| File | What it covers | Main decisions and tickets |
|---|---|---|
| [01-architecture.md](01-architecture.md) | Process topology, crate graph, dependency rules, lifecycle state machines, cancellation tree, error taxonomy | D4, D14, D20, D41, D45; tickets 01, 38 |
| [02-protocol.md](02-protocol.md) | JSON-RPC methods and notifications, schemas, durable vs ephemeral events, driver lease, error codes, versioning, codegen | D20, D22, D51; ticket 02 |
| [03-providers-and-models.md](03-providers-and-models.md) | Canonical Message/Part model, Protocol/Provider traits, 4 adapters, routes, replay, caching, errors and retry, catalog, credentials, conformance | D18, D26, D27, D41; tickets 03, 04, 14, 34 |
| [04-storage.md](04-storage.md) | SQLCipher schema, blobs, event log, migrations, FTS, retrieval index, retention, export, checkpoints | D24, D32, D49; tickets 12, 23 |
| [05-desktop-ui.md](05-desktop-ui.md) | Screens and flows for Chat, Code, Settings and first run; design tokens, renderer, artifact sandbox, browser screencast, terminal, accessibility | D19, D28, D34, D54–D57; tickets 29, 31, 33, 39 |
| [06-agent-runtime.md](06-agent-runtime.md) | Turn loop, context, compaction, instructions and memory, tools, permissions, web, MCP, skills, agents, hooks, shell, worktrees | D25, D30–D37, D44–D50; tickets 06, 07, 10, 11, 13, 15–21, 24 |
| [07-cli.md](07-cli.md) | Inline TUI, commands and flags, headless, importer | D42, D51, D53; tickets 05, 25, 26 |
| [08-security-and-policy.md](08-security-and-policy.md) | Threat model and mitigations; managed policy keys; audit; telemetry; workspace trust; encryption | C4, D21, D24, D28, D38, D39, D43; tickets 28, 32, 35 |
| [09-distribution-and-ops.md](09-distribution-and-ops.md) | Packaging, signing, updates, channels, feeds, CI checks | D40, D41; tickets 27, 34, 41 |
| [10-plan-and-estimates.md](10-plan-and-estimates.md) | **Work breakdown, estimates, timeline, "Is v1 = M1–M4 realistic?", risks, pending items** | D29, ticket 31 |
| [11-open-questions.md](11-open-questions.md) | Gaps the build must settle, each with a proposed default; all **Spec assumptions** | — |

## 3. How to read this spec

- **Start with this page, then [10-plan-and-estimates.md](10-plan-and-estimates.md)** if you plan work, or [01-architecture.md](01-architecture.md) if you build it.
- **Source of truth.** The decisions (D-numbers) in WAYFINDER.md and the ticket **Resolution** sections win over this spec. Where a later decision amends an earlier one, this spec applies the latest. The main amendments are:
  - D32 moves project RAG into v1 (amends D29).
  - D44 defers skill scripts in chat to v2 (amends D37); the sandbox (ticket 09) is v2.
  - D26 narrows D18 to 4 adapters in v1; Bedrock is v2.
  - D55 ships the interactive Terminal with the Code tab (amends D45).
  - D56 shows the browser inside Genie's window via CDP screencast (amends D16/D21).
  - D57 fixes the renderer approach and keeps D8 conditional until the macOS/Linux bench runs.
- **Citations.** Every section names the decisions and tickets it implements, in the form *(D22; t02)*. "t02" means ticket 02. "KB ch.07" means `knowledge/opencode/07-*.md`.
- **Spec assumptions.** Where the decisions leave a gap, the spec fills it and marks it **Spec assumption (SA-nn):** with a short reason. Every SA is listed in [11-open-questions.md](11-open-questions.md) with its proposed default. An SA is a proposal, not a decision. Review them first.
- **Type sketches.** Rust and TypeScript snippets show shapes and names, not finished code. Field lists are the minimum. Boilerplate (derives, error plumbing) is left out.
- **Estimates** are in focused engineer-weeks (ew), given as a low–high range. They include unit tests and code review. They exclude design, product management, legal and dedicated QA staff (see SA-37).
- **OpenCode code.** OpenCode is MIT-licensed. Code we adapt (for example `genie-process` from `opencode-pty`, `genie-markdown` from patchmark, the edit matcher) keeps an MIT notice header and carries no OpenCode branding (C3).

## 4. Glossary

Terms are used with these meanings in every file.

| Term | Meaning |
|---|---|
| **Daemon** | The shared per-user, per-channel `genie daemon` process. It holds the engine. Also called "the engine" in the UI ("Starting engine…"). |
| **Client** | The desktop app (through its Tauri Rust side) or the CLI. A client connects to the daemon and holds no state of its own beyond UI state. |
| **Channel** | A release stream: `stable`, `beta` or `dev`. Each has its own binaries, data dir, socket and DB, so they run side by side (D40). |
| **Session** | One conversation. `kind` is `chat` or `code`. A session is a message tree plus metadata. Subagents are child sessions. |
| **Incognito session** | A chat session that lives only in daemon memory. It is never written to the store, memory or search (t31). |
| **Turn** | The work the engine does in response to one admitted user message: one or more steps until the model stops. |
| **Step** | One model request and its streamed response, plus the tool calls it makes. A turn has 1..n steps. |
| **Inbox** | The durable per-session queue of user input. `steer` items are delivered at the next step boundary; `queue` items after the turn ends (D20). |
| **Session actor** | The single tokio task that owns a session's state and drains its inbox (D20). |
| **Driver / driver lease** | The one client allowed to send commands that change a session. Others are read-only and see **Take over** (D20, D22). |
| **Durable event** | A fact written to the store with a per-session `seq` in the same transaction as its projection (D20, D24). Replayed on resume. |
| **Ephemeral event** | A live-only update (token deltas, progress, frames). Never stored. Carries the `seq` of the durable item it belongs to. |
| **seq / cursor** | The per-session, gap-free sequence number of durable events. Clients resume with `after_seq`. |
| **synced** | The marker the daemon sends after replaying durable events, before live events (D22). |
| **resync_required** | The notice sent instead of dropping events when a slow client overflows; the client re-subscribes from `from_seq` (D22). |
| **Execution claim** | A row written when a turn starts. If the daemon restarts and finds a claim with no settled turn, the turn becomes `interrupted` and the UI shows **Resume** (D20). |
| **Ask** | A durable request for a user decision: permission, MCP elicitation, MCP sampling approval, `ask_user` question, browser confirmation or definition-hash approval. Stored in `permission_pending` (D22, D25). |
| **Part** | One piece of a message: text, reasoning, media, tool call, tool result, citation, artifact ref, compaction, and so on (t03, t12). |
| **Wire record** | The exact bytes Genie sent to and received from a provider for one step. Kept so normalisation stays append-only and reasoning replays correctly (D18, D24). |
| **Protocol (adapter)** | Code that speaks one provider wire format (e.g. Anthropic Messages). Not to be confused with the client/daemon protocol, which this spec calls "the Genie protocol" or "the protocol". |
| **Route** | A protocol + endpoint + auth + quirks that one model is reached through (e.g. "Claude via Foundry"). |
| **Compat / ModelPatch** | Plain-data quirk flags per route, and catalog-driven overlays per model (t03). |
| **Origin / AccountTag** | Who produced an opaque part (provider, model, protocol, account hash); decides whether it may be replayed (D18). |
| **Catalog** | Genie's signed model list with capabilities, pricing and quirks (D27). **Live discovery** adds models found at run time. |
| **Context baseline** | The cache-stable first context message: instructions, project knowledge, skills index, memory, environment (D32). |
| **`<system-update>`** | A small inline message that tells the model a baseline source changed, without rewriting the cached prefix (D31). |
| **Breakpoint** | A prompt-cache marker. Up to 4 per request: tools, system, baseline, conversation tail (D32). |
| **Spill** | Moving tool output above 2,000 lines / 50 KiB to a blob, leaving a marker the model can page (D32). |
| **Compaction** | Replacing older context with a summary (ours) or a provider-native checkpoint, shown as a divider (D33). |
| **Mode** | The permission mode of a session: `default`, `accept-edits`, `plan`, `bypass` (D25). Chat is always `default`. |
| **Rule** | A permission rule like `Bash(git commit:*)` in an `allow`, `ask` or `deny` list (D25). |
| **Managed policy** | Admin-delivered settings and denies above all other layers; tamper-proof (D38). |
| **Workspace trust** | The per-folder decision that lets project config, hooks, MCP servers, skills and agents load (D43). |
| **Definition-hash approval** | Separate approval of a project MCP server, hook or setup command, keyed by a hash of its definition (D43). |
| **Artifact** | A sandboxed document, code, SVG, Mermaid, HTML or React item the model creates with `artifact_*` tools (D28). |
| **Artifact scheme** | The custom URI scheme (`genie-artifact://`) that serves artifact content from an opaque origin (D28). |
| **Connector** | An MCP server, as shown in the UI (D30). **Connector directory**: Genie's signed list of vetted connectors. |
| **Skill** | A Claude-compatible `SKILL.md` folder (D37). **Skills directory**: Genie's signed list of curated skills. |
| **Agent / subagent** | An agent is a Markdown definition (prompt, tools, model). A subagent is an agent run as a background child session through `task` (D46, D47). |
| **Hook** | A user command that runs at an engine event (`tool.before`, …) in Genie's own format; Code/CLI only (D48). |
| **Checkpoint** | A before/after file snapshot of a mutating step, kept in a **shadow git** repo outside the user's repo (D49). |
| **Worktree** | A git worktree in `../<repo>.genie-worktrees/<slug>` on branch `genie/<slug>` for parallel Code sessions (D50). |
| **Screencast** | The live JPEG frame stream from Chrome/Edge via CDP `Page.startScreencast`, shown in the Browser tab (D56). |
| **Take over / Hand back** | The user takes control of the browser (agent paused) and later returns it to the agent (D21, D56). |
| **Taint** | A per-turn flag set when untrusted web content was read; it downgrades some auto-allows to asks (research/browser.md §4.4; SA-29). |
| **Signed feed** | A static, Ed25519-signed JSON document on Genie's CDN: updates, catalog, connector and skills directories (D40). |
| **Platform adapter** | The one TypeScript interface through which the React app reaches the shell (Tauri), so the shell can be swapped (D8). |
| **ew** | Engineer-week: one engineer's focused week of work. |
