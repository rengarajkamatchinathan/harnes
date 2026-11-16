# 33 — Settings UI

type: wayfinder:grilling · status: closed · claimed-by: charting session (2026-11-16) · blocked-by: 08,29

## Question
Which sections and controls does the Settings UI have, matching Claude Desktop's structure?

## To decide
- Sections: General, Appearance, Providers & Models, Permissions, Connectors/MCP, Extensions, Keybindings, Privacy, Account/License
- Generated from the config schema (08) vs hand-built

## References
- OpenCode ch.06: [06-config-permissions-and-policy.md](../knowledge/opencode/06-config-permissions-and-policy.md) — read TL;DR, Architecture, Genie takeaways
- OpenCode ch.18: [18-app-ui-and-gui-plugins.md](../knowledge/opencode/18-app-ui-and-gui-plugins.md) — read TL;DR, Architecture, Genie takeaways

## Resolution
Resolved 2026-11-16. User decisions are marked **(user)**; the section contents come from earlier decisions.

### Container (user)
- A **full page inside the main window**: it replaces the chat area, with a left section nav (D19), Back, and a search box over all settings. Deep links `genie://settings/<section>[/<item>]` (used by notifications, error banners and "Fix" links).

### Sections (v1)
| Section | Contents | Source |
|---|---|---|
| **General** | Language (en first, i18n-ready), launch at login, update channel / check for updates, default chat model + effort, notifications | 27, D27 |
| **Appearance** | Theme (System / Light / Dark), chat font (Default / System / Dyslexia-friendly), font size, density | D19 |
| **Your Genie** | Name, custom instructions, **Memory** (on/off/pause, view/edit/delete entries, reset), past-chat search on/off | D31 |
| **Providers & Models** | Per provider: credentials (labelled, active, source shown; API key / Entra sign-in), endpoint, discovered models/deployments, per-model **Advanced** (sampling, max output, default effort), catalog status, model overrides | D26, D27 |
| **Capabilities** | Web search, artifacts, browser (Chrome/Edge detection, profile, site permissions, clear data), skills (list, enable/disable), project retrieval on/off | D11, D21, D28, D32, 21 |
| **Connectors** | MCP servers (status, tools with per-tool toggles, auth/sign-in, logs link), **connector directory** (browse, add, see what runs), add custom (stdio / URL) | D30 |
| **Permissions** | Saved "always" approvals (chat + per project), rules list (allow/ask/deny) with scope, default mode (Code), bypass availability | D25 |
| **Usage & spend** | (user) Tokens and cost by day / provider / model / project, including cache and compaction lines; **optional monthly budget with warnings at 80% and 100%** (notifications only, no hard stop) | D27, D33 |
| **Privacy & data** | Retention (with admin limit shown), encryption status + **recovery key export**, export all data / single chat, delete all data, telemetry and crash-report opt-in (no prompt content) | D24, 35 |
| **Developer** | (user) Daemon status (version, uptime, socket, restart), MCP server logs, app/daemon logs, open settings files (each scope), `genie doctor` results + **Copy diagnostics** (sanitised, no prompt content), Problems (config diagnostics, D23) | D20, D23, D30 |
| **Account & licence** | Licence status/activation, organisation (if managed), about, third-party licences (incl. OpenCode MIT notice, C3) | 35 |
- **Code-mode section** (worktrees, branch prefix, etc.) and **Keyboard shortcuts** → v2 with the Code tab.

### How it's built (default)
- **Generated from the schema** (D23) wherever a setting is a plain value: the `schemars` metadata (title, description, enum labels, `scopes_allowed`, `restart_required`, `advanced`) renders a standard control (toggle, select, number, text, list). **Hand-built panels** for rich sections: Providers & Models, Connectors, Permissions, Memory, Usage & spend, Developer.
- Every control writes through `config.set {key, value, scope}` (D22/D23). Changes apply immediately (no Save button) and show an inline "Saved" or validation error. Settings that need a restart show a banner.
- **Scope picker** (User / Project / Local) only in Code mode for settings that allow it (D23). **Admin-locked** values are disabled, with a lock icon and "Managed by <org>".
- External file edits appear live (`config.changed`).
- Accessibility: full keyboard navigation, screen-reader labels from the schema, focus visible, and contrast meeting WCAG AA in both themes.
