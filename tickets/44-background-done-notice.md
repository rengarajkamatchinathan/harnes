# 44 — "Background command finished" shows as a user bubble with the whole log

type: bug · status: fixed (uncommitted) · claimed-by: — · blocked-by: — · related: 07, 43

## Problem
After the ATG dev server ran as a background job (ticket 43) and was stopped, Genie queued its `background_done` notice. In the chat it appeared as:
1. **a user bubble** with raw `<system-update>` tags, as if the user had typed it;
2. **the job's whole output**, because the template inlined `job.output` whenever it fitted 800 lines / 40 KB;
3. **raw terminal colour codes** (`␛[3m…`), since shell output was never stripped.

The model also received all of that as context.

## Fix
- [x] `genie-tools`: `strip_ansi` (CSI, OSC, two-character escapes) is applied to every shell result, background "output so far" and job output (`output.rs`, `shell/tool.rs`).
- [x] `background_done` notice (`prompts/hidden/background_done.md`, `runtime/host.rs::background_done_text`): command, "exit code N" / "stopped by Genie" / "terminated by a signal", a ref to the full output when spilled, and only the **last 20 non-empty lines** (300 chars each).
- [x] Protocol: `Message.source` (optional, additive; `synthetic` for Genie-written user-role messages) in `session.get`, matching `message.admitted.source`.
- [x] Desktop: synthetic messages render as a one-line notice with "Show details" (colour codes stripped client-side too, so notices stored earlier render cleanly) instead of a user bubble (`Transcript.tsx` `Notice`). The CLI transcript labels them `genie (notice)`.

Tests: `strip_ansi_removes_colours_and_osc`, `background_done_quotes_only_the_last_lines`, `background_done_without_output_or_ref`, golden `background_done`.

## Resolution
Fixed. Restart the daemon to pick it up.
