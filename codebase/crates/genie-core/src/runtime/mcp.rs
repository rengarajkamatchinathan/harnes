//! MCP in the engine (06 §7, D30, D43): `genie-mcp` managers fed from the settings hub,
//! server tools exposed to the turn loop, elicitation/sampling as asks.
//!
//! - **Managers.** One [`McpManager`] for user + managed servers (shared by every
//!   session), and one per project root for that project's `.mcp.json` / project-settings
//!   servers (only after trust **and** definition-hash approval, D43) and local servers.
//!   A project server overrides a user server of the same name (later scope wins).
//! - **Tools.** [`McpHub`] is a [`ToolProvider`]: each turn sees the connected servers'
//!   tools as `mcp__<server>__<tool>` [`Tool`]s. Calls are authorised through the turn's
//!   gate (permission resource = the tool name) and their results taint the turn unless
//!   the server is trusted (SA-29).
//! - **Asks.** Elicitation and sampling requests become asks of the turn whose tool call is
//!   running on that server ([`ActiveTurns`]); sampling then runs the chat's model.
//! - **Status.** Server status changes are broadcast for `mcp.status` on the workspace feed.

use std::borrow::Cow;
use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use futures::StreamExt;
use futures::future::BoxFuture;
use genie_mcp::auth::{AuthProvider, NoAuth};
use genie_mcp::config::{LiteralSecrets, McpScope, ScopedServer, SecretResolver, merge_scopes};
use genie_mcp::handler::{
    ClientHandler, ElicitRequest, ElicitResponse, SamplingRefusal, SamplingRequest,
    SamplingResponse,
};
use genie_mcp::manager::FIRST_CALL_GRACE;
use genie_mcp::result::Part as McpPart;
use genie_mcp::transport::Connector;
use genie_mcp::{
    McpEvent, McpManager, McpServer, McpServerInfo, McpStatus, McpTool, ServerOptions,
};
use genie_protocol::api::session::ToolSource;
use genie_protocol::ask::{AskAnswer, Decision, Preview};
use genie_protocol::common::{ConfigScope, SessionKind};
use genie_protocol::workspace::McpStatusChanged;
use genie_providers::event::StreamEvent;
use genie_providers::model::{LlmRequest, Message, Part, Role, TextPart, ToolChoice};
use genie_tools::{PermissionRequest, Tool, ToolCtx, ToolEnv, ToolError, ToolKind, ToolOutput};
use serde_json::{Map, Value, json};
use tokio::sync::broadcast;
use tokio_util::sync::CancellationToken;

use crate::config::{ConfigHub, project_root};
use crate::ids::new_id;
use crate::runtime::active::ActiveTurns;
use crate::runtime::ports::{
    ModelResolver, ProvidedTool, SessionSettings, SettingsSource, ToolProvider,
};
use crate::task::TaskScope;

/// Ports the MCP runtime needs from the daemon.
pub struct McpPorts {
    pub connector: Arc<dyn Connector>,
    pub auth: Arc<dyn AuthProvider>,
    pub secrets: Arc<dyn SecretResolver>,
}

impl McpPorts {
    /// stdio + Streamable HTTP (`reqwest`), no OAuth, literal secrets.
    pub fn default_ports() -> Result<McpPorts, String> {
        let http = genie_mcp::transport::http::ReqwestClient::new()?;
        Ok(McpPorts {
            connector: Arc::new(genie_mcp::transport::DefaultConnector::new(Arc::new(http))),
            auth: Arc::new(NoAuth),
            secrets: Arc::new(LiteralSecrets),
        })
    }
}

/// Sessions with a tool call running on each server (newest last).
#[derive(Default)]
struct Calls(Mutex<HashMap<String, Vec<String>>>);

struct CallGuard<'a> {
    calls: &'a Calls,
    server: String,
    session: String,
}

impl Calls {
    fn enter(&self, server: &str, session: &str) -> CallGuard<'_> {
        self.0
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .entry(server.to_string())
            .or_default()
            .push(session.to_string());
        CallGuard {
            calls: self,
            server: server.to_string(),
            session: session.to_string(),
        }
    }

    fn current(&self, server: &str) -> Option<String> {
        self.0
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(server)
            .and_then(|v| v.last().cloned())
    }
}

impl Drop for CallGuard<'_> {
    fn drop(&mut self) {
        let mut g = self.calls.0.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(v) = g.get_mut(&self.server)
            && let Some(i) = v.iter().rposition(|s| *s == self.session)
        {
            v.remove(i);
        }
    }
}

/// Elicitation and sampling → asks of the calling turn (06 §7).
struct AskHandler {
    turns: Arc<ActiveTurns>,
    calls: Arc<Calls>,
    settings: Arc<dyn SettingsSource>,
    models: Option<Arc<dyn ModelResolver>>,
}

fn accepted(a: &AskAnswer) -> bool {
    matches!(
        a.decision,
        Decision::Once | Decision::Always | Decision::Submit
    )
}

#[async_trait]
impl ClientHandler for AskHandler {
    async fn elicit(
        &self,
        server: &str,
        req: ElicitRequest,
        cancel: CancellationToken,
    ) -> ElicitResponse {
        let Some(turn) = self.calls.current(server).and_then(|s| self.turns.get(&s)) else {
            return ElicitResponse::decline();
        };
        let ask = req.to_ask(server, "", "", "");
        tokio::select! {
            r = turn.ask_with(ask) => match r {
                Ok(v) => serde_json::from_value::<AskAnswer>(v)
                    .map(|a| ElicitResponse::from_answer(&a))
                    .unwrap_or_else(|_| ElicitResponse::cancel()),
                Err(_) => ElicitResponse::cancel(),
            },
            () = cancel.cancelled() => ElicitResponse::cancel(),
        }
    }

    async fn sample(
        &self,
        server: &str,
        req: SamplingRequest,
        budget_left_tokens: u64,
        cancel: CancellationToken,
    ) -> Result<SamplingResponse, SamplingRefusal> {
        let session = self
            .calls
            .current(server)
            .ok_or(SamplingRefusal::Unavailable)?;
        let turn = self
            .turns
            .get(&session)
            .ok_or(SamplingRefusal::Unavailable)?;
        let models = self.models.clone().ok_or(SamplingRefusal::Unavailable)?;
        let settings = self
            .settings
            .settings(&session)
            .ok_or(SamplingRefusal::Unavailable)?;
        let model = models
            .resolve(&settings.model)
            .map_err(|_| SamplingRefusal::Unavailable)?;
        let ask = req.to_ask(
            server,
            &model.display_name,
            0.0,
            budget_left_tokens,
            "",
            "",
            "",
        );
        let ans = tokio::select! {
            r = turn.ask_with(ask) => r.ok().and_then(|v| serde_json::from_value::<AskAnswer>(v).ok()),
            () = cancel.cancelled() => None,
        };
        let Some(ans) = ans.filter(accepted) else {
            return Err(SamplingRefusal::Rejected(
                "the user did not approve the request".into(),
            ));
        };
        let _ = ans;
        let messages: Vec<Message> = req
            .messages
            .iter()
            .map(|m| {
                let role = match m.get("role").and_then(Value::as_str) {
                    Some("assistant") => Role::Assistant,
                    _ => Role::User,
                };
                let text = m
                    .get("content")
                    .and_then(|c| c.get("text"))
                    .and_then(Value::as_str)
                    .unwrap_or_default();
                Message::new(new_id("msg"), role, vec![Part::text(text)])
            })
            .collect();
        let mut lr = LlmRequest::new(model.route.model.clone(), messages);
        if let Some(sp) = &req.system_prompt {
            lr.system = vec![TextPart {
                text: sp.clone(),
                ..TextPart::default()
            }];
        }
        lr.max_output_tokens = Some(u32::try_from(req.max_tokens).unwrap_or(u32::MAX));
        lr.tool_choice = ToolChoice::None;
        let token = cancel.child_token();
        let mut s = model.client.stream(&model.route, lr, token.clone());
        let mut text = String::new();
        let mut tokens = 0u64;
        loop {
            let next = tokio::select! {
                n = s.next() => n,
                () = cancel.cancelled() => return Err(SamplingRefusal::Unavailable),
            };
            match next {
                None => break,
                Some(Err(e)) => {
                    return Err(SamplingRefusal::Rejected(format!("model call failed: {e}")));
                }
                Some(Ok(StreamEvent::TextDelta { delta, .. })) => text.push_str(&delta),
                Some(Ok(
                    StreamEvent::StepFinish { usage, .. } | StreamEvent::Finish { usage, .. },
                )) => {
                    tokens = tokens.max(usage.input_total + usage.output_total);
                }
                Some(Ok(_)) => {}
            }
        }
        Ok(SamplingResponse {
            model: model.route.model.wire_id.clone(),
            content: json!({"type": "text", "text": text}),
            stop_reason: Some("endTurn".into()),
            tokens_used: tokens,
        })
    }
}

/// One MCP server tool as a [`Tool`].
struct McpToolAdapter {
    def: McpTool,
    /// `mcp__<server>__<tool>` (what the model calls).
    exposed: String,
    server: McpServer,
    calls: Arc<Calls>,
    description: String,
}

#[async_trait]
impl Tool for McpToolAdapter {
    fn name(&self) -> &str {
        &self.exposed
    }

    fn description(&self, _env: &ToolEnv) -> Cow<'_, str> {
        Cow::Borrowed(&self.description)
    }

    fn input_schema(&self) -> &Value {
        &self.def.input_schema
    }

    fn kind(&self) -> ToolKind {
        ToolKind::External
    }

    fn authorizes(&self) -> bool {
        true
    }

    async fn execute(&self, input: Value, ctx: ToolCtx) -> Result<ToolOutput, ToolError> {
        ctx.authorize(PermissionRequest {
            resource: Some(self.def.model_name.clone()),
            preview: Some(Preview::McpArgs {
                server: self.def.server.clone(),
                tool: self.def.name.clone(),
                args: input.clone(),
            }),
            save_suggestions: Vec::new(),
        })
        .await?;
        let _guard = self.calls.enter(&self.def.server, &ctx.env.session_id);
        let (ptx, mut prx) = tokio::sync::mpsc::unbounded_channel::<genie_mcp::Progress>();
        let call = self.server.call_tool(
            &self.def.name,
            input,
            genie_mcp::CallOpts {
                cancel: Some(ctx.cancel.clone()),
                progress: Some(ptx),
                timeout: None,
            },
        );
        tokio::pin!(call);
        let res = loop {
            tokio::select! {
                r = &mut call => break r,
                Some(p) = prx.recv() => {
                    let fraction = p.total.filter(|t| *t > 0.0).map(|t| p.progress / t);
                    ctx.progress(p.message.unwrap_or_default(), fraction);
                }
            }
        };
        match res {
            Ok(r) if r.is_error => Err(ToolError::Failed {
                message: r.to_text(),
                may_have_run: true,
            }),
            Ok(r) => Ok(ToolOutput::Text(r.to_text())),
            Err(e) => Err(ToolError::Failed {
                message: e.to_string(),
                may_have_run: true,
            }),
        }
    }
}

/// Per-session "+" toggles (`mcp.set_enabled {session_id}`).
#[derive(Default, Clone)]
struct Toggles {
    servers_off: HashSet<String>,
    tools_off: HashSet<String>,
}

/// See the module docs.
pub struct McpHub {
    hub: Arc<ConfigHub>,
    scope: TaskScope,
    ports: McpPorts,
    handler: Arc<AskHandler>,
    calls: Arc<Calls>,
    managers: Mutex<HashMap<Option<PathBuf>, Arc<McpManager>>>,
    /// Project servers waiting for definition approval, per root (D43).
    awaiting: Mutex<HashMap<PathBuf, Vec<McpServerInfo>>>,
    toggles: Mutex<HashMap<String, Toggles>>,
    status: broadcast::Sender<McpStatusChanged>,
    store: Option<Arc<genie_store::Store>>,
}

fn obj(v: Option<Value>) -> Map<String, Value> {
    match v {
        Some(Value::Object(m)) => m,
        _ => Map::new(),
    }
}

impl McpHub {
    pub fn new(
        hub: Arc<ConfigHub>,
        scope: TaskScope,
        ports: McpPorts,
        settings: Arc<dyn SettingsSource>,
        models: Option<Arc<dyn ModelResolver>>,
        turns: Arc<ActiveTurns>,
        store: Option<Arc<genie_store::Store>>,
    ) -> Arc<McpHub> {
        let calls = Arc::new(Calls::default());
        Arc::new(McpHub {
            hub,
            scope,
            ports,
            handler: Arc::new(AskHandler {
                turns,
                calls: calls.clone(),
                settings,
                models,
            }),
            calls,
            managers: Mutex::new(HashMap::new()),
            awaiting: Mutex::new(HashMap::new()),
            toggles: Mutex::new(HashMap::new()),
            status: broadcast::channel(256).0,
            store,
        })
    }

    /// `mcp.status` changes for the workspace feed.
    pub fn subscribe_status(&self) -> broadcast::Receiver<McpStatusChanged> {
        self.status.subscribe()
    }

    fn manager(&self, root: Option<&Path>) -> Arc<McpManager> {
        let key = root.map(Path::to_path_buf);
        let (m, fresh) = {
            let mut g = self.managers.lock().unwrap_or_else(|e| e.into_inner());
            if let Some(m) = g.get(&key) {
                (m.clone(), false)
            } else {
                let m = Arc::new(McpManager::new(genie_mcp::Runtime {
                    tasks: self.scope.tracker(),
                    cancel: self.scope.token().child_token(),
                    connector: self.ports.connector.clone(),
                    handler: self.handler.clone(),
                    auth: self.ports.auth.clone(),
                    secrets: self.ports.secrets.clone(),
                    client_name: "genie".into(),
                    client_version: env!("CARGO_PKG_VERSION").into(),
                }));
                g.insert(key, m.clone());
                (m, true)
            }
        };
        if fresh {
            let mut rx = m.subscribe();
            let tx = self.status.clone();
            self.scope.spawn_cancellable(async move {
                loop {
                    match rx.recv().await {
                        Ok(McpEvent::Status {
                            server,
                            status,
                            error,
                        }) => {
                            let _ = tx.send(McpStatusChanged {
                                name: server,
                                status,
                                error,
                            });
                        }
                        Ok(_) | Err(broadcast::error::RecvError::Lagged(_)) => {}
                        Err(broadcast::error::RecvError::Closed) => break,
                    }
                }
            });
            self.reconcile_one(root, &m);
        }
        m
    }

    fn options(&self, d: &ScopedServer) -> ServerOptions {
        let policy = self.hub.policy();
        ServerOptions {
            trusted: policy.strings("mcp.trusted").contains(&d.name),
            sampling_allowed: policy.get("mcp.sampling").and_then(Value::as_str) != Some("off"),
            ..ServerOptions::default()
        }
    }

    /// Policy filters (08 §5.2): `mcp.allowed` (names), `mcp.disableLocal` (no stdio
    /// servers except managed ones).
    fn allowed(&self, d: &ScopedServer) -> bool {
        let policy = self.hub.policy();
        let allow = policy.strings("mcp.allowed");
        if !allow.is_empty() && !allow.contains(&d.name) && d.scope != McpScope::Managed {
            return false;
        }
        let local = matches!(&d.config, Ok(c) if !c.is_remote());
        !(local && d.scope != McpScope::Managed && policy.bool("mcp.disableLocal") == Some(true))
    }

    /// The definitions a manager should run, and the project servers awaiting approval.
    fn definitions(&self, root: Option<&Path>) -> (Vec<ScopedServer>, Vec<McpServerInfo>) {
        let mut layers: Vec<(McpScope, Map<String, Value>)> = Vec::new();
        let mut awaiting = Vec::new();
        match root {
            None => {
                layers.push((
                    McpScope::User,
                    obj(self.hub.layer_value(None, ConfigScope::User, "mcpServers")),
                ));
                layers.push((
                    McpScope::Managed,
                    obj(self.hub.policy().get("mcpServers").cloned()),
                ));
            }
            Some(r) => {
                if self.hub.trusted(r) {
                    let mut project = Map::new();
                    for d in self.hub.project_definitions(r) {
                        if self.hub.is_approved(r, &d) {
                            project.insert(d.name, d.value);
                        } else {
                            awaiting.push(McpServerInfo {
                                name: d.name,
                                status: McpStatus::Disabled,
                                scope: ConfigScope::Project,
                                tools: 0,
                                error: Some(
                                    "waiting for approval of this project server (trust.approve_definition)"
                                        .into(),
                                ),
                            });
                        }
                    }
                    layers.push((McpScope::Project, project));
                    layers.push((
                        McpScope::Local,
                        obj(self
                            .hub
                            .layer_value(Some(r), ConfigScope::Local, "mcpServers")),
                    ));
                }
            }
        }
        let defs = merge_scopes(layers)
            .into_iter()
            .filter(|d| self.allowed(d))
            .collect();
        (defs, awaiting)
    }

    fn reconcile_one(&self, root: Option<&Path>, m: &McpManager) {
        let (defs, awaiting) = self.definitions(root);
        m.apply(defs, |d| self.options(d));
        if let Some(r) = root {
            self.awaiting
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .insert(r.to_path_buf(), awaiting);
        }
    }

    /// Settings, trust or policy changed: reconcile every manager (start/stop/restart by
    /// definition hash).
    pub fn reconcile_all(&self) {
        let ms: Vec<(Option<PathBuf>, Arc<McpManager>)> = self
            .managers
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .iter()
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect();
        for (root, m) in ms {
            self.reconcile_one(root.as_deref(), &m);
        }
    }

    /// The managers a folder sees: its project manager (if any) and the shared one.
    fn managers_for(&self, cwd: Option<&Path>) -> (Arc<McpManager>, Option<Arc<McpManager>>) {
        let global = self.manager(None);
        let root = cwd.map(project_root);
        (global, root.map(|r| self.manager(Some(&r))))
    }

    fn session_root(s: &SessionSettings) -> Option<PathBuf> {
        (s.kind == SessionKind::Code)
            .then(|| s.workspace.clone())
            .flatten()
    }

    /// `mcp.list`.
    pub fn list(&self, cwd: Option<&Path>) -> Vec<McpServerInfo> {
        let (global, local) = self.managers_for(cwd);
        let mut out: BTreeMap<String, McpServerInfo> = global
            .list()
            .into_iter()
            .map(|i| (i.name.clone(), i))
            .collect();
        if let Some(l) = local {
            for i in l.list() {
                out.insert(i.name.clone(), i);
            }
        }
        if let Some(r) = cwd.map(project_root) {
            for i in self
                .awaiting
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .get(&r)
                .cloned()
                .unwrap_or_default()
            {
                out.insert(i.name.clone(), i);
            }
        }
        out.into_values().collect()
    }

    /// A running server by name: project managers first, then the shared one.
    pub fn server(&self, name: &str) -> Option<McpServer> {
        let ms: Vec<(Option<PathBuf>, Arc<McpManager>)> = self
            .managers
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .iter()
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect();
        let mut best = None;
        for (root, m) in ms {
            if let Ok(s) = m.get(name) {
                if root.is_some() {
                    return Some(s);
                }
                best = Some(s);
            }
        }
        best.or_else(|| self.manager(None).get(name).ok())
    }

    /// `mcp.add`: validate, write the entry to the scope's file, reconcile.
    pub fn add(
        &self,
        scope: ConfigScope,
        name: &str,
        config: Value,
        cwd: Option<&Path>,
    ) -> Result<(), String> {
        genie_mcp::ServerConfig::from_value(&config).map_err(|e| e.to_string())?;
        if name.is_empty() {
            return Err("name must not be empty".into());
        }
        let root = cwd.map(project_root);
        self.hub
            .edit_mcp_servers(root.as_deref(), scope, |m| {
                m.insert(name.to_string(), config);
                true
            })
            .map_err(|e| e.to_string())?;
        self.reconcile_all();
        if let Some(r) = &root {
            let _ = self.manager(Some(r));
        }
        Ok(())
    }

    /// `mcp.remove`.
    pub fn remove(
        &self,
        scope: ConfigScope,
        name: &str,
        cwd: Option<&Path>,
    ) -> Result<bool, String> {
        let root = cwd.map(project_root);
        let removed = self
            .hub
            .edit_mcp_servers(root.as_deref(), scope, |m| m.remove(name).is_some())
            .map_err(|e| e.to_string())?;
        self.reconcile_all();
        Ok(removed)
    }

    /// `mcp.set_enabled`: per session (and optionally one tool), or the server itself.
    pub fn set_enabled(
        &self,
        name: &str,
        enabled: bool,
        session_id: Option<&str>,
        tool: Option<&str>,
    ) -> Result<(), String> {
        match session_id {
            Some(sid) => {
                let mut g = self.toggles.lock().unwrap_or_else(|e| e.into_inner());
                let t = g.entry(sid.to_string()).or_default();
                let (set, key) = match tool {
                    Some(tool) => (&mut t.tools_off, genie_mcp::naming::tool_name(name, tool)),
                    None => (&mut t.servers_off, name.to_string()),
                };
                if enabled {
                    set.remove(&key);
                } else {
                    set.insert(key);
                }
                Ok(())
            }
            None => {
                let s = self
                    .server(name)
                    .ok_or_else(|| format!("no MCP server `{name}`"))?;
                s.set_enabled(enabled);
                Ok(())
            }
        }
    }

    /// `mcp.resource.read`: the first content item, stored as a blob.
    pub async fn read_resource(&self, name: &str, uri: &str) -> Result<(String, String), String> {
        let s = self
            .server(name)
            .ok_or_else(|| format!("no MCP server `{name}`"))?;
        let r = s.read_resource(uri).await.map_err(|e| e.to_string())?;
        let (bytes, mime) = match r.parts.into_iter().next() {
            Some(McpPart::Binary { mime, data, .. }) => (data, mime),
            Some(McpPart::Resource { mime, text, .. }) => (
                text.into_bytes(),
                mime.unwrap_or_else(|| "text/plain".into()),
            ),
            Some(McpPart::Text(t)) => (t.into_bytes(), "text/plain".into()),
            Some(McpPart::Json(v)) => (v.to_string().into_bytes(), "application/json".into()),
            Some(McpPart::ResourceLink { uri, .. }) => (uri.into_bytes(), "text/uri-list".into()),
            None => return Err("the resource is empty".into()),
        };
        let store = self.store.as_ref().ok_or("blob storage is not available")?;
        let id = store
            .put_blob(&bytes, Some(&mime))
            .map_err(|e| e.to_string())?;
        Ok((id, mime))
    }

    /// Stop every server (daemon shutdown).
    pub fn shutdown(&self) {
        for m in self
            .managers
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .values()
        {
            m.shutdown();
        }
    }

    fn session_tools(&self, session_id: &str, s: &SessionSettings) -> Vec<(McpTool, McpServer)> {
        let root = Self::session_root(s);
        let (global, local) = self.managers_for(root.as_deref());
        let toggles = self
            .toggles
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(session_id)
            .cloned()
            .unwrap_or_default();
        let mut out = Vec::new();
        let local_names: HashSet<String> = local
            .as_ref()
            .map(|l| l.servers().iter().map(|s| s.name().to_string()).collect())
            .unwrap_or_default();
        let mut push = |m: &McpManager, skip: &HashSet<String>| {
            for t in m.tools() {
                if skip.contains(&t.server)
                    || toggles.servers_off.contains(&t.server)
                    || toggles.tools_off.contains(&t.model_name)
                {
                    continue;
                }
                if let Ok(s) = m.get(&t.server) {
                    out.push((t, s));
                }
            }
        };
        push(&global, &local_names);
        if let Some(l) = &local {
            push(l, &HashSet::new());
        }
        out
    }
}

impl ToolProvider for McpHub {
    fn tools(&self, session_id: &str, settings: &SessionSettings) -> Vec<ProvidedTool> {
        self.session_tools(session_id, settings)
            .into_iter()
            .map(|(t, server)| {
                let description = if t.description.is_empty() {
                    t.title.clone().unwrap_or_else(|| t.name.clone())
                } else {
                    t.description.clone()
                };
                ProvidedTool {
                    server: Some(t.server.clone()),
                    source: ToolSource::Mcp,
                    tool: Arc::new(McpToolAdapter {
                        exposed: t.model_name.clone(),
                        def: t,
                        server,
                        calls: self.calls.clone(),
                        description,
                    }),
                }
            })
            .collect()
    }

    fn ready(&self, settings: &SessionSettings) -> BoxFuture<'static, ()> {
        let (global, local) = self.managers_for(Self::session_root(settings).as_deref());
        Box::pin(async move {
            global.wait_ready(FIRST_CALL_GRACE).await;
            if let Some(l) = local {
                l.wait_ready(FIRST_CALL_GRACE).await;
            }
        })
    }

    fn taint(&self, tool: &str) -> Option<Option<String>> {
        if !tool.starts_with("mcp__") {
            return None;
        }
        let ms: Vec<Arc<McpManager>> = self
            .managers
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .values()
            .cloned()
            .collect();
        for m in ms {
            if let Some(t) = m.tools().into_iter().find(|t| t.model_name == tool)
                && let Ok(s) = m.get(&t.server)
            {
                let src = s.taint();
                return Some(src.taints().then(|| src.label()));
            }
        }
        Some(Some(format!("the MCP tool `{tool}`")))
    }
}
