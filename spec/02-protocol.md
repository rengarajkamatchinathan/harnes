# 02 — The Genie protocol (client ↔ daemon)

Implements D20, D22, D25 (asks), D30 (elicitation/sampling), D51 (stream-json), D56 (screencast transport); tickets 01, 02, 25. Background: KB ch.13.

## 1. Transport and framing

- **JSON-RPC 2.0**, one JSON object per line (NDJSON, `\n`-terminated, UTF-8, no embedded newlines), **bidirectional** on the D20 socket/pipe (D22).
- Clients send requests and notifications. The daemon sends responses, notifications (`event`, `$/…`) and **requests to the client** (LSP/MCP style), e.g. `permission.ask`.
- Max line size: 16 MiB. Larger payloads (attachments, blobs) use chunked methods (§6.9).
- **Desktop:** the webview can't open a socket. The Tauri Rust side holds a `genie-client` connection and relays typed messages to React over IPC Channels, batching deltas every ~8 ms (D22, D57).
- **In-memory transport** in `genie-testkit` carries the same NDJSON lines for contract tests (t02).
- The same event messages are written by `genie -p --output-format stream-json` (D22, D51).
- The protocol is **internal in v1**: versioned and schema-generated, but only documented and promised stable from v2 (D22).

## 2. Handshake

```ts
// client → daemon, must be the first request
type InitializeParams = {
  protocol: string;            // semver of genie-protocol, e.g. "1.3.0"
  client_kind: "desktop" | "cli" | "headless";
  client_version: string;      // build version
  client_instance_id: string;  // stable per app install/window; used for lease reclaim (§5)
  token: string;               // contents of daemon.token
  capabilities: {
    asks: AskKind[];           // which ask kinds this client can render (§7)
    notifications: boolean;    // can show OS notifications for the daemon
    screencast?: boolean; terminal?: boolean;
  };
};
type InitializeResult = {
  engine_version: string; build: string; channel: "stable" | "beta" | "dev";
  protocol: string;
  capabilities: { features: string[] };   // e.g. "browser", "terminal", "rag"
  restart_pending?: boolean;              // version-skew path (01 §4.2)
  locked?: { reason: "passphrase_required" };  // secrets file not yet unlocked (D26)
};
```

- Wrong token → error `unauthorized`, connection closed.
- Major mismatch → `protocol_mismatch` and the D20 skew path (01 §4.2).
- `locked`: if the secrets store is the passphrase file and not yet unlocked, the client must call `secrets.unlock {passphrase}` before anything that needs the DB key (D26).

## 3. Subscriptions and resume

### 3.1 Session stream

```ts
// request
"session.subscribe": { session_id: string; after_seq?: number }   // omit = from start of retained log
// result
{ head_seq: number; driver?: ClientRef; status: SessionStatus }
// then notifications, in order:
"event": SessionEventEnvelope          // replayed durable events with seq > after_seq
"session.synced": { session_id; seq }  // replay done; live from here
"event": SessionEventEnvelope          // live durable + ephemeral
```

- Every **durable** event carries the session's next `seq` (gap-free, starting at 1) (D22).
- **Ephemeral** events carry `ref_seq`: the `seq` of the durable event that opened the item they belong to (e.g. the `part.started`). Clients drop them once the matching `*.ended` durable event arrives (t02).
- **No lossy streams.** Each subscription has a bounded outbound queue. On overflow the daemon drops that subscription's queue and sends `session.resync_required {session_id, from_seq}`. The client re-subscribes with `after_seq = from_seq` (t02).
- If `after_seq` is older than the retained log (§9), the daemon sends `resync_required {from_seq: 0, snapshot: true}` and the client reloads the session with `session.get` before subscribing from `head_seq`.

**Spec assumption (SA-10):** per-subscription queue limit of 8 MiB or 20,000 messages, whichever comes first. Reason: t02 requires a bound but gives none; 8 MiB holds several seconds of a 1,000 tok/s stream plus frames.

### 3.2 Workspace feed

`workspace.subscribe {}` → a snapshot (`workspace.snapshot`) then deltas: `session.listed` (created/updated/deleted, title, status, pinned, archived), `config.changed`, `policy.changed`, `mcp.status`, `catalog.updated`, `provider.status`, `notification`, `daemon.restarting`. The workspace feed has no `seq`; on reconnect the client takes a fresh snapshot (t02 "global feed").

### 3.3 Envelope

```ts
type SessionEventEnvelope = {
  session_id: string;
  seq?: number;            // durable only
  ref_seq?: number;        // ephemeral only
  ts: string;              // RFC 3339
  turn_id?: string; step?: number; subagent_of?: string;
  event: SessionEvent;     // discriminated by "type"
};
```

## 4. Session events

### 4.1 Durable events (stored, replayed)

| type | payload (main fields) | Notes |
|---|---|---|
| `session.created` | `kind, project_id?, workspace_id?, model, effort?, mode, agent?, parent_session_id?` | |
| `session.updated` | changed fields: `title, model, effort, mode, pinned, archived, active_leaf_id` | Title from the small model (D27). |
| `message.admitted` | `message_id, parent_id, role:"user", parts, delivery, source:"user"|"synthetic"` | From the inbox. |
| `inbox.changed` | `items:[{id, delivery, preview}]` | Queue shown above the composer. |
| `turn.started` | `turn_id, message_id` | Execution claim written in the same tx. |
| `step.started` | `step, model, route` | |
| `part.started` | `message_id, part_id, kind` | Opens a streamed part. |
| `part.ended` | `part_id, part` (full canonical part) | Text/reasoning/tool-input final value. |
| `tool.started` | `call_id, name, resource?` | After authorization. |
| `tool.ended` | `call_id, outcome:"ok"|"error"|"declined"|"blocked"|"cancelled", result_part_id, spill_ref?` | |
| `permission.asked` | `ask: Ask` (§7) | Also sent as a client request. |
| `permission.resolved` | `ask_id, decision, by: ClientRef|"rule"|"hook"|"policy", saved_rule?` | Audit trail (t13). |
| `step.ended` | `finish, usage, cost` | |
| `retry.scheduled` | `attempt, after_ms, error_class` | KB ch.03 keeps it durable. |
| `turn.settled` | `turn_id, status:"completed"|"failed"|"cancelled", error?` | Claim released. |
| `turn.interrupted` | `turn_id, reason:"daemon_restart"|"crash"` | Shows Resume. |
| `session.compacted` | `compaction_message_id, trigger, tokens_before, tokens_after, native:boolean` | D33. |
| `artifact.versioned` | `artifact_id, n, message_id, type, title` | D28. |
| `memory.changed` | `memory_id, op, scope` | "Memory updated" chip (D31). |
| `subagent.started` / `subagent.ended` | `child_session_id, agent, description` / `status, summary, cost` | D47. |
| `checkpoint.created` | `step_ref, before_tree, after_tree, skipped:[path]` | D49. |
| `branch.switched` | `active_leaf_id` | D24. |
| `lease.changed` | `driver?: ClientRef` | §5. |
| `browser.action` | `tab_id, tool, origin, decision, status, thumbnail_ref?` | D21 audit log. |
| `hook.ran` | `event, hook_id, decision?, duration_ms, error?` | D48. |
| `session.error` | `code, message, link?` | Non-turn errors. |

### 4.2 Ephemeral events (live only)

| type | payload |
|---|---|
| `part.delta` | `part_id, text?` (text/reasoning) or `json?` (tool input fragment) |
| `tool.progress` | `call_id, message?, fraction?, output_tail?` (shell tail, MCP progress) |
| `usage.updated` | running totals for the session and the context meter (D32) |
| `status` | `"thinking"|"retrying"|"compacting"|"interrupting"|"waiting_for_children"` |
| `browser.frame` | screencast frame (§6.6) |
| `terminal.output` | terminal bytes (§6.7) |

## 5. Driver lease

Implements D20 "hand-off" and D22.

- A session has at most one **driver**: a `ClientRef {client_instance_id, client_kind, label}`.
- Mutating commands (`session.prompt`, `interrupt`, `resume`, `update` of mode/model, `checkpoint.rewind`, `browser.takeover`, …) from a non-driver are rejected with `not_driver`.
- **Acquire:** the first mutating command on a session with no driver acquires the lease. `session.take_over` acquires it from another client; the old driver gets `lease.lost` (a notification) and a durable `lease.changed` event, and turns read-only with a **Take over** button.
- `session.release` gives it up. Closing a window releases its leases.
- **Reconnect:** if the driver's connection drops, the lease stays bound to its `client_instance_id` for 60 s, so a reconnecting client keeps it. After that the lease is vacant.
- **Answering asks** is not lease-gated: any authorised client may answer a pending ask (D22 "answer from the desktop, `genie approvals` or any attached client", D51). This avoids a stuck headless run.
- Read-only operations (subscribe, get, export, search) never need the lease.

**Spec assumption (SA-11):** lease acquisition on first mutating command, the 60 s reconnect grace, and asks being answerable by any client. Reason: t02 defines `take_over`/`release` but not the vacant and reconnect cases.

## 6. Methods

Names follow `area.verb`. Results are omitted where they are `{}`. All ids are ULID strings with a type prefix (`ses_`, `msg_`, `prt_`, `ask_`, `art_`, …).

**Spec assumption (SA-08):** the method set below beyond the commands named in t02 (`session.create | prompt | interrupt | resume | take_over | release | fork | revert`, `permission.respond`, `permission.pending`, `config.*`, `provider.*`, `mcp.*`, `browser.*`). Reason: t02 says the other areas are added "as subsystems need them"; this spec fixes names so codegen and UI work can start.

### 6.1 Daemon and secrets

| method | params → result |
|---|---|
| `daemon.status` | → `{version, channel, uptime_s, pid, socket, sessions_running, clients:[ClientRef]}` |
| `daemon.restart` | `{stop_running?: boolean}` (Developer section, D34) |
| `secrets.unlock` | `{passphrase}` → `{ok}` (D26 file backend) |
| `doctor.run` | → `{checks:[{id, status, detail, fix_link?}]}` |
| `diagnostics.collect` | `{include_logs: boolean}` → `{text}` sanitised, no prompt content (D34) |

### 6.2 Sessions

```ts
"session.list":   { kind?: "chat"|"code"; project_id?; workspace_id?; archived?: boolean; query?: string; cursor?; limit? }
               → { items: SessionSummary[]; next_cursor? }
"session.get":    { session_id; leaf_id? } → { session: SessionInfo; path: Message[] /* root→leaf */; siblings: Record<msg_id, msg_id[]> }
"session.create": { kind; project_id?; workspace_path?; model; effort?; agent?; mode?; incognito?: boolean;
                    worktree?: "auto"|"new"|"none" } → { session_id }
"session.update": { session_id; title?; pinned?; archived?; model?; effort?; mode?; active_leaf_id? }
"session.delete": { session_id }               // cascades (04 §8)
"session.prompt": { session_id; parts: InputPart[]; delivery: "steer"|"queue"; parent_id?: string;
                    client_msg_id: string /* idempotency */ } → { message_id; inbox_id }
"session.regenerate": { session_id; message_id }       // sibling assistant message
"session.interrupt":  { session_id; stop_subagents?: boolean }
"session.resume":     { session_id }                   // after turn.interrupted
"session.take_over":  { session_id };  "session.release": { session_id }
"session.fork":       { session_id; message_id } → { session_id }   // explicit "new chat from here"
"session.compact":    { session_id; focus?: string }   // Code/CLI /compact (D33)
"session.inbox.remove": { session_id; inbox_id }
"session.export":     { session_id; format: "json"|"markdown"; sanitize?: boolean } → { blob_id | text }
"session.search":     { query; scope?: "all"|"project"; project_id? } → { hits:[{session_id, message_id, snippet}] }
```

- **Edit = prompt with `parent_id`** set to the edited message's parent: it creates a sibling branch (D24).
- `client_msg_id` makes `prompt` idempotent across reconnects.
- `InputPart = {type:"text", text} | {type:"attachment", blob_id, name, mime} | {type:"resource", server, uri} | {type:"skill", id} | {type:"file_ref", path}`.

### 6.3 Asks (permissions, elicitation, sampling, questions)

See §7. Methods: `permission.pending {session_id?}` → `{asks: Ask[]}`; `permission.respond {ask_id, decision, scope?, message?, data?}`. Daemon → client request: `permission.ask {ask}` → `{decision, scope?, message?, data?}`.

### 6.4 Config, policy and trust

| method | params → result |
|---|---|
| `config.get` | `{keys?: string[], scope?: Scope, cwd?}` → `{values, sources: Record<key, Scope|"policy">, locked: string[]}` |
| `config.set` | `{key, value, scope: "user"|"project"|"local", cwd?}` (D23) |
| `config.schema` | → JSON Schema + UI metadata (D23, D34) |
| `config.diagnostics` | `{cwd?}` → `{problems: Diagnostic[]}` |
| `policy.effective` | → `{values, source_per_key}` (D38) |
| `trust.get` / `trust.set` | `{path}` / `{path, decision:"trust"|"once"|"deny"|"revoke"}` (D43) |
| `trust.pending_definitions` | `{path}` → `{items:[{kind:"mcp"|"hook"|"setup", name, hash, diff}]}` |
| `trust.approve_definition` | `{path, kind, name, hash, decision:"approve"|"skip"}` |

Notification: `config.changed {keys, scope}`.

### 6.5 Providers, catalog and usage

| method | params → result |
|---|---|
| `provider.list` | → `{providers:[{id, kind, status, credentials:[CredentialInfo], endpoint?}]}` |
| `provider.credential.add` | `{provider, label, kind:"key"|"oauth"|"external", secret?}` → `{credential_id}` |
| `provider.credential.remove` / `.activate` | `{credential_id}` |
| `provider.login.start` | `{provider, method:"pkce"|"device_code"}` → `{login_id, auth_url?, user_code?, verification_uri?}` (D26) |
| `provider.login.status` | `{login_id}` → `{state:"pending"|"done"|"failed", error?}` |
| `provider.test` | `{provider}` → `{ok, latency_ms, error?}` |
| `model.list` | `{provider?}` → `{models: ModelInfo[]}` (catalog + discovery + overrides, D27) |
| `catalog.status` / `catalog.refresh` | → `{source:"bundled"|"cached"|"fresh", fetched_at, etag}` |
| `usage.query` | `{from, to, group_by:["day"|"provider"|"model"|"project"]}` → rows incl. cache + compaction lines (D34) |
| `usage.budget.set` | `{monthly_usd?: number}` (warnings at 80/100%, no hard stop) |

### 6.6 Browser

| method | params → result |
|---|---|
| `browser.status` | → `{installed:[{kind:"chrome"|"edge"|"chromium", version, path}], running, profile_path}` |
| `browser.screencast.start` | `{session_id, tab_id?, max_width, max_height, quality}` → `{stream_id}` |
| `browser.screencast.ack` | `{stream_id, frame_seq}` (flow control; CDP `screencastFrameAck`) |
| `browser.screencast.stop` | `{stream_id}` |
| `browser.takeover` / `browser.handback` / `browser.stop` | `{session_id}` (D21) |
| `browser.input` | `{session_id, tab_id, event: MouseEvent|KeyEvent|WheelEvent|ImeEvent|InsertText}` (D56) |
| `browser.tabs` | `{session_id}` → `{tabs:[{tab_id, url, title, active}]}`; `browser.tab.activate` |
| `browser.popout` | `{session_id}`: show the real Chrome/Edge window |
| `browser.sites.list` / `browser.sites.set` | per-site states `blocked|ask|read|act` |
| `browser.data.clear` | `{what:"cookies"|"all"}` |

`browser.frame` (ephemeral): `{stream_id, frame_seq, tab_id, jpeg_b64, w, h, device_scale, ts}`. Frames are base64 in NDJSON in v1; at ~60 KB per frame and 15 fps this is about 1.2 MB/s on a local socket, well within budget. The Tauri side decodes and hands an `ImageBitmap`-ready buffer to the webview.

### 6.7 Terminal (Code tab, D55)

`terminal.open {session_id, cwd?, shell?, cols, rows}` → `{terminal_id}` · `terminal.attach {terminal_id, offset}` → `{truncated, checkpoint_b64?, replay_b64, end_offset}` · `terminal.input {terminal_id, data_b64}` · `terminal.resize {terminal_id, cols, rows}` · `terminal.close {terminal_id}`. Ephemeral `terminal.output {terminal_id, start, end, data_b64}`; durable-free `terminal.exited {terminal_id, code}` on the workspace feed. Reattach uses the replay ring + VT checkpoint model of KB ch.16.

### 6.8 MCP, skills and directories

| method | params → result |
|---|---|
| `mcp.list` | `{cwd?}` → servers with `status: pending|connected|disabled|failed|needs_auth`, scope, tools count (D30) |
| `mcp.add` / `mcp.remove` | `{scope, name, config}` / `{scope, name}` |
| `mcp.set_enabled` | `{name, enabled, session_id?, tool?}` (per-chat "+" toggles) |
| `mcp.auth.start` | `{name}` → `{auth_url}` |
| `mcp.logs` | `{name, tail}` → `{lines}` |
| `mcp.tools` / `mcp.resources` / `mcp.prompts` | `{name}` → lists |
| `mcp.resource.read` | `{name, uri}` → `{blob_id, mime}` |
| `skill.list` / `skill.set_enabled` / `skill.upload` | `{blob_id}` for `.zip`/`.skill` upload (D37) |
| `directory.list` | `{kind:"connectors"|"skills", query?}` → signed-feed entries |
| `directory.install` | `{kind, id, version}` → returns the "what will run" manifest first; second call with `confirm: true` |

### 6.9 Projects, artifacts, memory, blobs

| method | params → result |
|---|---|
| `project.list/create/update/delete` | name, instructions, starred (t12) |
| `project.file.add/remove` | `{project_id, blob_id, name}` |
| `project.index.status` | → `{mode:"full"|"retrieval", files, tokens, capacity_pct, indexing}` (D32) |
| `artifact.list` | `{session_id, leaf_id?}` → artifacts with versions on the active path (D28) |
| `artifact.get` | `{artifact_id, n}` → `{type, title, content | blob_id}` |
| `memory.list/update/delete` | `{scope?}` (D31) |
| `memory.set_state` | `{state:"on"|"paused"}`; `memory.reset {scope?}` |
| `blob.put.begin/chunk/end` | chunked upload (1 MiB chunks) → `{blob_id, sha256}` |
| `blob.put.path` | `{path}` → `{blob_id}`: the daemon reads a local file (same user) |
| `blob.get` | `{blob_id, offset, len}` → `{data_b64}` |

### 6.10 Code mode (M5/M6)

| method | params → result |
|---|---|
| `checkpoint.list` | `{session_id}` → per message `{message_id, files_changed}` (D49) |
| `checkpoint.preview` | `{session_id, message_id, restore:"files"|"conversation"|"both"}` → `{files:[{path, change}], external_changes:[path], skipped:[path]}` |
| `checkpoint.rewind` / `checkpoint.undo` | stage / clear the staged rewind |
| `git.status` / `git.diff` | `{session_id, path?}` (Changes panel, D54) |
| `git.commit` | `{session_id, message, paths?, then?: "push"|"pr"}` |
| `worktree.finish` | `{session_id, action:"merge"|"pr"|"keep"|"discard"}` (D50) |
| `files.tree` / `files.read` | `{session_id, path}` (Files panel) |
| `import.scan` / `import.preview` / `import.apply` | D53 (07 §5) |
| `export.all` | `{path, include_blobs}` (D24) |

## 7. Asks

All decisions the user must make share one durable shape (D22, D25, D30, D21, D43).

```ts
type AskKind = "permission" | "elicitation" | "sampling" | "question" | "browser_confirm" | "definition_hash";
type Ask = {
  id: string; session_id: string; turn_id?: string; kind: AskKind;
  subagent?: { session_id: string; agent: string };      // label prompts from children (D47)
  created_at: string;
  // permission
  tool?: string; resource?: string; preview?: Preview;    // diff, command, URL, MCP args
  save_suggestions?: { rule: string; scope: "session"|"local"|"user" }[];
  annotations?: { read_only_hint?: boolean; destructive_hint?: boolean };  // MCP info only (t13)
  // elicitation (MCP) / question (ask_user)
  form?: JsonSchema; sensitive_fields?: string[];
  // sampling (MCP)
  sampling?: { server: string; messages_preview: string; model: string; est_cost_usd: number; budget_left_tokens: number };
  // browser_confirm
  browser?: { origin: string; action: string; summary: string; screenshot_blob?: string };
};
type Decision = "once" | "always" | "reject" | "submit" | "cancel";
```

Flow:
1. The engine writes the ask to `permission_pending`, publishes durable `permission.asked`, and sends a `permission.ask` request to the driver if it can render that kind.
2. **No driver attached:** the session goes to `awaiting_ask` with no client → **paused**. The daemon asks the desktop app (if connected to the workspace feed) to show an OS notification, else uses the native notifier ("Genie needs your approval") (D22).
3. Any authorised client may answer with `permission.respond`, or by replying to the request.
4. The ask never times out on its own (D22). Headless `--ask-timeout` and `--on-ask deny` are client-side policies of the headless runner (D51).
5. `reject` with `message` sends the message to the model as feedback; `reject` without one ends that tool call. There is no reject cascade (t13).
6. After an `always`, other pending asks are re-evaluated and released if they now match (t13).
7. If the driver disconnects while a `permission.ask` request is open, the daemon sends `$/cancelRequest`; the ask stays pending.

## 8. Errors

JSON-RPC error objects with a typed `data.kind` (t02). Internal defects become `internal_error` and are never shown to the model.

| code | data.kind | Meaning |
|---|---|---|
| -32700 / -32600 / -32601 / -32602 | (standard) | parse, invalid request, method not found, invalid params |
| -32001 | `unauthorized` | bad or missing token |
| -32002 | `protocol_mismatch` | `{daemon_protocol, client_protocol}` |
| -32003 | `not_driver` | `{driver: ClientRef}` |
| -32004 | `not_found` | session, message, ask, blob… |
| -32005 | `conflict` | e.g. ask already answered, idempotency clash |
| -32006 | `locked` | secrets not unlocked |
| -32007 | `policy_blocked` | `{rule, source:"policy"}` |
| -32008 | `permission_denied` | deny rule |
| -32009 | `untrusted_workspace` | operation needs trust (D43) |
| -32010 | `provider_error` | `{class, status?, request_id?, retry_after_ms?}` |
| -32011 | `busy` | e.g. daemon draining |
| -32012 | `invalid_state` | e.g. resume when not interrupted |
| -32013 | `cancelled` | |
| -32099 | `internal_error` | `{incident_id}`; details only in logs |

**Spec assumption (SA-09):** these numeric codes. Reason: t02 fixes `data.kind` values but not codes; one code per kind lets non-Rust tools switch on the code.

## 9. Retention of the event log

Durable events are the source for resume, but the message/part tables are canonical (t12). Once a session has been idle for 30 days, events up to its last `turn.settled` may be pruned; the session keeps a `log_floor_seq`. A subscribe below the floor gets `resync_required {snapshot: true}` (§3.1).

**Spec assumption (SA-12):** the 30-day pruning rule. Reason: t12 allows compacting old events "once a session is idle" without a threshold.

## 10. Versioning

- `genie-protocol` has its own semver in the handshake (D22).
  - **Patch:** docs, no wire change.
  - **Minor:** additive only: new methods, new optional fields, new event types, new enum values. Clients must ignore unknown events and fields (`#[serde(other)]` / default branches).
  - **Major:** anything else. Refused at handshake (§2).
- Capabilities in the handshake gate optional features, so a minor bump never needs a client check of the version number.
- stream-json output for `genie -p` carries `"genie_protocol": "<semver>"` in its first `system/init` line (D51).

## 11. Codegen and drift check

Pipeline (t02, t38):
1. Rust types in `genie-protocol` (serde + schemars) are the only source.
2. `cargo xtask schema` writes `packages/protocol-ts/schema/*.json` (JSON Schema 2020-12) and a method table (`methods.json`: name, params type, result type, direction).
3. A small generator (Rust, in `xtask`) emits TypeScript types and a typed client stub: `client.session.prompt(params): Promise<Result>` and an `onEvent` discriminated union.
4. The config schema (D23) and the Settings metadata (D34) follow the same path.

CI checks:
- **Drift:** regenerate and `git diff --exit-code` on `packages/protocol-ts` and the config schema.
- **No internal deps** for `genie-protocol`.
- **Compatibility:** compare the new schema with the last released one (`schema-diff`); a breaking change without a major bump fails.
- **Contract tests:** golden NDJSON transcripts (subscribe → replay → synced → live, overflow → resync, take_over, ask with no client) run over the in-memory transport against the real core with fake providers.
