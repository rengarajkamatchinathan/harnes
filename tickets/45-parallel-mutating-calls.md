# 45 — Dependent commands run in parallel: three approval prompts at once

type: bug · status: fixed (uncommitted) · claimed-by: — · blocked-by: — · related: 13, 43

## Problem
Asked to start the ATG app, `gpt-5.2-atg` sent three `shell` calls in one step that depend on each other: check port 1420 (`lsof … || true`), start Vite on 1420 (`--strictPort`), start Vite on 1421. Genie dispatched every call as soon as its input finished streaming, all in parallel. The user saw three approval prompts at once, and approving them would have raced two dev servers. The read-only `lsof` port check also asked for approval.

## Fix
- [x] **Mutating calls in one step run one at a time, in the model's order** (`genie-core/src/runtime/turn.rs` `dispatch`, `Run::serial`). Each waits for the previous one's verdict. Read-only and external calls still run in parallel; dispatch still starts as soon as the input is complete (D20).
- [x] **A decline skips the dependent calls after it.** When the user declines a mutating call, the later mutating calls in that step are not run and don't ask. Their result is "Not run: the user declined an earlier tool call in this step…", so the model adjusts instead of asking again. "Always" on the first call saves the rule, and the next calls run under it without asking.
- [x] **Read-only list** (`genie-permissions/src/shell.rs`): `lsof`, `netstat`, `ss` (not `-K`/`--kill`), `pgrep`, `printenv`; PowerShell `Get-NetTCPConnection`, `netstat`. `lsof` had been listed as a "not read-only" example in the table test, with no recorded reason.

Tests: `mutating_calls_in_one_step_run_one_at_a_time_in_order`, `a_declined_call_skips_the_mutating_calls_after_it`, `always_saves_the_rule_and_the_next_mutating_call_needs_no_ask`. The existing `always_saves_the_rule_and_releases_other_pending_asks` now uses an external-kind tool, where parallel asks still happen. Also `read_only_table` and `read_only_shell_stays_in_the_workspace` (port check in the workspace → allow).

## Notes
- `genie-daemon` `real_daemon_locked_phase_then_engine_with_store_key_in_secrets_file` timed out once under full-suite load and passes on its own. It looks flaky and is unrelated to this change.

## Resolution
Fixed. Restart the daemon to pick it up.
