# Agents, skills and commands

> OpenCode @ 56625a3e1f · paths relative to opencode-reference/

## TL;DR

- An **agent** is a record with `model`, `system`, `description`, `mode` (`primary` | `subagent` | `all`), `hidden`, `color`, `steps` and a `permissions` ruleset (`packages/schema/src/agent.ts:23-35`). Built-ins are added by plugins. User agents come from JSON config (`agents: {}`) or Markdown files in `agent/`, `agents/`, `mode/`, `modes/` under each config directory.
- **The default ruleset allows everything** (`*` on `*`), except that reading `.env` files asks (`agent.ts:46-51`). With no matching rule the result is `ask` (`core/src/permission.ts:94-103`). Rules are evaluated last-match-wins, so config rules appended later override the defaults.
- Built-ins: **build** (default primary), **plan** (primary; edits denied except under `~/.opencode/plan`, **shell still allowed**), **general** (subagent; no question, no nested subagent), **explore** (subagent; deny-all, then allows shell, grep, glob, read and web, guided by a "read-only" prompt), and the hidden **compaction**, **title** and **summary** agents.
- An agent's `system` **replaces** the default system prompt (`session/model-request.ts:117-121`). `steps` caps the loop. When the cap is reached the next request uses `toolChoice: "none"` plus a "MAXIMUM STEPS REACHED" assistant message (`session/runner/llm.ts:261-280`).
- **Skills** are `SKILL.md` (or top-level `*.md`) files with frontmatter `name`, `description`, `metadata`, `disable-model-invocation`. They are discovered in `skill/` and `skills/` under config dirs, in config `skills: [path|url]` entries, and in `.claude/skills` and `.agents/skills` (global and project) for compatibility. `metadata["opencode/autoinvoke"]` and `disable-model-invocation` hide a skill from the model's list.
- The model loads a skill with the `skill` tool (permission action `skill`). A user attaches skills to a prompt, and those are injected as the same `<skill_content>` block. Managed policy `skill.use` can remove skills entirely.
- **Commands** are `{template, description, agent, model, subagent}`. They come from config `commands: {}` or `command(s)/**/*.md`. Templates support `$ARGUMENTS`, `$1..$N` (the last placeholder takes the rest of the arguments) and ``!`shell` `` interpolation. Built-ins are `/init` and `/review`, plus `/schedule` (flag-gated) and one `server:prompt` command per MCP prompt.

## Where it lives

| path | role | ~lines |
|---|---|---|
| `packages/schema/src/agent.ts` | `Agent.Info` + default ruleset | 59 |
| `packages/core/src/agent.ts` | agent registry, default selection | 115 |
| `packages/core/src/plugin/agent.ts` | build/general/explore/compaction/title/summary + prompts | 119 |
| `packages/core/src/plugin/plan.ts` | plan agent, plan-dir exception, enter/leave reminders | 116 |
| `packages/core/src/config/plugin/agent.ts` | JSON + Markdown agent loading, V1 migration, `~` expansion | 196 |
| `packages/schema/src/config/agent.ts` | config agent schema | 26 |
| `packages/core/src/tool/plugin/subagent.ts` | `subagent` tool | 276 |
| `packages/schema/src/skill.ts`, `packages/core/src/skill.ts` | skill model, registry, `<skill_content>` rendering | 66 + 184 |
| `packages/core/src/config/plugin/skill.ts`, `skill-scan.ts`, `skill-file.ts`, `compatibility.ts` | discovery, parsing, Claude/agents compat, watching | 192 + 46 + 62 + 94 |
| `packages/core/src/skill/discovery.ts` | remote skill index pull (URL sources) | 205 |
| `packages/core/src/skill/instructions.ts` | `<available_skills>` system guidance + diffs | 84 |
| `packages/core/src/plugin/skill.ts`, `plugin/skill/{opencode,report}.md` | built-in skills | 101 + md |
| `packages/core/src/tool/plugin/skill.ts` | `skill` tool | 69 |
| `packages/core/src/command.ts`, `packages/schema/src/config/command.ts` | command registry + config schema | 109 + 14 |
| `packages/core/src/plugin/command.ts`, `plugin/command/*.txt` | built-in `/init`, `/review`, MCP prompt commands | 113 |
| `packages/core/src/config/plugin/command.ts` | config/Markdown commands, templating | 226 |

## Architecture

```mermaid
flowchart LR
  subgraph Sources
    CFG[opencode.json(c)<br/>agents / skills / commands]
    DIRS[config dirs: global, system,<br/>project .opencode walk-up]
    CL[.claude/skills, .agents/skills<br/>global + project]
    URL[skills: https://... index.json]
    MCPP[MCP prompts]
  end
  DIRS -->|agent/ agents/ mode/ modes/ *.md| AG[Agent.Service]
  CFG --> AG
  BI1[AgentPlugin + PlanPlugin] --> AG
  DIRS -->|skill/ skills/| SK[Skill.Service]
  CFG --> SK
  CL --> SK
  URL -->|cache/skills/hash| SK
  BI2[SkillPlugin: opencode, report] --> SK
  POL[ManagedPolicy skill.use] -->|remove| SK
  DIRS -->|command/ commands/ *.md| CM[Command.Service]
  CFG --> CM
  BI3[CommandPlugin: init, review] --> CM
  MCPP --> CM
  AG --> RUN[Session runner<br/>system prompt, steps, permissions]
  SK --> INS[SkillInstructions<br/>available_skills]
  SK --> ST[skill tool / prompt attachments]
  AG --> SA[subagent tool]
  CM --> SESS[session.prompt]
```

## Key types and interfaces

| name | file:line | purpose |
|---|---|---|
| `Agent.Info` | `packages/schema/src/agent.ts:23` | runtime agent record |
| `Agent.Info.default` | `packages/schema/src/agent.ts:39` | new agents start as primary with allow-all + `.env` ask |
| `Agent.Interface` | `packages/core/src/agent.ts:40` | `get/resolve/select/list` |
| `selectedDefault` | `packages/core/src/agent.ts:55` | `default_agent` if selectable, else `build`, else first non-subagent, non-hidden agent |
| `ConfigAgent.Info` | `packages/schema/src/config/agent.ts:11` | config fields incl. `disabled`, `request` |
| `Permission.evaluate` | `packages/core/src/permission.ts:94` | last matching rule, else `ask` |
| `Skill.Info` | `packages/schema/src/skill.ts:27` | id, name, description, autoinvoke, path, content |
| `Skill.Source` | `packages/schema/src/skill.ts:45` | directory / url / embedded |
| `Skill.toModelOutput` | `packages/core/src/skill.ts:40` | `<skill_content>` block + sampled file list |
| `SkillFile.parse` | `packages/core/src/config/plugin/skill-file.ts:38` | frontmatter → `Skill.Info` |
| `SkillScan.scan` | `packages/core/src/config/plugin/skill-scan.ts:11` | BFS to depth 6, stops at a SKILL.md |
| `Command.Definition` | `packages/core/src/command.ts:22` | `{name, description, execute(invocation)}` |
| `ConfigCommand.Info` | `packages/schema/src/config/command.ts:7` | template, description, agent, model, subagent (`subtask` deprecated) |

## How it works: walkthroughs

### 1. Agent definition and loading

1. Built-ins are added in `AgentPlugin` (`plugin/agent.ts:55-117`) and `PlanPlugin` (`plugin/plan.ts:33-42`) through `editor.update`. An update creates an agent from `Info.default` if it does not exist (`core/src/agent.ts:80-85`). Each built-in therefore **starts from allow-all** and pushes extra rules after it.
2. `ConfigAgentPlugin` reads every config entry. Document entries supply `agents`, top-level `permissions` and `default_agent`. Directory entries are scanned for `{agent,agents}/**/*.md` (mode subagent unless set) and `{mode,modes}/*.md` (forced `primary`) (`config/plugin/agent.ts:22-25,135-146,193`). Directory entries are the system config dir, the global config dir, and each project `.opencode` directory found walking up from the location (`core/src/config.ts:310-337`; `config/discovery.ts:82,143-147`).
3. The agent id is the relative path without the folder prefix or `.md`, so `agents/review/security.md` → `review/security` (`config/plugin/agent.ts:157-161`). The Markdown body becomes `system` (`:182`).
4. Frontmatter keys follow `ConfigAgent.Info`: `model` (`provider/model#variant`), `request` (headers/body), `description`, `mode`, `hidden`, `color` (`#rrggbb`, invalid values dropped), `steps`, `disabled`, `permissions`. Unknown keys **without** any V2-only key (`permissions`, `request`, `system`, `disabled`) mark a V1 file, which is migrated through `ConfigAgentV1` (`config/plugin/agent.ts:33-35,163-182`). A legacy `variant:` is folded into `model#variant` (`:170-176`).
5. Merge order (`config/plugin/agent.ts:64-107`): global `permissions` from every document are appended to **every** agent, built-ins included. Each configured agent then gets field overrides. Its own `permissions` are appended last, so they win. `disabled: true` removes the agent. `~/` and `$HOME/` are expanded only for `read`, `edit` and `external_directory` resources. Shell resources stay raw (`:112-133`).

### 2. Built-in agents

| id | mode | prompt | permissions added after allow-all |
|---|---|---|---|
| build | primary (default) | default system prompt | `question` allow (`plugin/agent.ts:56-61`) |
| plan | primary | default + `<system-reminder>` enter/leave | `question` allow; `edit *` deny; `edit ~/.opencode/plan/*` allow (`plan.ts:31-41`) |
| general | subagent | default | `question` deny, `subagent` deny (`plugin/agent.ts:63-72`) |
| explore | subagent | "file search specialist… MUST NOT create, modify…" (`:7-17`) | `*` deny, then allow shell/grep/glob/webfetch/websearch/read, `.env` ask, `subagent` deny, `external_directory` allow (`:74-94`) |
| compaction | primary, hidden | none here (compaction prompt lives in `session/compaction.ts`) | none (`:96-100`) |
| title | primary, hidden | 3–5 word title rules (`:19-38`) | `*` deny (`:102-108`) |
| summary | primary, hidden | PR-style 2–3 sentence summary (`:40-50`) | `*` deny (`:110-116`) |

Plan mode is a permission set plus reminders. It is not a separate tool or state machine. A reminder is persisted when the session is created or switched into or out of `plan` (`plan.ts:75-92`). A `context` hook re-adds it after compaction (`plan.ts:57-73`). Blocked edit/write/patch errors are rewritten to "Cannot use X to modify files outside the Plan directory" (`plan.ts:44-53`).

### 3. Running an agent

1. The system prompt is `agent.system` if set, otherwise the default `SessionSystemPrompt` built from the tool names. Instructions (AGENTS.md, skills list, …) form a second system part (`session/model-request.ts:116-126`).
2. Per-agent `model` is used when the session or command switches to the agent. `request.headers/body` merge into provider calls.
3. `steps`: at `step >= steps`, the request is sent with `toolChoice: "none"` and `MAX_STEPS_PROMPT` (`session/runner/llm.ts:261,276-280`; `session/runner/max-steps.ts:1-3`).
4. Tool visibility = `Tool.snapshot(agent + session permissions)` (chapter 09).

### 4. Subagents

1. The `subagent` tool lists agents that are not primary, not hidden and not denied for the caller, as "Available subagents" in its description (`tool/plugin/subagent.ts:247-270`).
2. On a call it measures the depth of the parent chain against `experimental.subagent_depth` (default 1, so a subagent cannot spawn another), rejects `mode: "primary"`, and asserts `subagent` for the agent id (`:117-145`).
3. Model precedence: explicit `model` (validated as an available chat model, variant checked) → `agent.model` → parent model (`:69-98,178`).
4. It creates a child session titled with `description` and prompts "You are a subagent spawned by another session.\n<prompt>". It starts a `SubagentJob` and returns `running` at once. The parent later receives a `<subagent>` message each time the child goes idle (`:18-27,179-224`). Passing `sessionID` continues an existing child of this session. A different `agent` switches that child's agent (`:147-176`).
5. A user can attach agents to a prompt (`PromptInput.agents`, `packages/schema/src/prompt-input.ts:28`) *(the delegation semantics of attached agents were not traced: inferred to be an @-mention that nudges the model to call `subagent`)*.

### 5. Skills: discovery

1. **Sources** (`config/plugin/skill.ts:80-106`): `<configdir>/skill` and `<configdir>/skills` for every config directory entry, plus each item in the config `skills: []` array. An item is either an `http(s)` URL or a path, with `~/` expanded and relative paths resolved from the location.
2. **Claude/agents compat** (`config/plugin/compatibility.ts:41-80`): `~/.claude/skills`, `~/.agents/skills`, and project `.claude/skills` / `.agents/skills` found by the same walk-up (`config/discovery.ts:69-70,150-164`). The config option `claude: false` drops the `.claude` roots (`core/src/config.ts:66`). Only skills are imported from these dirs. Claude-format agents and commands are **not** loaded.
3. **Scan** (`skill-scan.ts:11-46`): breadth-first up to depth 6. Dot-dirs and ignored dirs are skipped. A directory that contains `SKILL.md` contributes that file and is not descended further. At the root only, loose `*.md` files are skills too.
4. **Parse** (`skill-file.ts:38-61`): id = the file's basename for a loose root `.md`, otherwise the parent directory name. `name` defaults to the id. `autoinvoke` = `metadata["opencode/autoinvoke"]` (boolean-ish: true/yes/on/1) or, failing that, `false` when `disable-model-invocation` is truthy. Invalid frontmatter is skipped with a debug log.
5. **URL sources** (`skill/discovery.ts:92-140`): fetch `<url>/index.json`, then download each listed skill's files into `cache/skills/<hash>/<name>`. Every name and path is validated as a safe segment and must stay same-origin and inside the root.
6. Later sources override earlier ones by id (Map overwrite, `config/plugin/skill.ts:152-156`). Changes are watched, debounced 100 ms, and rescanned (`:169-176`).
7. Built-ins: `opencode` (docs index for OpenCode itself) and `report` (bug-report workflow with a runtime diagnostics snapshot), both served from virtual `skill://` paths (`plugin/skill.ts:27-45`).
8. Policy: managed `skill.use` deny removes skills from the registry (`config/plugin/policy.ts:92-95`).

### 6. Skills: invocation

1. **Model-visible list**: `SkillInstructions.load(permissions)` keeps skills that are not denied by `skill` permission, have a description, and do not have `autoinvoke === false`. It renders them as `<available_skills><skill><id/><name/><description/></skill>…` with instructions to use the `skill` tool. Later changes go out as diff messages (added/changed/removed) rather than a re-render (`skill/instructions.ts:25-46,61-79`).
2. **Model invocation**: `skill({id})` → `permission.assert({action:"skill", resources:[id]})` → `Skill.prepare`, which emits `<skill_content name=…>`, the body, "Base directory for this skill: …" and up to 10 sampled sibling files as `<file>` entries (`tool/plugin/skill.ts:45-63`; `core/src/skill.ts:40-94`). Files are then read with the normal `read` tool, or with `skill://id/file` for virtual skills (`tool/plugin/read.ts:58-96`).
3. **User invocation**: a prompt carries `skills: [{id, mention}]` (`packages/schema/src/prompt-input.ts:29`). `session/prompt.ts:58-77` resolves each one with `Skill.prepare` and attaches the same `<skill_content>` text to the user message. No `skill` permission check runs on this path. The guidance tells the model not to invoke a skill again once it is present (`skill/instructions.ts:29`). Skills with `autoinvoke: false` remain user-invocable because only the list filters them.

### 7. Commands

1. Registry: `Command.Definition {name, description, execute(invocation)}`. An invocation carries `sessionID`, `prompt` and `delivery` (`command.ts:16-26`). A failure becomes `ExecutionError` (`command.ts:84-91`).
2. Config: `commands: {<name>: {template, description?, agent?, model?, subagent?}}`, or Markdown files at `{command,commands}/**/*.md` where the body is the template and the name is the relative path without `.md` (`config/plugin/command.ts:119-162`; schema `packages/schema/src/config/command.ts:7-14`).
3. Templating (`config/plugin/command.ts:164-215`): arguments are split on whitespace and respect quotes and `[Image N]` tokens (`:222`). `$ARGUMENTS` becomes the raw input. `$n` becomes argument n, and the **highest** placeholder takes the rest. A template with no placeholders gets the input appended. ``!`cmd` `` runs through the configured shell in the location directory, two at a time, with stdout and stderr combined (`:189-214`).
4. Dispatch (`:53-110`): if `subagent` (or deprecated `subtask`) is set, or the command's agent is a subagent, it creates a child session and starts a `SubagentJob`. Otherwise it switches the session's agent and/or model and prompts.
5. Built-ins (`plugin/command.ts:29-81`): `/init` ("guided AGENTS.md setup", `command/initialize.txt`), `/review` ("review changes [commit|branch|pr], defaults to uncommitted", `command/review.txt`), and `<server>:<prompt>` for every MCP prompt, with positional arguments mapped to the prompt's declared arguments (`:55-81,104-106`). `/schedule` is added when the `schedules` flag is on (`tool/plugin/schedule.ts:157-170`). TUI-only commands (e.g. new session, model switch) live in client packages *(inferred; not traced in this chapter)*.

## Configuration and extension points

- `agents: {<id>: ConfigAgent.Info}`, `default_agent`, top-level `permissions: [{action, resource, effect}]` (appended to all agents).
- Files: `<configdir>/{agent,agents}/**/*.md`, `<configdir>/{mode,modes}/*.md`, `<configdir>/{skill,skills}/**/SKILL.md`, `<configdir>/{command,commands}/**/*.md`.
- `skills: ["./path", "~/x", "https://host/skills/"]`. Config option `claude: false` disables `.claude` compat dirs.
- `commands: {<name>: {template, description, agent, model, subagent}}`.
- `experimental.subagent_depth`. Managed policy statements `skill.use`, `tool.use` (`packages/schema/src/config/policy.ts:9-27`).
- Env: `OPENCODE_CONFIG_DIR` overrides the global config dir (`packages/util/src/global.ts:79`). `OPENCODE_CONFIG_CONTENT` supplies inline config (`core/src/config.ts:295`). `OPENCODE_FLAGS`.
- Plugins can call `ctx.agent.transform`, `ctx.skill.transform` and `ctx.command.transform`, the same editors the built-ins use.

## Edge cases, invariants and gotchas

- **Allow-all default.** Without user `permissions`, build runs edit, write, shell and webfetch **without prompting**. Only `.env` reads ask. Anything that explicitly hits "no matching rule" asks, but `*`/`*` matches everything.
- **Plan is not read-only.** Shell and webfetch stay allowed in plan. Only the edit tools are denied outside the plan directory. Explore relies on its prompt for "read-only shell".
- Agent `system` replaces the whole default system prompt, including tool guidance. A custom agent that sets `system` loses OpenCode's base prompt.
- Global `permissions` are appended to **all** agents, built-ins included. A global `{"action":"*","resource":"*","effect":"deny"}` is overridden by explore's later allows, but it does disable build.
- Denying `*` (as explore, title and summary do) also wholly denies `execute`, so Code Mode tools become direct (and are then denied anyway).
- Skill id comes from the directory name, not frontmatter `name`. Two dirs with the same name collide and the later source wins silently.
- A user-attached skill bypasses the `skill` permission. Only managed policy can block it.
- The skill file list is capped at 10 sampled files. Large skills rely on the model reading further.
- ``!`cmd` `` interpolation in command templates runs **without** any permission check or scanner. A repo's `.opencode/commands/*.md` therefore executes shell when invoked (`config/plugin/command.ts:189-214`).
- Command names may contain `/` (subdirectories). MCP prompt command names use `:`.

## Tests worth reading

`packages/core/test/agent.test.ts`, `packages/core/test/config/agent.test.ts`, `packages/core/test/tool-subagent.test.ts`, `subagent-completion.test.ts`, `subagent-lifetime.test.ts`, `subagent-report.test.ts`, `packages/core/test/skill.test.ts`, `skill-discovery.test.ts`, `packages/core/test/skill/instructions.test.ts`, `packages/core/test/config/skill.test.ts`, `packages/core/test/tool-skill.test.ts`, `session-skill.test.ts`, `packages/core/test/command.test.ts`, `packages/core/test/config/command.test.ts`, `packages/core/test/config/command-subagent.test.ts`, `packages/core/test/config/policy.test.ts`, `permission.test.ts`.

## Genie takeaways

- **Subagents as Markdown (pick-list #15, ticket `../../tickets/19-subagents.md`)**: port the file layout (`agents/**/*.md`, path-derived ids, body = system prompt, frontmatter model/steps/permissions/mode/hidden). Under `.genie/agents` it also matches Claude's format, so the importer stays trivial. Port the always-background child session, follow-up by `sessionID`, the depth limit and the dynamic "Available subagents" list. **Avoid** the allow-all base ruleset: Genie agents should start from the session's permission mode, and subagents should inherit the parent's mode unless narrowed. Decide whether an agent `system` *replaces* or *appends* to the base prompt. OpenCode replaces it, which surprises users.
- **Plan mode**: OpenCode's plan is "deny edit except plan dir plus reminders", with shell left open. Genie's plan mode should also gate Bash (read-only classifier or ask), and the enter/leave reminder that survives compaction is worth porting.
- **Step limit**: port the "tools disabled, answer in text" final step (`toolChoice: none` + reminder) instead of hard-stopping.
- **Skills (pick-list #16, ticket `../../tickets/18-skills.md`)**: port the scanner (stop at SKILL.md, depth limit, root loose `.md`), id-from-directory, the `<available_skills>` list with diffs instead of re-renders (cache-friendly), `disable-model-invocation` (Claude-compatible) for hiding from the model, and the base-directory + sampled-file-list output. Genie keeps `.genie/skills` as the primary location. Claude dirs are better handled by the importer than by live compat scanning, since OpenCode's live scanning means a repo's `.claude/skills` silently enters context. Remote URL skill indexes are a later concern. If we adopt them, keep OpenCode's safe-segment and same-origin checks.
- **User-invoked skills** should still pass Genie's policy layer. OpenCode lets them bypass the `skill` permission.
- **Commands (ticket `../../tickets/21-slash-commands.md`)**: port the `$ARGUMENTS` / `$n` (last takes the rest) semantics, Markdown commands with `agent`/`model`/`subagent` frontmatter, and MCP prompts as `server:prompt` commands. **Do not port** unchecked ``!`cmd` `` interpolation. Route it through the Bash permission path, or require workspace trust (ticket 32).
- **Plugins (ticket `../../tickets/22-plugins.md`)**: OpenCode's agents, skills and commands are all "transform an editor" registries that plugins share. Genie restricts extensions to MCP + hooks + skills, so expose the same registries only to first-party code, and to skills/MCP prompts as data.

## Open questions / inferred

- Exact global config path (assumed XDG `~/.config/opencode` from `util/src/global.ts:12-13`) *(inferred)*.
- How the TUI turns `/name` and `@agent` input into `Command.execute` or `PromptInput.agents` was not traced *(inferred: client side)*.
- The compaction agent's prompt and model choice live in `session/compaction.ts` and are covered in the context-management chapter, not here.
- Whether `mode: "all"` agents appear in both the primary picker and the subagent list. Code suggests yes: `selectable` excludes only `subagent`, and the subagent list excludes only `primary` (`core/src/agent.ts:53-54`; `subagent.ts:256`).
