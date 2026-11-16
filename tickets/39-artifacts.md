# 39 — Artifacts (Chat v1)

type: wayfinder:grilling · status: closed · claimed-by: charting session (2026-11-16) · blocked-by: 03, 12, 29

## Question
How do artifacts work in Genie's desktop chat? They're in v1 (D10).

## To decide
- Artifact types: markdown/doc, code, HTML, React component, SVG, Mermaid. Which ship in v1?
- How the model creates and updates them: a provider-neutral tool call (e.g. `create_artifact` / `update_artifact`) vs inline tags. It must work on every provider (D2).
- **Sandboxing (security-critical):** run artifact code in a separate origin, in a sandboxed iframe with a strict CSP. No access to Tauri IPC or the daemon. Is network access allowed (allowlisted CDNs only)?
- Versioning: each update is a new version, with a version switcher in the panel.
- Storage: artifacts as message parts vs their own table (12). Copy, download and export.
- Panel UX: side panel next to the chat; full-screen view; works in the same window layout as Claude Desktop-style UX (29).

## References
- OpenCode ch.12: [12-storage-snapshots-and-worktrees.md](../knowledge/opencode/12-storage-snapshots-and-worktrees.md) — artifacts section
- Feature inventory: [research/claude-subsystems-inventory.md](../research/claude-subsystems-inventory.md) — Chat → artifacts rows

## Resolution
Resolved 2026-11-16. User decisions are marked **(user)**; the rest are defaults.

### Types in v1 (user): all four
| type | rendering | runs code? |
|---|---|---|
| `document` (Markdown) | Genie's markdown renderer (same as chat) | no |
| `code` (any language) | syntax-highlighted viewer | no |
| `svg` | sanitised (no scripts, no external refs) and shown as an image | no |
| `mermaid` | bundled Mermaid, rendered in the sandbox | sandboxed |
| `html` | single-file HTML/CSS/JS | **sandboxed** |
| `react` | a single component (default export), transpiled in the sandbox with **esbuild-wasm** | **sandboxed** |

### Network (user): none; libraries bundled
- Artifacts **cannot make network requests** (CSP `connect-src 'none'`, no external `script-src`/`img-src`/`font-src`). Data can't leave, and artifacts work offline.
- **Bundled library set** (versions pinned and shipped with Genie, served from the artifact scheme): React + ReactDOM, Tailwind (prebuilt utility CSS), lucide-react icons, a chart library (Recharts), d3, lodash, papaparse, mathjs, three.js, Mermaid, KaTeX. The list is in the system prompt / tool description, so models only import what exists; an unknown import gives a clear error.
- **Not in v1** (inventory "never"): artifacts calling the model, persistent artifact storage, public share links.

### Sandbox (default; security-critical)
- Artifacts are served from a **separate custom URI scheme** (`genie-artifact://<artifact>/<version>/`; on Windows `http://genie-artifact.localhost/`) by the Tauri Rust side, never from the app's own origin.
- Shown in an **`<iframe sandbox="allow-scripts">`** with no `allow-same-origin`, so it has an opaque origin: no cookies, no storage, no access to the parent DOM.
- **Tauri capabilities are not granted to the artifact origin:** no IPC, no plugins, no daemon access. Checked by a CI test that loads a hostile artifact and asserts that every IPC/fetch attempt fails.
- **CSP** on every artifact response: `default-src 'none'; script-src genie-artifact: 'unsafe-eval' 'wasm-unsafe-eval'; style-src genie-artifact: 'unsafe-inline'; img-src genie-artifact: data: blob:; font-src genie-artifact: data:; connect-src 'none'; frame-src 'none'; form-action 'none'`.
- The parent ↔ artifact channel is `postMessage` only, with a fixed message schema (ready, resize, runtime error, request fullscreen). Messages are validated, and anything else is ignored.
- Blocked: top-navigation, popups, downloads from inside the artifact (Genie's toolbar does downloads), `window.open`, and modals (`alert`/`confirm`/`prompt` are shimmed).

### How the model creates them (default; works on every provider)
- Provider-neutral **function tools**: `artifact_create {type, title, content, language?}`, `artifact_update {id, old_str, new_str}` (targeted edit), `artifact_rewrite {id, content}`. There are no provider-specific tags.
- **Streaming:** the panel opens as soon as `artifact_create` starts streaming its input (tool-input deltas, D22). Documents and code render live; HTML and React render once the input is complete.
- **Permissions:** creating and updating artifacts is always allowed (local, sandboxed). Admins can disable artifacts (→ 28); users can switch them off in Settings → Capabilities (→ 33).
- A runtime error inside an artifact is shown as a banner with a **"Fix with Genie"** button that sends the error and stack to the model as a new user message.

### Versions and storage (default; → D24 tables)
- `artifact` (id, session_id, type, title) + `artifact_version` (artifact_id, n, message_id, content → blob if large). Each create/update/rewrite is a new version tied to the **message** that produced it, so **branches (D24) get their own versions**: the panel shows the versions on the active path.
- The panel has a version switcher (`‹ v2/3 ›`), Copy, Download (`.md`, source file, `.html`, `.svg`, `.png` for diagrams, `.jsx`), Fullscreen, and Code/Preview tabs.

### UX (default; → 29, 31)
- A side panel to the right of the chat that can be resized and closed. Artifact cards in the message open it. A "Recent artifacts" list sits in the sidebar per chat/project (v1: per chat; global Artifacts tab → v2).

### Effort note
- The React path (esbuild-wasm + bundled libraries + sandbox hardening + the hostile-artifact CI test) is the largest piece. It should be built after documents, code, SVG, Mermaid and HTML, behind the same panel.
