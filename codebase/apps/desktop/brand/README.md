# Genie brand: "Split Gear"

Two crossed triangles with flat-cut tips that read as a six-tooth gear. The upper triangle is crimson and sits on the lower plate, with a transparent gap cut between them (an SVG mask, so it works on any background). Ink `#111827`, Crimson `#C8102E` (on dark: Paper `#F9FAFB` plate, `#E8344E` triangle). Wordmark: Manrope ExtraBold, lowercase, tracking −3%.

- `genie-mark*.svg`: the mark in light, dark and mono. `-idle` breathes the crimson triangle; `-thinking` turns the gear 120° per beat. Both are CSS-animated and honour reduced motion.
- `genie-app-icon.svg`: full-bleed app icon (Windows/Linux). `genie-app-icon-macos.svg`: inset to the macOS icon grid.
- `genie-favicon.svg`: larger mark and wider gap for 32 px and below.
- In the app the mark is `src/ui/GenieMark.tsx`.

Regenerating `src-tauri/icons/` (the CLI writes extra platform files; keep only the five in `tauri.conf.json`):

```sh
bun tauri icon brand/genie-app-icon.svg -o /tmp/genie-icons        # 32x32.png, 128x128.png, icon.png, icon.ico
bun tauri icon brand/genie-app-icon-macos.svg -o /tmp/genie-icons-mac  # icon.icns
```
