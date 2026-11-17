# 11 — Open questions and spec assumptions

Everything the decisions (D1–D57, C1–C4) and tickets left unspecified that the build must settle. Each item has a **proposed default**. Section 1 lists every **Spec assumption (SA-nn)** made in this spec (40 in total). Section 2 lists decisions only the user can make. Section 3 lists smaller build-time questions that don't change product behaviour.

How to close an item: accept the default (it becomes a build-ticket detail) or replace it (record a new D-line in WAYFINDER.md and update the spec file named in the table).

## 1. Spec assumptions

| ID | Topic | Proposed default | Why a default was needed | Where |
|---|---|---|---|---|
| SA-01 | Daemon idle exit | Exit after 10 min with no clients, no running turns, **no paused asks**, no background jobs, no terminals | t01 says "N minutes"; a pending approval must survive | 01 §4.3 |
| SA-02 | Endpoint names | `<data dir>/run/daemon.sock`; pipe `\\.\pipe\genie-<channel>-<sid-hash>`; token `run/daemon.token`, **new per daemon start** | t01 fixes mechanism, not names | 01 §1.2 |
| SA-03 | Ensure timings | 15 s readiness timeout, 50 ms → 1 s connect backoff, one automatic retry, then an error card | t01 says "timeout and retries" | 01 §4.1 |
| SA-04 | Version-skew rule | A newer client **build** triggers drain-and-restart when the daemon is idle, even with the same protocol major | Keeps one engine version in the field | 01 §4.2 |
| SA-05 | Actor eviction | Session actors dropped after 15 min with no subscribers and no turn | Bounds memory | 01 §5.5 |
| SA-06 | Typing during a turn | **Steer** by default; Alt+Enter = **queue** (desktop and CLI) | D20/D42 require both; default unstated | 05 §2.3, 07 §1.3 |
| SA-07 | Max steps per turn | Main agent: 200 in Code, 50 in Chat; final step without tools | t18 has per-agent `maxSteps`, no main default | 06 §1 |
| SA-08 | Protocol method set | Names in 02 §6 beyond t02's list (`project.*`, `artifact.*`, `memory.*`, `blob.*`, `terminal.*`, `checkpoint.*`, `git.*`, `trust.*`, `directory.*`, `usage.*`, …) | t02: "as subsystems need them" | 02 §6 |
| SA-09 | Error codes | One numeric code per `data.kind` (−32001…−32013, −32099) | t02 fixes kinds, not numbers | 02 §8 |
| SA-10 | Backpressure | Per-subscription queue 8 MiB or 20,000 messages → `resync_required` | t02 requires a bound | 02 §3.1 |
| SA-11 | Driver lease edge cases | First mutating command takes a vacant lease; 60 s reconnect grace by `client_instance_id`; **asks answerable by any client** | t02 covers take_over/release only | 02 §5 |
| SA-12 | Event-log pruning | After 30 days idle, prune events up to the last `turn.settled`; older cursors get a snapshot resync | t12: "once a session is idle" | 02 §9 |
| SA-13 | AccountTag | SHA-256(provider ‖ credential id ‖ tenant/project ‖ endpoint host), first 16 bytes | research Q6 | 03 §2 |
| SA-14 | Embedding provider | Catalog-flagged default model per provider; order: chat model's provider → OpenAI → Azure → Gemini; policy overrides | D32: "when available" | 03 §9 |
| SA-15 | Vector search | Brute-force cosine in Rust; no SQLite vector extension | Avoids an extension in SQLCipher; small per-project sets | 04 §7 |
| SA-16 | Recovery key | 256-bit key shown once (Base32 + `.txt`), wraps the data key in `recovery.bin` | D24: "optional recovery-key export" | 04 §2 |
| SA-17 | Export-all format | Plaintext `.zip`: manifest, sessions, projects, memory, decrypted blobs, artifacts | D24: no format for "all" | 04 §9 |
| SA-18 | RAG capacity cap | 10× the largest enabled model window, ceiling 20M tokens and 500 files per project | D32: "~10×, per-project cap" | 04 §7 |
| SA-19 | Maintenance schedule | Retention + blob GC at start and every 24 h; VACUUM weekly when free pages > 20% and idle | t12: "VACUUM on a schedule" | 04 §8 |
| SA-20 | Brand placeholders | Cool-grey neutrals; placeholder teal accent `oklch(0.62 0.13 195)`; placeholder wordmark | t29 defers accent/tokens/logo to spec; must not resemble Anthropic (C1) | 05 §6 |
| SA-21 | Minimum OS versions | Windows 10 22H2 / 11; macOS 13+; Ubuntu 22.04+ / Debian 12+ / Fedora 40+ with WebKitGTK 4.1 ≥ 2.42 | research/desktop-shell.md Q4 | 05 §12 |
| SA-22 | Accessibility target | WCAG 2.2 AA app-wide; axe in CI; NVDA/VoiceOver/Orca pass before GA | t33 names AA contrast only | 05 §11 |
| SA-23 | Screencast parameters | JPEG q60, ≤1280×800 CSS px × scale ≤2, ack flow control; ≥15 fps, ≤150 ms input-to-frame; stop when hidden | D56 asks the spec to check frame rate/latency | 05 §9 |
| SA-24 | Take-over input details | `insertText`/`imeSetComposition`; paste from Genie clipboard; copy only on user keypress during take-over; file chooser via native dialog; quarantine downloads; tab strip | D56 lists these as risks | 05 §9 |
| SA-25 | Terminal limits | ≤ 8 terminals per session; not restored after daemon restart | D55 silent | 05 §10 |
| SA-26 | Commit split button | Commit · Commit & push · Create PR; message pre-filled by the small model, editable | D54 names the button only | 05 §3.3 |
| SA-27 | Attachment limits | 32 MB per file, 20 files per message, then model limits | t31: limits from model capability only | 05 §2.3 |
| SA-28 | Browser outside the desktop | Profile at `<data dir>/browser/profile` (Chrome's own cookie encryption; ephemeral if no Linux keyring); CLI opens the real window; headless off unless `--allow 'Browser(site:…)'`/`--enable-browser` | t40 defers CLI, headless, profile location | 06 §10 |
| SA-29 | Taint rule | Untrusted content (fetch, browser read, untrusted MCP, native search) taints the turn; auto-allows for shell, outside-workspace writes, non-read-only MCP, new-domain fetch and cross-origin browser `act` become asks; clears at next user message; bypass doesn't add asks | research/browser.md requires taint; t13 never set the rule | 06 §5.5 |
| SA-30 | CLI management commands | `genie login/logout`, `mcp`, `trust`, `sessions`, `config`, `daemon` subcommands + Claude-Code-named slash built-ins | CLI-only installs need them | 07 §2 |
| SA-31 | Signing-key hierarchy | Offline hardware-backed root Ed25519 key (two custodians) signs a key list of per-feed online keys; clients pin the root | t35 item 5 | 09 §4 |
| SA-32 | Telemetry backend | Sentry protocol to a self-hosted Sentry-compatible endpoint on Synergech Azure; on-device scrubbing | t35: "backend choice → spec" | 08 §7 |
| SA-33 | Rollout gates | 48 h soak per stage; advance only if crash-free sessions ≥ 99.5% and ≤ 0.2 pts below previous stable; auto-pause on failure | D40: gates, no thresholds | 09 §4.1 |
| SA-34 | CI hardware | Hosted Win/macOS/Linux runners + self-hosted Linux NVIDIA box (X11 + Wayland) + Intel/AMD Linux box + oldest-supported macOS machine; secure signing secrets | Follows from D8/D9 even with t41 deferred | 09 §5.3 |
| SA-35 | Calendar conversion | Calendar weeks = ew ÷ (engineers × 0.85); start date 2027-01-04 for indicative dates | Needed to turn ew into a timeline | 10 §1 |
| SA-36 | Supported browsers | Chrome, Edge, Chromium ≥ 120 | Not specified | 06 §10 |
| SA-37 | Estimate scope | Estimates exclude PM, visual design, legal, dedicated QA, t41 work | Needed to read the numbers correctly | 10 §1 |
| SA-38 | Crate exceptions | `genie-process` and `genie-secrets` may be used by subsystem crates directly | Leaf utilities; core-trait indirection adds no safety | 01 §2.1 |
| SA-39 | Paging spilled output in chat | Chat-only `tool_output_read {ref, offset, limit}` tool | t11 says "a read tool"; chat has none | 06 §4.1 |
| SA-40 | Importer conflicts | OpenCode allow-after-deny exceptions imported as the deny + diagnostic | D53: "with diagnostics"; C4 favours safe side | 07 §5.3 |

## 2. Decisions the user should make

These change scope, dates or recorded decisions. The spec does not make them.

| # | Question | Proposed default | Needed by |
|---|---|---|---|
| U1 | **Team size** for v1 | 5 engineers (4 minimum); 3 is not enough for the decided scope (10 §5) | Now |
| U2 | **Confirm v1 = M1–M4** with GA at the high end of the range, or pre-agree cuts from 10 §5.2 | Keep M1–M4; pre-agree cut 1 (pop-out browser) as the first fallback | End of M1 |
| U3 | **Code/CLI track timing:** after Chat GA (D6) or overlapping M4 | Overlap: start M5 during M4 with the 5th engineer | End of M3 |
| U4 | **Release naming:** is the Code/CLI track part of "v1" (1.x) or a separate release? | "Genie 1.0" = Chat v1; "Genie 1.1/1.2" = Code tab and CLI; v2 items stay v2 | Before beta |
| U5 | External beta: invite-only on the beta channel at end of M3? | Yes, Synergech customers under NDA | End of M2 |
| U6 | Ticket 41: CI platform and repo host | Decide in week 1 (10 §8) | **M1 week 1–4** |
| U7 | Ticket 41: business model, licence enforcement, source licence | Deferred as recorded; decide before first external release | Before external beta |
| U8 | Ticket 35 owners for the 6 action items | As proposed in 10 §7 | Now |
| U9 | Who runs the macOS/Linux bench (t37) and when | Before M1 week 6 | M1 |
| U10 | Brand: accent, tokens, logo (t29) | Replace SA-20 placeholders | Before beta |

## 3. Build-time questions with proposed defaults

These don't change product behaviour; they are listed so build tickets don't rediscover them.

| # | Question | Proposed default | Where |
|---|---|---|---|
| B1 | Markdown parser in the webview: JS (prototype) or `genie-markdown` compiled to WASM? | JS for v1 (proven in prototype 37); revisit if WebKit perf fails | 05 §7 |
| B2 | Frames over NDJSON as base64 vs a binary side channel | Base64 in v1 (~1.2 MB/s is fine locally); binary framing only if profiling shows a problem | 02 §6.6 |
| B3 | PDF text extraction library for non-PDF models | A pure-Rust extractor (e.g. `pdf-extract`/`lopdf`) in the daemon; pdf.js only for preview | 03 §4 |
| B4 | ULID vs UUIDv7 ids | ULID with type prefix | 02 §6 |
| B5 | Highlighter for settled code blocks | highlight.js everywhere in v1 (D57); no shiki | 05 §7 |
| B6 | E2E test driver on macOS (no `tauri-driver` WebDriver support) | Native accessibility-based harness + daemon-level contract tests | 09 §5.2 |
| B7 | tree-sitter for code chunking in RAG | Not in v1; heuristic symbol splitting | 04 §7 |
| B8 | `gix` vs `git` CLI for checkpoints | `git` CLI in v1 (t23 allows both); `gix` later for capture speed | 04 §10 |
| B9 | Where the per-provider prompt-tuning table lives | `genie-providers/prompts/` as data, reviewed like catalog patches | 06 §2.1 |
| B10 | Favicon cache | Daemon-side, 7 days, in `cache/`, max 20 MB | 06 §8 |

## 4. Already deferred elsewhere (for completeness)

- Skill scripts in chat and `genie-sandbox` → v2 (D44); open v2 questions: Windows minimum for filtered network, "unrestricted internet" fallback (t09).
- Bedrock → v2 (D26). CEF Genie Browser → v2 (D21). Own search backend → v2 (D11). Plugins and packaged extensions → v2 (D52). ACP and a public SDK → v2 (D51, D22).
- Business model, licence enforcement, source licence, repo host, CI platform → ticket 41.
- Tauri 3 re-check when stable (D8 condition 4).
