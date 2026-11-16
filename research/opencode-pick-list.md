# OpenCode port pick-list

> Ticket [36](../tickets/36-feature-inventory.md) · Written 2026-11-16 · The *architecture* side. The behaviour side is [claude-subsystems-inventory.md](claude-subsystems-inventory.md).
> Source of truth for each verdict: the chapter's **Genie takeaways** in [knowledge/opencode](../knowledge/opencode/README.md).

**Verdicts**
- **port:** take the design (and, where marked *code*, translate the code) with little change. C3 applies: adapted code keeps OpenCode's MIT notice and carries no OpenCode branding.
- **adapt:** take the idea, but change its semantics or shape to fit Genie's rules (safe defaults, keychain, resumable protocol).
- **skip:** don't take it (a "should not copy" item, out of scope, or superseded). *later* means skip for now and revisit.

**Numbering:** the numbers the KB already cites keep their meaning: #1–#20, #22, #24–#28, #32, #33, #35–#41, #43, #45. #26 is still the in-process plugin ABI (= SKIP). The gaps (#21, #23, #29–#31, #34, #42, #44) are filled here from KB takeaways that had no number.
**Milestone** column: C = needed for desktop Chat v1 (D6), A = agent/Code/CLI milestone, L = later.

| # | Component | Verdict | Reason (one line) | KB | Ticket | M |
|---|---|---|---|---|---|---|
| 1 | AI protocol adapters (Messages, Responses, Chat, Gemini, Bedrock) + request normalisation + error classification and retry | **port** (*code*) | The hardest-won part of OpenCode: stream state machines, history repair, tool-id rewrite, overflow patterns, `model-retry` schedules | [07](../knowledge/opencode/07-ai-package-and-protocols.md) | 03, 34 | C |
| 2 | Provider modules + registry + `Compatibility` quirk flags | **port** | A clean split between adapter and provider; quirk flags per *model*, so Ollama and vLLM need no forks | [08](../knowledge/opencode/08-providers-models-and-credentials.md) | 03 | C |
| 3 | Model catalog (bundled snapshot + cached fetch + ETag refresh) and variants | **adapt** | Good strategy, but normalise into Genie's own schema; effort is per-model overlays, not an enum; check the models.dev data licence | [08](../knowledge/opencode/08-providers-models-and-credentials.md) | 04 | C |
| 4 | Edit tool fuzzy matcher (3-tier, line endings, BOM, closest-lines hint) | **port** (*code*) | Proven matcher; **add** read-before-edit and a staleness check, which OpenCode lacks | [09](../knowledge/opencode/09-tools.md) | 06 | A |
| 5 | `apply_patch` grammar + per-model swap | **port** | Some models are trained for patches; make the swap a catalog capability and add whole-patch rollback | [09](../knowledge/opencode/09-tools.md) | 06 | A |
| 6 | Shell tool design (2-min foreground → background, notify, scanner → per-command resources) | **port** | Matches Genie's Bash plan; keep the Windows shell order pwsh → powershell → Git Bash → cmd | [09](../knowledge/opencode/09-tools.md) | 07 | A |
| 7 | `opencode-pty` process daemon (Rust crate) | **port** (*vendor*) | MIT Rust, pipe mode + Windows Job Object; needs constant-time token compare, PTY tree-kill and sandbox spawn hooks | [16](../knowledge/opencode/16-pty-process-daemon.md) | 07, 09 | A |
| 8 | Web search / webfetch | **adapt** | Genie prefers provider-native search; borrow webfetch's per-redirect permission check and userinfo rejection | [09](../knowledge/opencode/09-tools.md) | 15 | C (native) / A |
| 9 | Permission engine (`{action, resource}`, pending map, once/always/reject + feedback) | **adapt** | Port the design, not the semantics: safe defaults, deny-wins instead of last-match-wins, engine-enforced modes, plan mode can't run the shell | [06](../knowledge/opencode/06-config-permissions-and-policy.md) | 13 | A |
| 10 | Managed policy domains (`provider.use`, `mcp.use`, …; last-good remote copy) | **port** | Maps directly onto Genie's file + MDM policy; make the whole managed layer tamper-proof, not only `policies` | [06](../knowledge/opencode/06-config-permissions-and-policy.md) | 28 | A |
| 11 | Compaction mechanics (window − max(15%, 16k), tail ~15k, 85→70→50→35% retry) | **port** | Sound and cache-friendly; write our own summary template text (see #28) | [04](../knowledge/opencode/04-context-and-compaction.md) | 16 | C (long chats) |
| 12 | Snapshots in a shadow git repo, per step | **port** | Captures Bash side effects that Claude's checkpoints miss; **add** retention/gc and a non-git fallback | [12](../knowledge/opencode/12-storage-snapshots-and-worktrees.md) | 23 | A |
| 13 | Worktrees (table, slug naming, remove-only-ours, porcelain discovery) | **adapt** | Use `.genie/worktrees` + `info/exclude` and named branches instead of detached; no pluggable strategies | [12](../knowledge/opencode/12-storage-snapshots-and-worktrees.md) | 24 | A |
| 14 | SQLite storage (WAL, migration journal, fresh-DB bootstrap) + durable/ephemeral events | **adapt** | Keep the schema shape and per-session `seq`; leave out the full projector machinery and cross-process claiming at first | [03](../knowledge/opencode/03-session-and-agent-loop.md), [12](../knowledge/opencode/12-storage-snapshots-and-worktrees.md) | 12 | C |
| 15 | Subagents as child sessions (Markdown agents, background, follow-up by id, depth limit) | **port** | Claude-compatible file format; the report-on-idle model is sound | [03](../knowledge/opencode/03-session-and-agent-loop.md), [10](../knowledge/opencode/10-agents-skills-and-commands.md) | 19, 18 | A |
| 16 | Skills scanner + `<available_skills>` index with diffs | **port** | Cache-friendly and Claude-compatible; user-invoked skills must still pass policy | [10](../knowledge/opencode/10-agents-skills-and-commands.md) | 21 | A |
| 17 | Instruction sources (keyed, hashed baseline + `<system-update>` deltas; nested on read) | **port** | Keeps the prompt cache stable; **add** `@imports` and size caps; stop the walk at the project root | [04](../knowledge/opencode/04-context-and-compaction.md), [05](../knowledge/opencode/05-memory-and-instructions.md) | 10, 11 | A (C for project instructions) |
| 18 | MCP client (5-state status, split timeouts, list_changed reload, result shaping, OAuth) | **port** | Good design; change the 12 h timeout, use `mcp__server__tool` names, host our own client metadata | [11](../knowledge/opencode/11-mcp-and-plugins.md) | 17, 14 | A |
| 19 | Session inbox + coordinator "doorbell" + jobs (block/background/cancel) | **port** | Gives steer/queue, interrupt and crash resume; one coordinator task per session | [01](../knowledge/opencode/01-architecture.md), [03](../knowledge/opencode/03-session-and-agent-loop.md) | 01, 12, 19 | C |
| 20 | Scheduler (lease-in-kv election, doorbell, `retry_at`) | **port** *later* | Small, self-contained SQLite design, for when scheduled tasks arrive | [12](../knowledge/opencode/12-storage-snapshots-and-worktrees.md) | 19 | L |
| 21 | `todowrite` tool | **adapt** | Keep the whole-list replace shape; ship it on by default (OpenCode keeps it behind an experimental flag) | [09](../knowledge/opencode/09-tools.md) | 06 | A |
| 22 | Formatter after edit (opt-in, first success wins) | **adapt** | Simple; return the post-format diff to the model so the next edit still matches | [09](../knowledge/opencode/09-tools.md) | 06 | A |
| 23 | `question` tool (multi-question form, free text) | **port** | Same UX as AskUserQuestion; make "dismissed" a control-flow result, not a defect | [09](../knowledge/opencode/09-tools.md) | 06, 02 | A |
| 24 | ACP server adapter | **skip** *later* | Port the method set only after protocol v1 is stable; IDEs are out of scope for now | [14](../knowledge/opencode/14-cli-headless-and-acp.md) | 25 | L |
| 25 | Headless `run` NDJSON stream (code) | **skip** | One-way and infers completion from EOF; take the ideas (retry event, per-step cost) and add `system/init` + `result` lines | [14](../knowledge/opencode/14-cli-headless-and-acp.md) | 25 | A |
| 26 | **In-process plugin ABI** (JS hooks over registries) | **skip** | Already decided: extensions only via MCP + hooks + skills (+ bundles); hot hooks stay internal Rust traits | [11](../knowledge/opencode/11-mcp-and-plugins.md) | 22 | — |
| 27 | Code Mode (MCP tools reachable only through model-written JS `execute`) | **skip** | Bypasses permission checks for several tools; plain MCP tools + tool search are simpler | [09](../knowledge/opencode/09-tools.md), [11](../knowledge/opencode/11-mcp-and-plugins.md) | 06, 17 | — |
| 28 | Prompt texts (system, compaction template, MAX_STEPS) | **skip** | Write Genie's own text; keep only the mechanics (tools-disabled final step with `tool_choice:none`) | [03](../knowledge/opencode/03-session-and-agent-loop.md), [04](../knowledge/opencode/04-context-and-compaction.md) | 11, 16 | — |
| 29 | Central tool-output truncation + spill file (2,000 lines / 50 KiB, 7-day sweep) | **port** | Simple and predictable; the marker names the path so Read can page through it | [04](../knowledge/opencode/04-context-and-compaction.md), [09](../knowledge/opencode/09-tools.md) | 11, 06 | A |
| 30 | Session export/import (`{info, messages}`, `--sanitize`) | **port** | Cheap; useful for bug reports and chat export | [12](../knowledge/opencode/12-storage-snapshots-and-worktrees.md) | 12 | C |
| 31 | Small-model policy for titles and summaries | **adapt** | Needed for chat titles in v1; use a catalog flag instead of a hard-coded family list | [08](../knowledge/opencode/08-providers-models-and-credentials.md) | 04 | C |
| 32 | patchmark streaming Markdown (settled vs streaming blocks, heal step) | **port** (*code*) | ~4K lines, no dependencies, has a spec and a fuzz harness; the CLI needs it; the desktop may reuse the split idea | [15](../knowledge/opencode/15-tui-and-rendering.md) | 05, 37 | A |
| 33 | Mermaid/LaTeX terminal renderer (merman) | **skip** *later* | Not portable as is; the desktop uses JS libraries; keep the "last good render while streaming" lesson | [15](../knowledge/opencode/15-tui-and-rendering.md) | 05 | L |
| 34 | Config as ordered layers + generated JSON Schema + per-key diagnostics | **adapt** | Use Claude's scope ladder and strict JSON; drop JSONC and `{file:}` substitution; schema drives the Settings UI | [06](../knowledge/opencode/06-config-permissions-and-policy.md) | 08, 26, 33 | C |
| 35 | Inline terminal UI (`opencode mini`: scrollback + repaintable footer) | **adapt** | Copy the design onto a ratatui inline viewport; the code is TypeScript on OpenTUI | [15](../knowledge/opencode/15-tui-and-rendering.md) | 05 | A |
| 36 | Theme tokens (seed colours → OKLCH scales → semantic variables) | **port** (*design*) | A good model; ship Genie's own brand theme, none of the 36 OpenCode themes | [18](../knowledge/opencode/18-app-ui-and-gui-plugins.md), [15](../knowledge/opencode/15-tui-and-rendering.md) | 29 | C |
| 37 | i18n discipline (keys for every string, `plural()`, English fallback, lazy locales) | **port** | Costs little on day one and a lot to retrofit, even if Genie ships English only | [18](../knowledge/opencode/18-app-ui-and-gui-plugins.md) | 29, 31 | C |
| 38 | Desktop app renderer (`packages/app`, ~347K lines of Solid) | **adapt** | Use it only as a feature and UX reference (timeline projection, docks, context tab); Genie is React | [18](../knowledge/opencode/18-app-ui-and-gui-plugins.md) | 31, 29 | C (reference) |
| 39 | GUI plugin architecture (plugin-per-feature registries) | **adapt** | Keep it as *internal* modularity; never run third-party JS with host privileges | [18](../knowledge/opencode/18-app-ui-and-gui-plugins.md) | 22 | A |
| 40 | Agent-driven browser plugin (CDP) | **skip** *later* | Needs a bundled Chromium on macOS (WKWebView has no CDP); the tool/permission split is still worth reusing | [18](../knowledge/opencode/18-app-ui-and-gui-plugins.md) | 30 | L |
| 41 | Electron desktop shell | **skip** | D1 is Tauri; take the lifecycle state machines, "credential never reaches the webview" and the release flow | [17](../knowledge/opencode/17-desktop-shell.md) | 30, 27 | — |
| 42 | Project identity (remote hash → cached marker → root commit; worktree → main checkout) | **port** | A stable key for per-project memory and settings; write the marker under `.genie/` | [01](../knowledge/opencode/01-architecture.md) | 12, 10 | C |
| 43 | Shared background service daemon (fixed port, plaintext password file, contender handoff) | **skip** | A KB "should not copy" item; borrow the stdio child + stdin-EOF lease and the version handshake. **Note:** D4 says "a core daemon serves both clients", so ticket 01 must choose its shape | [01](../knowledge/opencode/01-architecture.md), [13](../knowledge/opencode/13-server-protocol-and-sdk.md) | 01, 02 | C |
| 44 | Credential refresh (single flight, cross-process lock, re-read under lock) + pasted-redirect OAuth fallback | **port** | Needed for Entra and MCP OAuth; store secrets in the keychain, never plaintext SQLite | [08](../knowledge/opencode/08-providers-models-and-credentials.md) | 14 | C |
| 45 | Consumer-subscription providers (ChatGPT token sharing, Copilot device login, `claude-agent`) | **skip** | C2 and terms-of-service risk; copy no client ids | [08](../knowledge/opencode/08-providers-models-and-credentials.md) | 14 | — |

## Totals

**port 23 · adapt 12 · skip 10** (45 items). Of these, #20 is port-*later* and #24, #33, #40 are skip-*later*.

Needed for Chat v1 (M = C): #1, 2, 3, 8 (native), 11, 14, 17 (project instructions), 19, 30, 31, 34, 36, 37, 38, 42, 43 (as a decision), 44.

## Also considered (not numbered)

- **Generated TS/Rust clients with a CI drift check, and an in-memory transport for tests** ([13](../knowledge/opencode/13-server-protocol-and-sdk.md)): adapt → 02.
- **Durable/ephemeral event taxonomy with `seq` and reconnect replay** ([13](../knowledge/opencode/13-server-protocol-and-sdk.md)): adapt; OpenCode's SSE is not resumable (C4) → 02.
- **Desktop lifecycle as pure state machines; channel-scoped updater; per-channel DB names** ([17](../knowledge/opencode/17-desktop-shell.md), [14](../knowledge/opencode/14-cli-headless-and-acp.md)): port → 27, 30.
- **Per-feature GUI surfaces** (review diffs, terminal pane, context tab, `/btw`) ([18](../knowledge/opencode/18-app-ui-and-gui-plugins.md)): a reference for Code mode → 31 (v2).
- **LSP diagnostics:** the KB doesn't cover this; Claude Code ships an `LSP` tool. Needs research before ticket 06.
