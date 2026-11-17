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

mod keystore;
mod lease;
mod listener;
mod registry;
mod server;
mod services;
mod token;

use std::fs::File;
use std::io;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use genie_client::Endpoint;
use genie_core::config::ConfigHub;
use genie_core::journal::StoreJournal;
use genie_core::runtime::mcp::{McpHub, McpPorts};
use genie_core::runtime::models::CatalogResolver;
use genie_core::runtime::permissions::PermissionAuthorizer;
use genie_core::runtime::{ConversationStore, ModelResolver, NoModels, RuntimeBuilder};
use genie_core::{Engine, EngineBuilder, TaskScope};

pub use keystore::PlaceholderKeyStore;

pub use lease::LEASE_GRACE;
pub use registry::Registry;
pub use server::{DaemonState, ShutdownReason, serve_stream};
pub use services::Services;

/// Per-subscription outbound queue bound (02 §3.1, SA-10).
pub const SUB_QUEUE_MAX_BYTES: usize = 8 * 1024 * 1024;
pub const SUB_QUEUE_MAX_MSGS: usize = 20_000;

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
    /// Driver lease reconnect grace (02 §5, SA-11).
    pub lease_grace: Duration,
    /// Subscription queue bounds (SA-10).
    pub sub_queue_max_bytes: usize,
    pub sub_queue_max_msgs: usize,
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
            lease_grace: LEASE_GRACE,
            sub_queue_max_bytes: SUB_QUEUE_MAX_BYTES,
            sub_queue_max_msgs: SUB_QUEUE_MAX_MSGS,
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

/// Open the channel's store (spec 04). The data key comes from the placeholder keystore
/// until spec 08's keychain lands.
pub fn open_store(ep: &Endpoint) -> Result<Arc<genie_store::Store>, genie_store::StoreError> {
    let name = format!("genie{}.db", ep.channel.dir_suffix());
    let opts = genie_store::StoreOptions::new(ep.data_dir.clone(), name);
    // TODO(spec 08): the OS keychain instead of `PlaceholderKeyStore`.
    let keys = PlaceholderKeyStore::new(&ep.data_dir);
    genie_store::Store::open_with_keystore(&opts, &keys).map(Arc::new)
}

/// The org audit stream (08 §6): local rotating JSONL in `<data dir>/audit/` (or the
/// policy's `audit.jsonl.dir`), content only with `audit.includeContent`.
fn build_auditor(
    ep: &Endpoint,
    policy: &genie_config::EffectivePolicy,
) -> Option<Arc<genie_audit::Auditor>> {
    let dir = policy
        .get("audit.jsonl.dir")
        .and_then(|v| v.as_str())
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| ep.data_dir.join("audit"));
    let sink = match genie_audit::JsonlSink::open(
        &dir,
        genie_audit::audit::AUDIT_FILES,
        genie_audit::audit::AUDIT_FILE_BYTES,
    ) {
        Ok(s) => s,
        Err(e) => {
            tracing::warn!(error = %e, "audit log unavailable");
            return None;
        }
    };
    Some(Arc::new(
        genie_audit::Auditor::new(genie_audit::AuditContext {
            install_id: install_id(ep),
            user: genie_audit::AuditContext::os_user(),
            org: policy.org_name().map(String::from),
            include_content: policy.include_content(),
        })
        .with_sink(Arc::new(sink)),
    ))
}

/// A random id per install, kept in `<data dir>/install_id`.
fn install_id(ep: &Endpoint) -> String {
    let path = ep.data_dir.join("install_id");
    if let Ok(s) = std::fs::read_to_string(&path)
        && !s.trim().is_empty()
    {
        return s.trim().to_string();
    }
    let mut b = [0u8; 16];
    let id = match getrandom::fill(&mut b) {
        Ok(()) => hex::encode(b),
        Err(_) => "unknown".into(),
    };
    let _ = std::fs::write(&path, &id);
    id
}

/// Wire the engine (01 §3): store journal + the agent runtime (06) with the session
/// registry as its settings source, the settings/policy/trust hub, the permission
/// evaluator, MCP and audit. Falls back to in-memory state if the store can't be
/// opened, so the daemon still serves clients and reports the problem.
fn build_engine(
    ep: &Endpoint,
    root: &TaskScope,
    registry: Arc<registry::Registry>,
) -> (Engine, Option<Arc<dyn ConversationStore>>, Services) {
    let models: Arc<dyn ModelResolver> = match CatalogResolver::live() {
        Ok(r) => Arc::new(r),
        Err(e) => {
            tracing::error!(error = %e, "HTTP client unavailable; turns will fail");
            Arc::new(NoModels)
        }
    };
    let home = std::env::var("HOME")
        .or_else(|_| std::env::var("USERPROFILE"))
        .ok();
    let mut services = Services::default();
    let hub = match ConfigHub::new(
        ep.config_dir.clone(),
        ep.config_dir.join("trust.json"),
        home,
    ) {
        Ok(h) => Some(Arc::new(h)),
        Err(e) => {
            tracing::error!(error = %e, "settings hub unavailable");
            None
        }
    };
    if let Some(hub) = &hub {
        let mut pm = genie_config::PolicyManager::new(vec![Box::new(
            genie_config::FilePolicySource::system(),
        )])
        .with_remote_cache(ep.data_dir.join("policy-remote.json"));
        pm.refresh_local();
        pm.load_remote_cache();
        hub.set_policy(pm.effective());
        services.audit = build_auditor(ep, &hub.policy());
        services.policy = Some(std::sync::Mutex::new(pm));
    }
    let store = match open_store(ep) {
        Ok(s) => Some(s),
        Err(e) => {
            tracing::error!(error = %e, "store unavailable; sessions are not persisted");
            None
        }
    };
    let turns = Arc::new(genie_core::runtime::active::ActiveTurns::new());
    let mut rb = RuntimeBuilder::new(registry.clone(), models.clone()).turns(turns.clone());
    let mut eb = EngineBuilder::new();
    if let Some(store) = &store {
        rb = rb.store(store.clone());
        eb = eb.journal(Arc::new(StoreJournal::new(store.clone())));
    }
    if let Some(hub) = &hub {
        let mut authz = PermissionAuthorizer::new(hub.clone(), registry.clone());
        if let Some(a) = &services.audit {
            authz = authz.with_audit(a.clone());
        }
        rb = rb.authorizer(Arc::new(authz));
        match McpPorts::default_ports() {
            Ok(ports) => {
                let mcp = McpHub::new(
                    hub.clone(),
                    root.child("mcp"),
                    ports,
                    registry.clone(),
                    Some(models),
                    turns,
                    store.clone(),
                );
                rb = rb.tool_provider(mcp.clone());
                services.mcp = Some(mcp);
            }
            Err(e) => tracing::error!(error = %e, "MCP unavailable"),
        }
    }
    services.hub = hub;
    let runner = rb.build();
    let conversation = runner.conversation();
    services.runtime = Some(runner.runtime().clone());
    (
        eb.turn_runner(Arc::new(runner)).build(root),
        Some(conversation),
        services,
    )
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
    let registry = Arc::new(registry::Registry::default());
    let (engine, conversation, services) = build_engine(&ep, &root, registry.clone());
    let services = Arc::new(services);
    match engine.recover_on_start() {
        Ok(n) if n > 0 => tracing::info!(interrupted = n, "marked crashed turns interrupted"),
        Ok(_) => {}
        Err(d) => tracing::error!(incident_id = %d.incident_id, "crash recovery failed"),
    }

    let state = Arc::new(DaemonState::with_services(
        cfg.clone(),
        engine.clone(),
        token,
        root.clone(),
        registry,
        conversation,
        Some(services.clone()),
    ));
    state.start_background();
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
    if let Some(m) = &services.mcp {
        m.shutdown();
    }
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
