//! Client-side `ensure` (01 §4.1):
//!
//! ```text
//! Connect ──open──▶ Handshake ──ok──▶ Ready
//!    │                  └─skew──▶ daemon drains + restarts ──▶ Connect
//!    │                  └─skew, daemon busy──▶ Degraded / BusyUpdating
//!    └─not running──▶ Spawn (detached `genie daemon`) ──▶ WaitReady (50 ms → 1 s) ──▶ Connect
//!                                                          └─15 s──▶ Failed (one auto retry)
//! ```
//!
//! Spawn race: the daemon takes `daemon.lock` before binding; a loser exits with
//! `already_running` and this client simply connects. A socket whose lock is not held is
//! stale and is removed before spawning.

use std::ffi::OsString;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use genie_protocol::{ErrorKind, version};

use crate::connection::{ClientError, ClientInfo, Connection};
use crate::endpoint::Endpoint;
use crate::framing;

/// 01 §4.1, SA-03.
pub const READY_TIMEOUT: Duration = Duration::from_secs(15);
const BACKOFF_START: Duration = Duration::from_millis(50);
const BACKOFF_MAX: Duration = Duration::from_secs(1);
/// Max drain-and-restart cycles in one ensure before giving up.
const MAX_RESTARTS: u32 = 2;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EnsureState {
    Connect,
    /// "Starting engine…"
    Spawn,
    WaitReady,
    Handshake,
    /// Waiting for an older daemon to drain and exit.
    Restarting,
    Ready,
}

#[derive(Clone)]
pub struct EnsureOptions {
    pub endpoint: Endpoint,
    pub client: ClientInfo,
    /// Binary started when no daemon is running (the CLI's own exe, or the desktop's
    /// bundled `genie`). `None`: never spawn (standalone).
    pub daemon_exe: Option<PathBuf>,
    pub daemon_args: Vec<OsString>,
    pub ready_timeout: Duration,
    /// Automatic retries before reporting Failed (SA-03: one).
    pub auto_retries: u32,
}

impl EnsureOptions {
    pub fn new(
        endpoint: Endpoint,
        client: ClientInfo,
        daemon_exe: Option<PathBuf>,
        daemon_args: Vec<OsString>,
    ) -> Self {
        EnsureOptions {
            endpoint,
            client,
            daemon_exe,
            daemon_args,
            ready_timeout: READY_TIMEOUT,
            auto_retries: 1,
        }
    }
}

pub enum EnsureOutcome {
    Ready(Connection),
    /// Daemon is older and busy; same protocol major. Shows "Engine will update when
    /// tasks finish".
    Degraded(Connection),
    /// Daemon is older, busy, and speaks another major: "Genie is finishing N tasks
    /// before updating" with **Stop tasks and update**.
    BusyUpdating {
        running: u32,
    },
}

pub async fn ensure(
    opts: &EnsureOptions,
    on_state: impl Fn(EnsureState),
) -> Result<EnsureOutcome, ClientError> {
    let mut attempt = 0;
    loop {
        match try_once(opts, &on_state).await {
            Err(e @ (ClientError::Timeout(_) | ClientError::Spawn(_)))
                if attempt < opts.auto_retries =>
            {
                attempt += 1;
                tracing::warn!(error = %e, attempt, "ensure failed; retrying");
            }
            other => return other,
        }
    }
}

async fn try_once(
    opts: &EnsureOptions,
    on_state: &impl Fn(EnsureState),
) -> Result<EnsureOutcome, ClientError> {
    let ep = &opts.endpoint;
    let mut deadline: Option<Instant> = None;
    let mut backoff = BACKOFF_START;
    let mut restarts = 0;
    loop {
        on_state(EnsureState::Connect);
        match framing::connect(&ep.address).await {
            Ok(stream) => {
                on_state(EnsureState::Handshake);
                let token = ep.read_token()?;
                match Connection::handshake(stream, &opts.client, token).await {
                    Ok(conn) if conn.init.restart_pending == Some(true) => {
                        restarts += 1;
                        if restarts > MAX_RESTARTS {
                            return Err(ClientError::Spawn(
                                "daemon keeps asking for a restart".into(),
                            ));
                        }
                        on_state(EnsureState::Restarting);
                        // The daemon drains and exits; wait for EOF, then spawn our binary.
                        let _ = tokio::time::timeout(Duration::from_secs(10), conn.closed()).await;
                        drop(conn);
                        deadline = None;
                        backoff = BACKOFF_START;
                        continue;
                    }
                    Ok(conn) => {
                        on_state(EnsureState::Ready);
                        let older_daemon = version::decide_skew(
                            &conn.init.engine_version,
                            &conn.init.protocol,
                            &opts.client.version,
                            genie_protocol::PROTOCOL_VERSION,
                            true,
                        ) == version::SkewDecision::Degraded;
                        return Ok(if older_daemon {
                            EnsureOutcome::Degraded(conn)
                        } else {
                            EnsureOutcome::Ready(conn)
                        });
                    }
                    Err(ClientError::Rpc(e)) if e.kind() == ErrorKind::Busy => {
                        let running = e
                            .data
                            .as_ref()
                            .and_then(|d| d.get("running"))
                            .and_then(serde_json::Value::as_u64)
                            .and_then(|n| u32::try_from(n).ok())
                            .unwrap_or(0);
                        return Ok(EnsureOutcome::BusyUpdating { running });
                    }
                    // The daemon died between connect and handshake (e.g. drain): retry.
                    Err(ClientError::Closed | ClientError::Io(_))
                        if deadline.is_some_and(|d| Instant::now() < d) => {}
                    Err(e) => return Err(e),
                }
            }
            Err(e) if framing::is_not_running(&e) => {
                if deadline.is_none() {
                    deadline = Some(Instant::now() + opts.ready_timeout);
                    if ep.lock_held() {
                        // Another client's daemon is starting: just wait for it.
                        on_state(EnsureState::WaitReady);
                    } else {
                        remove_stale_socket(ep);
                        match &opts.daemon_exe {
                            Some(exe) => {
                                on_state(EnsureState::Spawn);
                                spawn_detached(exe, &opts.daemon_args)?;
                            }
                            None => on_state(EnsureState::WaitReady),
                        }
                    }
                }
            }
            Err(e) => return Err(ClientError::Io(e)),
        }
        if deadline.is_some_and(|d| Instant::now() >= d) {
            return Err(ClientError::Timeout("engine did not become ready"));
        }
        on_state(EnsureState::WaitReady);
        tokio::time::sleep(backoff).await;
        backoff = (backoff * 2).min(BACKOFF_MAX);
    }
}

fn remove_stale_socket(ep: &Endpoint) {
    #[cfg(unix)]
    if let crate::Address::Unix(p) = &ep.address
        && p.exists()
    {
        tracing::info!(path = %p.display(), "removing stale daemon socket");
        let _ = std::fs::remove_file(p);
    }
    #[cfg(windows)]
    let _ = ep; // Pipes vanish with their server; nothing to clean.
}

/// Start `genie daemon` detached from this process (and its console / job).
pub fn spawn_detached(exe: &std::path::Path, args: &[OsString]) -> Result<(), ClientError> {
    use std::process::{Command, Stdio};
    let mut cmd = Command::new(exe);
    cmd.args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        cmd.process_group(0);
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const DETACHED_PROCESS: u32 = 0x0000_0008;
        const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
        const CREATE_BREAKAWAY_FROM_JOB: u32 = 0x0100_0000;
        // Break away from a terminal's job so closing the terminal doesn't kill the daemon;
        // fall back when the job forbids breakaway.
        cmd.creation_flags(DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP | CREATE_BREAKAWAY_FROM_JOB);
        let spawned = without_inheritable_std_handles(|| {
            if cmd.spawn().is_ok() {
                return Ok(());
            }
            cmd.creation_flags(DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP);
            cmd.spawn().map(drop)
        });
        spawned.map_err(|e| ClientError::Spawn(format!("{}: {e}", exe.display())))
    }
    #[cfg(not(windows))]
    cmd.spawn()
        .map(drop)
        .map_err(|e| ClientError::Spawn(format!("{}: {e}", exe.display())))
}

/// `CreateProcess` with `bInheritHandles` (what std uses) passes *every* inheritable
/// handle to the child, including this process's stdout pipe. A long-lived daemon would
/// then hold e.g. `genie status | jq`'s pipe open and the pipeline would never end.
/// Clear the inherit flag on our std handles for the duration of the spawn.
#[cfg(windows)]
fn without_inheritable_std_handles<T>(f: impl FnOnce() -> T) -> T {
    use windows_sys::Win32::Foundation::{
        GetHandleInformation, HANDLE_FLAG_INHERIT, INVALID_HANDLE_VALUE, SetHandleInformation,
    };
    use windows_sys::Win32::System::Console::{
        GetStdHandle, STD_ERROR_HANDLE, STD_INPUT_HANDLE, STD_OUTPUT_HANDLE,
    };
    let mut cleared = Vec::new();
    for id in [STD_INPUT_HANDLE, STD_OUTPUT_HANDLE, STD_ERROR_HANDLE] {
        // SAFETY: plain handle queries/updates on this process's own std handles.
        unsafe {
            let h = GetStdHandle(id);
            if h.is_null() || h == INVALID_HANDLE_VALUE {
                continue;
            }
            let mut flags = 0u32;
            if GetHandleInformation(h, &mut flags) != 0
                && flags & HANDLE_FLAG_INHERIT != 0
                && SetHandleInformation(h, HANDLE_FLAG_INHERIT, 0) != 0
            {
                cleared.push(h);
            }
        }
    }
    let out = f();
    for h in cleared {
        // SAFETY: restoring the flag we cleared above.
        unsafe { SetHandleInformation(h, HANDLE_FLAG_INHERIT, HANDLE_FLAG_INHERIT) };
    }
    out
}
