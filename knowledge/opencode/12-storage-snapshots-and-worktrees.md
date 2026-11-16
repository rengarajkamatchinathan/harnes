# Storage, Snapshots and Worktrees

> OpenCode @ 56625a3e1f · paths relative to opencode-reference/

## TL;DR

- **One SQLite file per channel** holds everything durable: projects, sessions (`session_v2`), messages, parts, inbox, instructions, media blobs, schedules, worktrees, saved permissions, credentials, accounts, jobs, the event log counters and a generic `kv` table. The default name is `opencode.db` in the XDG data dir. Non-release channels get `opencode-<channel>.db`, and `OPENCODE_DB` overrides it (`packages/cli/src/database-path.ts:4-13`).
- **Drizzle schema plus hand-rolled TypeScript migrations.** A fresh DB is bootstrapped in one transaction from `schema.gen.ts`, and all 59 migration ids are marked done. Existing DBs replay only the missing ids. The old Drizzle journal is translated once (`core/src/database/migration.ts:24-108`). Three SQLite backends (bun, node, Cloudflare Durable Objects) are chosen by package `imports` conditions.
- **The v1→v2 session migrator runs in place in the background.** Legacy `session`/`message`/`part` rows are copied into `session_v2`/`session_message`, with resumable progress kept in `kv["migration.v1-v2"]` (`core/src/database/v1-migration.bun.ts:220,541-767`).
- **Sessions are event-sourced projections.** Domain events go through `Bus`. Projectors write rows, and `event_sequence` keeps a per-aggregate seq. Event *payloads* are persisted only when `persist` is on. That is off by default for the local server and forced on for workerd (`core/src/bus.ts:214`, `server/src/workerd.ts:73`).
- **Snapshots are git trees in a private, ref-less repo** at `<data>/snapshot/<projectID>/<hash(worktree)>`. That repo borrows the user's objects through `alternates` and seeds its index from the user's index. The engine captures a tree at step start and step end. Revert works in stages: **stage** (restore files and record what was undone) → **clear** (undo the revert) or **commit** (delete the later messages) (`core/src/snapshot.ts`, `core/src/session/revert.ts`).
- **Nothing ever prunes snapshot objects.** They are only packed (≥2048 loose objects or ≥16 packs, checked at most every 10 min). `file-retention.ts` is *not* snapshot retention. It removes 7-day-old tool-output and shell-output files.
- **Worktrees** are `git worktree add --detach` checkouts under `<data>/worktree/<projectID[0:6]>/<slug>`, tracked in the `worktree` table. Worktree strategies are pluggable (git by default), and a per-project `commands.start` hook runs after creation (`core/src/worktree.ts:204-255`).
- **The scheduler** keeps all state in the `schedule` table. Leader election is a lease row `kv["schedule:leader"]` with a 60 s TTL and a heartbeat every 20 s (`core/src/schedule.ts:97-98,539-570`). Firing admits a prompt with a deterministic message id `msg_<scheduleId>_<run>`, so a crash and retry is idempotent.

## Where it lives

| Path | Role | ~lines |
|---|---|---|
| `packages/core/src/database/database.ts` | `Database.Service` layer: pragmas, per-file bootstrap lock, 0600 perms | 127 |
| `packages/core/src/database/migration.ts` | Bootstrap and incremental migration runner | 155 |
| `packages/core/src/database/schema.gen.ts`, `migration.gen.ts`, `migration/*.ts` | Generated full schema plus 59 timestamped migrations | 354 + 122 + ~1.5k |
| `packages/core/src/database/sqlite.{bun,node,workerd}.ts` | Driver adapters selected via `#sqlite` | 17 / 25 / 226 |
| `packages/core/src/database/v1-migration{,.bun,.noop}.ts` | Legacy session migrator (bun only) | 1234 |
| `packages/core/src/database/path.ts`, `batch.ts`, `storage-failure.ts` | Path columns, 100-param batching, user-facing SQLite errors | 98 / 21 / 58 |
| `packages/core/src/*/sql.ts` (session, project, schedule, kv, worktree, event, job, credential, account, workspace, permission) | Drizzle table definitions | ~500 total |
| `packages/cli/src/database-path.ts` | DB filename/channel resolution | 13 |
| `packages/core/src/kv.ts` | Key/value service over the `kv` table | 92 |
| `packages/core/src/session/transfer.ts` | Session export/import | 369 |
| `packages/core/src/snapshot.ts` | Snapshot service | 248 |
| `packages/core/src/git.ts` | Git CLI wrapper: snapshot store, tree capture/diff/restore, pack, worktree | 1609 |
| `packages/core/src/session/revert.ts` | Revert stage/clear/commit | 112 |
| `packages/core/src/file-retention.ts` | mtime-based file cleanup helper | 23 |
| `packages/core/src/worktree.ts`, `worktree/{strategies,git,directory,sql}.ts` | Worktree service and strategies | 357 + ~200 |
| `packages/core/src/vcs.ts`, `plugin/vcs/{git,hg}.ts` | VCS info/status/diff via provider plugins | 262 + plugins |
| `packages/core/src/repository.ts`, `repository-cache.ts` | Remote repo refs and local tracking clones | 205 / 251 |
| `packages/core/src/schedule.ts`, `schedule/{sql,check}.ts` | Scheduler | 832 / 45 / 491 |
| `packages/core/src/artifact.ts` | Artifact store (flagged) | 617 |

## Architecture

```mermaid
flowchart LR
  CLI["opencode CLI / desktop"] -->|databasePath(global.data)| SRV["server process (routes.ts)"]
  SRV --> DB[("SQLite opencode[-channel].db<br/>WAL, FK on, 0600")]
  SRV --> SCHED["Schedule.run loop<br/>lease kv['schedule:leader']"]
  SCHED --> DB
  subgraph Engine["core (per Location)"]
    STEP["SessionStep.attempt"] -->|capture start/end| SNAP["Snapshot.Service"]
    REV["SessionRevert stage/clear/commit"] --> SNAP
    WT["Worktree.Service"] --> STRAT["WorktreeStrategies (git default)"]
    BUS["Bus + projectors"] --> DB
  end
  SNAP --> SREPO[("<data>/snapshot/&lt;project&gt;/&lt;hash&gt;<br/>bare-ish git dir, no refs")]
  SREPO -. alternates .-> UREPO[("user repo .git/objects")]
  STRAT -->|git worktree add --detach| WTDIR["<data>/worktree/&lt;proj6&gt;/&lt;slug&gt;"]
  WT --> DB
  RC["RepositoryCache"] --> REPOS["<data>/repos/host/owner/repo[@branch]"]
  RC -->|refresh stamp| DB
  ART["Artifact"] --> ADIR["<data>/artifacts/&lt;id&gt;"]
  ART -->|records| DB
```

```mermaid
erDiagram
  project ||--o{ session_v2 : "project_id (cascade)"
  project ||--o{ worktree : "project_id (cascade)"
  project ||--o{ permission : "project_id (cascade)"
  project ||--o{ project_directory : "deprecated"
  session_v2 ||--o{ session_message : "session_id (cascade)"
  session_message ||--o{ session_part : "message_id (cascade)"
  session_v2 ||--o{ session_inbox : "pending input"
  session_v2 ||--o{ instruction_entry : "(session_id,key)"
  session_v2 ||--|| instruction_state : "epoch"
  session_v2 ||--o{ blob_ref : "refs"
  blob ||--o{ blob_ref : "hash (cascade)"
  session_v2 ||--o{ schedule : "session_id (cascade)"
  event_sequence ||--o{ event : "aggregate_id"
  account ||--o| account_state : "active_account_id"
  session_v2 { text id PK; text project_id FK; text parent_id; text fork_session_id; text directory; json revert; json permission; real cost; int time_suspended }
  session_message { text id PK; text session_id FK; text type; int seq; json data }
  session_part { text message_id PK; int ordinal PK; text type; text call_id; json data }
  schedule { text id PK; json when; json action; json check; text status; int next_due_at; int retry_at; json state }
  kv { text key PK; json value }
  worktree { text project_id PK; text directory PK; text strategy }
```

There are also tables without foreign keys: `job` (background jobs keyed by `notification_id`), `credential`, `workspace`, `instruction_blob` and `control_account` (legacy).

## Key types and interfaces

| Name | file:line | Purpose |
|---|---|---|
| `Database.Service` / `layer(options)` | `core/src/database/database.ts:26,71` | The single Drizzle-over-Effect DB handle. A relative `path` resolves under `Global.data` (line 81) |
| `DatabaseMigration.Migration` | `core/src/database/migration.ts:16-20` | `{ id, foreignKeys?, up(tx) }`. `foreignKeys:false` disables or defers FK checks for table rebuilds |
| `Timestamps` | `core/src/database/schema.sql.ts:3-10` | `time_created`/`time_updated` epoch-ms columns that every table spreads in |
| `SessionTable` (`session_v2`) | `core/src/session/sql.ts:28-84` | Session header: lineage (`parent_id`, `fork_session_id`, `fork_boundary`), location, usage totals, `revert` JSON, per-session `permission`, idle/busy/archived timestamps. `time_suspended` is the execution claim |
| `SessionMessageTable` | `core/src/session/sql.ts:86-105` | One row per message with JSON `data`. `(session_id, seq)` is unique and gives the transcript order |
| `SessionPartTable` | `core/src/session/sql.ts:107-128` | Assistant content parts, PK `(message_id, ordinal)` |
| `SessionInboxTable` | `core/src/session/sql.ts:130-152` | Queued user, synthetic, compaction and move inputs with a `delivery` mode, ordered by `enqueued_seq` |
| `InstructionEntry/Blob/StateTable` | `core/src/session/sql.ts:154-183` | Per-session instruction key/values, content-hashed values and epoch state |
| `BlobTable`/`BlobRefTable` | `core/src/session/sql.ts:185-215` | Deduplicated media keyed by SHA-256 and referenced by message (behind the `media_blob` flag) |
| `EventSequenceTable`/`EventTable` | `core/src/event/sql.ts:4-26` | Per-aggregate seq and owner, plus the optional durable event log |
| `KV.Interface` | `core/src/kv.ts:27-32` | `get/set/remove/scan(prefix, after, limit≤1000)` |
| `Snapshot.Interface` | `core/src/snapshot.ts:43-68` | `capture()`, `files`, `diff`, `restore(Map<path, treeId>)` |
| `Git.Interface.tree` / `.objects` | `core/src/git.ts:173-200` | Tree capture/diff/restore and safe pack |
| `Worktree.Interface` | `core/src/worktree.ts:93-102` | `list/create/remove/refresh` |
| `WorktreeStrategies.Strategy` | `core/src/worktree/strategies.ts:15-24` | Pluggable backend: `create/remove/list` |
| `RepositoryCache.ensure` | `core/src/repository-cache.ts:125` | Clone or refresh a remote into `<data>/repos` |
| `Schedule.Interface` | `core/src/schedule.ts:62-81` | CRUD, `tick`, and `run` (the leader-elected loop) |
| `ScheduleTable` | `core/src/schedule/sql.ts:10-45` | `when`/`action`/`check` JSON, status, due/retry times, run counters, check `state` |
| `Artifact.ProviderImplementation` | `core/src/artifact.ts:54-63` | Pluggable artifact backends. The local provider is the default |

## How it works: walkthroughs

### 1. Opening the database

1. The CLI resolves the file. `OPENCODE_DB` wins. Release channels (`latest/dev/beta/next/prod`) or `OPENCODE_DISABLE_CHANNEL_DB` give `opencode.db`, and any other channel gives `opencode-<channel>.db`. `:memory:` is passed through (`cli/src/database-path.ts:5-12`). The server receives it as `options.database` (`cli/src/server-process.ts:115`). `opencode debug paths` prints it (`cli/src/commands/handlers/debug/paths.ts:12`).
2. `Database.layer` creates the file with mode 0600 and chmods the `-wal`/`-shm` sidecars (`database/database.ts:90-103`).
3. The pragmas are `journal_mode=WAL`, `synchronous=NORMAL`, `busy_timeout=5000`, `cache_size=-64000`, plus a passive checkpoint and `foreign_keys=ON`. All of them are skipped on Durable Objects (`database.ts:43-51`).
4. Migrations run under a semaphore keyed by filename, so two layers over one file never race (`database.ts:52-53,61-69`).
5. The driver comes from `package.json` `imports["#sqlite"]`: `bun:sqlite`, `node:sqlite`, or a Durable Object `SqlStorage` adapter (`core/package.json:33-38`). Workerd also caps statements at 100 bound parameters, which `DatabaseBatch` enforces everywhere (`database/batch.ts:5-21`).

### 2. Migrations

1. `apply` lists the non-`sqlite_` tables that do not start with `_`. Underscore tables are reserved for embedders (`migration.ts:28-30`).
2. If a `session` or `session_v2` table exists, it runs `applyOnly` (line 31). A non-empty DB without one dies (line 33). Otherwise it bootstraps: `schema.gen.ts` plus the legacy-credentials import, then it inserts every migration id into `migration` in one transaction (lines 36-49).
3. On its first run, `applyOnly` seeds the new `migration` journal from Drizzle's `__drizzle_migrations`. It matches by `name`, or failing that by a timestamp prefix (lines 65-107). Each pending migration then runs in its own transaction. `foreignKeys:false` sets `PRAGMA foreign_keys=OFF` (or `defer_foreign_keys` on DO) for the duration (lines 110-139).
4. Many migrations rebuild tables (DROP plus re-CREATE), for example `migration/20260612174303_project_dir_strategy.ts:24`. That is why the FK escape hatch exists.

### 3. v1 → v2 session migration

1. `V1Migration.layer` is merged into the server routes and forks `run()` at startup (`server/src/routes.ts:208`, `v1-migration.bun.ts:512-529`). `GET` status is served by `server/src/handlers/migration.ts:10`. Node and workerd builds use a no-op (`core/package.json:73-79`).
2. The migrator only acts if a legacy `session` table exists (`v1-migration.bun.ts:1229-1233`). The first pass deletes the old `event` rows in batches of 1000 and writes `kv["migration.v1-v2"] = {phase:"sessions"}` (lines 556-571).
3. It imports a sibling `opencode-next.db` if one is present and compatible (lines 799-836).
4. It walks the legacy sessions in descending-id order. For each one, inside a transaction, it: updates the cursor in kv; reassigns missing projects to `global`; replaces relative directories with the project worktree; resolves owner projects for orphan directories (5 s timeout per lookup, giving up after the first hang, lines 770-797); copies the row with `INSERT OR IGNORE INTO session_v2 … SELECT … FROM session`; transforms messages and parts through `transformSession`; and sets `event_sequence` to the watermark (lines 598-745).
5. Tool renames are applied during the transform (`bash→shell`, `task→subagent`, `apply_patch→patch`, `todowrite` removed; lines 996-998). The final step writes `{phase:"completed"}` (lines 749-763). The legacy tables are left in place.

### 4. Session persistence, export and import

1. Writes are event-driven. `bus.publish` assigns a seq through `event_sequence` and runs registered projectors inside the commit. For example, `RevertEvent.Staged` sets `session_v2.revert` (`session/projector.ts:1126-1131`). The `event` table only receives payloads when `persist` is true (`bus.ts:214,631`).
2. The execution claim is `session_v2.time_suspended`, set with a null guard so a re-claim is a no-op (`session/store.ts:251-259`). Sessions with a surviving claim are resumed on boot *(inferred from store.ts doc comments at lines 78-95)*.
3. **Export**: `opencode session export [id] [--sanitize]` calls the server. It returns `{info, messages}` with only settled messages. `--sanitize` redacts the title, metadata, directory, revert patches and tool content (`session/transfer.ts:61-67,219-245`; CLI `cli/src/commands/handlers/session/export.ts:70-74`).
4. **Import**: `opencode session import <file|url> [--directory]` POSTs to `/api/experimental/session/import` (`cli/.../session/import.ts:39-48`). The server rejects an id that already exists (409 / `ImportConflictError`, `transfer.ts:69-76`). It re-resolves the project from the target directory and renumbers `seq` from 1. When `media_blob` is on, it splits inline media into blobs. It then publishes `SessionEvent.Created` and inserts the messages in batches inside the same commit (`transfer.ts:84-185`).

### 5. Snapshots: capture

1. The service is per-Location. It is enabled only when `location.vcs?.type === "git"` and config `snapshots` is not false (`snapshot.ts:119`; config plugin `config/plugin/snapshot.ts:15-19`). Non-git projects get no snapshots.
2. On first use it discovers the user repo and computes `gitDirectory = <data>/snapshot/<projectID>/<Hash.fast(realpath(worktree))>` (`snapshot.ts:92-97`). If `HEAD` is missing, `git.repo.create` inits into a temporary sibling dir and renames it into place, which is race-safe across processes (`git.ts:446-477`). Setup writes `opencode.gitconfig` (no autocrlf, `manyFiles`, index v4, untracked cache; `git.ts:24-41`) and points `objects/info/alternates` at the user repo's objects. It then seeds the index from the user's index to reuse the stat cache (`git.ts:502-520`).
3. `capture()` calls `git.tree.capture` with the scope set to the Location directory relative to the worktree. The user repo supplies the ignore rules, and untracked files over **2 MiB** are skipped (`snapshot.ts:133-148`). Concurrent captures with the same key share one deferred (`git.ts:845-869`). Captures take a store lock, sweep temp indexes older than 1 h, and rebuild an unreadable index once (`git.ts:873-897,953-966`). The result is a **tree id with no commit and no ref**.
4. Every capture calls `packWhenDue`, which runs at most every 10 min (`snapshot.ts:122-131`). `objects.pack` only acts at ≥2048 loose objects or ≥16 packs (`git.ts:51-52,1208-1214`). It hands `pack-objects` an explicit object list because "Snapshot trees have no refs, so Git's own repack and prune would treat every snapshot as garbage… Nothing is pruned" (`git.ts:1201-1206`).
5. **When are snapshots taken?** At each LLM step: the start snapshot is forked concurrently with the provider request (`session/runner/step.ts:78-80`). At step end, a second capture is taken and `snapshots.files(start,end)` lists the changed paths. Both go into `Step.Ended` or the step failure (`step.ts:290-313`). This means snapshots are taken per step, not per tool call. Bash side effects inside the scope *are* captured, because the capture covers the filesystem rather than tool calls. `session/diff.ts:139-141` captures on demand for live diffs.

### 6. Revert: stage → clear | commit

1. `Session.revert.stage(sessionID, {messageID, files?})` refuses while the session is busy (`session/session.ts:332-343`).
2. `SessionRevert.stage` takes `original` from the existing staged revert's snapshot, or captures it now (`revert.ts:33`). `plan` walks the assistant messages after the boundary seq. For each file in their `snapshot.files`, it maps the path to the *earliest* step-start tree (`revert.ts:83-112`). Files from a previous staged revert are first reset to `original`. Then the planned paths are restored (`revert.ts:35-40`). Restore batches `git checkout <tree> --pathspec-from-file` using a temp index, and deletes paths absent from the tree (`git.ts:1138-1196`). The resulting diff from `original` is stored in `session_v2.revert` via `RevertEvent.Staged` (`revert.ts:42-53`).
3. **clear** restores every reverted file to `original` and nulls `revert` (`revert.ts:58-73`, projector `:1129-1131`).
4. **commit** happens explicitly, or implicitly on the next prompt or compaction (`session/session.ts:173,264`). It publishes `Committed`, and the projector deletes every `session_message` with `seq ≥ boundary` (`projector.ts:1132-1160`). It also drops inbox rows queued at or after that seq. The messages are destroyed for good. Files were already restored at stage time.

### 7. Worktrees

1. `opencode run -w/--worktree [name]` (`cli/src/commands/commands.ts:422-425`) and the `worktree.create`/`worktree.remove` HTTP handlers (`server/src/handlers/worktree.ts:17-19`) both call `Worktree.Service`.
2. `create` loads the project row and the strategies of the project's primary checkout. It waits for plugin activation, because strategies are plugin-extensible (`worktree.ts:121-128`). The source must be a known row of the project (`worktree.ts:188-193`).
3. Naming: the default parent is `<data>/worktree/<projectID.slice(0,6)>` (`worktree/strategies.ts:51`), which config `worktree.directory` can override (`~/`-expanded or project-relative, `config/plugin/worktree.ts:24-30`). The name is `input.name ?? Slug.create()` (adjective-noun). On a collision it appends `-2`…`-10` and then fails with `DestinationExistsError` (`worktree.ts:211-218`).
4. The git strategy runs `git worktree add --detach -- <dir> <ref|HEAD>` (`git.ts:1408-1418`). **No branch is created.** The `branch` input is only used as a ref to check out.
5. The row is upserted in `worktree` with its strategy, and `Worktree.Event.Updated` is published. If the project has `commands.start`, it runs through `bash -lc` in the new dir with `OPENCODE_WORKTREE_BASE`/`OPENCODE_WORKTREE_PATH` set (`worktree.ts:228-253`).
6. `remove` requires a stored row *with a strategy*. This guards against deleting discovered or unmanaged checkouts. It then runs `git worktree remove [--force]` (`worktree.ts:257-271`, `git.ts:1428-1438`).
7. `refresh` re-discovers with each strategy's `list` (`git worktree list --porcelain`, `git.ts:1442-1443`). It drops rows whose directory vanished. Discovery may claim an unowned row but never steals another strategy's row (`worktree.ts:162-169,273-327`).

### 8. Repository cache

1. `Repository.parse` normalizes remotes. `cachePath = <data>/repos/<host parts>/<segments>[@<urlencoded branch>]` (`repository.ts:111-119`, `Global.repos` in `util/src/global.ts:22`).
2. `ensure` takes a cross-process file lock (`EffectFlock`) on the key `repository-cache:<path>`. It reuses a checkout only if the checkout sits exactly at the path and its origin matches. Otherwise it deletes the stale dir (`repository-cache.ts:128-155`).
3. With `refresh:"daily"`, it refetches when more than 1 day has passed since the last *attempt*, which is stored in kv before any network work (`:22,157-165`). A refresh fetches, checks out the tracked branch, and runs `reset --hard origin/<branch>` ("newest wins"; `:180-209`). Its only consumer is `Reference` (config-declared reference repos, `reference.ts:49`).

### 9. Scheduler

1. Every server process forks `Schedule.run` (`server/src/process.ts:118-119`). Schedules require the `schedules` flag (`schema/src/config/flag.ts:16`) and are created via the schedule tool or the HTTP handler.
2. **Leader election** works on the lease row `kv["schedule:leader"] = {owner: uuid, until: now+60s}`. It is claimed through an upsert whose `WHERE` succeeds only if the caller already owns the row or the lease has expired (`schedule.ts:539-553`). While ticking, a heartbeat re-claims every 20 s (`:567-571`). The leader releases the lease when no active rows remain and on shutdown (`:555-565,615`).
3. **Loop**: close the doorbell; if there are no active rows, sleep (≤30 s); else claim. A new leader first runs `rewake` for sessions whose last scheduled prompt is still in `session_inbox` (`:510-525,579-596`). It then runs `tickAt(now)` and sleeps until the next `next_due_at` (capped at 30 s) or until the doorbell rings (`:597-613`).
4. **tick** selects active rows that are due and past `retry_at`, with up to 4 in parallel (`:452-491`). `step` re-checks leadership before each row (`:427`). If the session's location has schedules disabled, it sets `retry_at = now+1h` (`:429-434`). If the previous prompt is still pending, it marks the run skipped and re-wakes (`:435-449`).
5. **fire** evaluates the optional `check` (`shell`/`code` condition, mode `if`/`until`, 30 s timeout, `schedule/check.ts:23`) with its persisted `state`. It then calls `sessions.prompt` with id `msg_<id>_<run>` (`:259-287`) and updates the counters, `next_due_at` (in/at/every/cron with tz) and the status (`:401-419`). On failure it backs off 5 min. A missing directory pauses the row (`:470-487`).

### 10. Artifact store (brief)

Behind the `artifacts` flag (`artifact.ts:266`). The local provider writes files to `<data>/artifacts/<id>` through staging, backup and rename swaps (`artifact.ts:213-233,565-597`). Records live in kv under `artifact:item:<id>` (`:33,353-360`). Other providers can be registered through `State` editors (`:241-262`).

## Configuration and extension points

| Key / env / flag | Effect | Source |
|---|---|---|
| `OPENCODE_DB` | DB filename or path, or `:memory:` | `cli/src/database-path.ts:6` |
| `OPENCODE_DISABLE_CHANNEL_DB=1\|true` | Use `opencode.db` on any channel | `:8-9` |
| `OPENCODE_FLAGS` (e.g. `schedules,-media_blob`, `all`) | Experimental flags. On by default only on the `local`/`dev` channels | `core/src/flag.ts:26-35` |
| flag `media_blob` | Deduplicated `blob` table for media | `schema/src/config/flag.ts:15` |
| flag `schedules` / config `timezone` | Enable the scheduler / default tz | `flag.ts:16`, `schema/src/config.ts:56` |
| flag `artifacts` | Artifact store | `flag.ts:17` |
| config `snapshots: boolean` | Enable or disable snapshots | `schema/src/config.ts:67-68` |
| config `worktree.directory` | Parent dir for new worktrees | `schema/src/config/worktree.ts:6-7` |
| project `commands.start` | Command run after worktree creation | `project/sql.ts:28`, `worktree.ts:236` |
| `opencode run -w [name]` | New session in a new worktree | `cli/src/commands/commands.ts:422` |
| `opencode session export/import/delete/list` | Session transfer | `commands.ts:470-500` |
| Server option `events.persist` | Keep durable event payloads | `server/src/options.ts:24` |
| Plugin `WorktreeStrategies.Editor.add` | Register a new worktree backend (it becomes the selected one) | `worktree/strategies.ts:59-63` |
| Plugin VCS providers (`plugin/vcs/git.ts`, `hg.ts`) | Info/status/diff backends | `vcs.ts:47-53` |

## Edge cases, invariants and gotchas

- **Owner-only perms and underscore namespace.** The DB and its sidecars are chmod 0600 on POSIX (`database.ts:90-103`). Tables whose names start with `_` are ignored by bootstrap. A non-empty DB with no session table is fatal (`migration.ts:26-33`).
- **Bootstrap vs. replay paths differ.** Fresh installs never run the individual migrations. A schema bug fixed only in a migration but not in `schema.gen.ts` would diverge *(inferred risk)*.
- **v1 rows survive.** The migrator copies data and leaves the `session`/`message`/`part` tables in place. Sessions with a missing project are reassigned to `Project.ID.global` (`v1-migration.bun.ts:617,651`).
- **Event log is mostly off locally.** Only `event_sequence` advances by default. Recovery of orphaned turns on workerd depends on `persist:true` (`server/src/workerd.ts:70-73`).
- **Snapshots require git**, and the Location must be inside the realpath'd worktree (`snapshot.ts:110-117`). Ignored files are still captured, but they are filtered out of `files`/`diff` using the *user* repo's ignore rules (`:160-198`). Large untracked files (>2 MiB) are silently not captured, so a revert cannot restore them.
- **Snapshot storage grows without bound.** There is no gc or prune, only packing. The `alternates` file makes the store depend on the user's `.git/objects`. Packing copies the objects so the store does not rely on them (`git.ts:1201-1206`). Deleting the user repo after packing is fine. Before packing, it might not be *(inferred)*.
- **`restore` rejects paths that escape the worktree** (`snapshot.ts:200-209`). It errors when snapshots are disabled, whereas capture just returns `undefined`.
- **Revert is destructive on commit**, and commit happens implicitly when the next prompt arrives. Staging a second revert reuses the first `original` snapshot, so you can move the boundary without compounding.
- **Worktrees are detached.** Branch management is left to the user or agent. `remove` refuses unmanaged rows. The suffix limit is 10.
- **Repository cache refresh does a hard reset**, so readers can see files change underneath them (`repository-cache.ts:2-7`).
- **Scheduler double-fire protection** has three layers: the lease, `claimLeader` before each row, and the deterministic message id reconciled by `admission.reconcile` (`session/session.ts:161-166`). `update()` only writes rows still `active`, so a pause during a fire wins, except for the run counters (`schedule.ts:240-246,404`).
- **SQLite errors are mapped** to storage-neutral messages: BUSY/LOCKED become "in use by another process", plus FULL, CORRUPT, NOTADB and others (`database/storage-failure.ts:11-20`).
- **`file-retention.ts` is generic.** It deletes files with mtime older than the cutoff, 8 at a time. Callers: tool output `tool_*` files and shell output files, both with 7-day retention (`tool-output.ts:15,45`, `shell.ts:49,214`). Running shell processes keep their files.

## Tests worth reading

- `packages/core/test/database.test.ts`, `database-migration.test.ts`, `database-drizzle.test.ts`, `storage-failure.test.ts`
- `packages/core/test/v1-migration.test.ts`
- `packages/core/test/kv.test.ts`
- `packages/core/test/snapshot.test.ts`, `config/snapshot.test.ts`, `session-revert.test.ts`, `git.test.ts`
- `packages/core/test/worktree.test.ts`, `plugin/worktree.test.ts`, `vcs.test.ts`, `vcs-hg.test.ts`
- `packages/core/test/repository.test.ts`, `repository-cache.test.ts`, `reference.test.ts`
- `packages/core/test/schedule.test.ts`, `tool-schedule.test.ts`
- `packages/core/test/artifact.test.ts`, `shell-retention.test.ts`
- `packages/core/test/preload.ts` (sets `OPENCODE_DB=:memory:` for all tests)

## Genie takeaways

**Storage ([12 sessions](../../tickets/12-sessions-persistence.md), pick-list #14).**
- **Port:**
  - one SQLite file per install (per channel), WAL plus `busy_timeout`, 0600 perms;
  - a `migration(id, time_completed)` journal of Rust migrations;
  - the "fresh DB = bootstrap the full schema and mark all migrations done" trick, which keeps first launch fast. In Rust, `rusqlite` plus a hand-rolled runner mirrors `migration.ts` closely. Keep the `foreign_keys:false` per-migration escape hatch for table rebuilds.
- **Adapt the schema shape:**
  - session header row, messages with a monotonic per-session `seq` (unique index), parts keyed `(message_id, ordinal)`, a pending-input inbox and a generic `kv`;
  - `ON DELETE CASCADE` from session downwards;
  - since Genie runs one main session per engine instance, keep `parent_id` for subagents but skip OpenCode's cross-process session claiming (`time_suspended`) unless the desktop and VS Code clients ever share a DB.
- **Avoid initially:**
  - the full event-sourcing and projector machinery. OpenCode itself leaves event payload persistence off locally;
  - the DO/workerd backend;
  - the v1→v2 migrator. We have no legacy data. But **do** copy its pattern for future migrations: a background, resumable cursor in `kv` with progress reported to the UI.
- **Export/import:** port `{info, messages}` JSON with a `--sanitize` redaction mode and an id-conflict check. This is cheap and useful for bug reports.

**Checkpoints ([23 checkpoints](../../tickets/23-checkpoints.md), pick-list #12).** OpenCode answers the "shadow git vs. blobs" question with **shadow git**:
- a private git dir per (project, worktree hash) outside the repo;
- `alternates` plus a seeded index for speed;
- ref-less tree ids recorded on each assistant step;
- path-level restore through a temporary index.

Recommend porting this design via the `git` CLI (or `gix` for capture). It gives Genie Bash-side-effect coverage for free, which Claude-style file-tool checkpoints lack.

Gaps Genie must fill:
1. **No retention.** Add a cap (e.g. N checkpoints or days, matching Claude's ~100 / 30 days). Then gc unreferenced trees, by keeping a ref per retained checkpoint and letting `git gc` prune the rest.
2. **Non-git projects get no checkpoints.** A blob fallback (content-addressed files in SQLite, like OpenCode's `blob` table) could cover them.
3. Make the 2 MiB untracked-file skip visible to the user.
4. Keep the stage/clear/commit semantics. Implicit commit on the next prompt is a good UX default. Offer "code only / conversation only / both" by splitting restore from message deletion; OpenCode's `files:false` already does the conversation-only case.

**Worktrees ([24 worktrees](../../tickets/24-worktrees.md), pick-list #13).**
- **Port:**
  - the table `(project_id, directory, strategy)`;
  - slug naming with numeric suffixes;
  - "remove only what we created";
  - the post-create `start` command with `*_WORKTREE_BASE/PATH` env;
  - discovery via `git worktree list --porcelain`.
- **Adapt:** Genie's decision is `.genie/worktrees` inside the repo, not the data dir. That requires adding the folder to `.git/info/exclude`. Also consider creating a named branch (`genie/<slug>`) instead of `--detach`, because detached worktrees make PR flows awkward. Skip pluggable strategies.

**Scheduler (M3, pick-list #20).** Port the lease-in-kv election (an upsert with a `WHERE owner=me OR until<=now` condition), the 30 s max sleep plus doorbell, deterministic per-run message ids, and `retry_at` backoff. Use `next_due_at` with a partial index. This is a small, self-contained design that fits SQLite well. The `check` conditions (`shell`/`code`, `if`/`until`) are optional extras.

**Repository cache / artifacts:** low priority. If Genie adds "reference repos", reuse the daily-attempt stamp in kv plus a file lock.

## Open questions / inferred

- The exact XDG data dir on macOS (`~/.local/share/opencode` by default) is *(inferred)*. `#global-roots` imports XDG base dirs whose source was not read.
- Boot-time resume of sessions with a surviving `time_suspended` claim is *(inferred)* from the `SessionStore` comments. The resume path in `session/execution/restart.ts` was not traced.
- Whether the snapshot store survives deletion of the user's `.git` before the first pack is *(inferred)* from the `alternates` setup.
- The full `transformSession` mapping of v1 parts (`v1-migration.bun.ts:227-498`) was skimmed, not audited.
- There is no `opencode snapshot` or `worktree list` CLI subcommand apart from `run -w`. Worktree management is otherwise HTTP/UI-driven *(inferred from the commands.ts survey)*.
