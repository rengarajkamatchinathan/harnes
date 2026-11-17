# 07 — Shell execution

type: wayfinder:grilling · status: closed · claimed-by: charting session (2026-11-16) · blocked-by: 06,09

## Question
How do we run agent shell commands on Windows, macOS and Linux?

## To decide
- Pipe mode, not a PTY, for agent commands (KB)
- Reuse or port the opencode-pty Rust crate (MIT, keep attribution)
- Command scanner → per-command permission resources
- Timeout → background
- Windows Job Object
- PowerShell vs bash on Windows

## References
- OpenCode ch.09: [09-tools.md](../knowledge/opencode/09-tools.md) — read TL;DR, Architecture, Genie takeaways
- OpenCode ch.16: [16-pty-process-daemon.md](../knowledge/opencode/16-pty-process-daemon.md) — read TL;DR, Architecture, Genie takeaways

## Resolution
Resolved 2026-11-16. User decisions are marked **(user)**; the rest are defaults from KB ch.09/16 and D20/D25.

### Behaviour: like Claude Code (user confirmed after discussion)
- The agent's `shell` tool runs each command in a **hidden shell process** (not the user's visible terminal) in the project directory and captures the output for the model; the user sees a summary + expandable output in the transcript.
- **`cd` persists between commands; other shell state (variables, functions, aliases) does not.** Each call starts a fresh shell in the last cwd.
- **Timeout:** 2 min by default (the model can request up to 10 min); then the command **moves to the background** instead of being killed. The model is told "running in background, you'll be notified" and **not to poll**; completion arrives as a synthetic message (D20 inbox). `run_in_background: true` starts it in the background immediately.
- **Stop / interrupt / session end** kills the command **and every child it started** (Windows Job Object, Unix process group).
- Output: combined stdout+stderr to a file; the model gets a bounded tail + head with a path/ref to page the rest (D32 spill).
- Errors split: "couldn't start → safe to retry" vs "may have run → never auto-retry".

### Windows shell (user): PowerShell default, Git Bash optional
- Default order: `pwsh` → Windows PowerShell 5.1. Setting `shell: "bash"` (user or project) switches to Git Bash when installed. The tool description names the active shell so the model writes the right syntax. macOS/Linux: the user's login shell (zsh/bash) in non-interactive mode, zsh with `+o nomatch`.

### Process runner (user): migrate `opencode-pty` into Genie
- Create **`genie-process`** in our repo **from the `opencode-pty` source** (MIT): copy and adapt it into Genie's codebase, with Genie naming, our code style, and **MIT attribution kept** (file headers + `LICENSE` notices for opencode-pty and the Ghostty parts, C3/35 item 4). It is not taken as an external dependency.
- **Used as a library inside the daemon** (D20); no separate PTY daemon process, since the shared daemon already gives restart survival and client sharing.
- **Pipe mode first** (agent commands). **PTY mode** (portable-pty + libghostty-vt) goes behind a Cargo feature for the later desktop terminal pane; it needs Zig in CI, only when the feature is turned on.
- **Fixes to apply while migrating** (from the KB): constant-time token compare; full process-tree kill for PTYs (process group / Job Object); the Linux blocked-writer leak (non-blocking master or writer timeout); a **spawn hook** so the v2 sandbox (D44) can wrap `program/args`.

### Permissions (default; D25)
- A **command scanner** (our own, design from OpenCode) splits compound commands (`&&`, `||`, `;`, pipes, subshells) into sub-commands; each is checked against `Bash(...)`/`PowerShell(...)` rules. **Unparseable commands fail closed (ask).** "Always allow" saves a prefix from an arity table (e.g. `git commit:*`, `npm run test:*`).
- Read-only commands (e.g. `ls`, `git status`, `Get-ChildItem`) can be auto-allowed by a built-in list in `default` mode; everything else asks. `plan` mode: no shell (D25).
- The working directory must stay inside the workspace + `additionalDirectories`; `cd` outside asks.

### Amendment (D55, 2026-11-17)
- The interactive **Terminal tab** ships with the Code tab: PTY mode of `genie-process` (portable-pty + libghostty-vt) + xterm.js in the webview. The PTY feature and Zig in CI move forward accordingly. Reattach after reload uses the ported checkpoint + replay-offset model (KB ch.16).
