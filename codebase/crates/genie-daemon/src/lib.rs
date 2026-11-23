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

mod agents;
mod blobs;
mod code;
mod keystore;
mod lease;
mod listener;
mod manage;
mod observe;
mod ops;
mod preview;
mod projects;
mod providers;
mod registry;
pub mod secrets;
mod server;
mod services;
mod sync;
mod terminal;
mod token;
mod transfer;

use std::fs::File;
use std::io;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use genie_client::Endpoint;
use genie_core::config::ConfigHub;
use genie_core::journal::StoreJournal;
use genie_core::runtime::mcp::{McpHub, McpPorts};
use genie_core::runtime::permissions::PermissionAuthorizer;
use genie_core::runtime::{ConversationStore, ModelResolver, NoModels, RuntimeBuilder};
use genie_core::{Engine, EngineBuilder, TaskScope};

pub use code::CodeServices;
pub use keystore::PlaceholderKeyStore;
pub use providers::{Feed, HttpFeed, ProviderHub};
pub use secrets::{Secrets, SecretsChoice, SecretsKeyStore};

pub use blobs::Uploads;
pub use lease::LEASE_GRACE;
pub use observe::{EventTap, HttpOtlp, OTLP_HEADERS_ACCOUNT, SentryExporter};
pub use registry::Registry;
pub use server::{DaemonState, ShutdownReason, serve_stream};
pub use services::Services;
pub use sync::{HttpPolicyFetcher, fetch_signed};

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
    /// Secrets backend (D26): keychain / `secrets.enc`; tests inject a memory backend.
    pub secrets: SecretsChoice,
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
            secrets: SecretsChoice::Auto,
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

/// Open the channel's store (spec 04) with the data key from `keys` (the keychain or
/// `secrets.enc`, 04 §2). A key left in the old placeholder file is migrated first.
pub fn open_store(
    ep: &Endpoint,
    keys: &dyn genie_store::KeyStore,
) -> Result<Arc<genie_store::Store>, genie_store::StoreError> {
    let name = format!("genie{}.db", ep.channel.dir_suffix());
    let opts = genie_store::StoreOptions::new(ep.data_dir.clone(), name);
    if let Err(e) = secrets::migrate_placeholder(&ep.data_dir, keys) {
        tracing::error!(error = %e, "placeholder keystore migration failed; file kept");
    }
    // A missing key is never resolved here by setting the DB aside: that needs the
    // user's say (04 §2), through `storage.start_fresh`.
    genie_store::Store::open_with_keystore(&opts, keys).map(Arc::new)
}

/// [`open_store`], and when the key is missing, look in the key store this build does
/// not use ([`secrets::alternate_backend`]): a DB written by a dev build has its key in
/// `secrets.enc`, one written by a release build in the keychain. A key found there
/// must open the DB before anything is copied; then it (and every other secret the
/// current store lacks, e.g. provider credentials) is copied into the current store,
/// so the next start needs no fallback.
pub fn open_store_any_key(
    ep: &Endpoint,
    secrets: &Secrets,
) -> Result<Arc<genie_store::Store>, genie_store::StoreError> {
    use genie_store::StoreError;
    let backend = secrets.backend();
    let missing = match open_store(ep, &SecretsKeyStore(backend.clone())) {
        Err(e @ StoreError::KeyMissing { .. }) => e,
        r => return r,
    };
    let Some(alt) = secrets::alternate_backend(ep, backend.kind()) else {
        return Err(missing);
    };
    let Ok(Some(raw)) = alt.backend.get(genie_store::keys::DATA_KEY_ACCOUNT) else {
        return Err(missing);
    };
    let key = genie_store::crypto::Key::from_bytes(&raw)?;
    let opts = genie_store::StoreOptions::new(ep.data_dir.clone(), opts_name(ep));
    let store = match genie_store::Store::open(&opts, key) {
        Ok(s) => s,
        Err(e) => {
            tracing::warn!(error = %e, "the other key store's data key does not open this DB");
            return Err(missing);
        }
    };
    let copied = secrets::adopt_accounts(&*alt.backend, &*backend, &alt.accounts);
    tracing::warn!(
        from = alt.backend.kind().as_str(),
        to = backend.kind().as_str(),
        copied,
        "DB key found in the other key store (a dev/release build switch); copied over"
    );
    Ok(Arc::new(store))
}

/// What clients are told when the store could not be opened.
fn storage_problem(e: &genie_store::StoreError) -> genie_protocol::handshake::StorageProblem {
    match e {
        genie_store::StoreError::KeyMissing { recovery_available } => {
            genie_protocol::handshake::StorageProblem {
                reason: "key_missing".into(),
                detail: "The database's encryption key is not in this computer's key store.".into(),
                recovery_available: *recovery_available,
            }
        }
        e => genie_protocol::handshake::StorageProblem {
            reason: "open_failed".into(),
            detail: e.to_string(),
            recovery_available: false,
        },
    }
}

pub(crate) fn opts_name(ep: &Endpoint) -> String {
    format!("genie{}.db", ep.channel.dir_suffix())
}

/// The org audit stream (08 §6): local rotating JSONL in `<data dir>/audit/` (or the
/// policy's `audit.jsonl.dir`), content only with `audit.includeContent`.
///
/// With policy `audit.otlp.endpoint` the records are also queued for the OTLP exporter
/// (headers from the keychain account `audit.otlp.headers`), flushed by `sync`.
#[allow(clippy::type_complexity)]
fn build_auditor(
    ep: &Endpoint,
    policy: &genie_config::EffectivePolicy,
    secrets: &Secrets,
) -> (
    Option<Arc<genie_audit::Auditor>>,
    Option<(
        Arc<genie_audit::OtlpSink>,
        Arc<dyn genie_audit::OtlpTransport>,
    )>,
) {
    let otlp = policy
        .get("audit.otlp.endpoint")
        .and_then(|v| v.as_str())
        .filter(|e| !e.trim().is_empty())
        .map(|endpoint| {
            let transport: Arc<dyn genie_audit::OtlpTransport> = Arc::new(observe::HttpOtlp::new(
                genie_providers::oauth::http_client().unwrap_or_default(),
                endpoint,
                observe::otlp_headers(secrets.backend().as_ref()),
            ));
            (Arc::new(genie_audit::OtlpSink::default()), transport)
        });
    let ctx = genie_audit::AuditContext {
        install_id: install_id(ep),
        user: genie_audit::AuditContext::os_user(),
        org: policy.org_name().map(String::from),
        include_content: policy.include_content(),
    };
    let mut auditor = genie_audit::Auditor::new(ctx);
    if let Some(sink) = jsonl_sink(ep, policy) {
        auditor = auditor.with_sink(Arc::new(sink));
    }
    if let Some((sink, _)) = &otlp {
        auditor = auditor.with_sink(sink.clone());
    }
    let auditor = auditor.is_enabled().then(|| Arc::new(auditor));
    (auditor, otlp)
}

fn jsonl_sink(
    ep: &Endpoint,
    policy: &genie_config::EffectivePolicy,
) -> Option<genie_audit::JsonlSink> {
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
    Some(sink)
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
    secrets: Arc<Secrets>,
) -> (Engine, Option<Arc<dyn ConversationStore>>, Services) {
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
        // System file + MDM (macOS profile / Windows registry), signed remote URL (08 §5.1).
        let mut pm = genie_config::PolicyManager::new(genie_config::system_sources())
            .with_remote_cache(ep.data_dir.join("policy-remote.json"));
        if let Ok(handle) = tokio::runtime::Handle::try_current() {
            pm = pm.with_remote(Arc::new(sync::HttpPolicyFetcher {
                client: genie_providers::oauth::http_client().unwrap_or_default(),
                handle,
            }));
        }
        pm.refresh_local();
        pm.load_remote_cache();
        hub.set_policy(pm.effective());
        let (audit, otlp) = build_auditor(ep, &hub.policy(), &secrets);
        services.audit = audit;
        services.otlp = otlp;
        services.policy = Some(std::sync::Mutex::new(pm));
    }
    // Telemetry (08 §7): crash reports from panics; consent applied by `sync::start`.
    services.telemetry = observe::open_telemetry(&ep.data_dir, ep.channel.as_str());
    if let Some(t) = &services.telemetry {
        if let Some(h) = &hub {
            t.set_consent(observe::consent_from(h));
        }
        t.install_panic_hook();
    }
    services.secrets = Some(secrets.clone());
    let store = match open_store_any_key(ep, &secrets) {
        Ok(s) => {
            match registry.attach_store(s.clone()) {
                Ok(r) => tracing::info!(
                    loaded = r.loaded,
                    recovered = r.recovered,
                    dropped_incognito = r.dropped_incognito,
                    "sessions loaded from the store"
                ),
                Err(e) => tracing::error!(error = %e, "saved sessions could not be loaded"),
            }
            Some(s)
        }
        Err(e) => {
            // Clients are told (`InitializeResult.storage`) and offered recovery; the
            // daemon still serves so that the user can act on it.
            tracing::error!(error = %e, "store unavailable; sessions are NOT persisted");
            services.storage_problem = Some(storage_problem(&e));
            None
        }
    };
    // Providers, credentials and the catalog (03 §8, §10); the model resolver reads both.
    let providers = Arc::new(
        ProviderHub::new(secrets.clone(), ep.data_dir.clone())
            .with_config(hub.clone())
            .with_store(store.clone())
            .with_audit(services.audit.clone()),
    );
    let models: Arc<dyn ModelResolver> = match providers.resolver() {
        Ok(r) => Arc::new(r),
        Err(e) => {
            tracing::error!(error = %e, "HTTP client unavailable; turns will fail");
            Arc::new(NoModels)
        }
    };
    services.providers = Some(providers);
    let turns = Arc::new(genie_core::runtime::active::ActiveTurns::new());
    let mut rb = RuntimeBuilder::new(registry.clone(), models.clone()).turns(turns.clone());
    let mut eb = EngineBuilder::new();
    // Incognito sessions (and their subagents) never reach disk (D24, D31): their events,
    // messages, spilled output and artifacts stay in process memory. Only their spend is
    // added to the usage totals.
    let ephemeral: genie_core::journal::Ephemeral = {
        let reg = registry.clone();
        Arc::new(move |id: &str| reg.is_incognito(id))
    };
    let conversation: Option<Arc<dyn ConversationStore>> = store.as_ref().map(|s| {
        Arc::new(genie_core::runtime::RoutedConversation::new(
            Arc::new(genie_core::runtime::StoreConversation::new(s.clone())),
            ephemeral.clone(),
            Some(s.clone()),
        )) as Arc<dyn ConversationStore>
    });
    if let (Some(store), Some(conv)) = (&store, &conversation) {
        rb = rb
            .store(store.clone())
            .conversation(conv.clone())
            .spill(Some(Arc::new(genie_core::runtime::RoutedSpill::new(
                Arc::new(genie_core::runtime::bridge::BlobSpill::new(store.clone())),
                ephemeral.clone(),
            ))));
    }
    // Durable events → audit (tool calls, usage, browser actions) + usage analytics.
    let journal: Arc<dyn genie_core::journal::Journal> = match &store {
        Some(store) => Arc::new(genie_core::journal::RoutedJournal::new(
            Arc::new(StoreJournal::new(store.clone())),
            ephemeral.clone(),
        )),
        None => Arc::new(genie_core::journal::MemoryJournal::new()),
    };
    eb = eb.journal(observe::EventTap::wrap(
        journal,
        services.audit.clone(),
        services.telemetry.clone(),
    ));
    // Artifacts, projects + retrieval, memory state, attachments, chat search
    // (D28, D31, D32, 06 §2.5).
    rb = projects::wire(
        rb,
        &mut services,
        store.as_ref(),
        &models,
        root,
        hub.as_ref(),
    );
    if let Some(a) = &services.artifacts {
        a.set_ephemeral(ephemeral.clone());
    }
    // Shell (Code, 06 §13) with background jobs bound to the engine below; web tools
    // (Chat + Code, 06 §8) tainting the turn.
    let jobs = genie_core::runtime::host::EngineJobs::new(store.clone());
    let rb_taints = rb.taints();
    let shell_scope = root.child("shell");
    let shell = Arc::new(genie_tools::shell::ShellRuntime::new(
        genie_tools::shell::ShellConfig::new(
            Arc::new(genie_core::runtime::host::SettingsDirs(registry.clone())),
            shell_scope.tracker(),
            shell_scope.token().clone(),
        )
        .with_jobs(jobs.clone()),
    ));
    // Dev servers (ticket 47): kill-trees on their own scope, ended with the daemon.
    let dev_scope = root.child("preview");
    let dev = Arc::new(genie_tools::preview::DevServers::new(
        Arc::new(genie_core::runtime::host::SettingsDirs(registry.clone())),
        dev_scope.tracker(),
        dev_scope.token().clone(),
    ));
    services.preview = Some(dev.clone());
    rb = rb
        .shell(shell.clone())
        .preview(&dev)
        .web(genie_tools::web::WebConfig::default());
    // Image generation (Chat + Code): `imageGeneration.*` settings, provider credentials
    // from the model resolver, images kept as store blobs.
    if let Some(store) = &store {
        match genie_providers::executor::ReqwestTransport::new() {
            Ok(t) => {
                let backend = genie_core::runtime::images::EngineImages::new(
                    models.clone(),
                    hub.clone(),
                    Arc::new(t),
                );
                rb = rb.images(genie_tools::image::ImageConfig::new(
                    Arc::new(backend),
                    Arc::new(genie_core::runtime::images::BlobImages(store.clone())),
                ));
            }
            Err(e) => tracing::warn!(error = %e, "no HTTP client; image generation is off"),
        }
    }
    // Code file tools on the shared path locks + shadow-git checkpoints (06 §4.2, D49).
    let code_cfg = genie_core::runtime::code::code_config(
        registry.clone(),
        Arc::new(genie_core::locks::PathLocks::new()),
    );
    let checkpoints = genie_core::runtime::checkpoint::Checkpoints::new(
        ep.data_dir.join("snapshots"),
        registry.clone(),
    );
    rb = rb.code(code_cfg.clone()).checkpoints(checkpoints.clone());

    // The agent browser (06 §10): built now, Chrome launched on first use.
    let browser_host = {
        let btasks = root.child("browser").tracker();
        let backend: Arc<dyn genie_browser::BrowserBackend> =
            Arc::new(genie_browser::SystemChromeBackend::new(
                genie_browser::backend::SystemChromeConfig::new(&ep.data_dir),
                btasks.clone(),
            ));
        let mut host = genie_core::runtime::browser::EngineBrowserHost::new(
            turns.clone(),
            rb_taints.clone(),
            registry.clone(),
        );
        host.models = Some(models.clone());
        host.store = store.clone();
        host.conversation = conversation.clone();
        let host = Arc::new(host);
        let sites = Arc::new(genie_browser::SitePolicy::new());
        if let Some(h) = &hub {
            let p = h.policy();
            sites.set_admin(
                p.strings("browser.siteAllowlist"),
                p.strings("browser.siteBlocklist"),
            );
            sites.set_local_only(
                p.bool("browser.agentLocalOnly") == Some(true),
                p.bool("browser.userLocalOnly") == Some(true),
            );
            if let Some(v) = h.value(None, "browser.sites")
                && let Ok(list) =
                    serde_json::from_value::<Vec<genie_protocol::api::browser::SitePermission>>(v)
            {
                for s in &list {
                    sites.set(s);
                }
            }
        }
        let browser = genie_browser::Browser::new(
            backend.clone(),
            host.clone(),
            sites,
            genie_browser::BrowserConfig {
                downloads_dir: Some(ep.data_dir.join("browser").join("downloads")),
                // Settings → ⋮ Keep cookies (ticket 47); read when needed, default on.
                keep_cookies: hub.clone().map(|h| {
                    genie_browser::Flag(Arc::new(move || {
                        h.value(None, "browser.keepCookies")
                            .and_then(|v| v.as_bool())
                            .unwrap_or(true)
                    }))
                }),
                ..Default::default()
            },
            btasks,
        );
        rb = rb.browser(&browser);
        services.browser = Some(browser);
        services.browser_backend = Some(backend);
        Some(host)
    };
    if let Some(hub) = &hub {
        let p = hub.policy();
        rb = rb.web_search(Some(genie_tools::web::SearchPolicy {
            enabled: p.bool("webSearch.enabled") != Some(false),
            allowed_domains: p.strings("webSearch.allowedDomains"),
            blocked_domains: p.strings("webSearch.blockedDomains"),
        }));
        let mut authz = PermissionAuthorizer::new(hub.clone(), registry.clone());
        if let Some(c) = &conversation {
            authz = authz.with_conversation(c.clone());
        }
        if let Some(a) = &services.audit {
            authz = authz.with_audit(a.clone());
        }
        let authz = Arc::new(authz);
        services.authz = Some(authz.clone());
        rb = rb.authorizer(authz);
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
                rb = rb.tool_provider(mcp.clone()).mcp_prompts(mcp.clone());
                services.mcp = Some(mcp);
            }
            Err(e) => tracing::error!(error = %e, "MCP unavailable"),
        }
    }
    // Agents, subagents and hooks (06 §11–12).
    rb = rb
        .agents(Arc::new(genie_core::runtime::agents::AgentCatalog::new(
            Some(ep.config_dir.clone()),
            hub.clone(),
        )))
        .subagents(
            genie_core::runtime::subagents::SubagentConfig::default(),
            root.child("subagents"),
        );
    if let Some(hub) = &hub {
        rb = rb
            .config_hub(hub.clone())
            .hooks(genie_core::runtime::hooks::HooksHub::with_audit(
                Arc::new(genie_core::runtime::hooks::ConfigHooks { hub: hub.clone() }),
                services.audit.clone(),
            ));
    }
    if let Some(hub) = &hub {
        let skills = genie_core::runtime::skills::SkillsHub::new(
            ep.config_dir.clone(),
            hub.clone(),
            registry.clone(),
        );
        rb = rb.skills(skills.clone());
        services.skills = Some(skills);
    }
    services.store = store.clone();
    services.hub = hub;
    let runner = rb.build();
    let conversation = runner.conversation();
    services.runtime = Some(runner.runtime().clone());
    services.code = Some(Arc::new({
        let mut c = CodeServices::new(registry.clone());
        c.conversation = Some(conversation.clone());
        c.checkpoints = Some(checkpoints);
        c.code = Some(code_cfg);
        c.store = store.clone();
        c
    }));
    let subagents = runner.runtime().subagents.clone();
    {
        let sh = shell.clone();
        let br = services.browser.clone();
        let dev = dev.clone();
        let scope = root.child("session-end");
        let sa = subagents.clone();
        eb = eb.on_session_end(Arc::new(move |id: &str, evicted: bool| {
            if let Some(sa) = &sa {
                sa.session_ended(id, evicted);
            }
            // An evicted actor keeps its running jobs; they report back when done.
            if !evicted || sh.running_jobs(id).is_empty() {
                sh.end_session(id);
            }
            // Dev servers outlive turns and actor eviction, not the session.
            if !evicted {
                dev.end_session(id);
            }
            if let Some(b) = br.clone() {
                let id = id.to_string();
                scope.spawn(async move { b.end_session(&id).await });
            }
        }));
    }
    let engine = eb.turn_runner(Arc::new(runner)).build(root);
    jobs.bind(engine.clone());
    if let Some(sa) = &subagents {
        // Durable report outbox (06 §11): reports survive a crash before delivery.
        if let Some(s) = &store {
            sa.set_store(s.clone());
        }
        sa.bind(
            engine.clone(),
            Some(registry.clone() as Arc<dyn genie_core::runtime::subagents::ChildRegistrar>),
        );
    }
    if let Some(h) = &browser_host {
        h.bind(engine.clone());
    }
    services.browser_host = browser_host;
    (engine, Some(conversation), services)
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

    // Secrets (D26): keychain or `secrets.enc`; a locked file waits for `secrets.unlock`.
    let secrets = Secrets::open(&cfg.secrets, &ep);
    secrets.unlock_from_env();
    if secrets.is_locked()
        && let Some(reason) = secrets::locked_phase(&cfg, &token, secrets.clone()).await?
    {
        let _ = std::fs::remove_file(ep.token_path());
        drop(lock);
        return Ok(reason);
    }

    let root = TaskScope::root();
    let registry = Arc::new(registry::Registry::default());
    let (engine, conversation, services) = build_engine(&ep, &root, registry.clone(), secrets);
    let services = Arc::new(services);
    match engine.recover_on_start() {
        Ok(n) if n > 0 => tracing::info!(interrupted = n, "marked crashed turns interrupted"),
        Ok(_) => {}
        Err(d) => tracing::error!(incident_id = %d.incident_id, "crash recovery failed"),
    }
    if let Some(sa) = services.runtime.as_ref().and_then(|r| r.subagents.as_ref()) {
        let n = sa.recover_reports();
        if n > 0 {
            tracing::info!(
                reports = n,
                "delivered subagent reports from before a restart"
            );
        }
    }

    let state = Arc::new(DaemonState::with_services(
        cfg.clone(),
        engine.clone(),
        token,
        root.clone(),
        registry.clone(),
        conversation,
        Some(services.clone()),
    ));
    // Child sessions on the workspace feed (`session.listed`, 06 §11).
    let weak = Arc::downgrade(&state);
    registry.set_listener(Arc::new(move |change, summary| {
        if let Some(st) = weak.upgrade() {
            st.listed(change, summary);
        }
    }));
    state.start_background();
    if let Some(dev) = &services.preview {
        use genie_protocol::methods::Method as _;
        let weak = Arc::downgrade(&state);
        dev.set_on_change(Arc::new(move |sid, servers| {
            if let Some(st) = weak.upgrade() {
                st.workspace_send(
                    genie_protocol::methods::PreviewChangedNote::NAME,
                    &genie_protocol::workspace::PreviewChanged {
                        session_id: sid.to_string(),
                        servers,
                    },
                );
            }
        }));
    }
    if let Some(h) = &services.browser_host {
        let weak = Arc::downgrade(&state);
        h.set_can_answer(Arc::new(move |sid, kind| {
            weak.upgrade().is_some_and(|st| st.can_render(sid, kind))
        }));
    }
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
    if let Some(b) = &services.browser {
        let _ = tokio::time::timeout(SHUTDOWN_GRACE, b.before_shutdown()).await;
    }
    if let Some(b) = &services.browser_backend {
        let _ = tokio::time::timeout(SHUTDOWN_GRACE, b.shutdown()).await;
    }
    root.cancel();
    if !root.close_and_wait(SHUTDOWN_GRACE).await {
        tracing::warn!(
            remaining = root.task_count(),
            "tasks still running after grace period"
        );
    }
    // Close the store explicitly: subsystems bound to the engine keep `Arc`s to it in
    // reference cycles, so dropping ours would not release the DB files.
    if let Some(s) = &services.store {
        s.close();
    }
    listener::cleanup(&ep.address);
    let _ = std::fs::remove_file(ep.token_path());
    drop(lock);
    Ok(reason)
}
