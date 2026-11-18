# 42 — Compaction loop drops the user's newest request

type: bug · status: fixed (uncommitted) · claimed-by: — · blocked-by: — · related: 04, 11, 16

## Problem
In a Code session on `azure-foundry/gpt-5.2-atg`, the user asked "start the appln". The model ran `bun install`, then Genie compacted, lost the request and described the repo again instead of running `bun tauri dev`.

Seen in session `ses_01m7ndr91jz5xvr5rv1zxqwgeb` (2026-11-18 00:25 local): 5 compactions in a few turns, with only 5–16k tokens of context.

## Root causes
1. **8k fallback window for an unlisted base model.** `gpt-5.2` is not in the bundled catalog, so `azure_deployment` returned `DEFAULT_CONTEXT = 8_192` (`genie-catalog/src/discovery.rs`).
2. **Threshold collapses to 0 on small windows.** `compaction_threshold = window − max(15%, 16_000)` gives `8192 − 16000 → 0`, so every step compacts (`genie-core/src/runtime/context.rs`).
3. **Compaction can run on back-to-back steps.** The guard only blocked a second compaction within the same step (`genie-core/src/runtime/turn.rs`).
4. **The newest user message is not preserved.** The kept tail is chosen by a token budget, counting back from the newest message. Mid-turn, the newest message is a tool result (`bun install` here). When it alone exceeds the budget, nothing is kept and the **whole turn**, including the request, is summarised. The summary's *Goal* stayed "Describe the repository", so the model went back to that.

## Fix status
- [x] 1: unlisted `gpt-5.x` deployments borrow the closest listed sibling's limits (`family_sibling`). Committed.
- [x] 2: the default reserve is capped at `window / 4`. Committed.
- [x] 3: no compaction on back-to-back steps; real overflow still compacts. Committed.
- [x] 4: `compact_inner` (`genie-core/src/runtime/turn.rs`) keeps the tail from the newest user message when it fits in half the threshold. Otherwise the summary instruction quotes the message (`compaction_prompt_with`, `prompts/hidden/compaction.md`) and asks for it as the *Goal*. Tests: `mid_turn_compaction_keeps_the_users_request`, `oversized_turn_tail_quotes_the_request_in_the_summary_prompt` (both fail without the fix); golden `compaction_request`. Manual `/compact` is unchanged.
- [ ] Restart the running daemon after rebuilding. The pid that ran this session was started before fixes 1–3.

## Notes
- Shell/PTY is not the cause. `shell` already supports `run_in_background` and moves a command to the background after 2 min. Background jobs are wired in `genie-daemon/src/lib.rs`.
- `bun tauri dev` in the ATG repo starts cleanly by hand (server on :3000, Vite on :1420, desktop window opens).

## Resolution
Fixes 1–4 are in. Restart the daemon to pick them up.
