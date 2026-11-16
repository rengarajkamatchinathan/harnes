# 29 — Brand, name and design system

type: wayfinder:grilling · status: closed · claimed-by: charting session (2026-11-16) · blocked-by: —

## Question
Confirm 'Genie' as the product name. What is the visual identity? It must be Claude-like in UX and our own in branding (C1).

## To decide
- Name, CLI binary, trademark check
- Design tokens: palette, type scale, radius, density, light/dark
- Which Claude Desktop UX patterns we keep (sidebar, composer, model picker, artifacts panel, settings nav)
- Component base: shadcn/ui + Radix + Tailwind (D5)

## References
- OpenCode ch.18: [18-app-ui-and-gui-plugins.md](../knowledge/opencode/18-app-ui-and-gui-plugins.md) — read TL;DR, Architecture, Genie takeaways

## Resolution
Resolved 2026-11-16 (user).
- **Name:** **Genie** is the product name; the CLI binary is `genie`. A trademark check is required before launch (→ 35). "Genie" is a crowded name in AI products.
- **Visual direction:** calm neutral greys plus **one distinctive accent colour of our own**. Clean sans-serif UI font (Inter or Geist; both open licences) and a monospace for code. No Anthropic fonts, colours, illustrations or logo shapes (C1).
- **Components:** **shadcn/ui + Radix + Tailwind**, themed by CSS-variable design tokens (colour, type scale, radius, spacing, density).
- **Theme:** light and dark in v1, **following the system** by default, with a manual override in Settings → Appearance.
- **UX patterns we keep from Claude Desktop (patterns only):** collapsible left sidebar (new chat, search, projects, recent chats); centred conversation column; bottom composer with attachments, model picker and tool/connector toggles; artifacts side panel (39); browser window (40); Settings with a left-nav section list (33).
- **Deferred to the spec phase:** exact accent colour, token values, logo/icon. Any design or brand team input goes here.
