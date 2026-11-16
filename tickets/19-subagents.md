# 19 — Subagents and background jobs

type: wayfinder:grilling · status: closed · claimed-by: charting session (2026-11-16) · blocked-by: 18

## Question
How do subagents run and report back?

## To decide
- Task tool, child sessions, shared cache key
- Parallelism, cancellation, cost accounting

## References
- OpenCode ch.03: [03-session-and-agent-loop.md](../knowledge/opencode/03-session-and-agent-loop.md) — read TL;DR, Architecture, Genie takeaways
- OpenCode ch.10: [10-agents-skills-and-commands.md](../knowledge/opencode/10-agents-skills-and-commands.md) — read TL;DR, Architecture, Genie takeaways

## Resolution
Resolved 2026-11-16. User decisions are marked **(user)**; the rest are defaults from KB ch.03/10 and D20/D24/D46.

### Run style (user): always background (OpenCode)
- `task {agent, description, prompt, model?, session_id?}` creates a **child session** (D24 `parent_session_id`) and returns immediately with `{status: "running", session_id}`.
- The main agent keeps working. When a child goes idle, its **final assistant text is delivered to the parent as a steered synthetic `<subagent …>` message** (D20 inbox), which wakes the parent if it's idle. The report is written in the same transaction as the child's terminal event (KB outbox pattern), so it survives crashes.
- **Follow-up:** `task` with an existing `session_id` sends a new prompt to that child (keeps its context).
- **No polling:** the tool result and description tell the model results arrive automatically. A `task_wait {session_ids?}` tool lets the parent explicitly block until named children finish (useful before a final answer).
- **Turn completion:** a parent turn isn't "done" while it has running children, unless the model ends it explicitly. **Headless `-p`** (→ 25) exits only after all children have reported and the parent's final turn has ended (or on a timeout), so scripted runs stay deterministic.

### Nesting (user): depth 1, configurable
- Only the main agent can call `task` (subagents don't get the tool). Setting `subagents.maxDepth` (1–3, default 1) allows deeper nesting; policy can cap it (D38).

### Limits and cost (default)
- Max **concurrent** children per session (default 4; extra calls queue). Per-child `maxSteps` from the agent definition (D46).
- **Shared prompt-cache key** with the root session (D32). Child usage/cost rolls up into the parent session and Usage & spend (D34), also shown per child.
- **Cancellation:** child tokens are children of the parent turn's token (D20). Interrupting the parent asks "Stop running subagents too?" (default yes); deleting the parent deletes children.
- **Permissions:** children inherit the parent's mode/rules, narrowed by the agent definition (D46). Permission prompts from a child appear in the parent's UI, labelled with the subagent name.

### UI (default)
- Desktop (Code) and CLI show each subagent as a collapsible card: name, description, status (running/done/failed), step count, cost; expand to view its transcript (read-only). Running children appear in the status line ("2 subagents running").
