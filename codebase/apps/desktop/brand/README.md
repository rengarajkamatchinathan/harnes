# Genie brand: "Magic Braces"

Two curly braces around a four-point star. Ink `#111827`, Wish Violet `#7C3AED` (on dark: Paper `#F9FAFB` braces, `#A78BFA` star). Wordmark: Manrope ExtraBold, lowercase, tracking −3%.

- `genie-mark*.svg` — the mark (light, dark, mono); `-idle` / `-thinking` are animated (CSS, honours reduced motion).
- `genie-app-icon.svg` — full-bleed app icon (Windows/Linux); `genie-app-icon-macos.svg` — inset to the macOS icon grid.
- `genie-favicon.svg` — heavier strokes for 32 px and below.
- In the app the mark is `src/ui/GenieMark.tsx`.

Regenerating `src-tauri/icons/` (the CLI writes extra platform files; keep only the five in `tauri.conf.json`):

```sh
pnpm tauri icon brand/genie-app-icon.svg -o /tmp/genie-icons        # 32x32.png, 128x128.png, icon.png, icon.ico
pnpm tauri icon brand/genie-app-icon-macos.svg -o /tmp/genie-icons-mac  # icon.icns
```
