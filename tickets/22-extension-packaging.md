# 22 — Extension packaging (plugins)

type: wayfinder:grilling · status: closed · claimed-by: charting session (2026-11-17) · blocked-by: 17,20,21

## Question
How are extensions packaged and distributed?

## To decide
- No in-process plugin ABI (already decided per KB #26 SKIP)
- A plugin is a bundle of skills, commands, agents, hooks and MCP servers
- Desktop GUI extensions: never with full main-process access

## References
- OpenCode ch.11: [11-mcp-and-plugins.md](../knowledge/opencode/11-mcp-and-plugins.md) — read TL;DR, Architecture, Genie takeaways
- OpenCode ch.18: [18-app-ui-and-gui-plugins.md](../knowledge/opencode/18-app-ui-and-gui-plugins.md) — read TL;DR, Architecture, Genie takeaways

## Resolution
Resolved 2026-11-17. User decisions are marked **(user)**; the rest are defaults from KB ch.11/18 and D30/D37/D43/D48.

### Timing (user): v2
- v1 covers one-click installs through the **skills directory** (D37) and the **connector directory** (D30). **Plugin bundles** and **packaged one-click MCP extensions** (`.mcpb`-style) ship in **v2**.
- v1 designs for it: the skill, command, agent, hook and MCP registries already accept a "source" tag, so a plugin can contribute to them later without refactoring.

### Format (user): Claude plugin-compatible
- Genie reads **Claude Code's plugin layout** (`.claude-plugin/plugin.json` manifest; `skills/`, `commands/`, `agents/`, `hooks/`, `.mcp.json`) and **Claude marketplace JSON**, so existing Claude plugins install in Genie.
- **Hooks inside Claude plugins** run through the D48 adapter (`genie hook-adapter claude`), because Genie's native hook format is its own. Genie-native plugins may also ship hooks in the D48 format (an optional `genie` section in the manifest).
- Plugin components are namespaced (`<plugin>:<skill>`, `/plugin:command`, `mcp__<plugin>_<server>__…`) to avoid collisions.

### Rules (default; for v2)
- **No in-process plugin ABI** (KB #26 SKIP). Plugins only add data (skills, commands, agents) and **out-of-process** pieces (hooks as subprocesses, MCP servers). **No GUI plugins with main-process/Tauri access**; any future GUI extension point uses the D28 artifact-style sandbox.
- Install from a marketplace URL, a git ref (commit-pinned; a mutable ref is flagged) or a local folder into a content-keyed cache; `-name` disable directives.
- **Trust and approval:** installing shows every component (scripts, hooks, MCP commands) before enabling; hooks/MCP need D43-style hash approval, re-asked on update.
- Policy (D38): `plugin.use` allowlist, marketplace allowlist, managed-only; checked **before** install.
- The Genie skills/connector directories (D30/D37) can also list plugins in v2, with the same pinning + checksums + signed feed (D40).
