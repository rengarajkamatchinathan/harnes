# Sandboxing local code execution (ticket 09)

> Research for [tickets/09-sandbox.md](../tickets/09-sandbox.md) · decisions D9, D20, D25, D35, D37 in [WAYFINDER.md](../WAYFINDER.md) · requirements from [tickets/21-skills-commands.md](../tickets/21-skills-commands.md) "Scripts in chat" · 2026-11-16
> Anything marked *(unverified)* was not checked against a primary source in this session. Crate versions come from the crates.io API and runtime sizes from the GitHub/nodejs.org release APIs, both fetched on 2026-11-16.

## TL;DR

- **Recommendation for v1: run real, native CPython and Node inside each OS's own sandbox, with a `genie-sandbox` Rust crate as the single entry point.** The interpreters are a **Genie Runtime** we build, sign and pin. The sandbox is per OS:
  - **macOS:** Seatbelt profile (deny by default) plus a process-tagging cleanup sweep.
  - **Linux:** our own bundled **bubblewrap** (user, PID, IPC and net namespaces, plus a seccomp filter), with resource limits from a cgroup v2 scope.
  - **Windows:** Microsoft's **MXC ProcessContainer** (`mxc-sdk` 1.1.0, MIT) where the OS supports it, otherwise an **AppContainer + Job Object** backend we build ourselves.
- **Network is off by default on every OS.** When a skill's declared domain is approved, traffic goes through a **Genie egress proxy on the host** that checks each hostname. The OS sandbox makes sure the proxy is the only thing the script can reach: Seatbelt allows one loopback port; on Linux an empty network namespace reaches the proxy through a bind-mounted Unix socket; on Windows a WFP egress filter is scoped to the container.
- **Why not WASM:** the skills D37 must run are Claude-compatible, and they assume native Python and Node. Anthropic's own `pdf`/`docx` skills use pip packages (pypdf, pdfplumber, reportlab, pandas), the npm `docx` package, and **subprocess CLIs** (qpdf, poppler `pdftoppm`, pandoc, LibreOffice). WASI CPython has **no numpy/pandas wheels**. Pyodide (314.0.8) has numpy/pandas/matplotlib, but **cannot spawn processes**, needs a JS host, and is **not a security boundary on its own** (CVE-2026-5752). WASM stays as an option for later: a tiny no-dependency "pure compute" lane.
- **Fallbacks:**
  - **Windows** builds without ProcessContainer (Windows 10, or Windows 11 before the August 2026 update) use AppContainer + Job. Network there is **off, or unrestricted after an explicit warning**, unless an admin does a one-time setup that adds a loopback exemption.
  - **Linux** hosts where unprivileged user namespaces are blocked use **Landlock + seccomp + cgroup**, with network off only.
  - If no sandbox can be built, `skill_run` **refuses**. There is no unsandboxed fallback in Chat.
- **Interpreter provisioning:** a **bundled, pinned "Genie Runtime" component, downloaded on first use** (or included in an offline installer).
  - Contents: python-build-standalone CPython 3.14 (23.7–36.9 MB compressed) with a curated set of wheels, and Node 26 LTS (30.6–41.2 MB compressed) with a curated set of npm packages.
  - Estimated total: about **120–170 MB download / 400–550 MB on disk** *(estimate)*.
  - Not used in v1: the user's own Python or Node (versions drift, packages are unknown, and the paths we would have to open up are too broad).
- **Code mode later:** the same crate with a different policy. The workspace is read-write, the system toolchains are read-only, secrets are denied, and the same proxy is used. It sits behind a per-session "Sandbox" setting and is hooked in at the daemon's spawn point (KB ch.16: wrap `program/args` before `create_pipe`).
- **Effort:** about **18–24 engineer-weeks** for v1, including a 2-week spike and a cross-OS hostile-script CI suite. That is roughly 2 engineers for 2.5–3 months, plus about 0.15 FTE ongoing for runtime CVEs and OS changes.
- **Top risks:**
  1. **Windows coverage.** ProcessContainer is brand new and needs Windows 11 24H2+ with the August 2026 update. The fallback has no domain-level egress control without admin.
  2. **Ubuntu 24.04+ AppArmor restriction on user namespaces.** Our .deb/.rpm must install an AppArmor profile for our bundled `bwrap`. AppImage and Flatpak builds can't.
  3. **macOS cleanup is best effort, and Seatbelt is "deprecated".** It is still widely used and still works on macOS 26, but Apple has made no promises.

## 1. Requirements recap (v1, from 21/D37)

| # | Requirement | Notes for the design |
|---|---|---|
| R1 | Python 3 + Node | Must run real-world skills: pandas, numpy, pypdf, pdfplumber, reportlab, openpyxl, python-docx/pptx, Pillow, matplotlib; npm `docx`, `pptxgenjs`, `pdf-lib`. Anthropic's skills also call `qpdf`, `pdftoppm`, `pandoc`, `soffice` as subprocesses (§4.1). |
| R2 | Read-only skill dir + per-run scratch dir | Scratch is the only writable path. HOME and TMP point into it. |
| R3 | Attached files copied in on request | Copy-in (not bind or ACL on the original), so Windows only ever ACLs **our own** dirs. |
| R4 | No other filesystem access | Deny by default. The runtime dir and minimal OS libraries are read-only. |
| R5 | No network by default; per-domain allow | Ask through D25 `SkillRun(network:domain)` for domains declared in frontmatter. |
| R6 | CPU / memory / wall-time limits | e.g. 60 s, 1 GB (21). |
| R7 | No escape through child processes | Children inherit the sandbox **and** are tracked for kill. |
| R8 | Killed with the session | D20: daemon session actor → kill tree. |
| R9 | Windows, macOS, Linux at **full support** (D9) | Each OS gets a primary backend plus a defined degraded mode. No "Linux later". |

## 2. Options per OS

### 2.1 OS-native sandboxes

| | **macOS: Seatbelt** | **Linux: bubblewrap (+seccomp)** | **Linux: Landlock + seccomp (no namespaces)** | **Windows: MXC ProcessContainer** | **Windows: AppContainer + Job Object** | Windows: restricted token / sandbox user + WFP | Windows Sandbox (VM) |
|---|---|---|---|---|---|---|---|
| Mechanism | `sandbox_init` / `sandbox-exec` SBPL profile, inherited by all children, can't be removed | user+mount+PID+IPC+UTS(+net) namespaces, bind-mount allowlist, seccomp-BPF | LSM path rules (ABI 1+), TCP port rules (ABI 4), signal and abstract-socket scoping (ABI 6), UDP (ABI 10) | OS-built "BaseContainer" (PSEC) or AppContainer tiers, per-container WFP, per-container proxy | AppContainer token (no capabilities), ACL grants on our dirs, Job for limits and kill | Codex "elevated" / Anthropic `srt` Windows alpha: dedicated local user, restricted token, WFP or firewall rules keyed on the SID | Hyper-V VM |
| R2/R4 filesystem | ✅ deny-default, subtree rules. SIP still wins | ✅ only the mounts we list exist | ✅ deny-default for fs ops (ABI 1–3 cover most) | ✅ ro/rw/deny lists | ✅ AppContainer sees almost nothing that isn't ACL'd for its SID. Program Files is readable by `ALL APPLICATION PACKAGES` | ⚠️ ACL-based; denied reads harder | ✅ |
| R5 network off | ✅ `(deny network*)` | ✅ `--unshare-net` (empty namespace) | ✅ seccomp blocks inet sockets, or Landlock TCP rules + UDP (ABI 10) | ✅ no capabilities | ✅ no `internetClient` | ✅ firewall / WFP | ⚠️ config |
| R5 per-domain allow | ⚠️ **one loopback port only** (`localhost:<proxy>`); the hostname check is in our proxy | ✅ proxy through a bind-mounted Unix socket + in-ns bridge (srt design), or slirp4netns (MXC) | ❌ Landlock TCP rules are **port-only, no address**; can't pin to a proxy safely | ✅ "Model 2: proxy-only egress", WFP lets the container reach only the proxy's loopback port; **no UAC** | ⚠️ AppContainers **can't reach loopback** without a loopback exemption, which needs admin *(the API's admin requirement is unverified; the CLI needs it)* | ✅ but **admin setup** | ❌ |
| R6 limits | ⚠️ `RLIMIT_CPU` yes; memory needs a watchdog (no cgroups; `RLIMIT_AS` not reliable on macOS *(unverified)*) | ✅ cgroup v2 via the systemd user manager (`MemoryMax`, `CPUQuota`, `TasksMax`); rlimits fallback | ✅ same cgroup / rlimit story | ⚠️ not documented in MXC; can wrap in our own Job *(unverified that nesting works with PSEC)* | ✅ Job: `JOB_MEMORY`, `ACTIVE_PROCESS`, CPU rate cap | ✅ Job | n/a |
| R7/R8 kill tree | ⚠️ **best effort**: no OS process-tree primitive. MXC tags each run with random mach-lookup rules and sweeps; a pgid kill alone loses `setsid` children | ✅ PID namespace: killing its init kills all; `--die-with-parent` | ✅ cgroup `cgroup.kill`, or daemon as `PR_SET_CHILD_SUBREAPER` + pgid | ✅ "child processes stop when the foreground command exits" (Codex doc) | ✅ `KILL_ON_JOB_CLOSE`, no breakaway | ✅ Job | ✅ |
| OS coverage | All supported macOS (11+); still works on macOS 26 | Needs unprivileged userns. **Ubuntu 23.10+/24.04 AppArmor restricts it** unless the `bwrap` path has a profile; distro `bwrap` not always installed | Kernel ≥5.13; network rules need ≥6.7 (ABI 4) *(kernel mapping unverified for ABI ≥7)*. Debian 12 (6.1) has no network rules | **Win 11 24H2/25H2 ≥ build x.9278 (Aug 2026 update)**; 26H1/26H2 too. Win 10 ❌ | Windows 8+ (AppContainer for Win32) | Win 10/11, admin once | Pro/Enterprise only, one instance |
| Install / admin | None | Ship `bwrap` (LGPL, ~60–80 KB *(unverified)*). **Package must install an AppArmor profile** (root, at install) | None | None (comes with the OS) | None | **UAC prompt once**; blocked by some corporate policies (Codex error 1385) | Enable an optional feature (admin) |
| Startup overhead | ms *(unverified)* | ms *(unverified)* | µs | tens of ms *(unverified)* | tens of ms *(unverified)* | tens of ms | seconds |
| Package compat (native Python/Node) | ✅ full | ✅ full | ✅ full | ✅ (native process) | ✅ mostly. Some libraries probe HKCU or the profile dir *(unverified, prototype needed)* | ✅ | ✅ |
| Rust support | No maintained crate fits: `birdcage` 0.8.1 (2024), `nono` 0.82.0 (Landlock+Seatbelt, active), `mxc-sdk`. Generating the profile text ourselves is easy | spawn `bwrap` from `tokio::process`; `seccompiler` 0.5.0, `hakoniwa` 1.8.1 (namespaces+landlock+seccomp, active) | `landlock` 0.4.7 (official), `seccompiler` 0.5.0, `extrasafe` 0.5.1 | **`mxc-sdk` 1.1.0** (MIT, Microsoft, updated 2026-11-14) | `windows` 0.62.2 / `windows-sys` 0.61.2; `win32job` 2.1.0; `rappct` 0.13.3 (AppContainer/LPAC, 1 maintainer) | `windows`; Codex `windows-sandbox-rs` (Apache-2.0) as reference | — |
| Verdict | **v1 primary (macOS)** | **v1 primary (Linux)** | **v1 fallback (Linux)**, network off only | **v1 primary (Windows)** where available | **v1 fallback (Windows)** | Optional "enhanced network" admin setup later | Reject |

Notes:
- **Seatbelt deprecation.** The `sandbox-exec` CLI and the `sandbox_init` header have been marked deprecated since 10.8. They still ship, and Apple's own apps and Chromium still use them. Seatbelt is used by Codex, Anthropic's `srt`, Gemini CLI and MXC. Reports from macOS 26 show the mechanism works. The known Tahoe breakages came from profiles that were too tight (zsh 5.9 reads `hw.*` sysctls). There is no Apple commitment either way: if Apple removed it, every agent tool on the market would break at the same time. The only per-hostname alternative on macOS is a **Network Extension** content filter. That needs a signed System Extension, an entitlement and user approval, which is too heavy for v1.
- **Ubuntu AppArmor.** On 24.04 `kernel.apparmor_restrict_unprivileged_userns=1` by default. The `bwrap-userns-restrict` profile is an optional extra on 24.04 and enabled by default on later releases *(per secondary reports; unverified)*. Codex tells users to load the profile; `srt` tells them to flip the sysctl. Genie should **ship its own `bwrap` at a fixed path and install an AppArmor profile for it from the .deb/.rpm** (the way browsers and Electron apps ship profiles that allow `userns`). This is a ticket-27 packaging item.
- **Windows fallback network.** Without a loopback exemption an AppContainer can't talk to a localhost proxy at all. So the fallback offers three things: no network (default); "unrestricted internet for this run", which grants `internetClient` after a warning naming the domains requested; or a one-time admin "Enable filtered network for skills" helper that registers the loopback exemption for Genie's stable AppContainer SID. Even with the exemption, the container can reach **any** loopback port, so local unauthenticated services become reachable. That is why we prefer ProcessContainer, whose WFP rule pins the proxy port.
- **MXC tiers.** On Windows, ProcessContainer picks BaseContainer (PSEC), then AppContainer+BFS, then AppContainer+DACL. The DACL tier **stamps ACEs on host objects** and undoes them on drop. A secondary analysis says the MXC README warns no profile should yet be treated as a security boundary *(unverified; not found in the current README)*. Genie only calls MXC when the probe reports the PSEC tier, and uses its own AppContainer+Job code otherwise, which only ACLs Genie-owned dirs thanks to copy-in (R3).

### 2.2 WASM runtimes

| | wasmtime + WASI CPython | Pyodide (Emscripten) in Node/Deno | wasmtime + JS engine (QuickJS/StarlingMonkey) |
|---|---|---|---|
| Isolation | ✅ strong (capability-based WASI, no ambient authority) | ⚠️ **Pyodide is not a boundary**: Python can reach the JS host through its FFI (CVE-2026-5752 in a Pyodide-based sandbox, n8n CVE-2025-68668). Needs Deno permissions or an OS sandbox underneath | ✅ strong |
| Packages | ❌ CPython on WASI is tier 2, but **no numpy/pandas WASI wheels**. PEP 816 (accepted) only fixes the WASI SDK version from 3.15; a wheel platform tag is still "planned". Sockets wait for WASI 0.3 | ✅ numpy 2.4.6, pandas 3.0.2, matplotlib, scipy, lxml, Pillow in 314.0.8; pure-Python wheels from PyPI via micropip | ❌ no Node APIs, no npm ecosystem compatibility |
| Subprocesses (qpdf, pandoc, soffice) | ❌ | ❌ | ❌ |
| Network | ✅ none unless the host provides it (easy per-domain host function) | through the JS host's fetch (Deno `--allow-net=domain` works) | ✅ host function |
| Limits | ✅ fuel/epoch interruption, memory cap (wasm32 ≤ 4 GB) | ⚠️ JS host limits | ✅ |
| Size | CPython wasm ~25–30 MB *(unverified)* + wasmtime in the daemon (~10–20 MB *(unverified)*) | core 6.8 MB; full distribution 337.5 MB (bz2); plus Deno 38.8–42.9 MB zip or Node | small |
| Perf | pure Python 1.5–3× slower than native *(unverified)*; startup in seconds without snapshotting | similar; numpy vectorised is near native *(unverified)* | interpreter-speed JS |
| Rust | `wasmtime` / `wasmtime-wasi` 50.0.1 (monthly releases, Bytecode Alliance) | `deno_core` 0.413 / `deno_runtime` 0.268 (embedding is heavy) | `rquickjs` 0.14.0, `boa_engine` 0.22.0 |
| Verdict | Reject for v1 (no packages) | Reject as the primary; would need an OS sandbox anyway | Reject (not Node) |

**Conclusion:** WASM fails R1 for the skills we actually need to run (the Claude-compatible skill corpus, §4.1). Its main advantage, identical behaviour on every OS, comes with a hard ceiling: no subprocesses, no native wheels outside Pyodide, no npm. Revisit it for a "lightweight compute" lane, or when WASI 0.3 and a WASI wheel tag land.

### 2.3 Lightweight VMs and containers

| | Apple Virtualization.framework | WSL2 | Firecracker | gVisor (runsc) | Docker/Podman |
|---|---|---|---|---|---|
| Fit for a desktop app on all 3 OSes | macOS only; needs `com.apple.security.virtualization`, a Linux kernel + rootfs image we ship (100s of MB), and boots in ~1–2 s *(unverified)* | Windows only; enabling the feature needs **admin + reboot**, often off in enterprises | **Linux only** (KVM); nothing for macOS/Windows | Linux only; usually used with Docker | Requires a user-installed daemon; the Docker socket itself was a sandbox-escape vector (Pillar 2026) |
| Isolation | strongest | strong | strongest | strong | medium |
| Verdict | Possible **"heavy mode" v2+** (e.g. running LibreOffice from an image) | Opt-in v2+ | Reject | Reject | Reject for v1 (Gemini CLI offers it as opt-in) |

MXC also wraps several of these (`windows_sandbox`, `wslc`, `microvm`, `hyperlight`, `lxc`), most of them experimental. If Genie adopts `mxc-sdk` on Windows, a VM tier could later become a configuration change rather than new code.

### 2.4 Hybrids

- **(d1) WASM for Python by default, OS sandbox for heavy work.** Two code paths and two package sets. Users get confusing "works here, not there" behaviour, and every pandas+qpdf skill lands on the heavy path anyway. **Reject.**
- **(d2) Deno for JS + Pyodide-in-Deno for Python, with an OS sandbox underneath.** One runtime, built-in `--allow-net=domain`. But it isn't Node (skills expect `node` and `npm` packages; Deno's Node compat is good but not complete *(unverified)*) and it has no subprocess CLIs. **Reject for v1.**
- **(d3) Native interpreters + OS sandbox + runtime-level seat belts** (`python -I`, Node `--permission` as defence in depth). **Recommended.** Node's own docs say its permission model "does not protect against malicious code", so it is only a second layer and never the boundary.

## 3. How other tools do it

| Tool | macOS | Linux | Windows | Network | Known weaknesses |
|---|---|---|---|---|---|
| **Anthropic sandbox-runtime (`srt`)**, used by Claude Code (Bash/PowerShell tool) | `sandbox-exec` with a generated profile; outbound only to the proxy's localhost port | `bwrap`, network namespace removed; host proxies reached through **bind-mounted Unix sockets bridged with `socat`**; seccomp blocks Unix sockets (x86-64/arm64) | **Alpha:** a dedicated `srt-sandbox` local user + **WFP egress fence keyed on its SID** + write-restricted token + ACEs; one-time elevated install. Claude Code docs still say native Windows is unsandboxed (use WSL2) | Host HTTP + SOCKS5 proxies; `allowedDomains`/`deniedDomains` with wildcards and ports; **refuses names resolving to private/loopback/link-local/metadata**; optional TLS termination | Reads are allowed everywhere by default (`~/.ssh` is readable unless denied); escape hatch `dangerouslyDisableSandbox` (configurable); `allowLocalBinding` exposes localhost services; Linux deny globs only literal and expanded at wrap time; Ubuntu 24.04 needs the userns sysctl; Windows OS-TLS clients fail; CVE-2026-25725 (incomplete bwrap read-only protection, per Cymulate); Ona showed the agent **path-aliasing a denylist** (`/proc/self/root/usr/bin/npx`) and then turning off its own sandbox |
| **OpenAI Codex CLI** | Seatbelt | `bwrap` (first on PATH, else a bundled helper that needs userns); earlier releases used Landlock + seccomp *(unverified)*. Startup warning if userns is blocked; Ubuntu 24.04 needs the AppArmor profile | Three tiers: **MXC** (recommended; Win 11 24H2/25H2; no admin, no extra accounts or firewall rules); **elevated** (sandbox users `CodexSandboxOffline/Online`, firewall rules, DPAPI-stored passwords, admin setup); **unelevated** (restricted token from the current user + ACLs; offline only through an env var, which is weak). Private desktop for UI isolation | Modes `read-only` / `workspace-write` / `danger-full-access`; no local domain allowlist documented | "GitPwned": the allowlist trusted `git show` without checking its arguments (fixed in v0.95.0); config-based escapes; deny-read globs evaluated only at start |
| **Gemini CLI** | Seatbelt profiles `permissive-/restrictive-/strict-` × `open/proxied` (default `permissive-open`: broad reads, network open) | Docker/Podman, optional gVisor `runsc`, LXC (experimental) | "Low" integrity level set with `icacls` on writable paths (persists after the session) | `-proxied` profiles route through a proxy | **Opt-in**, permissive by default; Docker socket reachable (fixed GHSA-v4xv-rqh3-w9mc); the integrity-level changes persist on disk |
| **Microsoft MXC** (also used by GitHub Copilot CLI per press reports *(unverified)*) | Seatbelt; **tags each run with random mach-lookup rules and sweeps** to kill escaped children | bwrap, deny-default baseline, socket-family seccomp, io_uring blocked, proxy through slirp4netns + in-ns iptables (many host tools needed) | ProcessContainer (above) | Proxy-only egress or IP/CIDR WFP rules; hostname filtering is the caller's proxy's job | Young (repo created Feb 2026); DACL tier modifies host ACLs; macOS cleanup best effort; Linux proxy mode has heavy dependencies |
| **Cursor / Antigravity** *(from Pillar's write-up only)* | Seatbelt (Antigravity's was a deny-list profile → escape) | — | — | — | Workspace files run by trusted host components (venv interpreter, git fsmonitor, `.vscode` tasks, hook configs) |

**Lessons for Genie:**
1. **Deny by default, including reads.** srt and Gemini default to "read everything", and Antigravity's denylist profile was escaped.
2. **The model must not be able to turn the sandbox off.** Chat `skill_run` has no unsandboxed retry.
3. **Outputs are data, never auto-executed by the host.** Scratch files come back as attachments and are never loaded as config, hooks or interpreters.
4. **No path-string denylists.** Use mount and ACL allowlists.
5. **Local daemons are escape hatches.** The Genie daemon's socket/pipe and token file must be invisible to the sandbox, and so must the Docker socket.
6. **Check resolved addresses in the proxy** (SSRF guard shared with D35 `web_fetch`).

## 4. Interpreter provisioning

### 4.1 What skills actually need

Anthropic's public `pdf` skill uses `pypdf`, `pdfplumber`, `pandas`, `reportlab`, `pytesseract`, `pdf2image` and the CLIs `qpdf`, `pdftotext`/`pdfimages` (poppler) and optionally `pdftk`/Tesseract. The `docx` skill uses the npm `docx` package, `pandoc`, LibreOffice `soffice`, `pdftoppm` and `zip`. Users will import these skills (D37 is Claude-compatible), so **native interpreters with native wheels plus the ability to exec helper CLIs** are hard requirements, not nice-to-haves.

### 4.2 Options

| | **Bundled "Genie Runtime" (recommended)** | Detect user's Python/Node | WASM build |
|---|---|---|---|
| Contents | python-build-standalone **CPython 3.14.10** `install_only_stripped`: win-x64 23.7 MB, mac-arm64 27.0 MB, linux-x64 36.9 MB (gz). **Node 26.13.1 LTS**: win-x64 41.2 MB zip, darwin-arm64 30.6 MB xz, linux-x64 34.1 MB xz. Curated wheels (numpy, pandas, pypdf, pdfplumber, reportlab, openpyxl, python-docx, python-pptx, Pillow, matplotlib, lxml) ≈ 60–90 MB compressed *(estimate)*; npm set (`docx`, `pptxgenjs`, `pdf-lib`) a few MB | Whatever is installed | Pyodide full 337.5 MB, or WASI CPython without packages |
| Total | **≈120–170 MB download, 400–550 MB on disk per platform** *(estimate)* | 0 | — |
| Reproducibility | ✅ pinned, same on every machine; skills can be tested against it | ❌ version drift, missing packages, Store-shim `python.exe` on Windows, conda/pyenv layouts | ✅ |
| Sandbox fit | ✅ one known read-only path to allow (and ACL on Windows) | ❌ must open site-packages, user dirs, conda envs → breaks R4 | ✅ |
| Signing | macOS: sign every Mach-O (`python3`, `node`, every `.so`/`.dylib`) with hardened runtime and notarise; extension modules loaded later may need `disable-library-validation` *(unverified)*. Windows: Authenticode on `.exe`/`.dll`/`.pyd`/`.node`. Linux: package or detached signature | n/a | one artifact |
| Updates | Its own signed component channel (same pattern as the browser report's "download on first use"); monthly Python/Node security patch rebuilds | User's job | Ours |
| Licences | PSF, MIT (Node), BSD/MIT etc. for wheels; a notice file is needed (→ 35) | n/a | — |

**Recommendation:** use the bundled Genie Runtime.
- **Delivery:** it is downloaded the first time `skill_run` is used ("Setting up the skill runtime…"). Offline and enterprise installers include it. It installs per user under the Genie data dir and is read-only to the sandbox.
- **Built-in skills** (21 §Sources 3) are tested against the pinned runtime.
- **Extra pip/npm packages** follow two rules:
  - v1 allows skill-declared extra packages only if they install **into a per-skill cache through the egress proxy**, with PyPI or npm domains asked like any other domain. This is an open question.
  - System CLIs (qpdf, poppler, pandoc, LibreOffice) are **detected on the host and exposed read+exec** inside the sandbox only when the skill declares them. Whether to bundle the small ones (qpdf, poppler) is an open question.
- **System interpreters:** "use system Python/Node" stays out of v1. If it is added later, it is an advanced toggle with explicit extra read paths.

## 5. Network egress control

| Approach | macOS | Linux | Windows | Comment |
|---|---|---|---|---|
| **No network at all** (default) | `(deny network*)` | `--unshare-net` (empty namespace, `lo` only) | ProcessContainer egress deny / AppContainer without capabilities | Simple and robust everywhere. **v1 default.** |
| **Host proxy + OS pinning to the proxy** (recommended for allowed domains) | Allow `network-outbound` only to `localhost:<proxy-port>` (note: Seatbelt's `localhost` also matches the host's other addresses on that port) | Unix socket of the proxy bind-mounted in; a tiny static `genie-netbridge` inside the namespace listens on `127.0.0.1:3128` and forwards (srt does this with socat; we ship our own to avoid the dependency) | ProcessContainer "Model 2" (WFP pins to the proxy loopback port, AppContainer-profile peer identity); fallback: loopback exemption (admin) or none | Enforcement = OS stops everything except the proxy; policy = proxy checks hostnames. Clients that ignore `HTTP(S)_PROXY` **fail closed**, they don't escape |
| IP/CIDR firewall rules | ❌ | slirp4netns + in-namespace iptables (MXC) | WFP rules (MXC Model 1) | CDNs make domain→IP mapping unstable; DNS inside the sandbox is a leak channel. Not for v1 |
| Runtime permission flags (Deno `--allow-net=host`, Node `--allow-net`) | | | | Not a boundary (Node docs); only a second layer |
| Network Extension / WFP callout driver | NE filter needs a system extension + entitlement | — | Kernel driver signing | v2+ if per-host filtering without a proxy is ever needed |

**Genie egress proxy design (lives in the daemon or in a small sidecar per user):**
- One listener per run, with a policy per run: the set of approved domains with ports (default 443 only, plus 80 if declared). An HTTP proxy handles `CONNECT` for TLS and absolute-URI plain HTTP. SOCKS5 is optional and deferred (srt has it; Python and Node clients mostly use `HTTP(S)_PROXY`).
- **The proxy resolves DNS.** The sandbox has no DNS at all. After resolving, the proxy **rejects loopback, private, link-local, CGNAT and metadata addresses** (re-using the D35 `web_fetch` SSRF guard), and pins the resolved IP for the connection to defeat DNS rebinding.
- There is **no TLS interception** (no CA injection, no pinning breakage). Hostname policy uses the `CONNECT` authority, with a check that the TLS SNI matches *(design choice; peeking at SNI is cheap)*.
- **Asks:** domains are declared in skill frontmatter. Before the run starts, Genie asks per domain (D25 `SkillRun(network:domain)`, "once" or "always for this skill"). Undeclared domains are **denied immediately** and logged into the tool result (`network: blocked example.com`), so the model can tell the user. Pausing a live socket for a mid-run prompt is deferred to Code mode.
- **Env injection:**
  - Set `HTTP_PROXY`, `HTTPS_PROXY`, `ALL_PROXY` and their lowercase forms. Never set `NO_PROXY`.
  - Node needs `NODE_USE_ENV_PROXY=1` for `fetch`/`http` to honour these *(available in recent Node LTS; unverified exact version)*.
  - Python `requests`, `urllib` and `httpx` honour the env vars by default. `aiohttp` needs `trust_env` *(unverified)*.
- **Accounting:** bytes in/out are counted per run and logged, which feeds the Usage/Developer panes.

## 6. Recommendation

### 6.1 v1 design: `genie-sandbox` crate

```
skill_run ─► SkillRunner (daemon session actor, D20)
              ├─ stage: scratch dir (per run), copy-in requested attachments, write policy
              ├─ RuntimeManager: ensure Genie Runtime vX (download/verify/sign-check on first use)
              ├─ EgressProxy (only if approved domains) ─ per-run listener + policy
              ├─ Sandbox::spawn(policy, argv, env) ──► backend per OS
              │     macOS  : Seatbelt profile (generated SBPL) + rlimits + RSS watchdog + run-tag sweep
              │     Linux  : bundled bwrap (--unshare-all [--share-net never], --die-with-parent,
              │              --clearenv, ro binds: runtime, skill dir, minimal /usr,/lib,/etc subset;
              │              rw: scratch) + seccomp (socket families, io_uring, ptrace, keyctl,
              │              unshare/clone NEWUSER) + systemd transient scope (MemoryMax, CPUQuota, TasksMax)
              │              fallback: Landlock(best ABI) + seccomp + cgroup/subreaper; network off only
              │     Windows: mxc-sdk ProcessContainer when probe = PSEC tier
              │              fallback: AppContainer (Genie profile SID, no caps) + ACL on our dirs only
              │              + Job (KILL_ON_JOB_CLOSE, JOB_MEMORY, ACTIVE_PROCESS, CPU rate, no breakaway)
              ├─ wall-clock timer → kill tree; session close/cancel → kill tree (R8)
              └─ collect: scratch/out/** (no symlink follow, size caps) → attachments/artifacts
```

**Policy defaults:**
- **Filesystem:** the skill dir is ro, the runtime is ro, minimal OS libraries are ro, scratch is rw. HOME, TMP and `XDG_*` point into scratch.
- **Environment:** a **clean environment** with no API keys and no `SSH_AUTH_SOCK`. Python runs with `python -I`; `PYTHONNOUSERSITE=1`; `NODE_OPTIONS` is cleared.
- **Limits:** 60 s wall, 1 GB memory, 2 CPU cores' worth, 64 processes, 512 MB scratch.
- **Network:** off.
- **Hardening:** daemon socket/pipe and token never visible. Each backend probes itself at daemon start and reports to Settings → Developer → doctor (D34).

**Degraded-mode rules (D9 "full support"):**
- Every OS has a working v1 path with **network off**.
- Per-domain network needs one of the following:
  - macOS: always available.
  - Linux: bwrap available.
  - Windows: ProcessContainer, or the admin loopback setup.
- Otherwise, a skill that declares domains gets a clear prompt: "This skill needs internet access to X. Filtered access isn't available on this system. Run without network / Allow unrestricted internet for this run / Cancel". Admin policy can remove the "unrestricted" choice (→ 28).
- If no sandbox can be built at all (e.g. no userns *and* Landlock disabled), `skill_run` is **unavailable**, never unsandboxed.

**Why we write our own code for macOS/Linux instead of adopting `mxc-sdk` everywhere:**
- MXC's Linux proxy mode needs slirp4netns, iptables, nsenter and more on the host. srt's Unix-socket bridge needs none of these.
- On macOS, profile generation is about 300 lines.
- MXC does not document resource limits.
- Keeping our own trait lets us swap backends. The spike should still measure whether `mxc-sdk` on macOS/Linux is good enough to save that work.

### 6.2 Code-mode extension path (→ 07)

- The same `Sandbox` trait and proxy, with a **workspace policy**:
  - **Filesystem:** the workspace is rw; system toolchains (`/usr`, Homebrew, `%ProgramFiles%`, the user's language toolchains) are ro. Secrets (`~/.ssh`, `~/.aws`, keychains, `.env*` by rule) are denied. Writes to `.genie/`, `.git/hooks`, `.git/config`, `.vscode/tasks.json` and hook configs are denied (Pillar lessons).
- **Network:** the same proxy, with D25 asks per domain and a live ask mid-command (Code has an interactive user).
- **Wiring:** the opencode-pty-style process daemon gets a `spawn_hook` that wraps `program/args` (KB ch.16 takeaway).
  - Linux and macOS wrap with `bwrap` / Seatbelt.
  - Windows uses ProcessContainer through `mxc-sdk`'s spawn with streaming stdio.
  - PTYs: ConPTY under ProcessContainer *(unverified, test needed)*.
- **Setting:** `sandbox: off | on | strict`, admin-lockable (28). "Off" is the default in Code until the spike shows low friction *(open question)*.
- **Hard parts to plan for:**
  - Toolchains that need broad reads (Docker, package caches).
  - The Windows TLS-through-OS-stack issue in srt's sandbox-user approach (another reason to prefer ProcessContainer).
  - Watchman-style daemons.
  - Nested sandboxes (Chromium/Electron tests).

### 6.3 Effort (rough)

| Work item | Eng-weeks |
|---|---|
| Spike: AppContainer/ProcessContainer with pandas+Node on Win 10/11, bwrap AppArmor on Ubuntu 24.04/26.04, Seatbelt on macOS 14–26, `mxc-sdk` evaluation | 2 |
| Core crate: policy model, staging/copy-in/out, limits, kill semantics, doctor probes | 2 |
| macOS backend (SBPL, watchdog, run-tag sweep) | 2 |
| Linux backend (bundled bwrap, seccomp, systemd scope via `zbus`, netbridge helper, Landlock fallback) | 3–4 |
| Windows backend (`mxc-sdk` integration + AppContainer/Job fallback + optional admin loopback helper) | 3–4 |
| Egress proxy (CONNECT/HTTP, SSRF guard, ask integration, accounting) | 1.5–2 |
| Genie Runtime pipeline (build Python+wheels / Node+npm for 6 targets, sign/notarise, component channel, updater) | 3–4 |
| Hostile-script CI suite on all 3 OSes (fs escape, symlink tricks, fork bombs, setsid, raw sockets, DNS, proxy bypass, daemon socket probing) | 1.5–2 |
| **Total** | **≈18–24** (2 engineers ≈ 2.5–3 months), plus ≈0.15 FTE ongoing |

Packaging work is in ticket 27, not in this estimate: AppArmor profile in the .deb/.rpm, plus runtime notices. `skill_run` tool/UX wiring is ticket 21.

## 7. Risks

1. **Windows coverage and maturity.** ProcessContainer requires Windows 11 24H2/25H2 with the Aug 2026 cumulative update (build ≥ x.9278). `mxc-sdk` is 9 months old. Windows 10 and older Windows 11 builds get the AppContainer fallback, which has no domain-filtered network without admin. AppContainer may also break some Python/Node packages (registry or profile probes) *(unverified)*.
2. **Linux user-namespace policy.** Ubuntu's AppArmor restriction, hardened distros (`kernel.unprivileged_userns_clone=0`, CIS profiles) and Flatpak/Snap packaging (nested userns is blocked) can all disable bwrap. Mitigations: the .deb/.rpm installs an AppArmor profile, the Landlock fallback keeps scripts running (network off), and the doctor explains the cause.
3. **macOS: no OS process-tree kill, and Seatbelt is formally deprecated.** Cleanup is a best-effort sweep. Memory limits need a watchdog. Apple could change SBPL semantics in any release (macOS 26 already broke tight profiles).
4. **Runtime supply chain and size.** Genie becomes a Python/Node redistributor: about 150 MB extra, CVE patch duty, notarising thousands of `.so` files, and licence notices.
5. **Proxy-cooperative clients.** Tools that ignore proxy env vars simply fail (safe, but users will see confusing errors). Allowed domains can still be used to exfiltrate data, e.g. an allowed `github.com` gist. The UI must show the domains clearly.
6. **Host components treating scratch outputs as trusted.** Outputs must never be auto-opened or executed. Artifacts render them in the D28 iframe sandbox.
7. **Antivirus and EDR.** Defender or EDR products may slow down or quarantine the unpacked runtime, or flag AppContainer launches *(unverified)*.

## 8. Open questions (need the user)

1. **Windows floor.** Is the Windows target "Windows 11 24H2+ updated" for filtered network, with Windows 10 and older Windows 11 builds as network-off only? Or must filtered network work on Windows 10 too, which would mean shipping an admin setup step (sandbox user + WFP, like Codex "elevated" or srt)?
2. **Runtime delivery.** Download on first use (smaller installer, recommended), or always bundled (+≈150 MB installer, works offline)?
3. **Package policy.** Curated set only, or per-skill `pip`/`npm` installs through the proxy (PyPI/npm domains asked)? Bundle qpdf/poppler, and leave pandoc/LibreOffice to the host?
4. **Fallback "unrestricted internet for this run".** Should it exist at all, or should we always refuse when filtered network isn't available? It matters for admins (→ 28).
5. **Adopt `mxc-sdk`.** Adopt it (Microsoft dependency, MIT) for Windows only, as recommended, or also for macOS/Linux if the spike shows parity?
6. **Default limits.** 60 s / 1 GB / 64 processes are proposed. Do we need a per-skill override in frontmatter?

## Sources

- Anthropic sandbox-runtime (srt) README: https://github.com/anthropic-experimental/sandbox-runtime
- Claude Code sandboxing docs: https://code.claude.com/docs/en/sandboxing
- Codex sandboxing: https://learn.chatgpt.com/codex/sandboxing
- Codex Windows sandbox: https://learn.chatgpt.com/docs/windows/windows-sandbox.md (from https://developers.openai.com/codex/windows.md)
- Codex Windows sandbox internals (secondary): https://codex.danielvaughan.com/2026/05/14/codex-cli-windows-sandbox-engineering-restricted-tokens-acls-elevated-architecture/
- Gemini CLI sandbox docs: https://github.com/google-gemini/gemini-cli/blob/main/docs/cli/sandbox.md
- Microsoft MXC repo: https://github.com/microsoft/mxc · wiki: https://github.com/microsoft/mxc/wiki
- MXC ProcessContainer networking: https://github.com/microsoft/mxc/blob/main/docs/backends/process-container/networking.md
- MXC ProcessContainer OS support: https://github.com/microsoft/mxc/blob/main/docs/backends/process-container/os-version-support.md
- MXC bubblewrap backend: https://github.com/microsoft/mxc/blob/main/docs/backends/bwrap/bubblewrap-backend.md
- MXC Seatbelt backend: https://github.com/microsoft/mxc/blob/main/docs/backends/seatbelt/seatbelt-backend.md
- MXC internals (secondary): https://www.originhq.com/research/mxc-execution-containers-internals
- MXC coverage (secondary): https://www.helpnetsecurity.com/2026/07/07/microsoft-execution-containers-ai-agents-constraints/
- Landlock kernel docs: https://docs.kernel.org/userspace-api/landlock.html
- Ubuntu AppArmor userns restriction: https://discourse.ubuntu.com/t/understanding-apparmor-user-namespace-restriction/58007 · VS Code issue: https://github.com/microsoft/vscode/issues/316046 · Launchpad: https://bugs.launchpad.net/bugs/2046477
- AppContainer loopback: https://learn.microsoft.com/en-us/windows/win32/api/netfw/nf-netfw-networkisolationsetappcontainerconfig · https://learn.microsoft.com/en-us/previous-versions/windows/apps/hh780593(v=win.10)
- Seatbelt on macOS 26 reports: https://forum.cursor.com/t/sandbox-fails-to-initialize-for-zsh-on-macos-26-tahoe-darwin25-zsh-5-9-reads-unwhitelisted-hw-sysctls/161023 · https://hn.svelte.dev/item/44283454
- Node permission model: https://nodejs.org/api/permissions.html
- Deno security: https://docs.deno.com/runtime/fundamentals/security/
- Pyodide packages (314.0.8): https://pyodide.org/en/stable/usage/packages-in-pyodide.html · releases: https://github.com/pyodide/pyodide/releases
- CPython WASI status (Mar 2026): https://snarky.ca/state-of-wasi-support-for-cpython-march-2026/
- Pyodide sandbox escape CVE-2026-5752 (secondary): https://blog.barrack.ai/pyodide-sandbox-escape-cohere-terrarium-openai-codex/
- python-build-standalone releases: https://github.com/astral-sh/python-build-standalone/releases
- Node.js dist index: https://nodejs.org/dist/index.json
- Bubblewrap releases (v0.13.1): https://github.com/containers/bubblewrap/releases
- Pillar Security, cross-tool sandbox escapes: https://www.pillar.security/blog/the-week-of-sandbox-escapes
- Ona, denylist/sandbox escape (via mirror): https://hn.jakechampion.name/reader/https://ona.com/stories/how-claude-code-escapes-its-own-denylist-and-sandbox
- Cymulate, CVE-2026-25725: https://cymulate.com/blog/the-race-to-ship-ai-tools-left-security-behind-part-1-sandbox-escape/
- Anthropic skills (dependencies): https://github.com/anthropics/skills/blob/main/skills/pdf/SKILL.md · https://github.com/anthropics/skills/blob/main/skills/docx/SKILL.md
- Crates (crates.io API, 2026-11-16): `mxc-sdk` 1.1.0, `wasmtime` 50.0.1, `landlock` 0.4.7, `seccompiler` 0.5.0, `hakoniwa` 1.8.1, `nono` 0.82.0, `birdcage` 0.8.1, `extrasafe` 0.5.1, `windows` 0.62.2, `windows-sys` 0.61.2, `win32job` 2.1.0, `rappct` 0.13.3, `process-wrap` 10.0.1, `rquickjs` 0.14.0, `boa_engine` 0.22.0, `deno_core` 0.413.0 — https://crates.io/
- Repo context: [knowledge/opencode/16-pty-process-daemon.md](../knowledge/opencode/16-pty-process-daemon.md) (Genie takeaways: no sandbox hooks; wrap before `create_pipe`; Job Objects exist), [knowledge/opencode/09-tools.md](../knowledge/opencode/09-tools.md)
