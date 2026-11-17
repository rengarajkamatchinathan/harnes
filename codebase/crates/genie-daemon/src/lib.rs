//! `genie-daemon`: one shared daemon per user and channel (spec/01-architecture.md §1, §4).
//!
//! * Endpoint: user-only Unix socket (peer UID checked) or named pipe (user-SID DACL,
//!   `FILE_FLAG_FIRST_PIPE_INSTANCE`, peer SID checked). No TCP port.
//! * Token: 32 random bytes per start in `run/daemon.token` (SA-02); `initialize` must
//!   carry it; compared in constant time.
//! * Spawn race: exclusive `run/daemon.lock` before binding; the loser exits
//!   `already_running`.
//! * Version skew (01 §4.2), idle exit after 10 min (01 §4.3, SA-01), clean shutdown,
//!   crash recovery via execution claims, standalone lease mode (01 §4.4).

mod listener;
mod server;
mod token;

use std::fs::File;
use std::io;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use genie_client::Endpoint;
use genie_core::{Engine, EngineBuilder, TaskScope};

pub use server::{DaemonState, ShutdownReason, serve_stream};

/// 01 §4.3, SA-01.
pub const IDLE_TIMEOUT: Duration = Duration::from_secs(10 * 60);
/// Clean shutdown waits this long for tasks (01 §4.3).
pub const SHUTDOWN_GRACE: Duration = Duration::from_secs(5);

/// Process exit code when another daemon holds the lock.
pub const EXIT_ALREADY_RUNNING: i32 = 3;

#[derive(Clone)]
pub struct DaemonConfig {
    pub endpoint: Endpoint,
    pub engine_version: String,
    pub build: String,
    pub idle_timeout: Duration,
    /// Standalone (01 §4.4): exit when stdin reaches EOF (the parent's lease).
    pub lease_on_stdin: bool,
    pub write_logs: bool,
}

impl DaemonConfig {
    pub fn new(endpoint: Endpoint) -> Self {
        DaemonConfig {
            endpoint,
            engine_version: env!("CARGO_PKG_VERSION").to_string(),
            build: option_env!("GENIE_BUILD").unwrap_or("dev").to_string(),
            idle_timeout: IDLE_TIMEOUT,
            lease_on_stdin: false,
            write_logs: true,
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum DaemonError {
    #[error("another daemon is already running for this channel")]
    AlreadyRunning,
    #[error("io: {0}")]
    Io(#[from] io::Error),
    #[error("{0}")]
    Internal(String),
}

/// Holds the spawn lock for the daemon's lifetime.
struct LockGuard {
    _file: File,
}

fn acquire_lock(path: &Path) -> Result<LockGuard, DaemonError> {
    let f = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(path)?;
    match f.try_lock() {
        Ok(()) => Ok(LockGuard { _file: f }),
        Err(std::fs::TryLockError::WouldBlock) => Err(DaemonError::AlreadyRunning),
        Err(std::fs::TryLockError::Error(e)) => Err(DaemonError::Io(e)),
    }
}

fn create_run_dir(dir: &Path) -> io::Result<()> {
    std::fs::create_dir_all(dir)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}

/// Run the daemon until idle exit, restart, lease end or signal.
pub async fn run(cfg: DaemonConfig) -> Result<ShutdownReason, DaemonError> {
    let ep = cfg.endpoint.clone();
    create_run_dir(&ep.run_dir)?;
    if cfg.write_logs {
        genie_audit::init_logging(&ep.logs_dir(), "daemon", false)?;
    }
    let lock = acquire_lock(&ep.lock_path())?;
    if let Err(e) = genie_process::become_subreaper() {
        tracing::warn!(error = %e, "could not become child subreaper");
    }

    let token = token::generate()?;
    token::write(&ep.token_path(), &token)?;

    let root = TaskScope::root();
    let engine: Engine = EngineBuilder::new().build(&root);
    match engine.recover_on_start() {
        Ok(n) if n > 0 => tracing::info!(interrupted = n, "marked crashed turns interrupted"),
        Ok(_) => {}
        Err(d) => tracing::error!(incident_id = %d.incident_id, "crash recovery failed"),
    }

    let state = Arc::new(DaemonState::new(
        cfg.clone(),
        engine.clone(),
        token,
        root.clone(),
    ));
    let listener = listener::bind(&ep.address)?;
    tracing::info!(
        address = %ep.address, version = %cfg.engine_version, channel = %ep.channel,
        pid = std::process::id(), "daemon listening"
    );

    // Accept loop.
    let accept_scope = root.child("accept");
    {
        let state = state.clone();
        let scope = accept_scope.clone();
        accept_scope.spawn(async move { listener::accept_loop(listener, state, scope).await });
    }
    // Idle watcher.
    {
        let state = state.clone();
        root.child("idle")
            .spawn_cancellable(async move { state.idle_watch().await });
    }
    if cfg.lease_on_stdin {
        let state = state.clone();
        root.child("lease").spawn_cancellable(async move {
            use tokio::io::AsyncReadExt;
            let mut stdin = tokio::io::stdin();
            let mut buf = [0u8; 256];
            while matches!(stdin.read(&mut buf).await, Ok(n) if n > 0) {}
            state.request_shutdown(ShutdownReason::LeaseEnded);
        });
    }
    {
        let state = state.clone();
        root.child("signals").spawn_cancellable(async move {
            if tokio::signal::ctrl_c().await.is_ok() {
                state.request_shutdown(ShutdownReason::Signal);
            }
        });
    }

    let reason = state.wait_shutdown().await;
    tracing::info!(?reason, "daemon shutting down");

    // Clean shutdown (01 §4.3): stop accepting → tell clients → cancel root → wait ≤5 s
    // (running turns settle as interrupted) → close store → kill child groups → remove
    // endpoint files.
    accept_scope.cancel();
    state.broadcast_restarting(reason);
    engine.shutdown();
    root.cancel();
    if !root.close_and_wait(SHUTDOWN_GRACE).await {
        tracing::warn!(
            remaining = root.task_count(),
            "tasks still running after grace period"
        );
    }
    listener::cleanup(&ep.address);
    let _ = std::fs::remove_file(ep.token_path());
    drop(lock);
    Ok(reason)
}
