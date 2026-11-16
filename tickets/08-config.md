# 08 — Config files, layers and schema

type: wayfinder:grilling · status: closed · claimed-by: charting session (2026-11-16) · blocked-by: —

## Question
Which config files exist, and how do the layers merge?

## To decide
- Layers: managed → user → project → local
- One JSON schema that drives both the files and the Settings UI (see 33)
- Hot reload
- the daemon is the source of truth
- Per-key diagnostics for bad config

## References
- OpenCode ch.06: [06-config-permissions-and-policy.md](../knowledge/opencode/06-config-permissions-and-policy.md) — read TL;DR, Architecture, Genie takeaways

## Resolution
Resolved 2026-11-16. User decisions are marked **(user)**; the rest are defaults from KB ch.06.

### Format (user)
- **Strict JSON** (`settings.json`), no comments. The JSON Schema is published for editor autocomplete via `$schema`.

### Settings UI writes (user)
- **The user scope by default.** In Code mode, settings that make sense per project show a **User / Project / Local** scope picker. Values locked by managed policy are shown as locked, with the org name.

### Layers (default; highest wins)
1. **Managed** (admin): an OS system file (`/Library/Application Support/Genie/`, `/etc/genie/`, `C:\ProgramData\Genie\`) plus macOS MDM plist / Windows registry policy. **Managed `policies` are tamper-proof**; nothing below can loosen them (→ 28).
2. **CLI flags / `--settings <file>`** (session only).
3. **Local project:** `<project>/.genie/settings.local.json`. Personal and git-ignored; also holds saved "always allow" approvals (an auditable file, not an opaque DB row; → 13).
4. **Project:** `<project>/.genie/settings.json` (committed). Loaded only after **workspace trust** (→ 32).
5. **User:** `~/.genie/settings.json`.
- Only these exact paths: **no walk up to `/`**. Project root = git root, or else the opened folder.
- **Merge:** scalars → highest layer wins; objects → deep-merged per key; arrays (permission rules, MCP servers, instructions) → concatenated with layer tags. Each domain folds the layer list itself (OpenCode's `Vec<Layer>` shape), so permissions can apply their own deny→ask→allow semantics (→ 13).

### What is NOT in settings files
- **Secrets:** API keys and tokens live in the OS keychain (→ 14). Settings may reference `${env:VAR}` only inside the `providers` block.
- **Chat data:** chat "Projects" (instructions, knowledge files), conversations and artifacts live in the store (→ 12), not in settings.
- **Per-device UI state** (window size, sidebar collapsed, last tab) goes in a separate `~/.genie/state.json`.

### Behaviour
- **The daemon is the single source of truth.** Clients read and write through `config.get / config.set {key, value, scope}` (D22), never the files directly. A `config.changed` event is sent on every change.
- **Hot reload:** the daemon watches files (including ones that don't exist yet) with a 100 ms debounce and suppresses events when the content hasn't changed. External edits show up live in the UI.
- **Diagnostics are per key:** an invalid key is dropped with a warning (shown in Settings → a "Problems" badge, and in `genie config doctor`); the rest of the file still loads. Malformed JSON keeps the last good copy of that layer.
- **Schema:** Rust types → JSON Schema (`schemars`) → (a) `$schema` for editors, (b) generated TS types, (c) metadata (title, description, enum labels, `scopes_allowed`, `restart_required`) that **drives the Settings UI** (→ 33). Deprecated keys are marked and migrated with diagnostics (reused by the 26 importer).
- **Dev and stable coexist:** channel-specific dirs/files (`~/.genie` vs `~/.genie-beta`) (→ 27).
