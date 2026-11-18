# 43 — A dev server started in the foreground blocks the turn for up to 10 min

type: bug · status: fixed (uncommitted; CLI key open) · claimed-by: — · blocked-by: — · related: 07, 42

## Problem
Asked to "start the appln", `gpt-5.2-atg` called `shell` with `bun run dev:app` and `timeout_ms: 600000` instead of `run_in_background: true`. The app came up (:3000, :1420), but a dev server never exits, so the call blocked the turn until the 10-minute timeout moved it to the background. The UI showed "Running a command" the whole time. The only way out was stop, and stop kills the app.

## Done
- [x] The `shell` description and its `timeout_ms` / `run_in_background` field docs say servers, watchers, `npm/bun run dev` and `tauri dev` must use `run_in_background`, never a long timeout (`genie-tools/src/shell/tool.rs`).
- [x] GPT tuning line: start servers with `run_in_background: true`, then check they came up (`genie-core/prompts/tuning/gpt.md`, golden `code_plan_gpt`).

## To do
- [x] **Move to background** from the desktop. A "Move to background" link sits on a running `shell` row (`Transcript.tsx`) and calls the new `session.background_tool {session_id, call_id}` (lease-gated, additive; schema check passes). The engine registers a per-call `detach` token (`TurnCtx::detach_token`, `SessionHandle::background_tool`), and `shell` hands the child to `BackgroundJobs`. The model is told "The user moved…".
- [ ] The same in the CLI (a key such as Ctrl+B) through `session.background_tool`.
- [x] **Harness safety net, independent of the model.** `shell` now waits in 250 ms slices. Every 1.5 s (`ShellConfig::server_probe`) it checks whether the process tree listens on a TCP port (`genie-process/src/ports.rs`: `lsof -g <pgid>` on macOS, `/proc` on Linux, job process ids + `netstat -ano` on Windows). If it does, the command moves to the background at once, with the ports and a reminder to use `run_in_background`.
- [ ] Possibly cap a model-chosen `timeout_ms` for a command whose output has gone quiet. Probably unnecessary now.

Tests: `a_listening_server_moves_to_the_background`, `detach_moves_a_running_command_to_the_background`, `quick_commands_still_return_their_output` (genie-tools/tests/shell.rs) and parser unit tests in `ports.rs`. Checks: clippy on macOS plus Windows and Linux targets for genie-process; desktop typecheck and vitest.

## Resolution
Fixed for the desktop and the daemon. The CLI key remains open.
