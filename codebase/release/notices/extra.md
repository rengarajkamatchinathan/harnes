## Other bundled components

These are not Rust crates, so `cargo xtask notices` can't find them in the dependency graph.
The release owner keeps this list current (09 §1, t35 item 4).

- **OpenCode** (MIT, https://github.com/sst/opencode): design references throughout; any
  adapted code is listed in `release/opencode-derived.txt` and carries the MIT notice.
  Copyright (c) 2025 opencode. Permission is hereby granted, free of charge, to any person
  obtaining a copy of this software... (full MIT text added at first adapted file).
- **ripgrep** (MIT OR Unlicense, https://github.com/BurntSushi/ripgrep): bundled with the
  desktop app for search tools, when bundled (not shipped by the CLI-only package yet).
- **Artifact runtime libraries** (spec 05, D28): listed here once `apps/desktop` bundles them.
- **npm dependencies** of the desktop app: generated from `pnpm licenses list --prod --json`
  once the pnpm workspace exists (Known gaps).
