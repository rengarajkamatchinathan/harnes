# 10 — Plan and estimates

Implements the D29 scope warning ("the spec phase must estimate effort and confirm v1 = M1–M4") and ticket 31's suggested milestones, with later amendments: D32 (RAG in v1), D44 (no chat sandbox in v1), D26 (4 adapters), D55 (terminal with the Code tab), D56 (in-window browser), D57 (renderer).

## 1. How to read the numbers

- **Unit:** focused **engineer-weeks (ew)**, as a **low–high range**. They include design-in-code, unit/integration tests, code review and docs for engineers. They **exclude** product management, visual/brand design, legal work (ticket 35 items), dedicated QA staff and the business-model work in ticket 41 (**SA-37**).
- **Calendar conversion:** calendar weeks = ew ÷ (engineers × **0.85**), allowing for meetings, support, holidays and review load (**SA-35**). Work is assumed to start **2027-01-04** for the dates shown.
- **Risk** per item: **L** (known pattern, ported design), **M** (some unknowns or cross-OS work), **H** (new ground, external dependency, or security-critical).
- **Where the estimates come from:**
  - **Research reports where they exist.** Browser option B: **14–18 ew** (research/browser.md §6), plus a D56 delta for the in-window screencast. Sandbox: **18–24 ew**, **v2** (D44). CEF Genie Browser: 26–33 ew, **v2**. Provider adapters: 8–12k lines of Rust for five protocols (research/provider-layer.md).
  - **Prototype 37** de-risked the daemon → Tauri → React path and the renderer on Windows (D57).
  - **OpenCode ports** (pick-list "port" items: inbox/doorbell, retry, compaction mechanics, edit matcher, shell design, `opencode-pty`, patchmark, shadow git) are estimated lower than greenfield work, but every port still needs Rust translation and Genie's safety changes.
  - Everything else is engineering judgement from the size of the decided design. **Multi-OS testing, signing and security hardening are budgeted explicitly**, not hidden in feature items.

## 2. Summary

| Milestone | Scope | ew (low–high) |
|---|---|---|
| **M1 core chat** | daemon, protocol, store, secrets, config, 4 adapters, auth, catalog, conformance, desktop shell, renderer, chat UI, projects, settings basics | **74–102.5** |
| **M2 tools** | tool loop, permissions + asks, web search/fetch, code execution, MCP (full v1 set), artifacts (non-React), compaction | **36.5–51.5** |
| **M3 intelligence** | memory, chat search, incognito, instruction framework, chat skills, slash command registry + `/` menu, signed directories, React artifacts, project RAG, usage & spend | **27–37** |
| **M4 browser + hardening** | browser (B + screencast), policy, audit, telemetry, recovery, packaging/signing, updater/feeds, security, a11y, D8 validation, release QA | **44–63** |
| **Chat v1 (M1–M4)** | | **≈182.5–255** |
| **M5 Code engine + CLI** | code tools, `genie-process`, shell, modes, instructions, trust, checkpoints, worktrees, agents, subagents, hooks, commands, TUI, `genie chat`, headless, importer | **48.5–64.5** |
| **M6 Desktop Code tab** | repo-grouped sessions, tool rows, Changes/Files/Terminal panels, PTY terminal, Code `/` commands | **18.5–26** |
| **Code/CLI track (M5–M6)** | | **≈67–90.5** |
| **All of v1 (Chat + Code/CLI)** | | **≈249.5–345.5** |
| Ongoing after GA | provider upkeep 0.25–0.5 FTE; catalog/directory curation 0.1–0.2 FTE | — |

## 3. Work breakdown

### 3.1 M1 — core chat (74–102.5 ew)

| ID | Work item | ew | Depends on | Risk |
|---|---|---|---|---|
| 1.01 | Monorepo, workspace lints, `cargo deny`, `xtask`, schema/TS codegen + drift checks (t38) | 1.5–2.5 | t41 (CI) | L |
| 1.02 | `genie-protocol` v1 types: handshake, sessions, events, asks, errors (02) | 1.5–2 | — | L |
| 1.03 | `genie-daemon`: UDS/named pipe, token, peer checks, ensure + lock, version skew, idle exit, on 3 OSes (01 §4) | 3–4 | 1.02 | M |
| 1.04 | `genie-client` + in-memory transport + reconnect/resume | 1.5–2 | 1.02 | L |
| 1.05 | `genie-core`: EngineBuilder, DirContext, session actor, inbox steer/queue, bus + seq, subscribe/resume/resync, driver lease, interrupt/Resume, cancellation tree, single-step turn loop | 5–7 | 1.03, 1.06 | H |
| 1.06 | `genie-store`: SQLCipher, migrations + backups, schema v1, AEAD blobs, FTS5, cascade delete, single-chat export (04) | 4–5 | — | M |
| 1.07 | `genie-secrets`: keychain on 3 OSes, Argon2id file, unlock flow, DB key (D26) | 2–3 | — | M |
| 1.08 | `genie-config`: layers, schemars schema + metadata, watcher, per-key diagnostics, `state.json`, policy file read (basic) (D23) | 3–4 | 1.01 | M |
| 1.09 | `genie-providers` core: canonical model, Executor, SSE parser, append-only compile, replay filter, history repair, id rewrite, Compat/ModelPatch, error classification + overflow corpus, retry policy (03 §3–7) | 4–5 | — | H |
| 1.10 | Anthropic Messages adapter + routes (API, Foundry, Vertex) | 2.5–3.5 | 1.09 | M |
| 1.11 | OpenAI Responses adapter (OpenAI, Azure v1) | 2–3 | 1.09 | M |
| 1.12 | OpenAI Chat adapter + dialects (Foundry, Ollama, vLLM, OpenRouter) | 2–3 | 1.09 | M |
| 1.13 | Gemini adapter (API, Vertex) incl. thought signatures | 2–3 | 1.09 | M |
| 1.14 | Auth: API keys, **Entra PKCE + device code** (own OAuth client), Google ADC, refresh single-flight + cross-process lock | 3–4 | 1.07; Entra app (t35-2) | H |
| 1.15 | `genie-catalog`: schema, signature check, bundled snapshot, ETag refresh, live discovery (Foundry deployments, /models, Ollama), overrides, effort map, small-model rule, pricing | 3–4 | 1.09 | M |
| 1.16 | Conformance suite: cassette recorder/replayer, golden wire tests, catalog patch tests, scrubber, live-smoke harness; initial cassettes for 4 adapters (D41) | 3–4 | 1.09 | M |
| 1.17 | Desktop shell: Tauri app, Platform adapter, `genie-client` relay over IPC Channels (8 ms batching), "Starting engine…", deep links, notifications, window state | 3–4 | 1.04 | M |
| 1.18 | Design system: tokens, light/dark/system, shadcn/Radix components, i18n scaffolding, app frame with right-panel shell (D19) | 3–4 | — | L |
| 1.19 | Renderer: settled/tail split, virtualised transcript, highlight.js, KaTeX/Mermaid pre-warm off path, thinking sections; perf harness from prototype 37 in CI (D57) | 4–5 | 1.17 | H |
| 1.20 | Chat UI: sidebar (new, recents, rename, delete, pin, search), composer (attachments, model + effort pickers, stop, steer/queue), branches `‹2/3›`, edit/regenerate, Resume and retry banners, cost per message, context ring, export | 5–7 | 1.05, 1.18, 1.19 | M |
| 1.21 | Projects: CRUD, instructions, knowledge files (full mode), starred | 1.5–2.5 | 1.20 | L |
| 1.22 | Settings framework (full page, nav, search, deep links, schema-generated controls, locked values) + General, Appearance, Providers & Models, Privacy (basic), Developer (basic) | 4–5 | 1.08 | M |
| 1.23 | Context assembly v1: baseline message, custom + project instructions, breakpoints, token accounting, attachment conversion | 2–3 | 1.05, 1.09 | M |
| 1.23b | Prompt content v0 (06 §2.6): base-prompt fragments, chat guidance, tag formats, system-block placement + `Compat` fallback, `<system-update>` lowering per adapter, hidden prompts, golden snapshot tests; prompt eval set (first ~40 cases) in the nightly run | 1.5–2.5 | 1.23, 1.16 | M |
| 1.24 | Titles via small model; usage/cost recording + `usage_daily` | 1–1.5 | 1.15 | L |
| 1.25 | First run: consent, provider setup, passphrase/recovery prompt | 1–1.5 | 1.22 | L |
| 1.26 | Logging (`tracing`, rotation) + crash-report client | 1–1.5 | — | L |
| 1.27 | CI build matrix on 3 OSes, dev-channel builds, perf-gate runners | 2–3 | t41 | M |
| 1.28 | Stabilisation, cross-OS fixes, **internal alpha A1** | 2–3 | all | M |
| | **M1 total** (incl. 1.23b prompt content, added after the prompt review) | **74–102.5** | | |

### 3.2 M2 — tools (36.5–51.5 ew)

| ID | Work item | ew | Depends on | Risk |
|---|---|---|---|---|
| 2.01 | Tool framework: trait, registry, availability, input repair, spill, dispatch-while-streaming + join, `Declined` (06 §4) | 2–3 | 1.05 | M |
| 2.02 | `genie-permissions`: rule grammar, class evaluation, chat mode, durable asks, save "always", re-evaluation; ask cards; no-client pause + OS notification | 3–4 | 2.01 | M |
| 2.03 | Native web search for 3 providers + citation mapping + citation UI + favicon proxy + domain policy (D36) | 2.5–3.5 | 2.01 | M |
| 2.04 | `web_fetch`: SSRF guard, per-redirect permission, HTML→Markdown (D35) | 1.5–2 | 2.02 | M |
| 2.05 | Native code execution for 3 providers, file upload on use, outputs as attachments (D35) | 2–3 | 2.01 | M |
| 2.06 | `genie-mcp` core: stdio + Streamable HTTP, status, timeouts, list_changed, result shaping, naming, process groups | 3–4 | 2.01 | M |
| 2.07 | MCP OAuth: CIMD, DCR, loopback, refresh, keychain refs | 2–3 | 2.06, 1.14 | H |
| 2.08 | MCP resources, prompts as slash commands, elicitation forms, sampling approvals + budgets | 3–4 | 2.06, 2.02 | M |
| 2.09 | MCP tool search (deferred loading) | 1–1.5 | 2.06 | L |
| 2.10 | Connectors UI: Settings → Connectors, per-chat "+" toggles, logs, add custom | 2.5–3.5 | 2.06 | L |
| 2.11 | Artifacts: tools, versions per message/branch, panel, document/code/SVG/Mermaid/HTML, custom scheme + iframe sandbox + CSP + postMessage, Fix with Genie (D28) | 5–7 | 2.01, 1.19 | H |
| 2.12 | Hostile-artifact CI test, first version, 3 engines | 1–1.5 | 2.11 | M |
| 2.13 | Compaction: triggers, our summary, native where supported, divider, re-injection, overflow steps (D33) | 3–4 | 1.23 | M |
| 2.14 | Capabilities + Permissions settings sections | 1.5–2.5 | 2.02 | L |
| 2.15 | Turn loop: multi-step, max steps, length continuation, retry events and UI | 1.5–2 | 2.01 | L |
| 2.16 | Stabilisation, **internal alpha A2** | 2–3 | all | M |
| | **M2 total** | **36.5–51.5** | | |

### 3.3 M3 — intelligence (27–37 ew)

| ID | Work item | ew | Depends on | Risk |
|---|---|---|---|---|
| 3.01 | Memory: tools, table, chip, Settings → Your Genie, injection cap, notice, policy off, incognito exclusion (D31) | 2.5–3.5 | 2.01 | L |
| 3.02 | `chat_search` over FTS with scopes and chat citations | 1–1.5 | 1.06 | L |
| 3.03 | Incognito: in-memory store behind the `Store` trait, banner, exclusions | 1.5–2.5 | 1.06 | M |
| 3.04 | Instruction-source framework: keyed/hashed sources, frozen baseline, `<system-update>` deltas | 1.5–2 | 1.23 | M |
| 3.05 | Chat skills: scanner, index, `skill`/`skill_read`, `/skill`, enable/disable, zip upload validation, 4 built-in skills | 3–4 | 3.04 | M |
| 3.06 | Signed directories (connectors + skills): feed format, signing tools, browse/install UI, "what will run" dialog, checksum verification | 3–4 | 2.10, 3.05, 4.13 (keys) | M |
| 3.07 | React artifacts: esbuild-wasm in sandbox, bundled library set, import errors, CSP tuning | 3–4 | 2.11 | H |
| 3.08 | Project RAG: chunking, FTS/BM25, embeddings + cache, hybrid ranking, mode switch at 50%, `project_search`, pre-injection, capacity bar (D32) | 4–5 | 1.21, 1.06 | M |
| 3.09 | Caching refinements: 1 h TTL for knowledge, cache-key sharing | 0.5–1 | 1.23 | L |
| 3.10 | Usage & spend section, monthly budget warnings | 1.5–2 | 1.24 | L |
| 3.11 | Claude Desktop MCP import + first-run offer (subset of D53) | 1–1.5 | 2.06 | L |
| 3.13 | **Slash commands, shared:** engine command registry (built-ins, Markdown, skills, MCP prompts), routes, `command.list`, `commands.changed`, `command` input part with stored expansion, `command_not_prompt` guard; desktop `/` menu (filter, groups, tooltips, token + argument hint) and the Chat built-ins' panels (06 §9.1, 05 §2.3) | 2.5–3 | 3.05, 2.08 | L |
| 3.12 | Stabilisation, **external beta readiness** | 2–3 | all | M |
| | **M3 total** | **27–37** | | |

### 3.4 M4 — browser + hardening (44–63 ew)

| ID | Work item | ew | Depends on | Risk |
|---|---|---|---|---|
| 4.01 | Browser backend: CDP client over `--remote-debugging-pipe`, Chrome/Edge/Chromium detection on 3 OSes, Genie profile, `BrowserBackend` | 3–4 | 2.01 | M |
| 4.02 | Snapshot serializer: AX tree + DOMSnapshot, refs, hidden-text stripping, diffs, `browser_read`, screenshots | 3–4 | 4.01 | M |
| 4.03 | 19 `browser_*` tools, site permissions, sensitive-action gates, URL exfiltration guard, **taint**, step caps, audit log | 4–5 | 4.02, 2.02 | H |
| 4.04 | **Browser tab (D56):** screencast canvas, take-over input forwarding, IME/clipboard/file chooser, tab strip, banner, confirmation cards, pop-out, Settings → browser | 4–6 | 4.01 | H |
| 4.05 | Injection red-team suite + cross-OS browser QA | 2–3 | 4.03 | M |
| | *Browser subtotal: 16–22 ew = research B-only 14–18 + ~2–4 for D56 in-window screencast* | | | |
| 4.06 | Hostile-artifact test complete + artifact red team (React path) | 1–1.5 | 3.07 | M |
| 4.07 | Managed policy: file + macOS profile + Windows registry/ADMX + signed remote URL, merge, enforcement points, effective-policy view (D38) | 3–4 | 1.08 | M |
| 4.08 | Audit: OTLP exporter + JSONL rotation + event wiring | 1.5–2.5 | 4.07 | L |
| 4.09 | Telemetry: consent wiring, crash on / usage opt-in, scrubbing, admin force-off, backend setup | 1.5–2.5 | 1.26 | M |
| 4.10 | Recovery key export/restore, key-loss flow | 1–1.5 | 1.07 | M |
| 4.11 | Retention job, delete-all, export-all | 1–1.5 | 1.06 | L |
| 4.12 | **Packaging & signing:** MSI + ADMX, MSIX/per-user, winget, Azure Trusted Signing; universal DMG, notarisation, brew cask; deb/rpm + signed repos, AppImage; CLI install scripts | 6–8 | 1.27 | H |
| 4.13 | Updater + signed-feed infra (Azure Storage + CDN), key hierarchy, channels with separate data dirs, daemon drain on update, staged rollout gates | 3–4 | 4.12 | H |
| 4.14 | Security hardening: fuzzing, threat-model review, external pen-test support and fixes | 3–4 | most | H |
| 4.15 | Accessibility pass (WCAG 2.2 AA, 3 screen readers) | 1.5–2.5 | UI | M |
| 4.16 | **D8 validation:** macOS/Linux perf runs in CI, WebKit fixes, NVIDIA/Wayland workarounds | 2–4 | 1.19, bench runs | H |
| 4.17 | Account & licence section, generated third-party notices | 0.5–1 | 4.12 | L |
| 4.18 | Release QA, beta programme, bug-fix buffer, **GA** | 3–4 | all | M |
| | **M4 total** | **44–63** | | |

### 3.5 M5 — Code engine + CLI (48.5–64.5 ew)

| ID | Work item | ew | Depends on | Risk |
|---|---|---|---|---|
| 5.01 | Code tools: read/write/edit (matcher port)/multi_edit/patch (atomic)/glob/grep (bundled rg)/list/todo_write/ask_user; per-path locks; formatter | 4–5 | 2.01 | M |
| 5.02 | `genie-process` pipe mode: migrate `opencode-pty`, KB fixes, Job Object/pgroup, spawn hook, MIT attribution | 2–3 | — | M |
| 5.03 | Shell tool: hidden shell, cd persistence, PowerShell/Git Bash/zsh, 2 min → background, notifications, spill; **command scanner for bash and PowerShell** + arity table + read-only list | 4–5 | 5.02, 2.02 | H |
| 5.04 | Modes accept-edits/plan/bypass in the engine + plan approval flow | 1.5–2 | 2.02 | L |
| 5.05 | Instruction files: discovery, nested on demand, rules with `paths`, `@imports`, caps | 2–2.5 | 3.04 | L |
| 5.06 | Workspace trust + definition-hash approvals with diffs, `--trust` | 2–2.5 | 1.08 | M |
| 5.07 | Checkpoints: shadow git with alternates, per-step snapshots, non-git fallback, rewind files/conversation/both, stage/commit, gc, disk usage | 4–5 | 5.02, 1.06 | H |
| 5.08 | Worktrees: auto/opt-in, sibling folder, branch, setup command, finish actions, cleanup, reconciliation | 2.5–3.5 | 5.06 | M |
| 5.09 | Agents: definitions, built-ins, `--agent`, narrowing | 1.5–2 | 2.01 | L |
| 5.10 | Subagents: background child sessions, outbox report, `task_wait`, depth/concurrency, cost roll-up, cancellation cascade | 3–4 | 5.09 | M |
| 5.11 | Hooks: events, matcher, argv runner, envelope, failClosed, merge, re-check, hash approval, `hook-adapter claude` | 3–4 | 5.02, 5.06 | M |
| 5.12 | Slash commands for Code on the 3.13 registry: Markdown (`$ARGUMENTS`, `$1..$N`, `@file`, `!cmd` via shell permission), Code built-ins, CLI footer runners for `client`-route built-ins | 1.5–2 | 3.13 | L |
| 5.13 | **CLI inline TUI:** ratatui inline viewport, `genie-markdown` (patchmark port), syntect, themes, footer (prompt editor, slash menu, asks, forms, status line, mode cycling, steer/queue) | 7–9 | 1.04 | H |
| 5.14 | `genie chat` (list/open/continue, open-in-desktop links) | 1–1.5 | 5.13 | L |
| 5.15 | Headless `-p`: output formats, flags, exit codes, `--standalone`, ask pausing, `genie approvals`, `--ask-timeout`, `--on-ask` | 2–3 | 5.13 | M |
| 5.16 | Importer: Claude Code, Claude Desktop, OpenCode; preview diagnostics; secrets → keychain; idempotency; report; CLAUDE.md link | 3–4 | 5.06, 5.11 | M |
| 5.17 | CLI-only distribution polish + `genie update` | 1–1.5 | 4.13 | L |
| 5.18 | Code compaction: `/compact`, read-state re-injection | 0.5–1 | 2.13 | L |
| 5.19 | Cross-OS QA for the Code engine and CLI (shells, paths, terminals) | 3–4 | all | M |
| | **M5 total** | **48.5–64.5** | | |

### 3.6 M6 — Desktop Code tab (18.5–26 ew)

| ID | Work item | ew | Depends on | Risk |
|---|---|---|---|---|
| 6.01 | Code mode frame: repo-grouped sessions, branch badge, status dot, new session (folder, agent, worktree), mode selector, scope picker | 2.5–3.5 | 5.x | L |
| 6.02 | Compact expandable tool rows (diffs, shell output, todos, subagent cards, plan approval) | 3–4 | 6.01 | M |
| 6.03 | Changes panel: branch, Commit split button, uncommitted diffs, checkpoints + rewind dialog | 3–4 | 5.07 | M |
| 6.04 | Files panel: tree with M/A/D markers, preview | 1.5–2.5 | 6.01 | L |
| 6.05 | **Terminal tab:** `genie-process` PTY mode (portable-pty + libghostty-vt; Zig in CI), xterm.js, multiple terminals, reattach with checkpoint + offset (D55) | 4–5 | 5.02 | H |
| 6.06 | Browser tab reuse in Code, worktree finish UI, trust and hash-approval dialogs | 1.5–2.5 | 4.04, 5.06, 5.08 | L |
| 6.08 | Code commands in the desktop `/` menu: `/mode`, `/plan`, `/compact`, `/rewind`, `/worktree`, `/agents`, `/init` (05 §3.2) | 1–1.5 | 3.13, 5.12, 6.01 | L |
| 6.07 | Code beta stabilisation | 2–3 | all | M |
| | **M6 total** | **18.5–26** | | |

### 3.7 v2 (not in v1 totals; rough)

| Item | ew | Basis |
|---|---|---|
| Bedrock adapters (Mantle on Anthropic Messages + ConverseStream), SigV4, AWS credentials | 4–6 | D26, research |
| Skill scripts in chat + `genie-sandbox` (3 OS backends, egress proxy, runtime pipeline) | 18–24 | research/sandbox.md, D44 |
| CEF "Genie Browser" helper (option A) on the same backend trait | ~14–18 on top of v1 B work (of research's 26–33) | research/browser.md |
| Own web search backend (BYO Brave/Tavily/Exa) | 2–3 | D11 |
| Plugin bundles + packaged MCP extensions (Claude plugin format) | 5–8 | D52 |
| ACP server (`genie acp`) | 2–3 | D51 |
| Public protocol + SDK (docs, stability, TS/Python SDK) | 4–6 | D22 |
| Code-mode settings section, keyboard shortcuts | 1–2 | t33 |
| Global Artifacts tab | 1 | t31 |
| Licence enforcement / business model | TBD | t41 |

## 4. Timeline for 3–5 engineers

### 4.1 Capacity view

Calendar weeks = ew ÷ (N × 0.85).

| Scope | 3 engineers | 4 engineers | 5 engineers |
|---|---|---|---|
| M1 (74–102.5) | 28–39 wk | 21–29 wk | 17–24 wk |
| M2 (36.5–51.5) | 14–20 wk | 11–15 wk | 9–12 wk |
| M3 (27–37) | 11–15 wk | 8–11 wk | 6–9 wk |
| M4 (44–63) | 17–25 wk | 13–19 wk | 10–15 wk |
| **Chat v1 (182.5–255)** | **72–100 wk (17–23 months)** | **54–75 wk (12–17 months)** | **43–60 wk (10–14 months)** |
| Code/CLI track (67–90.5) | 26–35 wk | 20–27 wk | 16–21 wk |
| **All of v1 (249.5–345.5)** | **98–135 wk (23–31 months)** | **73–102 wk (17–24 months)** | **59–81 wk (14–19 months)** |

The 5-engineer column assumes the work splits cleanly. In M1 it doesn't fully: the engine core (1.05) and the renderer (1.19) are each one person's critical path. Expect the 5-engineer M1 to land near the top of its range.

**Indicative dates, 4 engineers, start 2027-01-04:** A1 (end M1) June–July 2027 · A2 (end M2) August–November 2027 · Beta (end M3) October 2027–January 2028 · **Chat v1 GA (end M4) January–June 2028** · Code/CLI GA (sequential) May–November 2028, or earlier with the overlap in §4.3.

### 4.2 Parallel tracks (4 engineers, baseline)

Tracks follow the brief: **E** engine/providers, **U** desktop UI, **T** tools/MCP/browser, **P** platform/release. People move between tracks by phase.

| Phase | E (engine/providers) | U (desktop UI) | T (tools/MCP/browser) | P (platform/release) |
|---|---|---|---|---|
| **M1** | 1.05 core, 1.06 store, 1.23 context, 1.24 | 1.17 shell, 1.18 design system, 1.19 renderer, 1.20 chat UI, 1.21 | *works on providers in M1:* 1.09 core, 1.10–1.13 adapters, 1.15 catalog, 1.16 conformance | 1.01 repo/codegen, 1.02 protocol, 1.03 daemon, 1.04 client, 1.07 secrets, 1.08 config, 1.14 auth, 1.26, 1.27 CI; then 1.22 settings with U |
| **M2** | 2.01 tools, 2.02 permissions, 2.13 compaction, 2.15 turn loop | 2.11 artifacts (with T), ask cards, citation UI, 2.10 connectors UI, 2.14 | 2.03 search, 2.04 fetch, 2.05 code exec, 2.06–2.09 MCP | 2.12 hostile test; **starts 4.12 packaging + 4.13 updater/feeds early**; 4.07 policy |
| **M3** | 3.04 instructions, 3.08 RAG, 3.09, 3.02, 3.03, 3.13 command registry | 3.07 React artifacts, 3.01 memory UI, 3.10 usage, 3.05 skills UI, 3.13 `/` menu | 3.05 skills engine, 3.06 directories, **starts 4.01–4.02 browser** | 4.08 audit, 4.09 telemetry, 4.10 recovery, 4.11 retention, 4.16 D8 validation |
| **M4** | 4.03 browser policy/tools (with T); **starts M5 5.01–5.04** | 4.04 Browser tab, 4.15 a11y | 4.03, 4.05 browser | 4.12–4.14, 4.17, 4.18 release |
| **M5–M6** | 5.x engine (tools, shell, checkpoints, agents, subagents, hooks) | 6.x Code tab | 5.13 TUI, 5.14–5.16 | 6.05 terminal, 5.17, Code QA |

Key points of this plan:
- **The platform track runs from day one.** Packaging, signing, the updater, feeds and policy are not left to the end of M4. They start in M2, so M4 is mostly browser work and release QA, and the "hardening" risk is spread out.
- **The provider owner** (D18, t34) is the T engineer in M1. They stay the named owner after M1.
- **The browser starts in M3** on the T track, so its 16–22 ew don't all fall in M4.

### 4.3 With 3 or 5 engineers

- **3 engineers:** merge T into E and P. M1 alone is 7–9 months. Chat v1 takes 17–23 months, and the Code/CLI track can't overlap. Not recommended for the decided scope (see §5).
- **5 engineers:** add a second UI engineer (U2), the biggest bottleneck in M1–M3 (chat UI, settings, artifacts, browser tab, then the Code tab). From M4, the fifth engineer starts the **Code/CLI track in parallel** (M5 engine items that reuse M2's tool framework and permissions). Chat v1 GA in 10–14 months; Code/CLI GA about 4–7 months later instead of 5–8.

## 5. Is v1 = M1–M4 realistic?

**Short answer: yes with 4–5 engineers and a 12–17 month horizon for Chat v1; no with 3 engineers. The Code/CLI track adds 4–8 months after that.**

The evidence:
- Chat v1 is ≈182.5–255 ew. That is Claude Desktop's chat feature set plus a browser agent, on 3 OSes and 4 provider families (t31's scope warning stands).
- The largest single items are fixed costs that no cut removes: the engine core, the 4 adapters + conformance, the renderer, and signing/packaging on 3 OSes. Together they are ≈50–70 ew.
- Two external gates can still move the plan: the **D8 macOS/Linux bench runs** and the **CI platform choice** (ticket 41).

### 5.1 Recommendation

1. **Keep v1 = M1–M4 as the scope**, as decided (D29), and **staff 4 engineers minimum, 5 preferred**. With 5, plan Chat v1 GA for **Q4 2027–Q1 2028** and commit externally at the **high end** of the range.
2. **Ship order inside v1 (internal milestones):**
   - **A0 (week ~8–10):** prototype parity: daemon + store + Anthropic and OpenAI streaming in the desktop on all three OSes. Gate: the D8 bench passes on macOS/Linux.
   - **A1 = end of M1:** Synergech internal dogfood (chat only, no tools).
   - **A2 = end of M2:** internal dogfood with MCP, artifacts, search. Threat-model review #1.
   - **Beta = end of M3 + platform track ready:** invited external beta on beta channel, signed builds, updater live, browser behind a "preview" flag if not yet complete.
   - **GA = end of M4:** browser complete, pen test closed, staged rollout.
3. **Start the Code/CLI track during M4** (with the 5th engineer, or the E engineer once browser policy work is done). It reuses M2's tool framework, permissions and asks, so starting early is cheap and shortens the gap to Code GA.
4. **Pull platform work forward** (packaging, signing, updater, feeds, policy start in M2) so M4 is not a cliff.
5. **Plan the provider upkeep** (0.25–0.5 FTE) from GA onwards; it does not shrink.

### 5.2 If a date forces cuts

These are **options for the user to decide**, not decisions; each amends a recorded decision. Listed from least to most user-visible. Savings are in ew.

| Order | Cut or defer to v1.1 | Saves | Amends |
|---|---|---|---|
| 1 | Show the browser in a **pop-out real window only** (D21's original UX) instead of the in-window screencast; keep the banner in Genie | 3–4 | D56 |
| 2 | **Embeddings** in project RAG (keep BM25 retrieval, add embeddings in v1.1) | 1–1.5 | D32 |
| 3 | **Curated directories** (keep manual MCP add, skill upload and built-in skills) | 3–4 | D30, D37 |
| 4 | **React artifacts** (keep document/code/SVG/Mermaid/HTML) | 3–4 (+ part of 4.06) | D28 |
| 5 | **MCP sampling and elicitation** (keep tools, resources, prompts) | 1.5–2 | D30 |
| 6 | **Provider-native code execution** | 2–3 | D35 |
| 7 | **Whole browser to v1.1** (fast follow, ~3–4 months after GA) | 16–22 | D17 |

Cuts 1–6 together save ≈14–19 ew (≈4–5 calendar weeks for 4 engineers). Only cut 7 moves GA by a quarter. **My recommendation is to not cut the browser** (it is a stated v1 differentiator, D17) but to decide cut 1 early if the screencast spike (§6, R4) shows poor latency or IME problems.

## 6. Risk register

| # | Risk | Likelihood / impact | Mitigation | Owner |
|---|---|---|---|---|
| R1 | **D8 gate still pending on macOS/Linux** (D57): WKWebView or WebKitGTK may fail the streaming perf gate | Medium / High | Run `run-bench.sh` on the oldest supported macOS and Ubuntu LTS (NVIDIA + Intel/AMD, X11 + Wayland) **before M1 week 6**; renderer mitigations already proven on Windows; Platform adapter keeps a swap to Tauri 3 Chromium runtime (Linux first) to weeks of work; if macOS fails, reopen D1 | U lead |
| R2 | **WebKitGTK specifically** (NVIDIA blank windows, Wayland, scrolling regressions, a lagging distro WebKit) | High / Medium | DMABUF workaround on NVIDIA; minimum WebKitGTK version; perf gate on real Linux hardware in CI (SA-34); fallback per D9 to Tauri 3's Chromium runtime on Linux; budget 4.16 | P |
| R3 | **Provider maintenance burden:** 4 protocols × monthly vendor changes (new events, API versions, deprecations) | High / Medium | Named owner (D18); tolerant decoding; cassettes + golden tests + nightly live smoke with drift report (D41); 0.25–0.5 FTE after GA; catalog patches instead of code changes where possible | T (provider owner) |
| R4 | **Browser screencast UX** (D56): frame rate, input latency, IME, clipboard, file pickers, multi-tab, HiDPI; Chrome/Edge missing on some Linux/macOS machines; CDP detectability and CAPTCHAs | Medium / High | 1-week spike at the start of 4.04 against SA-23 targets; pop-out fallback always available (cut 1); clear "install a browser" path; `browser_handoff` for CAPTCHAs; no stealth features | T |
| R5 | **Scope and schedule:** v1 is ≈182.5–255 ew before Code; scope creep from "like Claude" comparisons | High / High | Fixed v1 list (t31 + amendments); internal milestones with go/no-go; cut list §5.2 pre-agreed; commit externally at the high end | Eng lead |
| R6 | **Prompt injection** via web, browser, MCP and repos leads to harmful actions | Medium / High | Deterministic gates, taint, trust + hash approvals, policy; red-team suites in CI; pen test; honest disclosure that risk is reduced, not removed | T + security reviewer |
| R7 | **Artifact sandbox escape** due to engine differences (custom schemes, iframe sandbox, CSP) across WebView2/WKWebView/WebKitGTK | Low / High | Opaque origin + no capabilities + CSP; hostile-artifact CI on all 3 engines (2.12, 4.06); pen test scope | U |
| R8 | **CI platform and repo host undecided** (t41) blocks signing, secrets for live tests and perf runners | High / Medium | Decide the CI part of t41 by **M1 week 4**; keep CI definitions thin wrappers around `xtask` commands so a later switch is cheap | P |
| R9 | **Signing, notarisation and Linux repos** break releases late (Azure Trusted Signing onboarding, Apple notarisation of a bundled binary, GPG repo hosting) | Medium / Medium | Start 4.12/4.13 in M2; nightly signed builds on beta; verify with `spctl`/`signtool` in CI | P |
| R10 | **External dependencies:** Entra multi-tenant app registration, trademark for "Genie", models.dev licence, signing-key custody (t35) | Medium / High | Assign owners now (§7); Entra app needed for 1.14 in M1; trademark before any external beta | PM / legal |
| R11 | **Reasoning replay and caching bugs** cost money or break sessions (account-bound blocks, preserved-thinking prefix checks) | Medium / Medium | Persisted wire records and AccountTag from day one; golden tests; cost dashboards in Developer; usage alerts in beta | E |
| R12 | **Shared daemon lifecycle:** orphans, antivirus on Windows, version skew during updates, named-pipe edge cases | Medium / Medium | Prototype 37 path; lock file + ensure race tests; drain-and-restart tests; Job Objects; doctor checks | P |
| R13 | **Key loss** (keychain reset, OS reinstall, no Linux keyring) makes chat history unreadable | Medium / Medium | Recovery key prompt at first run; passphrase file fallback; never delete unreadable data silently | E |

## 7. Pending external items

| Item | What is needed | Needed by | Suggested owner |
|---|---|---|---|
| **Ticket 41 — CI platform + repo host** | Choose GitHub + Actions / Azure DevOps / GitHub + Azure Pipelines | **M1 week 4** (blocks 1.01, 1.27, signing, live tests) | Eng lead + IT |
| Ticket 41 — business model, licence enforcement, source licensing | Decide; v1 keeps a `licence` module boundary and an About-only Account section | Before first external (non-Synergech) release | Product / leadership |
| Ticket 35 item 1 — **trademark check for "Genie"** | Clearance or a new name | Before external beta (end M3) | Legal |
| Ticket 35 item 2 — **Genie multi-tenant Entra app** + admin-consent guide | Register a public client in the Synergech tenant | **M1** (1.14 Entra sign-in) | IT / Azure admin |
| Ticket 35 item 3 — **models.dev data licence** | Confirm commercial reuse, or build the catalog from provider docs only | M1 (1.15) | Legal |
| Ticket 35 item 4 — **third-party notices** | Notice generation, OpenCode MIT headers, Chromium-terms check | M4 (4.17) | P + legal |
| Ticket 35 item 5 — **signing keys custody and rotation** | Approve SA-31 hierarchy, hardware, custodians | M2 (before feeds go live) | Security lead |
| Ticket 35 item 6 — **Chrome/Edge terms** for CDP-driven use | Legal review (no redistribution in v1) | Before M4 browser beta | Legal |
| **macOS/Linux bench runs** (t37, D57) | Run `run-bench.sh` on oldest supported macOS and Ubuntu LTS (NVIDIA + Intel/AMD, X11/Wayland, with/without `WEBKIT_DISABLE_DMABUF_RENDERER`) | **Before M1 week 6** | User (as recorded) / U lead |
| Brand input (t29) | Accent colour, tokens, logo/icon (SA-20 placeholder until then) | Before external beta | Design |
| Azure Trusted Signing + Apple Developer ID + GPG identities | Accounts and certificates | M2 | IT / P |

## 8. First four weeks (to unblock everything else)

1. Week 1: decide the CI platform (t41); create the repo per t38; start the Entra app registration and trademark check.
2. Weeks 1–2: `genie-protocol` v1 types and codegen; store schema v1; provider core skeleton with the cassette executor.
3. Weeks 2–4: daemon transport on 3 OSes from the prototype-37 code paths; design tokens and app frame; Anthropic adapter first (most complex replay rules).
4. By week 6: macOS/Linux bench results in hand; go/no-go on D8 for each OS.
