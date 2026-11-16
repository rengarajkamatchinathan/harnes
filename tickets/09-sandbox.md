# 09 — Sandboxing

type: wayfinder:research · status: closed · claimed-by: research-agent (2026-11-16) · blocked-by: —

## Question
How do we sandbox tool execution on each OS?

## To decide
- macOS Seatbelt, Linux bubblewrap/landlock, Windows (AppContainer / Job Objects / none)
- Network egress control
- The opencode-pty daemon has no sandbox hooks, so we design them in

## Now on the Chat v1 path (D37)
Skill scripts in chat run in a local sandbox. **v1 requirements** (from 21): Python 3 + Node; read-only skill dir + per-run scratch dir; attached files copied in on request; **no network by default** (per-domain asks); CPU/memory/time limits; no escape; killed with the session; Windows, macOS **and Linux at full support** (D9). Evaluate OS sandboxes (Seatbelt, bubblewrap/landlock/seccomp, AppContainer/restricted token + Job Object) **vs WASM runtimes** (wasmtime + WASI Python/Node, Pyodide), vs a mix. Also decide how interpreters are provided (bundled vs detected) and the size cost.

## References
- OpenCode ch.16: [16-pty-process-daemon.md](../knowledge/opencode/16-pty-process-daemon.md) — read TL;DR, Architecture, Genie takeaways

## Resolution
_Proposed. Awaiting user review._ Full report: [research/sandbox.md](../research/sandbox.md).
- **v1:** a pinned, signed **Genie Runtime** (CPython 3.14 + curated wheels, Node 26 LTS + curated npm), downloaded on first use. It runs under each OS's **native sandbox** through one `genie-sandbox` crate:
  - **macOS:** Seatbelt (deny by default) plus a run-tag cleanup sweep and a memory watchdog.
  - **Linux:** bundled `bwrap` (all namespaces, `--die-with-parent`) plus seccomp and a systemd cgroup scope.
  - **Windows:** MXC ProcessContainer (`mxc-sdk` 1.1.0) where available, else AppContainer + Job Object.
- WASM was rejected: Claude-compatible skills need native wheels and subprocess CLIs (qpdf, poppler, pandoc), and Pyodide is not a boundary on its own.
- **Network is off by default.** Approved domains go through a host **egress proxy** that checks hostnames and resolved IPs. The OS sandbox pins egress to the proxy: one loopback port on macOS, a Unix-socket bridge into an empty network namespace on Linux, a WFP proxy-only rule on Windows.
- **Fallbacks:**
  - **Linux** without userns: Landlock + seccomp, network off.
  - **Windows** without ProcessContainer (Windows 10 or older Windows 11 builds): AppContainer with network off, unless an admin enables a loopback exemption.
  - If no sandbox can be built, `skill_run` is unavailable. It never runs unsandboxed.
- **Code mode:** the same crate with a workspace policy and a spawn hook in the process daemon (→ 07).
- **Effort:** about 18–24 eng-weeks. **Size:** about 120–170 MB download.
- **Needs the user:** the Windows floor for filtered network, runtime download vs bundled, package policy, whether to keep the "unrestricted internet" fallback, and whether to adopt `mxc-sdk`.

### User decision (2026-11-16)
- **Skill scripts in chat are deferred to v2.** Chat v1 skills = instructions + reference files only (scripts are listed with "runs in Code mode"). In Code mode/CLI, scripts run through `shell` under normal permissions (→ 07). This saves ~18–24 engineer-weeks in v1 and amends D37.
- **The design in research/sandbox.md is accepted for when it is built** (v2 chat scripts and the optional Code-mode sandbox): OS-native sandboxes behind `genie-sandbox`, a host proxy for network filtering, no WASM.
  - **Runtime delivery:** download on first script run (~150 MB, admin pre-staging via MSI/MDM/mirror).
  - **Packages:** a fixed, curated, signed set only (no per-skill pip/npm installs).
- Still open for v2: the Windows minimum for filtered network, and an "unrestricted internet for this run" fallback (default: no).
