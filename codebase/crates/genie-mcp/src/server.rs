//! One configured server: lifecycle and status state machine, reconnect with backoff,
//! tool cache with debounced `list_changed` reload, log ring, and the typed operations the
//! engine calls (tools, resources, prompts).
//!
//! ```text
//!            enable                 handshake + tools/list ok
//! disabled ─────────▶ pending ──────────────────────────────▶ connected
//!    ▲                  │  ▲                                     │
//!    │ disable          │  │ backoff (1 s → 60 s), or restart()  │ crash / EOF / error
//!    └──────────────────┤  └──────────── failed ◀────────────────┘
//!                       │ 401, no fresh token                    │ 401
//!                       └──────────▶ needs_auth ◀────────────────┘
//!                                     (restart() after auth)
//! ```
//!
//! Transient connect failures are retried inside the startup deadline (30 s); after that the
//! server is `failed` and reconnects with exponential backoff, giving up after
//! `max_attempts` consecutive failures until `restart()`. A connection that lived longer than
//! `stable_after` resets the failure count. `404` on the HTTP session reconnects at once.

use std::collections::BTreeMap;
use std::sync::{Arc, RwLock};
use std::time::Duration;

use genie_protocol::api::mcp::{McpServerInfo, McpStatus};
use genie_protocol::ask::Annotations;
use serde_json::{Value, json};
use tokio::sync::{broadcast, mpsc, watch};
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;
use tokio_util::task::TaskTracker;

use crate::auth::{AuthKey, AuthProvider};
use crate::client::{ClientCtx, McpClient, Progress, RequestOpts, ServerNotification};
use crate::config::{McpScope, SecretResolver, ServerConfig};
use crate::error::McpError;
use crate::handler::{ClientHandler, SamplingBudget};
use crate::logs::LogRing;
use crate::naming;
use crate::result::{self, Part, TaintSource, ToolResult};
use crate::schema;
use crate::transport::{CloseReason, ConnectEnv, Connector};

/// 06 §7 runtime limits.
#[derive(Debug, Clone)]
pub struct Timeouts {
    /// Connect + initialize + first tools/list.
    pub startup: Duration,
    /// Each `*/list`.
    pub list: Duration,
    /// `tools/call` (configurable; 10 min, not 12 h).
    pub tool: Duration,
    /// Other requests (`resources/read`, `prompts/get`, subscribe).
    pub request: Duration,
}

impl Default for Timeouts {
    fn default() -> Timeouts {
        Timeouts {
            startup: Duration::from_secs(30),
            list: Duration::from_secs(30),
            tool: Duration::from_secs(600),
            request: Duration::from_secs(60),
        }
    }
}

/// Reconnect policy after a server fails.
#[derive(Debug, Clone)]
pub struct Backoff {
    pub initial: Duration,
    pub max: Duration,
    /// Consecutive failures before giving up until `restart()`.
    pub max_attempts: u32,
    /// A connection that lived this long resets the failure count.
    pub stable_after: Duration,
    /// Delay between transient connect retries inside the startup deadline.
    pub connect_retry: Duration,
    /// `list_changed` debounce.
    pub debounce: Duration,
}

impl Default for Backoff {
    fn default() -> Backoff {
        Backoff {
            initial: Duration::from_secs(1),
            max: Duration::from_secs(60),
            max_attempts: 6,
            stable_after: Duration::from_secs(60),
            connect_retry: Duration::from_millis(500),
            debounce: Duration::from_millis(300),
        }
    }
}

impl Backoff {
    pub fn delay(&self, failures: u32) -> Duration {
        let exp = failures.saturating_sub(1).min(16);
        self.initial.saturating_mul(1 << exp).min(self.max)
    }
}

#[derive(Debug, Clone)]
pub struct ServerOptions {
    pub enabled: bool,
    /// On the org/user allowlist of trusted servers: output does not taint the turn (SA-29).
    pub trusted: bool,
    /// Policy can ban sampling (D30).
    pub sampling_allowed: bool,
    /// Per-server sampling token budget (D30); `None` = unlimited.
    pub sampling_budget_tokens: Option<u64>,
    pub timeouts: Timeouts,
    pub backoff: Backoff,
}

impl Default for ServerOptions {
    fn default() -> ServerOptions {
        ServerOptions {
            enabled: true,
            trusted: false,
            sampling_allowed: true,
            sampling_budget_tokens: Some(100_000),
            timeouts: Timeouts::default(),
            backoff: Backoff::default(),
        }
    }
}

/// Shared services, injected by the engine.
#[derive(Clone)]
pub struct Runtime {
    /// Every task hangs off this tracker (01 §2.1 rule 4).
    pub tasks: TaskTracker,
    /// Cancelling it stops every server.
    pub cancel: CancellationToken,
    pub connector: Arc<dyn Connector>,
    pub handler: Arc<dyn ClientHandler>,
    pub auth: Arc<dyn AuthProvider>,
    pub secrets: Arc<dyn SecretResolver>,
    pub client_name: String,
    pub client_version: String,
}

/// Broadcast to the engine (status line in Settings / "+" menu, tool registry reloads,
/// resource refresh).
#[derive(Debug, Clone, PartialEq)]
pub enum McpEvent {
    Status {
        server: String,
        status: McpStatus,
        error: Option<String>,
    },
    ToolsChanged {
        server: String,
    },
    ResourcesChanged {
        server: String,
    },
    PromptsChanged {
        server: String,
    },
    ResourceUpdated {
        server: String,
        uri: String,
    },
}

/// A server tool as the engine sees it.
#[derive(Debug, Clone, PartialEq)]
pub struct McpTool {
    pub server: String,
    /// The server's own tool name (what `tools/call` gets).
    pub name: String,
    /// `mcp__<server>__<tool>`; the manager re-assigns it when names collide across servers.
    pub model_name: String,
    pub title: Option<String>,
    pub description: String,
    /// Sanitised input schema.
    pub input_schema: Value,
    pub output_schema: Option<Value>,
    /// Informational only, never trusted for decisions (D25).
    pub annotations: Option<Annotations>,
    pub schema_warnings: Vec<String>,
}

impl McpTool {
    fn from_raw(server: &str, raw: &Value) -> Option<McpTool> {
        let name = raw.get("name")?.as_str()?.to_string();
        let s = schema::sanitize_input_schema(raw.get("inputSchema"));
        let ann = raw.get("annotations");
        let annotations = ann.map(|a| Annotations {
            read_only_hint: a.get("readOnlyHint").and_then(Value::as_bool),
            destructive_hint: a.get("destructiveHint").and_then(Value::as_bool),
        });
        Some(McpTool {
            server: server.to_string(),
            model_name: naming::tool_name(server, &name),
            title: raw
                .get("title")
                .or_else(|| ann.and_then(|a| a.get("title")))
                .and_then(Value::as_str)
                .map(String::from),
            description: schema::cap_description(
                raw.get("description")
                    .and_then(Value::as_str)
                    .unwrap_or_default(),
            ),
            input_schema: s.schema,
            output_schema: raw.get("outputSchema").cloned(),
            annotations,
            schema_warnings: s.warnings,
            name,
        })
    }
}

/// Options for one tool call.
#[derive(Clone, Default)]
pub struct CallOpts {
    pub cancel: Option<CancellationToken>,
    pub progress: Option<mpsc::UnboundedSender<Progress>>,
    /// Overrides the server's tool timeout.
    pub timeout: Option<Duration>,
}

/// `resources/read` result.
#[derive(Debug, Clone, PartialEq)]
pub struct ResourceRead {
    pub parts: Vec<Part>,
    pub taint: TaintSource,
}

/// `prompts/get` result.
#[derive(Debug, Clone, PartialEq)]
pub struct PromptResult {
    pub description: Option<String>,
    /// `(role, content)`.
    pub messages: Vec<(String, Part)>,
    pub taint: TaintSource,
}

#[derive(Debug, Clone, PartialEq)]
struct Snapshot {
    status: McpStatus,
    error: Option<String>,
}

enum Control {
    Restart,
    Enable(bool),
}

struct Inner {
    name: String,
    scope: McpScope,
    config: Result<ServerConfig, String>,
    opts: ServerOptions,
    rt: Runtime,
    status: watch::Sender<Snapshot>,
    client: RwLock<Option<Arc<McpClient>>>,
    tools: RwLock<Vec<McpTool>>,
    logs: LogRing,
    events: broadcast::Sender<McpEvent>,
    control: mpsc::UnboundedSender<Control>,
    budget: Arc<SamplingBudget>,
    challenge: RwLock<Option<String>>,
    stop: CancellationToken,
}

/// Handle to a running server. Cheap to clone; shared by every session (one process per
/// server config, 06 §7).
#[derive(Clone)]
pub struct McpServer {
    inner: Arc<Inner>,
}

impl McpServer {
    /// Start supervising `config`. A config error leaves the server `failed` with it.
    pub fn start(
        name: &str,
        scope: McpScope,
        config: Result<ServerConfig, String>,
        opts: ServerOptions,
        rt: Runtime,
        events: broadcast::Sender<McpEvent>,
    ) -> McpServer {
        let (status, _) = watch::channel(Snapshot {
            status: McpStatus::Pending,
            error: None,
        });
        let (ctl_tx, ctl_rx) = mpsc::unbounded_channel();
        let inner = Arc::new(Inner {
            name: name.to_string(),
            scope,
            config,
            budget: Arc::new(SamplingBudget::new(opts.sampling_budget_tokens)),
            stop: rt.cancel.child_token(),
            opts,
            status,
            client: RwLock::new(None),
            tools: RwLock::new(Vec::new()),
            logs: LogRing::default(),
            events,
            control: ctl_tx,
            challenge: RwLock::new(None),
            rt,
        });
        let sup = inner.clone();
        inner.rt.tasks.spawn(supervise(sup, ctl_rx));
        McpServer { inner }
    }

    pub fn name(&self) -> &str {
        &self.inner.name
    }

    pub fn scope(&self) -> McpScope {
        self.inner.scope
    }

    pub fn config(&self) -> Result<&ServerConfig, &str> {
        self.inner.config.as_ref().map_err(String::as_str)
    }

    pub fn status(&self) -> McpStatus {
        self.inner.status.borrow().status
    }

    pub fn error(&self) -> Option<String> {
        self.inner.status.borrow().error.clone()
    }

    pub fn info(&self) -> McpServerInfo {
        let s = self.inner.status.borrow().clone();
        McpServerInfo {
            name: self.inner.name.clone(),
            status: s.status,
            scope: self.inner.scope.to_config_scope(),
            tools: u32::try_from(self.tools().len()).unwrap_or(u32::MAX),
            error: s.error,
        }
    }

    /// Wait until the status satisfies `f` or `timeout` passes; returns the last status.
    pub async fn wait_status(&self, timeout: Duration, f: impl Fn(McpStatus) -> bool) -> McpStatus {
        let mut rx = self.inner.status.subscribe();
        let _ = tokio::time::timeout(timeout, rx.wait_for(|s| f(s.status))).await;
        self.status()
    }

    pub fn logs(&self, tail: usize) -> Vec<String> {
        self.inner.logs.tail(tail)
    }

    pub fn log_ring(&self) -> LogRing {
        self.inner.logs.clone()
    }

    pub fn tools(&self) -> Vec<McpTool> {
        self.inner
            .tools
            .read()
            .map(|t| t.clone())
            .unwrap_or_default()
    }

    pub fn sampling_budget(&self) -> Arc<SamplingBudget> {
        self.inner.budget.clone()
    }

    pub fn taint(&self) -> TaintSource {
        TaintSource {
            server: self.inner.name.clone(),
            trusted: self.inner.opts.trusted,
        }
    }

    /// Server instructions from `initialize`, if connected.
    pub fn instructions(&self) -> Option<String> {
        self.client()
            .ok()
            .and_then(|c| c.init().instructions.clone())
    }

    /// Reconnect now (after OAuth, after "failed", or to pick up a new binary).
    pub fn restart(&self) {
        let _ = self.inner.control.send(Control::Restart);
    }

    pub fn set_enabled(&self, enabled: bool) {
        let _ = self.inner.control.send(Control::Enable(enabled));
    }

    /// Stop for good (server removed or daemon shutting down): closes the connection and
    /// kills a stdio server's tree.
    pub fn stop(&self) {
        self.inner.stop.cancel();
    }

    /// `mcp.auth`: the URL to open for interactive authorisation.
    pub async fn authorize(&self) -> Result<String, McpError> {
        let Ok(ServerConfig::Http { url, .. }) = &self.inner.config else {
            return Err(McpError::Config("only remote servers use OAuth".into()));
        };
        let challenge = self.inner.challenge.read().ok().and_then(|c| c.clone());
        self.inner
            .rt
            .auth
            .begin(&AuthKey::new(&self.inner.name, url), challenge.as_deref())
            .await
            .map_err(McpError::Transport)
    }

    fn client(&self) -> Result<Arc<McpClient>, McpError> {
        let c = self.inner.client.read().ok().and_then(|c| c.clone());
        match c {
            Some(c) if !c.closed().is_cancelled() => Ok(c),
            _ => Err(McpError::NotConnected {
                server: self.inner.name.clone(),
                status: status_str(self.status()).to_string(),
            }),
        }
    }

    /// `tools/call`. The result is untrusted unless the server is trusted (SA-29).
    pub async fn call_tool(
        &self,
        tool: &str,
        args: Value,
        opts: CallOpts,
    ) -> Result<ToolResult, McpError> {
        let c = self.client()?;
        let args = if args.is_null() { json!({}) } else { args };
        let raw = c
            .request(
                "tools/call",
                json!({"name": tool, "arguments": args}),
                RequestOpts {
                    timeout: Some(opts.timeout.unwrap_or(self.inner.opts.timeouts.tool)),
                    cancel: opts.cancel,
                    progress: opts.progress,
                },
            )
            .await?;
        Ok(result::tool_result(&raw, self.taint()))
    }

    pub async fn list_resources(&self) -> Result<Vec<Value>, McpError> {
        self.client()?
            .list_all("resources/list", "resources", self.inner.opts.timeouts.list)
            .await
    }

    pub async fn list_resource_templates(&self) -> Result<Vec<Value>, McpError> {
        self.client()?
            .list_all(
                "resources/templates/list",
                "resourceTemplates",
                self.inner.opts.timeouts.list,
            )
            .await
    }

    pub async fn read_resource(&self, uri: &str) -> Result<ResourceRead, McpError> {
        let raw = self
            .client()?
            .request("resources/read", json!({"uri": uri}), self.req_opts())
            .await?;
        Ok(ResourceRead {
            parts: result::resource_contents(&raw),
            taint: self.taint(),
        })
    }

    /// `resources/subscribe` (when the server supports it); updates arrive as
    /// [`McpEvent::ResourceUpdated`].
    pub async fn subscribe_resource(&self, uri: &str) -> Result<bool, McpError> {
        let c = self.client()?;
        if !c.init().resources_subscribe() {
            return Ok(false);
        }
        c.request("resources/subscribe", json!({"uri": uri}), self.req_opts())
            .await?;
        Ok(true)
    }

    pub async fn list_prompts(&self) -> Result<Vec<Value>, McpError> {
        self.client()?
            .list_all("prompts/list", "prompts", self.inner.opts.timeouts.list)
            .await
    }

    pub async fn get_prompt(
        &self,
        name: &str,
        arguments: BTreeMap<String, String>,
    ) -> Result<PromptResult, McpError> {
        let raw = self
            .client()?
            .request(
                "prompts/get",
                json!({"name": name, "arguments": arguments}),
                self.req_opts(),
            )
            .await?;
        let messages = raw
            .get("messages")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .map(|m| {
                let role = m
                    .get("role")
                    .and_then(Value::as_str)
                    .unwrap_or("user")
                    .to_string();
                let part = m
                    .get("content")
                    .map(result::content_part)
                    .unwrap_or_else(|| Part::Text(String::new()));
                (role, part)
            })
            .collect();
        Ok(PromptResult {
            description: raw
                .get("description")
                .and_then(Value::as_str)
                .map(String::from),
            messages,
            taint: self.taint(),
        })
    }

    fn req_opts(&self) -> RequestOpts {
        RequestOpts {
            timeout: Some(self.inner.opts.timeouts.request),
            ..RequestOpts::default()
        }
    }
}

pub(crate) fn status_str(s: McpStatus) -> &'static str {
    match s {
        McpStatus::Pending => "pending",
        McpStatus::Connected => "connected",
        McpStatus::Disabled => "disabled",
        McpStatus::Failed => "failed",
        McpStatus::NeedsAuth => "needs_auth",
        McpStatus::Unknown => "unknown",
    }
}

impl Inner {
    fn set(&self, status: McpStatus, error: Option<String>) {
        let snap = Snapshot { status, error };
        if *self.status.borrow() == snap {
            return;
        }
        self.logs.push(
            "lifecycle",
            &match &snap.error {
                Some(e) => format!("{}: {e}", status_str(status)),
                None => status_str(status).to_string(),
            },
        );
        let _ = self.events.send(McpEvent::Status {
            server: self.name.clone(),
            status,
            error: snap.error.clone(),
        });
        self.status.send_replace(snap);
    }

    fn set_client(&self, c: Option<Arc<McpClient>>) {
        if let Ok(mut g) = self.client.write() {
            if let Some(old) = g.take() {
                old.close();
            }
            *g = c;
        }
    }

    fn event(&self, f: impl FnOnce(String) -> McpEvent) {
        let _ = self.events.send(f(self.name.clone()));
    }

    async fn connect_once(
        &self,
        cfg: &ServerConfig,
        notify: mpsc::UnboundedSender<ServerNotification>,
        timeout: Duration,
    ) -> Result<Arc<McpClient>, McpError> {
        let env = ConnectEnv {
            server: self.name.clone(),
            tasks: self.rt.tasks.clone(),
            logs: self.logs.clone(),
            secrets: self.rt.secrets.clone(),
            auth: self.rt.auth.clone(),
            parent: self.stop.clone(),
        };
        let pair = self.rt.connector.connect(cfg, &env).await?;
        let ctx = ClientCtx {
            server: self.name.clone(),
            tasks: self.rt.tasks.clone(),
            logs: self.logs.clone(),
            handler: self.rt.handler.clone(),
            notify,
            sampling_allowed: self.opts.sampling_allowed,
            sampling_budget: self.budget.clone(),
            client_name: self.rt.client_name.clone(),
            client_version: self.rt.client_version.clone(),
        };
        let client = Arc::new(McpClient::connect(pair, ctx, timeout).await?);
        Ok(client)
    }

    /// Connect, initialize and list tools, retrying transient failures until the startup
    /// deadline.
    async fn connect(
        &self,
        cfg: &ServerConfig,
        notify: &mpsc::UnboundedSender<ServerNotification>,
    ) -> Result<Arc<McpClient>, McpError> {
        let deadline = Instant::now() + self.opts.timeouts.startup;
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            let attempt = async {
                let c = self.connect_once(cfg, notify.clone(), left).await?;
                self.reload_tools(&c).await?;
                Ok::<_, McpError>(c)
            };
            let r = tokio::time::timeout(left, attempt)
                .await
                .unwrap_or_else(|_| {
                    Err(McpError::Timeout {
                        method: "startup".into(),
                        secs: self.opts.timeouts.startup.as_secs(),
                    })
                });
            match r {
                Ok(c) => return Ok(c),
                Err(e)
                    if e.is_transient()
                        && Instant::now() + self.opts.backoff.connect_retry < deadline =>
                {
                    self.logs
                        .push("lifecycle", &format!("connect failed, retrying: {e}"));
                    tokio::select! {
                        () = self.stop.cancelled() => return Err(McpError::Cancelled),
                        () = tokio::time::sleep(self.opts.backoff.connect_retry) => {}
                    }
                }
                Err(e) => return Err(e),
            }
        }
    }

    async fn reload_tools(&self, c: &McpClient) -> Result<(), McpError> {
        let tools = if c.init().has("tools") {
            c.list_all("tools/list", "tools", self.opts.timeouts.list)
                .await?
        } else {
            Vec::new()
        };
        let tools: Vec<McpTool> = tools
            .iter()
            .filter_map(|t| McpTool::from_raw(&self.name, t))
            .collect();
        for t in &tools {
            for w in &t.schema_warnings {
                self.logs.push("schema", &format!("{}: {w}", t.name));
            }
        }
        if let Ok(mut g) = self.tools.write() {
            *g = tools;
        }
        Ok(())
    }
}

enum Next {
    /// Try again after the backoff for this many consecutive failures.
    Retry(u32),
    /// Reconnect immediately.
    Now,
    /// Wait for restart()/enable.
    Park,
    Stop,
}

async fn supervise(s: Arc<Inner>, mut ctl: mpsc::UnboundedReceiver<Control>) {
    let mut enabled = s.opts.enabled;
    let mut failures: u32 = 0;
    loop {
        if s.stop.is_cancelled() {
            break;
        }
        let cfg = match &s.config {
            Ok(c) => c.clone(),
            Err(e) => {
                s.set(McpStatus::Failed, Some(e.clone()));
                s.stop.cancelled().await;
                break;
            }
        };
        let next = if enabled {
            run_once(&s, &cfg, &mut ctl, &mut enabled, &mut failures).await
        } else {
            s.set(McpStatus::Disabled, None);
            Next::Park
        };
        match next {
            Next::Stop => break,
            Next::Now => continue,
            Next::Retry(n) if n < s.opts.backoff.max_attempts => {
                let d = s.opts.backoff.delay(n);
                s.logs.push(
                    "lifecycle",
                    &format!("reconnecting in {} ms", d.as_millis()),
                );
                tokio::select! {
                    () = s.stop.cancelled() => break,
                    () = tokio::time::sleep(d) => {}
                    c = ctl.recv() => match c {
                        None => break,
                        Some(Control::Restart) => failures = 0,
                        Some(Control::Enable(e)) => enabled = e,
                    },
                }
            }
            Next::Retry(_) | Next::Park => {
                tokio::select! {
                    () = s.stop.cancelled() => break,
                    c = ctl.recv() => match c {
                        None => break,
                        Some(Control::Restart) => failures = 0,
                        Some(Control::Enable(e)) => { enabled = e; failures = 0; }
                    },
                }
            }
        }
    }
    s.set_client(None);
    if let Ok(mut t) = s.tools.write() {
        t.clear();
    }
    s.set(McpStatus::Disabled, Some("stopped".into()));
}

async fn run_once(
    s: &Arc<Inner>,
    cfg: &ServerConfig,
    ctl: &mut mpsc::UnboundedReceiver<Control>,
    enabled: &mut bool,
    failures: &mut u32,
) -> Next {
    s.set(McpStatus::Pending, None);
    let (ntx, mut nrx) = mpsc::unbounded_channel();
    let connected = tokio::select! {
        () = s.stop.cancelled() => return Next::Stop,
        r = s.connect(cfg, &ntx) => r,
    };
    let client = match connected {
        Ok(c) => c,
        Err(McpError::NeedsAuth { www_authenticate }) => {
            if let Ok(mut ch) = s.challenge.write() {
                *ch = www_authenticate;
            }
            s.set(McpStatus::NeedsAuth, Some("authorisation required".into()));
            return Next::Park;
        }
        Err(McpError::Cancelled) => return Next::Stop,
        Err(e) => {
            *failures += 1;
            s.set(McpStatus::Failed, Some(e.to_string()));
            return Next::Retry(*failures);
        }
    };
    let started = Instant::now();
    s.set_client(Some(client.clone()));
    s.set(McpStatus::Connected, None);
    s.event(|server| McpEvent::ToolsChanged { server });
    let closed = client.closed();
    let mut reload_at: Option<Instant> = None;
    let far = Instant::now() + Duration::from_secs(365 * 24 * 3600);
    let reason = loop {
        tokio::select! {
            () = s.stop.cancelled() => return Next::Stop,
            () = closed.cancelled() => break client.close_reason().unwrap_or(CloseReason::Local),
            c = ctl.recv() => match c {
                None => return Next::Stop,
                Some(Control::Restart) => {
                    s.set_client(None);
                    *failures = 0;
                    return Next::Now;
                }
                Some(Control::Enable(true)) => {}
                Some(Control::Enable(false)) => {
                    s.set_client(None);
                    if let Ok(mut t) = s.tools.write() {
                        t.clear();
                    }
                    *enabled = false;
                    s.set(McpStatus::Disabled, None);
                    s.event(|server| McpEvent::ToolsChanged { server });
                    return Next::Park;
                }
            },
            n = nrx.recv() => match n {
                Some(ServerNotification::ToolsListChanged) => {
                    reload_at = Some(Instant::now() + s.opts.backoff.debounce);
                }
                Some(ServerNotification::ResourcesListChanged) => s.event(|server| McpEvent::ResourcesChanged { server }),
                Some(ServerNotification::PromptsListChanged) => s.event(|server| McpEvent::PromptsChanged { server }),
                Some(ServerNotification::ResourceUpdated { uri }) => s.event(|server| McpEvent::ResourceUpdated { server, uri }),
                None => {}
            },
            () = tokio::time::sleep_until(reload_at.unwrap_or(far)), if reload_at.is_some() => {
                reload_at = None;
                match s.reload_tools(&client).await {
                    Ok(()) => s.event(|server| McpEvent::ToolsChanged { server }),
                    Err(e) => s.logs.push("lifecycle", &format!("tools reload failed: {e}")),
                }
            }
        }
    };
    s.set_client(None);
    if let Ok(mut t) = s.tools.write() {
        t.clear();
    }
    s.event(|server| McpEvent::ToolsChanged { server });
    match reason {
        CloseReason::Unauthorized { www_authenticate } => {
            if let Ok(mut ch) = s.challenge.write() {
                *ch = www_authenticate;
            }
            s.set(McpStatus::NeedsAuth, Some("authorisation required".into()));
            Next::Park
        }
        CloseReason::SessionExpired => {
            s.logs.push("lifecycle", "session expired; reconnecting");
            Next::Now
        }
        CloseReason::Local => Next::Stop,
        r => {
            if started.elapsed() >= s.opts.backoff.stable_after {
                *failures = 0;
            }
            *failures += 1;
            s.set(McpStatus::Failed, Some(r.to_string()));
            Next::Retry(*failures)
        }
    }
}
