# OpenCode knowledge base

A deep reference to how **OpenCode** is built, for designing and implementing **Genie** features.
Readers: Genie engineers and future Claude sessions working on Genie tickets.


## How to use it

1. **Working a Genie ticket?** Find the subsystem in the table below and read that chapter's **TL;DR**, **Architecture** and
   **Genie takeaways**. The takeaways link back to the ticket and to [pick-list](../../research/opencode-pick-list.md) items.
2. **Porting something?** Follow the **walkthroughs** and **key types** to the exact files, then apply the porting rules in
   [opencode-reference.md](../../research/opencode-reference.md) (attribution for adapted code, no OpenCode branding).
3. **Claims marked *(inferred)*** weren't confirmed line-by-line. Verify them in the code before relying on them.

Every chapter uses the same sections: TL;DR · Where it lives · Architecture · Key types · Walkthroughs · Configuration ·
Edge cases · Tests worth reading · Genie takeaways · Open questions.

## Chapters

| # | Chapter | Covers | Genie tickets |
|---|---|---|---|
| 01 | [Architecture](01-architecture.md) | Monorepo map, package rules, process topology (service daemon), Location/Instance model, event bus, startup, build | 01 |
| 02 | [Effect runtime patterns](02-effect-runtime-patterns.md) | How Effect v4 is used; translating it to Rust | 01 |
| 03 | [Session and agent loop](03-session-and-agent-loop.md) | Turn lifecycle, tool loop, retries, cancellation, jobs, subagents | 01, 12, 19 |
| 04 | [Context and compaction](04-context-and-compaction.md) | System prompt assembly, caching, tool-output limits, compaction | 11, 16 |
| 05 | [Memory and instructions](05-memory-and-instructions.md) | AGENTS.md discovery, instructions/references, Claude-compat plugin | 10 |
| 06 | [Config, permissions and policy](06-config-permissions-and-policy.md) | Config discovery and keys, permission rules and prompts, managed policy | 08, 13, 28, 32 |
| 07 | [AI package and protocols](07-ai-package-and-protocols.md) | Canonical model, LLM loop, per-protocol wire handling, reasoning replay | 03, 34 |
| 08 | [Providers, models and credentials](08-providers-models-and-credentials.md) | Provider modules/plugins, models.dev catalog, variants, credentials/OAuth | 03, 04, 14 |
| 09 | [Tools](09-tools.md) | Tool framework and every built-in tool, edit/patch, shell, web, Code Mode | 06, 07 |
| 10 | [Agents, skills and commands](10-agents-skills-and-commands.md) | Agent definitions and built-ins, skills, commands | 18, 19, 21 |
| 11 | [MCP and plugins](11-mcp-and-plugins.md) | MCP client and OAuth, plugin API and every hook | 17, 20, 22 |
| 12 | [Storage, snapshots and worktrees](12-storage-snapshots-and-worktrees.md) | SQLite schema, snapshots (git), worktrees, scheduler, artifacts | 12, 23, 24 |
| 13 | [Server, protocol and SDK](13-server-protocol-and-sdk.md) | HttpApi groups, SSE events, generated client, SDK, service lifecycle | 01, 02 |
| 14 | [CLI, headless and ACP](14-cli-headless-and-acp.md) | Command tree, `run` NDJSON, ACP server, updater/install | 02, 25, 27 |
| 15 | [TUI and rendering](15-tui-and-rendering.md) | OpenTUI app, streaming markdown (patchmark), Mermaid, themes | 05 |
| 16 | [PTY process daemon (Rust)](16-pty-process-daemon.md) | The `opencode-pty` crate in depth; reusability | 07 |
| 17 | [Desktop shell](17-desktop-shell.md) | Electron main process, service, updater, packaging; Tauri seams | Desktop (far fog), 27 |
| 18 | [App UI and GUI plugins](18-app-ui-and-gui-plugins.md) | Solid renderer, session UI, GUI plugin SDK and built-ins, i18n | Desktop (far fog) |

## Key findings (2026-11-16)

**What OpenCode gets right, and Genie should copy:**
- **Event-sourced sessions on SQLite** (ch. 03, 12). Prompts are saved to an inbox before they run, a per-session coordinator handles interrupts, and a durable "claim" gives crash resume.
- **Cache-stable instructions** (ch. 04, 05). AGENTS.md, the skills index and the environment are frozen into a versioned system part per session. Later edits arrive as `<system-update>` messages, and compaction rebuilds the baseline. Up to 4 cache breakpoints, keyed by the root session so subagents share the cache.
- **Compaction** (ch. 04). Triggers at window − max(15%, 16k), uses a fixed Markdown summary template, keeps about 15k tokens of recent messages, and retries overflow at 85 → 70 → 50 → 35%. Tool output is truncated centrally at 2,000 lines / 50 KiB and spilled to a file.
- **Request normalization before any protocol** (ch. 07). Repairs tool history, fixes UTF-16, rewrites tool ids per provider, places cache breakpoints. Quirk flags are per *model* (`LanguageModelCompatibility`), and effort becomes per-model request patches (`variant.ts`).
- **Provider error classification and retry** (ch. 07). About 40 context-overflow patterns, quota kept separate from rate limits, honours `x-should-retry`, capped backoff, 1-hour budget when offline.
- **Deny before hooks** (ch. 11). Configured deny rules are checked before any plugin hook, so a hook can't turn a deny into an allow. This matches Genie's rule.
- **Snapshots in a shadow git repo, before and after every step** (ch. 12). Captures changes made through Bash too, unlike Claude's file-tool-only checkpoints. Missing: pruning and gc.
- **Shell** (ch. 09, 16). A command scanner turns each command into per-command permission resources; 2 min timeout then background; non-PTY "pipe mode" through the Rust daemon, with a Windows Job Object.
- **Inline terminal UI exists** (ch. 15). `packages/tui/src/mini/` (`opencode mini`) is a split-footer inline UI, the closest reference for Genie's CLI.
- **Generated clients with a drift check in CI; one route graph on three hosts** (ch. 13). Includes an in-memory transport for hermetic tests.

**What Genie should *not* copy:**
- **Permissive defaults** (ch. 06, 09). Every agent starts at allow-everything; plan mode still allows the shell. Rules are last-match-wins. Managed `permissions` can likely be overridden by project agent rules; only `policies` is tamper-proof.
- **Code Mode as the default tool path** (ch. 09, 11). MCP and `opencode_*` tools are reachable only via a model-written JS `execute` tool. Several of those tools, and the `` !`cmd` `` substitution in command templates, have no permission check.
- **No read-before-edit or staleness checks** (ch. 09).
- **The shared background daemon on a fixed port**, with a plaintext password in a registration file (ch. 01, 13, 17).
- **A non-resumable SSE stream** (ch. 13). No event ids; pending permission asks are ephemeral.
- **Credentials as plaintext JSON in SQLite** (ch. 08), with no OS keychain.
- **Third-party GUI plugins running inside the Electron main process** with full Node access (ch. 18).
- **MCP defaults** (ch. 11): a 12 h tool timeout, `<server>_<tool>` names that can collide, and an OAuth client metadata URL hosted on opencode.ai.

**Findings that bear on Genie decisions** (proposals, not yet decided; see the map's Notes):
1. **Protocol (ticket 02):** add a per-session `seq` on events, a resume cursor in the init handshake, and a `steer` command for mid-turn user input.
2. **Provider layer (ticket 03):** replay opaque reasoning only to the same **provider + model**, not just the same provider. Widen the opaque payload. Add request normalization, per-model quirk flags, per-model effort patches, and a richer Usage type with cache read/write.
3. **Desktop:** OpenCode moved **from Tauri to Electron** (`sign-desktop-macos.sh:5`, reason unrecorded). The agent-driven browser needs CDP, which macOS's built-in webview lacks. Worth a research ticket before Tauri is final.
4. **Checkpoints (ticket 23):** a shadow git repo with per-step snapshots is a strong default; add retention and gc.
5. **Pick-list corrections** were already applied (#7, #32, #35).

## Quality checks (2026-11-16)

- 18 chapters, about 62K words. Every chapter has all 10 template sections and at least one mermaid diagram.
- All **425** unique full-path `file:line` citations resolve to an existing file and line in the clone. A random sample matched the cited code exactly.
- Each chapter lists its unconfirmed claims, marked *(inferred)*, under "Open questions".
- Writers reported that a few range citations in ch. 08 and ch. 12 (written by helper agents) weren't checked line by line.

## Related

- [OpenCode reference rules and comparison](../../research/opencode-reference.md)
- [Port pick-list (45 components)](../../research/opencode-pick-list.md)
- [Claude Code / Desktop subsystem inventory](../../research/claude-subsystems-inventory.md): the *behaviour* Genie matches
- [Wayfinder map](../../WAYFINDER.md)
