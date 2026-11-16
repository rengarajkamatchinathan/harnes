# MCP client and the plugin system

> OpenCode @ 56625a3e1f · paths relative to opencode-reference/

## TL;DR

- **MCP config lives under `mcp.servers.<name>`**, with an optional global `mcp.timeout`. Each server is `local` (stdio) or `remote` (Streamable HTTP only, with no SSE fallback). Each can set `protocol` (`legacy` is the default, then `auto` and `2026-07-28`), `pool`, `codemode` and three timeouts: startup 30 s, catalog 30 s, execution 12 h.
- **MCP config reaches the engine through a built-in plugin.** `ConfigMcpPlugin` turns config documents into `mcp.transform(...)` edits. Any plugin can add, edit or remove servers the same way; the official Figma plugin is just an MCP server plus a set of skills.
- **Tool naming:** each tool is named `<server>_<tool>`, with every character outside `[a-zA-Z0-9_-]` replaced by `_`. That same string is the permission action.
  - By default, MCP tools do **not** appear as direct model tools. They sit behind OpenCode's single `execute` "Code Mode" tool, unless a server sets `codemode: false` or `execute` is denied.
- **Remote servers get OAuth automatically.** Each one is registered as an OAuth *integration* whose credential lives in the global store, keyed by `mcp_` plus sha1(name + NUL + url).
  - The login picks static client_id, then CIMD (`https://opencode.ai/oauth/opencode/client.json`), then dynamic client registration.
  - The callback is a loopback server on `callback_port`, else the port in `redirect_uri`, else an ephemeral port.
- **Plugins are in-process JS/TS modules.** The default export is `{id, effect(ctx)}` (Effect API) or `{id, setup(ctx)}` (Promise API, adapted to Effect).
  - `ctx` exposes ~25 domains. Most offer `transform(editor => …)`; five domains offer `hook(name, cb)`: `tool`, `session`, `permission`, `shell` and `aisdk`.
  - Hook callbacks mutate the event object in place, run sequentially in registration order, and only `tool.execute.before` may fail.
- **Nearly everything built in is a plugin.** That covers every tool (read/edit/shell/…), config loaders, providers, agents, plan mode, goal mode and more: 40-odd `pre` plugins and 19 `post` plugins. User and npm/git plugins load between the two groups, and config can switch built-ins off with `"-<id>"`.
- **Plugins share the host's `effect` and `@opencode/plugin`.** A Bun loader plugin serves both modules, and throws if a plugin tries to load its own copies from its `node_modules`.
- **Genie verdict:** port the MCP client design (pick-list #18) and keep the hook *event list* as reference input for ticket 20. Skip the in-process plugin ABI (#26), as already decided.

## Where it lives

| Path | Role | ~lines |
|---|---|---|
| `packages/schema/src/mcp.ts` | MCP config/status/resource schemas | 171 |
| `packages/schema/src/config/mcp.ts` | `mcp: {timeout, servers}` config key | 22 |
| `packages/core/src/mcp/index.ts` | `Mcp.Service`: server registry, lifecycle, elicitation, events, attach | 1211 |
| `packages/core/src/mcp/client.ts` | SDK wrapper: transports, version negotiation, retries, timeouts | 834 |
| `packages/core/src/mcp/connection.ts` | per-server connection state machine | 270 |
| `packages/core/src/mcp/pool.ts` | cross-directory connection sharing (`pool: true`) | 385 |
| `packages/core/src/mcp/stdio.ts` | stdio transport over the Location's process spawner | 227 |
| `packages/core/src/mcp/socket-client.ts` | inverted WebSocket transport for client-hosted servers | 86 |
| `packages/core/src/mcp/oauth.ts` | OAuth login, refresh, CIMD/DCR | 659 |
| `packages/core/src/mcp/instructions.ts` | server `instructions` → `<mcp_instructions>` system text | 100 |
| `packages/core/src/tool/mcp.ts` | registers MCP tools into the tool registry | 177 |
| `packages/core/src/config/plugin/mcp.ts` | config → MCP transform | 57 |
| `packages/core/src/plugin/mcp-codemode-defaults.ts` | per-vendor codemode/header defaults | 41 |
| `packages/cli/src/commands/handlers/mcp/*` | `opencode mcp list/add/auth/logout` | 284 |
| `packages/plugin/src/effect/*`, `promise/*` | public plugin API (Effect and Promise flavours) | ~2.6K |
| `packages/plugin/src/runtime*.ts`, `host.ts`, `source*.ts` | shared-module runtime, entrypoint resolution, hot reload | ~400 |
| `packages/core/src/plugin.ts` | plugin registry: activation, prefix diff, failure isolation | 320 |
| `packages/core/src/plugin/supervisor.ts` | combines internal/sdk/instance/package plugins, reload triggers | 248 |
| `packages/core/src/plugin/internal.ts` | built-in plugin list (pre/post) | 333 |
| `packages/core/src/plugin/module.ts` | loads npm/git/local plugin modules | 186 |
| `packages/core/src/plugin/host.ts` | builds `ctx` for each plugin | 751 |
| `packages/core/src/plugin/hooks.ts` | hook registry + `trigger` | 108 |
| `packages/core/src/rpc.ts`, `protocol/src/groups/rpc.ts` | plugin RPC, `POST /api/rpc/:rpcID/:method` | 288 + 30 |
| `packages/plugins/figma`, `packages/gui-plugins` | external official plugin; desktop GUI built-ins | — |

## Architecture

```mermaid
flowchart LR
  subgraph Config
    C[opencode.json(c) mcp.servers] --> CMP[ConfigMcpPlugin]
  end
  P[other plugins e.g. figma] -->|ctx.mcp.transform| MS
  CMP -->|mcp.transform| MS[Mcp.Service<br/>entries Map, reconcile]
  MS --> CONN[McpConnection / McpPool]
  CONN --> CL[McpClient.connect]
  CL -->|local| STDIO[McpStdio: spawn via Environment]
  CL -->|remote| HTTP[StreamableHTTPClientTransport + OAuth provider]
  CL -->|attached| WS[socket-client: app-opened WebSocket]
  MS -->|bus: mcp.tools.changed| MT[McpTool layer]
  MT -->|tools.transform add namespace=server| TR[Tool registry]
  TR -->|codemode !== false| CM[execute Code Mode tool]
  TR -->|codemode false| DIRECT[direct model tools]
  MS -->|prompts| CMD[CommandPlugin: server:prompt slash cmds]
  MS -->|resources| RES[list_mcp_resources / read_mcp_resource]
  MS -->|elicitation| FORM[Form service: global session]
```

```mermaid
flowchart TB
  SUP[PluginSupervisor.activate] --> INT[PluginInternal.list: pre + post]
  SUP --> SDK[SdkPlugins + InstancePlugins]
  SUP --> SRC[ConfigPluginSource.operations: plugins[] + plugin/ plugins/ dirs]
  SRC --> MOD[PluginModule.load: npm/git via Arborist, or local dir]
  SUP --> ORD["ordered = pre(+sdk+instance) ++ packages ++ post"]
  ORD --> REG[Plugin.activate: keep unchanged prefix, close + reload suffix]
  REG --> HOST[PluginHost.make → ctx]
  HOST --> HK[PluginHooks.register]
  HOST --> ST[State.transform on services]
```

## Key types and interfaces

| Name | file:line | Purpose |
|---|---|---|
| `Mcp.TimeoutConfig` | `packages/schema/src/mcp.ts:7` | `startup`, `catalog`, `execution` ms |
| `Mcp.Protocol` | `packages/schema/src/mcp.ts:20` | `"legacy" \| "auto" \| "2026-07-28"` |
| `Mcp.LocalConfig` | `packages/schema/src/mcp.ts:26` | `command[]`, `cwd`, `environment`, `disabled`, `codemode`, `timeout`, `protocol`, `pool` |
| `Mcp.OAuthConfig` | `packages/schema/src/mcp.ts:45` | `client_id/secret`, `scope`, `callback_port`, `redirect_uri`, `auth_server_metadata_url` |
| `Mcp.RemoteConfig` | `packages/schema/src/mcp.ts:57` | `url`, `headers`, `oauth \| false`, + same knobs |
| `Mcp.Status` | `packages/schema/src/mcp.ts:94` | connected/pending/disabled/failed/needs_auth |
| `ConfigMCP.Info` | `packages/schema/src/config/mcp.ts:19` | `{timeout, servers}` |
| `Mcp.Interface` | `packages/core/src/mcp/index.ts:160` | servers/add/connect/disconnect/remove/attach/tools/callTool/prompts/resources/subscribe… |
| `ServerEntry` | `packages/core/src/mcp/index.ts:103` | per-server config, status, startup latch, connection, integrationID |
| `McpClient.connect` | `packages/core/src/mcp/client.ts:302` | opens one connection over any transport |
| `McpTool.name` | `packages/core/src/tool/mcp.ts:24` | `<server>_<tool>` registry/permission name |
| `CLIENT_METADATA_URL` | `packages/core/src/mcp/oauth.ts:36` | CIMD client_id URL |
| Effect `Plugin` / `define` | `packages/plugin/src/effect/plugin.ts:62`, `:67` | `{id, effect(ctx) => Effect<void, never, Scope>}` |
| Effect `Context` | `packages/plugin/src/effect/plugin.ts:27` | the `ctx` surface |
| Promise `Plugin` | `packages/plugin/src/promise/plugin.ts:64` | `{id, setup(ctx) => Cleanup \| void}` |
| `Hooks` / `ModelHooks` / `Transform` | `packages/plugin/src/effect/registration.ts:12`, `:19`, `:27` | registration signatures; `ModelHooks` take `{providerID}` scoping |
| `SessionHooks` | `packages/plugin/src/effect/session.ts:201` | the 15 session hook names |
| `ToolHooks` / `ToolFailures` | `packages/plugin/src/effect/tool.ts:20`, `:49` | execute.before/after |
| `PluginHooks.Interface` | `packages/core/src/plugin/hooks.ts:42` | `register`, `trigger`, `has` |
| `Generation` slot / `Activation` | `packages/core/src/plugin.ts:281`, `:289` | registry bookkeeping |
| `ConfigPlugin.Plugins` | `packages/schema/src/config/plugin.ts:14` | `(string \| {package, options})[]` |

## How it works: walkthroughs

### 1. MCP config → connected server

1. `ConfigMcpPlugin` subscribes to `config.updated` and then reads `config.entries()` (`core/src/config/plugin/mcp.ts:13`, `:38`). Inside `mcp.transform` it merges every document's `mcp.timeout` in config order, collects `mcp.servers` (later documents win on a name), and calls `editor.set(name, {...server, timeout: {...global, ...server.timeout}})`. It skips names already set by an earlier transform (`:42-55`).
2. `Mcp.Service`'s state reconcile (`core/src/mcp/index.ts:820`) diffs the configured servers against `entries`. New or changed configs are replaced (`replaceServer`, `:695`); a `disabled` server gets status `disabled` (`:702`).
3. `makeEntry` (`:556`) decides whether the server is pooled. It must have `pool: true`, must not use `protocol: "legacy"`, and must be remote or running on the host (`:558-561`). Pooling forces `protocol: "auto"` when the protocol is unset (`:589`). Status events publish `mcp.status.changed`, and on connect it also publishes tools/resources/prompts changed (`:613-623`).
4. `McpClient.connect` (`core/src/mcp/client.ts:302`) branches by transport:
   - **local:** `McpStdio.make` spawns the command through the Location's `Environment`, so it can run remotely. If the command is `opencode`, it sets `BUN_BE_BUN=1` (`:465-476`).
   - **remote:** builds a `StreamableHTTPClientTransport` with headers, the OAuth `authProvider` and a logging fetch (`:486-491`). It appends `?codemode=false` unless `codemode: false` is set or the host is in a known-strict list: googleapis, zohomcp or loopback (`:483`, `:799`). On a 400/404 it retries once without that parameter.
   - **attached:** a transport the client app opened (WebSocket at `/api/experimental/client-mcp`, `protocol/src/groups/mcp.ts:10`).
5. Initialize declares `elicitation {form,url}` and `roots` capabilities, plus the events extension if the flag is on. Version negotiation maps `protocol` (`client.ts:350-370`, `:809`). Transient failures retry twice (250 ms, 1 s) within the startup deadline (`:336`, `:495`). Errors are classified as follows:
   - `UnauthorizedError` → `NeedsAuthError` (status `needs_auth`).
   - An unsupported protocol version → a message that lists the versions the server supports (`:770-780`).

### 2. Tool registration and a call

1. `McpTool.layer` takes `mcp.tools()` once. Inside `tools.transform` it adds each tool with `options.namespace = normalizedName(server)` and `codemode: tool.codemode !== false` (`core/src/tool/mcp.ts:49-60`).
   - Later `mcp.tools.changed` events feed a sliding PubSub debounced at 100 ms, which triggers `tools.reload()` (`:131-141`).
   - `flush` waits for startup with a grace of `STARTUP_GRACE_MS = 2000` per connecting server (`mcp/index.ts:127`, `:970`).
2. Effective name: `${namespace.replaceAll(".", "_")}_${normalizedName(tool)}` (`core/src/tool/runtime.ts:289-294`).
3. In `Tool.snapshot`, tools whose `codemode` isn't false go into the Code Mode inventory, which is reached through `execute`. If `execute` is wholly denied, they become direct tools instead (`core/src/tool.ts:263-268`).
4. `execute` calls `permission.assert({action: "<server>_<tool>", resources:["*"]})` and then `mcp.callTool` (`tool/mcp.ts:63-76`).
   - An attached client's tool with the same server name shadows the configured one (`mcp/index.ts:994`).
   - `isError` results become a `ToolFailure` carrying the joined text (`tool/mcp.ts:84`).
5. Result shaping (`tool/mcp.ts:94-101`, `:155-171`):
   - `structuredContent` wins.
   - Otherwise, if the server declared no `outputSchema` and the text looks like JSON (starts with `{` or `[`), it is parsed.
   - Media is inlined only if the read tool accepts that MIME type. Other textual blobs are inlined with a label. Binary blobs are saved through `ToolOutput.save` and replaced by a path note.
   - The general tool-output truncation (2,000 lines / 50 KiB, configurable via `tool_output`) applies to the result (`core/src/tool-output.ts:13-14`) *(inferred: exact application point for MCP results not traced)*.

### 3. Prompts, resources, instructions, elicitation, events

- **Prompts** become slash commands named `<server>:<prompt>`, sanitized (`core/src/plugin/command.ts:104`). Positional args map to the prompt's declared arguments (`:61-66`).
- **Resources** are reached through the built-in tools `list_mcp_resources` / `read_mcp_resource` in namespace `opencode` (`core/src/tool/plugin/mcp-resource.ts:11-20`).
- **Instructions:** each server's `instructions` is rendered into `<mcp_instructions><server name=…>`. Code Mode servers get a line telling the model to call them via `execute` under `tools["<ns>"]` (`core/src/mcp/instructions.ts`). Changes are sent as diffs (replaced/added/changed).
- **Elicitation** becomes a `Form` attached to the sentinel session id `"global"` (`mcp/index.ts:134`, `:480-554`). URL mode makes an "external" field. In the legacy era, `notifications/elicitation/complete` settles the form.
- **MCP Events** (draft extension, behind the `mcp_events` flag) are cursor-resumable subscriptions published as `experimental.mcp.event`. A dropped stream reopens with backoff from 1 s up to a 30 s cap (`mcp/index.ts:140`, `:357-380`).

### 4. OAuth (`core/src/mcp/oauth.ts`)

1. On reconcile, `register` (`mcp/index.ts:409`) turns every remote server (unless `oauth: false`) into an Integration with one `oauth` method. The integration id is `mcp_` plus 16 hex characters of sha1(name\0url) (`:414-420`), so the same URL under two names keeps separate accounts.
2. `authorize` (`oauth.ts:419`) reuses a previously registered client from the stored credential and starts a loopback HTTP server on 127.0.0.1 (`:494-496`). It discovers the auth server from the configured `auth_server_metadata_url` or from protected-resource metadata, then chooses how to register (`:524-543`):
   - a static `client_id` if one is configured;
   - **CIMD** if the authorization server advertises `client_id_metadata_document_supported` and auth method `none`;
   - DCR otherwise.
3. Both `127.0.0.1` and `localhost` redirect URIs are registered, to cope with servers like Clerk that rewrite the host (`:544-560`).
4. `connectProvider` (`:269`) only reads and refreshes the selected credential. It never starts a login.
   - Refresh happens 30 s ahead of expiry (`:46`).
   - On a 401 it refreshes only if the stored token is still the one that was sent.
   - `invalid_grant`, `invalid_client` and `unauthorized_client` mean the user must log in again (`:48-52`).

### 5. `opencode mcp` CLI

Defined at `packages/cli/src/commands/commands.ts:238-280`:
- `mcp list` shows status from the server.
- `mcp add <name> [--url U --header k=v] [-- cmd args] [--env k=v] [--global]` writes `mcp.servers.<name>` with jsonc-parser edits. The target is the first existing file among `opencode.json(c)` and `.opencode/opencode.json(c)` in the cwd, or the global config with `--global` (`handlers/mcp/add.ts:40-68`). The `--` command is read straight from `process.argv` (`:17-19`).
- `mcp auth [name]` and `mcp logout <name>` handle credentials.

Experimental HTTP: `PUT/DELETE /api/experimental/mcp/:server`, `.../connect`, `.../disconnect`, and `GET /api/mcp` (`protocol/src/groups/mcp.ts:26-106`).

### 6. Plugin loading and activation

1. **Config:** `plugins: (string | {package, options})[]` (`schema/src/config.ts:106`, `schema/src/config/plugin.ts:6-14`). A `-target` entry is a removal, and wildcards `*` / `prefix.*` are allowed (`core/src/config/plugin/source.ts:116-122`, `supervisor.ts:25-26`). Relative and `file://` paths resolve against the config file's directory. Directories named `plugin/` and `plugins/` under config roots are auto-discovered (`core/src/plugin/source-directory.ts:7`).
2. **Supervisor** (`core/src/plugin/supervisor.ts:141-190`): `pre = internal.pre + sdk + instance`, `post = internal.post`. Operations are applied in order. Anything not matching an existing plugin id (and not starting with `opencode.`) is loaded as a module. The final order is `pre ++ packages ++ post`, and duplicate ids are reported as failures rather than killing the whole generation (`:94-114`).
   - Activation runs twice: first with what is already installed locally, then again after installing pending packages (`:150-168`).
   - Update checks run in the background, and reloads are triggered by config changes, source-file watch events, SDK updates and a 24 h timer (`:205-208`).
3. **Module load** (`core/src/plugin/module.ts:101-166`):
   - Managed policy `plugin.use` can deny a target (`:53`).
   - npm and git specifiers install through `Npm.Service` using `@npmcli/arborist` into `<cache>/npm/<key>`. Git targets get a `git-<slug>-<sha12>` key and are marked mutable unless pinned to a commit (`packages/util/src/npm.ts:66-94`, `:148`).
   - `Host.resolve` maps package exports `./server` (or root), `./tui`, `./rpc` and `./gui-experimental` to entrypoints (`packages/plugin/src/host.ts:25-65`).
   - The default export must have `id` plus `effect` or `setup`. Promise plugins are wrapped by `fromPromise` (`module.ts:81-93`, `:185`).
   - Local sources are hash-fingerprinted and watched, so edits hot-reload (`packages/plugin/src/source.ts:8-46`).
4. **Shared `effect`:** `ensurePluginRuntime` registers a Bun plugin that serves `effect` and `@opencode/plugin` (all subpaths) from the host. An `onLoad` filter throws `"plugins must use the host's \"effect\" and \"@opencode/plugin\" modules"` for any copy inside a plugin's own node_modules (`packages/plugin/src/runtime.ts:12-36`, `runtime-modules.ts:7`). This keeps Effect's `Context` tags and fibers identity-compatible across the boundary.
5. **Registry** (`core/src/plugin.ts:103-198`):
   - Only the unchanged prefix of `(id, revision)` stays alive. Everything after the first change is closed in reverse order and reloaded, because registrations are order-dependent.
   - A failing new revision falls back to reloading the previous activation (`:174-187`).
   - If a plugin's `transform` throws later, that plugin is disabled with a `ref` and state is refreshed (`:65-78`, `:200-254`).
6. **ctx construction:** `PluginHost.make` (`core/src/plugin/host.ts`) wraps core services, for example `tool.hook` → `hooks.register("tool", …)` (`:506`). `ctx.storage` is the KV store namespaced `plugin:<hex(id)>:` (`:663-690`).

### 7. Hook dispatch

`PluginHooks.trigger` (`core/src/plugin/hooks.ts:88-95`) loops over callbacks for `domain.name` in registration order. It skips any callback whose `providerID` option doesn't match `event.model.providerID`, yields each callback, and returns the (mutated) event. There is no priority setting and no parallelism; the order is plugin activation order.

## Configuration and extension points

### Every hook

| Hook | Fires at | What a callback can change | Can fail/block |
|---|---|---|---|
| `tool` `execute.before` | `core/src/tool.ts:124`, used by `run` (`:185`) and step execution (`:311`) | `tool` (rename/alias; used by ToolInputRepair), `input` | **Yes**: a `Tool.Error` rejects the call (`plugin/src/effect/tool.ts:48-51`) |
| `tool` `execute.after` | `tool.ts:163` (error), `:175` (completed) | `result` (output/content/metadata) or `error` | No |
| `permission` `evaluate` | `core/src/permission.ts:190` | `effect` (allow/ask/deny), `message` | No; **a configured deny is decided at `:186` before the hook runs**, so a hook cannot unblock it |
| `shell` `create.before` | `core/src/shell.ts:912` | `command`, `cwd`, `timeout`, `shell`, `env` | No |
| `session` `fork` | `core/src/session.ts:388` | nothing (notification; failures only logged) | No |
| `session` `prompt` | `core/src/session/prompt.ts:42` | `prompt` {text, files, agents, skills}, `metadata`, `delivery` | No |
| `session` `context` | `core/src/session/model-request.ts:465`, primary at `:468` | `system`, `messages`, `options`, `tools` | No |
| `session` `compaction` | `model-request.ts:469` | as context + `result` (skip the model call) | No |
| `session` `generate` | `model-request.ts:470` | as context | No |
| `session` `title` | `model-request.ts:471` | request + `result` (skip the model call) | No |
| `session` `experimental.model.resolve` | `core/src/session/runner/model.ts:109` | `model` (same provider) | No |
| `session` `model.request` | `model-request.ts:361` | `baseURL`, `headers` | No |
| `session` `http.request` / `http.response` | `model-request.ts:382`, `:396` | `request` / `response` objects | No |
| `session` `experimental.ws.handshake/send/receive` | `model-request.ts:416`, `:424`, `:428` | `url`/`headers`; raw `frame` | No |
| `session` `retry` | `model-request.ts:440`, invoked from `runner/step.ts:247` | `decision` `{retry, delay}` | No |
| `session` `experimental.idle` | `core/src/session/runner/llm.ts:184` | none, but may admit input (e.g. a synthetic message) so the run continues instead of going idle | No |
| `aisdk` `sdk` / `language` | wired at `core/src/plugin/host.ts:170-197` | the AI-SDK provider object / `LanguageModelV3` | No |

Session hooks that carry a `model` accept `{providerID}` scoping (`registration.ts:19-25`).

### Transform points

`ctx.<x>.transform(editor => …)` exists for `agent`, `command`, `model`, `provider`, `reference`, `skill`, `vcs`, `websearch`, `worktree`, `integration`, `mcp`, `tool` and `experimental.artifact` (`packages/plugin/src/effect/{agent:15,command:25,model:26,provider:31,reference:14,skill:15,vcs:39,websearch:13,worktree:19,integration:108,mcp:15,tool:55,artifact:49}.ts`).
- Transforms are re-run on `reload()`, and their registrations die with the plugin scope *(inferred from `State.Transformable` usage)*.
- Other ctx domains include `event.subscribe`, `session.*` (prompt, create, compact, synthetic…), `experimental.generate`, `storage`, `rpc` and `permission.assert`.

### Built-ins are plugins

`core/src/plugin/internal.ts:247-310` lists them.
- `pre` (in order) starts with ToolInputRepair, ConfigWorktree, ConfigMcp, McpCodeModeDefaults, WellKnown, VcsGit, Agent, ForkNotice, Plan, Command, Goal, Skill, VcsHg and ModelsDev. It continues with all provider and websearch plugins, Patch, Optimize, Verbosity, Identity, then every tool (Edit, Glob, Grep, ImageGenerate, McpEvents, McpResources, Question, Read, Artifact, Schedule, Shell, Skill, Subagent, Todo, WebFetch, WebSearch, Write) and Warming.
- `post` holds the config loaders (instructions, agents, commands, compaction, formatter, shell, snapshot, providers, policy…), the browser plugin and DynamicProvider.
- `guarded` ids (the Opencode provider and ConfigPolicy) cannot be removed by repository config (`:314`).
- `opencode plugin list --builtin` shows them (`cli/src/commands/commands.ts:286-293`).

### RPC, TUI, GUI

- **RPC:** `ctx.rpc.register(definition, handlers)` exposes typed methods and events. Clients call `POST /api/rpc/:rpcID/:method` with `{input}` → `{output}` (`protocol/src/groups/rpc.ts:14`, `core/src/rpc.ts`). A package can ship a `./rpc` export for clients (`host.ts:59`).
- **TUI plugins** use a separate `{id, setup}` API (`packages/plugin/src/tui/plugin.ts`) and are loaded by the TUI from its own config, not by the server (`core/src/plugin/module.ts:121`).
- **GUI plugins:** a prebuilt CJS `./gui-experimental` bundle runs in each desktop/web window, and a `main` export runs in the Electron main process. The bundle's hash becomes its revision, so edits reload windows (`module.ts:136-161`, `packages/gui-plugins/README.md`). Every desktop feature beyond the shell is itself a GUI plugin.
- **`packages/plugins/`** holds official plugins installed from Git URLs, not bundled. The only one is Figma, which `mcp.transform`-adds `https://mcp.figma.com/mcp` and `skill.transform`-adds bundled SKILL.md files (`packages/plugins/figma/index.ts:7-38`).

### MCP keys and flags

- `mcp.timeout.{startup,catalog,execution}`
- `mcp.servers.<n>.{type,command,cwd,environment,url,headers,oauth,disabled,codemode,timeout,protocol,pool}`
- the `mcp_events` flag (`schema/src/config/flag.ts:20`)
- the managed-policy action `mcp.use` (`schema/src/config/policy.ts:23`)

v1 configs used `enabled`, camelCase OAuth fields, a single `timeout` defaulting to 5 s and port 19876 (`core/src/v1/config/mcp.ts:17-44`); they are migrated.

## Edge cases, invariants and gotchas

- **The 12 h default execution timeout** is effectively unbounded. The SDK timeout does not reset on progress (`client.ts:43`, `:724`). Attached clients get 10 min (`:44`).
- **There is no SSE transport.** Old SSE-only servers fail unless they also speak Streamable HTTP *(inferred: no `SSEClientTransport` reference in core/src)*.
- **Lifecycle survives cancellation.** Operations are forked into the root scope so a dropped HTTP request can't leave a server half-connected (`mcp/index.ts:285-290`).
- **Clean shutdown.** Server scopes close in parallel at shutdown (`:271-273`). Stdio shutdown: close stdin, wait 2 s, SIGTERM, then SIGKILL 2 s later. The last 1,000 bytes of stderr are kept for error messages (`stdio.ts:11-19`).
- **The `?codemode=false` rewrite** changes the URL sent to remote servers. The OAuth identity stays on the configured URL.
- **Identity matters.** The tool registry compares tools by identity, so tagged tool lists are cached per listing (`mcp/index.ts:649-666`).
- **Elicitation isn't tied to a session.** Forms are not attributed to a session (`"global"`), so a UI must show them outside any session.
- **Name collisions.** Because of sanitizing, the servers `a.b` and `a_b` both produce namespace `a_b` *(inferred)*.
- **Hooks mutate in place.** There is no merge logic: the last writer wins, in plugin order. Only `execute.before` can reject.
- **Promise plugins:** `setup` may return a cleanup function, and any other return value (such as a v1-style hooks object) is ignored (`promise/adapter.ts:674-676`). A rejected `execute.before` promise becomes a `Tool.Error` (`:693-699`).
- **Failure isolation.** A plugin whose transform throws is disabled. A plugin whose new revision fails setup keeps its old activation.
- **The shared Effect instance is mandatory.** Plugins that bundle their own `effect` fail to load.
- **Plugins are trusted code.** They run in-process with full engine privileges. The only gate is the managed-policy `plugin.use` deny.

## Tests worth reading

- `packages/core/test/mcp.test.ts`, `mcp-oauth.test.ts`, `mcp-stdio.test.ts`, `mcp-attach.test.ts`, `mcp-socket-client.test.ts`, `mcp-instructions.test.ts`, `mcp-events.test.ts`, `tool-mcp-events.test.ts`, `mcp-import-boundary.test.ts`
- fixtures: `packages/core/test/fixture/mcp*.ts` (echo, timeout, legacy-silent, modern-stdio, output-schema)
- `packages/core/test/plugin/supervisor.test.ts`, `supervisor-reload.test.ts`, `module.test.ts`, `promise-tool.test.ts`, `promise-tool-cancel.test.ts`, `permission-assert.test.ts`, `tool-input-repair.test.ts`, `mcp-codemode-defaults.test.ts`
- `packages/core/test/session-model-request-hooks.test.ts`, `rpc-handler-errors.test.ts`
- `packages/plugin/test/host.test.ts`

## Genie takeaways

Ticket links: [17 MCP](../../tickets/17-mcp.md), [20 hooks](../../tickets/20-hooks.md), [22 plugins](../../tickets/22-plugins.md), [14 auth](../../tickets/14-cli-auth.md). Pick-list: [#18 MCP, #26 plugin API](../../research/opencode-pick-list.md).

**Port the design (#18, ticket 17):**
- the five-state status model (`pending/connected/disabled/failed/needs_auth`);
- per-server split timeouts. Choose a saner default than 12 h for execution, for example 10 min with a config override;
- retrying transient connect failures within the startup deadline;
- `UnsupportedProtocolVersion` errors that name the supported versions;
- a short startup grace (2 s) before the first model call, rather than blocking on slow servers;
- a debounced reload of the tool list on `list_changed`;
- tool-result shaping: structured content first, JSON-looking text parsed, binary saved to a file with a path note.

**Adapt (ticket 17, `.mcp.json` scopes):**
- OpenCode merges servers across config documents in a fixed order. Genie's `.mcp.json` scopes (user/project/local) need the same "later scope wins per name" rule plus a global timeout merge.
- Keep tool naming deterministic and sanitized. Claude-compatible `mcp__<server>__<tool>` naming is better for importing permission rules than OpenCode's `<server>_<tool>`, which can collide.
- Make the permission action equal the tool name, as OpenCode does.

**OAuth (ticket 14):**
- Keep credentials out of config, keyed by hash(name, url).
- Support static client_id → CIMD → DCR, and register both the `127.0.0.1` and `localhost` redirects.
- Refresh 30 s ahead of expiry, and refresh on 401 only if the stored token is unchanged (this is safe across processes).
- Genie must host its **own** client-metadata document. OpenCode's URL is specific to OpenCode.

**Elicitation:** route it to the single main session. Genie has one session per engine, so the "global" sentinel problem disappears.

**Avoid or defer:**
- Code Mode wrapping of MCP tools (pick-list #27 SKIP). Genie should expose MCP tools directly, with deferred/tool-search loading if the count grows.
- Connection pooling across directories. It has little value with one session per engine.
- MCP Events (still a draft).

**Hooks (ticket 20):** OpenCode's hook points are a useful map of what extensions actually need. Rough mapping to Claude-style command hooks:

| OpenCode hook | Claude-style event |
|---|---|
| `execute.before` | PreToolUse |
| `execute.after` | PostToolUse |
| `prompt` | UserPromptSubmit |
| `compaction` | PreCompact |
| `experimental.idle` | Stop (it can keep the session working, as Stop's block does) |
| `fork` | SessionStart(resume/fork) |
| `permission.evaluate` | PermissionRequest |

The permission invariant matches Genie's "a hook can never override a deny": OpenCode evaluates configured deny rules *before* the `evaluate` hook (`permission.ts:186-190`). Copy that ordering exactly.

Hooks Genie should **not** expose to external processes, because they are too hot or too privileged for a subprocess JSON contract: `context`, `http.request/response`, `ws.*`, `model.request` and `aisdk`. If needed, keep them internal Rust traits.

**Plugins (ticket 22, #26 SKIP):** don't build an in-process ABI. What's worth copying is the *packaging* idea, and the Figma plugin shows it well: a plugin that is just "an MCP server + skills" maps cleanly onto Genie's manifest bundle (commands, agents, skills, hooks, MCP).
- Also worth copying: npm/git install into a content-keyed cache dir, marking a git ref as mutable unless it is a commit, `-name` disable directives with wildcards, guarded ids that repository config cannot disable, and a policy deny (`plugin.use`) checked before install.
- For hot reload, the fingerprint-and-watch approach in `plugin/src/source.ts` is a good model for reloading skills and hooks.

**Internally**, OpenCode's "everything is a plugin" layering, with ordered transforms over registries (tool/agent/command/provider), is a sound *internal* architecture for Genie's Rust engine: a registry plus an ordered list of contributors that is rebuilt on reload. Build it as Rust traits, not as a public ABI.

## Open questions / inferred

- Exactly where generic tool-output truncation applies to MCP results (registry wrapper vs per-tool) was not traced.
- `State.Transformable` semantics (re-running transforms on reload, disposal by scope) are inferred from usage, not read in `core/src/state.ts`.
- Whether any SSE fallback exists outside `core/src` (for example in a patched SDK) was not checked.
- Collisions between sanitized server namespaces are not explicitly handled in the code I read.
- `connection.ts`/`pool.ts` internals (slot release, `unpoolable` and `legacy` sets) were only skimmed.
- I only skimmed the TUI and GUI plugin APIs. Their full surface belongs in the desktop and TUI chapters.
