# 10 — Memory and instruction files

type: wayfinder:grilling · status: closed · claimed-by: charting session (2026-11-16) · blocked-by: 08

## Question
Which instruction files do we read, and how?

## To decide
- AGENTS.md (+ CLAUDE.md compat?) discovery order, rendered farthest-first
- Files above the workspace root are gated by workspace trust (32)
- User memory for desktop chat
- Imports/references

## References
- OpenCode ch.05: [05-memory-and-instructions.md](../knowledge/opencode/05-memory-and-instructions.md) — read TL;DR, Architecture, Genie takeaways

## Resolution
Resolved 2026-11-16. User decisions are marked **(user)**; the rest are defaults from KB ch.05 and D23/D24/D29.

### Instruction files (user): GENIE.md + AGENTS.md
- Both are read. If both exist in a directory, both load, **GENIE.md first**. **CLAUDE.md is not read live**; the importer (26) offers a one-time conversion of CLAUDE.md into GENIE.md (or a GENIE.md that `@imports` it).
- Either name can be disabled in settings (`instructions.files: ["GENIE.md", "AGENTS.md"]`).

### Discovery (default; Code mode / CLI)
- Order, rendered **farthest-first:** managed (`<managed dir>/GENIE.md`) → user (`~/.genie/GENIE.md`) → project root → each directory down to the cwd → `GENIE.local.md` (git-ignored, personal).
- The walk **stops at the project/git root**. Files above the root are never read; project files load only after workspace trust (→ 32).
- **Nested on demand:** when a file under a subdirectory is read, any GENIE.md/AGENTS.md between it and the root that hasn't been injected yet is added once per session. `.genie/rules/*.md` with `paths:` globs are injected when a matching file is read. A ledger is kept so compaction re-injects them (→ 16).
- **`@path` imports**, up to 4 hops deep, relative to the importing file, with no imports from outside the project unless they're in the user dir.
- **Size caps:** 64 KiB per file and 256 KiB in total, with a visible truncation note and a warning in Settings → Problems (D23).
- Extra `instructions` sources in settings: paths/globs; **URLs only from managed policy** (org-pushed, last-good copy kept).

### Chat instructions (default)
- **Custom instructions** ("Your Genie": name + instructions, user scope) → in every chat.
- **Project instructions** (D24 `project` table) → in every chat in that project.
- Order in the prompt: Genie base prompt → org (managed) instructions → custom instructions → project instructions → memory → (code) instruction files.

### Cache stability (default; ported from OpenCode)
- Every source is a keyed, hashed `InstructionSource`. Each session **freezes a baseline** into a versioned system part. Later changes are sent as small `<system-update>` messages (a diff if shorter) and never rewrite the cached prefix. Compaction rebuilds the baseline (→ 11/16).
- A turn never starts with a half-read file (unavailable → wait or show an error).

### Memory (user): written by the model with a memory tool; on by default
- Tools: `memory_save {text, scope: global|project}`, `memory_update {id, text}`, `memory_delete {id}`. They're always allowed (local data) and shown in the chat as a **"Memory updated" chip** that opens view/edit/undo.
- **Storage:** a `memory` table in the encrypted store (D24): id, scope (`global` or project_id), text, source session/message, created/updated. Sensitive categories (health, finances, etc.) are not saved unless the user asks explicitly; the tool instructions say so.
- **Use:** memories for the current scope go into the prompt as a memory instruction source (baseline + deltas, so the cache stays valid). Capped (e.g. 8 KiB); if over the cap, the most recently used entries are included and the model can call `memory_search`.
- **Default on (user)**, with a first-run notice explaining memory and how to manage it. Settings → Your Genie → Memory: view all, edit, delete, **pause**, **reset**. **Admin policy can disable it org-wide** (→ 28). **Incognito chats never read or write memory** (D29). Memory is included in data export and removed by retention/deletion (D24).
- **Code mode:** the same tools write project-scoped memory (project = workspace). File-based `MEMORY.md` à la Claude Code is not used; the DB is the single store.

### Past-chat search tool (default; D29)
- `chat_search {query, scope: current_project|non_project}` over FTS5 (D24) returns snippets with chat links, which the model cites. Incognito chats are excluded. Always allowed; admins can disable it.
