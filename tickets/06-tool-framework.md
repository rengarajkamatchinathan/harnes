# 06 — Tool framework and built-in tools

type: wayfinder:grilling · status: closed · claimed-by: charting session (2026-11-16) · blocked-by: 13

## Question
What is the Tool trait, and which built-ins ship?

## To decide
- Tool trait: schema, execute, permission resource, output limits
- Built-ins: read, write, edit/multi-edit or patch, glob, grep, list, web fetch, todo, task
- Read-before-edit and staleness checks (OpenCode lacks them: add them)
- Do NOT make Code Mode the default tool path (KB)

## References
- OpenCode ch.09: [09-tools.md](../knowledge/opencode/09-tools.md) — read TL;DR, Architecture, Genie takeaways

## Resolution
Resolved 2026-11-16. User decisions are marked **(user)**; the rest are defaults from KB ch.09 and D20/D25/D32.

### Tool trait (default)
```rust
trait Tool: Send + Sync {
    fn name(&self) -> &str;                       // e.g. "web_fetch", "mcp__gh__create_issue"
    fn description(&self, ctx: &ToolCtx) -> Cow<str>;
    fn input_schema(&self) -> &JsonSchema;        // schemars-generated
    fn kind(&self) -> ToolKind;                   // ReadOnly | Mutating | External | ProviderHosted
    fn availability(&self, ctx: &ToolCtx) -> Availability; // mode, model capability, admin policy, chat vs code
    async fn execute(&self, input: Value, ctx: ToolCtx) -> Result<ToolOutput, ToolError>;
}
```
- **The tool owns authorization:** `execute` calls `ctx.permissions.assert(resource, save_suggestions)` (D25) at the right moment (e.g. after an edit has matched). The registry only **hides** tools that are fully denied, or unavailable for the mode/model.
- `ToolError::Declined` is its own type and is never turned into tool output (D20). Other errors become model-readable messages (input-validation errors list up to 5 issues).
- **Input repair:** aliases for habits from other harnesses (`bash`→`shell`, `str_replace`→`edit`, camel/snake key variants).
- Output goes through central truncation/spill (D32). Tools run while the model is still streaming and are joined at step end (D20).
- Tool sets are chosen per session kind: **chat** and **code** registries share the same framework.

### Chat v1 tools
| tool | notes | permission |
|---|---|---|
| `artifact_create/update/rewrite` | D28 | always allowed (admin can disable) |
| `memory_save/update/delete/search` | D31 | always allowed; off when memory is paused or in incognito |
| `chat_search` | D31 | always allowed (admin can disable) |
| `project_search` | D32 (retrieval mode only) | always allowed |
| **`web_fetch`** (user) | HTML→Markdown, 5 MB cap, 30 s timeout, ≤10 redirects, **permission checked at each redirect**, URLs with credentials rejected, private/loopback IPs blocked by default (SSRF guard) | **URLs the user pasted in this chat are allowed**; other domains → ask once/always (`WebFetch(domain:…)`) |
| `web_search` (provider-native) | D11; details → 15 | Capabilities toggle |
| **`code_execution` (provider-native)** (user) | Anthropic code execution / OpenAI Code Interpreter / Gemini code execution where the model supports it (D27 capability); **greyed out elsewhere**, like web search. Files the user attaches are uploaded to the provider sandbox only when the model calls it, with a first-use notice "files will be sent to <provider>'s sandbox". Outputs (charts, files) come back as attachments/artifacts. Admins can disable it (→ 28). | Capabilities toggle + first-use notice |
| `browser_*` | D21; tool surface per research/browser.md | D21 site rules |
| `mcp__*` | D30 | D25/D30 |
| `skill` | → 21 | always allowed for enabled skills |

### Code-mode tools (decided now; built after chat, D6)
| tool | notes |
|---|---|
| `read` | text/images/PDF; 2,000 lines / 50 KiB / 2,000 chars per line; **records mtime + hash for read-before-edit** |
| `write` | new files, or overwrite **only after read**; staleness check |
| `edit` | `old/new/replace_all`; three-tier matcher (exact → Unicode-normalised → trailing-whitespace-tolerant), LF view keeping CRLF/BOM, "closest lines" hint on miss; **read-before-edit + staleness check in the per-path lock; approval asked after the match succeeds** |
| `multi_edit` | several edits to one file, applied atomically |
| `patch` | Codex-style envelope for models flagged `prefers_patch` in the catalog (D27), **atomic with rollback** (OpenCode's isn't) |
| **`glob`, `grep`** (user) | **on every OS**, backed by system `rg` or a bundled, checksum-pinned ripgrep; same hidden-file rules for both; 100 results / 30 s |
| `list` | directory listing with ignore rules |
| `shell` | → 07 (pipe mode, 2 min → background, command scanner) |
| `todo_write` | plan/todo list shown in the UI |
| `task` | subagents → 19 |
| `ask_user` | structured question to the user (driving client, D22) |
- Formatter after edits is opt-in (`formatter` setting); the post-format diff is returned to the model.
- File mutations are serialised per path inside the daemon (D20 shared daemon covers all clients).
