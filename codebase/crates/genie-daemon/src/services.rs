//! Daemon methods backed by the engine's subsystems (02 §6.4, §6.8): config, policy,
//! trust, MCP and `tools.list`, plus the background tasks that turn their changes into
//! workspace-feed notifications (`config.changed`, `policy.changed`, `mcp.status`).

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use genie_audit::{Attrs, AuditEvent, Auditor};
use genie_config::{ConfigError, PolicyManager, TrustError};
use genie_core::config::{ConfigHub, HubEvent};
use genie_core::runtime::AgentRuntime;
use genie_core::runtime::mcp::McpHub;
use genie_core::runtime::skills::SkillsHub;
use genie_protocol::api::config::{ApproveDecision, PendingDefinitionsResult, TrustGetResult};
use genie_protocol::api::mcp::{
    AuthUrlResult, BlobMimeResult, LinesResult, McpItemsResult, McpListResult,
};
use genie_protocol::api::mcp::{DirectoryKind, IdResult};
use genie_protocol::api::session::ToolsListResult;
use genie_protocol::common::{ConfigScope, Empty, SessionKind};
use genie_protocol::methods::{self as m, Method};
use genie_protocol::workspace::{ConfigChanged, PolicyChanged};
use genie_protocol::{ErrorKind, Request, Response, RpcError};
use serde::Serialize;
use serde_json::{Value, json};
use tokio::sync::broadcast;

use crate::server::{Conn, DaemonState};

/// Subsystems the daemon serves methods from. Every field is optional so tests can wire
/// only what they exercise; a missing one answers `not_implemented`.
#[derive(Default)]
pub struct Services {
    pub hub: Option<Arc<ConfigHub>>,
    pub mcp: Option<Arc<McpHub>>,
    pub runtime: Option<Arc<AgentRuntime>>,
    pub audit: Option<Arc<Auditor>>,
    /// Managed policy sources, re-read periodically.
    pub policy: Option<Mutex<PolicyManager>>,
    pub skills: Option<Arc<SkillsHub>>,
    /// Blob storage (`skill.upload`).
    pub store: Option<Arc<genie_store::Store>>,
    /// The agent browser (06 §10) and its backend (shut down with the daemon).
    pub browser: Option<Arc<genie_browser::Browser>>,
    pub browser_backend: Option<Arc<dyn genie_browser::BrowserBackend>>,
    pub browser_host: Option<Arc<genie_core::runtime::browser::EngineBrowserHost>>,
    /// Artifacts (D28), projects/retrieval (D32), memory state (D31) and chunked blob
    /// uploads (02 §6.9): served by `crate::projects` / `crate::blobs`.
    pub artifacts: Option<Arc<genie_core::runtime::artifacts::ArtifactHub>>,
    pub projects: Option<Arc<genie_core::runtime::projects::ProjectsHub>>,
    pub memory_ctl: Option<Arc<genie_core::runtime::memory_ctl::MemoryControl>>,
    pub uploads: crate::blobs::Uploads,
    /// Code mode: checkpoints, worktrees, git and files methods (02 §6.10).
    pub code: Option<Arc<crate::code::CodeServices>>,
    /// Secrets unlock, providers, credentials, models, catalog, usage (02 §6.1, §6.5).
    pub providers: Option<Arc<crate::providers::ProviderHub>>,
    /// Telemetry (08 §7): consent from settings + policy, local queue, export loop.
    pub telemetry: Option<Arc<genie_audit::Telemetry>>,
    /// OTLP audit export queue and its transport (08 §6).
    pub otlp: Option<(
        Arc<genie_audit::OtlpSink>,
        Arc<dyn genie_audit::OtlpTransport>,
    )>,
    /// The secrets backend (doctor: backend kind, lock state).
    pub secrets: Option<Arc<crate::secrets::Secrets>>,
    /// The permission evaluator (session-scoped "always" rules for
    /// `permissions.rules.list/remove`).
    pub authz: Option<Arc<genie_core::runtime::permissions::PermissionAuthorizer>>,
    /// The pending `data.delete_all.confirm` confirmation token and when it expires.
    pub delete_token: Mutex<Option<(String, std::time::Instant)>>,
}

pub(crate) enum Handled {
    Now(Result<Value, RpcError>),
    /// Answered later from a task.
    Later,
    NotMine,
}

fn to_result<T: Serialize>(v: &T) -> Result<Value, RpcError> {
    serde_json::to_value(v).map_err(|e| RpcError::internal(e.to_string()))
}

fn params<M: Method>(r: &Request) -> Result<M::Params, RpcError> {
    let v = if r.params.is_null() {
        json!({})
    } else {
        r.params.clone()
    };
    serde_json::from_value(v).map_err(|e| RpcError::invalid_params(e.to_string()))
}

pub(crate) fn not_implemented(method: &str) -> RpcError {
    RpcError::new(
        ErrorKind::MethodNotFound,
        format!("{method} is not available in this build"),
    )
    .with_data(json!({ "reason": "not_implemented" }))
}

pub(crate) fn config_err(e: ConfigError) -> RpcError {
    match e {
        ConfigError::Locked { .. } => RpcError::new(ErrorKind::Locked, e.to_string()),
        ConfigError::Untrusted => RpcError::new(ErrorKind::UntrustedWorkspace, e.to_string()),
        ConfigError::Io(_) => RpcError::new(ErrorKind::InternalError, e.to_string()),
        _ => RpcError::invalid_params(e.to_string()),
    }
}

fn trust_err(e: TrustError) -> RpcError {
    match e {
        TrustError::NotTrusted => RpcError::new(ErrorKind::UntrustedWorkspace, e.to_string()),
        TrustError::PolicyNever => RpcError::new(ErrorKind::PolicyBlocked, e.to_string()),
        TrustError::TooBroad(_) | TrustError::BadDecision => {
            RpcError::invalid_params(e.to_string())
        }
        _ => RpcError::new(ErrorKind::InternalError, e.to_string()),
    }
}

/// Canonical form of a folder path (trust decisions and project roots compare by it).
pub(crate) fn canonical(p: &str) -> String {
    std::fs::canonicalize(p)
        .map(|c| c.display().to_string())
        .unwrap_or_else(|_| p.to_string())
}

fn cwd_path(cwd: Option<&String>) -> Option<PathBuf> {
    cwd.filter(|c| !c.is_empty())
        .map(|c| PathBuf::from(canonical(c)))
}

impl Services {
    fn audit(&self, ev: AuditEvent, attrs: Attrs) {
        if let Some(a) = &self.audit {
            a.record(None, ev, attrs);
        }
    }

    pub(crate) fn policy_problems(&self) -> Vec<genie_protocol::common::Diagnostic> {
        self.policy
            .as_ref()
            .map(|p| p.lock().unwrap_or_else(|e| e.into_inner()).problems())
            .unwrap_or_default()
    }

    /// Methods served here. `NotMine` lets the server fall through to its own table.
    pub(crate) fn dispatch(
        self: &Arc<Self>,
        state: &Arc<DaemonState>,
        conn: &Arc<Conn>,
        r: &Request,
    ) -> Handled {
        // First: the locked-secrets gate and the provider methods.
        if let Some(p) = &self.providers
            && let Some(h) = p.dispatch(state, conn, r)
        {
            return h;
        }
        if r.method.starts_with("browser.") {
            return match &self.browser {
                Some(b) => self.browser_async(b.clone(), state, conn, r),
                None => Handled::NotMine,
            };
        }
        // trust.list, permissions.rules.*, data.delete_all.* (Settings → Privacy).
        if let Some(res) = crate::manage::dispatch(self, state, r) {
            return Handled::Now(res);
        }
        let memory_before = crate::projects::memory_scope(self, r);
        if let Some(res) = crate::projects::dispatch(self, r) {
            if res.is_ok() {
                crate::projects::memory_changed(state, r, memory_before);
            }
            return Handled::Now(res);
        }
        // export.all, session.import, import.*, agents.list (04 §9, 07 §5, 06 §11).
        if let Some(res) = crate::transfer::dispatch(self, state, r) {
            return Handled::Now(res);
        }
        if let Some(h) = crate::ops::dispatch(self, state, conn, r) {
            return h;
        }
        let name = r.method.as_str();
        let hub = || self.hub.clone().ok_or_else(|| not_implemented(name));
        let mcp = || self.mcp.clone().ok_or_else(|| not_implemented(name));
        let res = match name {
            n if n == m::ConfigGet::NAME => params::<m::ConfigGet>(r).and_then(|mut p| {
                p.cwd = p.cwd.map(|c| canonical(&c));
                to_result(&hub()?.get(&p))
            }),
            n if n == m::ConfigSet::NAME => params::<m::ConfigSet>(r).and_then(|p| {
                let cwd = p.cwd.map(|c| canonical(&c));
                hub()?
                    .set(&p.key, p.value, p.scope, cwd.as_deref())
                    .map_err(config_err)?;
                self.audit(
                    AuditEvent::SettingsChanged,
                    Attrs::new()
                        .with("key", p.key)
                        .with("scope", scope_str(p.scope)),
                );
                to_result(&Empty {})
            }),
            n if n == m::ConfigSchema::NAME => hub().and_then(|h| to_result(&h.schema())),
            n if n == m::ConfigDiagnostics::NAME => {
                params::<m::ConfigDiagnostics>(r).and_then(|p| {
                    let cwd = p.cwd.map(|c| canonical(&c));
                    to_result(&hub()?.diagnostics(cwd.as_deref(), self.policy_problems()))
                })
            }
            n if n == m::PolicyEffective::NAME => {
                hub().and_then(|h| to_result(&h.policy_effective()))
            }
            n if n == m::TrustGet::NAME => params::<m::TrustGet>(r).and_then(|p| {
                let path = canonical(&p.path);
                to_result(&TrustGetResult {
                    state: hub()?.trust_get(&path),
                    path,
                })
            }),
            n if n == m::TrustSet::NAME => params::<m::TrustSet>(r).and_then(|p| {
                let path = canonical(&p.path);
                hub()?.trust_set(&path, p.decision).map_err(trust_err)?;
                self.audit(
                    AuditEvent::TrustChanged,
                    Attrs::new()
                        .with("path", path)
                        .with("decision", json!(p.decision)),
                );
                to_result(&Empty {})
            }),
            n if n == m::TrustPendingDefinitions::NAME => params::<m::TrustPendingDefinitions>(r)
                .and_then(|p| {
                    let items = hub()?
                        .pending_definitions(&canonical(&p.path))
                        .map_err(trust_err)?;
                    to_result(&PendingDefinitionsResult { items })
                }),
            n if n == m::TrustApproveDefinition::NAME => params::<m::TrustApproveDefinition>(r)
                .and_then(|p| {
                    let path = canonical(&p.path);
                    let ok = hub()?
                        .approve_definition(&path, p.kind, &p.name, &p.hash, p.decision)
                        .map_err(trust_err)?;
                    if !ok && p.decision == ApproveDecision::Approve {
                        return Err(RpcError::new(
                            ErrorKind::Conflict,
                            "the definition changed; review it again",
                        ));
                    }
                    if ok {
                        self.audit(
                            AuditEvent::TrustChanged,
                            Attrs::new()
                                .with("path", path)
                                .with("approved", p.name)
                                .with("hash", p.hash),
                        );
                    }
                    to_result(&Empty {})
                }),

            n if n == m::McpList::NAME => params::<m::McpList>(r).and_then(|p| {
                let cwd = cwd_path(p.cwd.as_ref());
                to_result(&McpListResult {
                    servers: mcp()?.list(cwd.as_deref()),
                })
            }),
            n if n == m::McpAdd::NAME => params::<m::McpAdd>(r).and_then(|p| {
                let cwd = cwd_path(p.cwd.as_ref());
                mcp()?
                    .add(p.scope, &p.name, p.config, cwd.as_deref())
                    .map_err(RpcError::invalid_params)?;
                if let Some(t) = &self.telemetry {
                    t.usage(genie_audit::UsageEvent::new(
                        genie_audit::UsageName::McpServerAdded,
                    ));
                }
                to_result(&Empty {})
            }),
            n if n == m::McpRemove::NAME => params::<m::McpRemove>(r).and_then(|p| {
                let cwd = cwd_path(p.cwd.as_ref());
                if !mcp()?
                    .remove(p.scope, &p.name, cwd.as_deref())
                    .map_err(RpcError::invalid_params)?
                {
                    return Err(RpcError::new(
                        ErrorKind::NotFound,
                        format!("no MCP server `{}` in that scope", p.name),
                    ));
                }
                to_result(&Empty {})
            }),
            n if n == m::McpSetEnabled::NAME => params::<m::McpSetEnabled>(r).and_then(|p| {
                mcp()?
                    .set_enabled(
                        &p.name,
                        p.enabled,
                        p.session_id.as_deref(),
                        p.tool.as_deref(),
                    )
                    .map_err(|e| RpcError::new(ErrorKind::NotFound, e))?;
                to_result(&Empty {})
            }),
            n if n == m::McpLogs::NAME => params::<m::McpLogs>(r).and_then(|p| {
                let h = mcp()?;
                let s = server(&h, &p.name)?;
                to_result(&LinesResult {
                    lines: s.logs(p.tail as usize),
                })
            }),
            n if n == m::McpTools::NAME => params::<m::McpTools>(r).and_then(|p| {
                let h = mcp()?;
                let s = server(&h, &p.name)?;
                let items = s
                    .tools()
                    .into_iter()
                    .map(|t| {
                        json!({
                            "name": t.name,
                            "model_name": t.model_name,
                            "title": t.title,
                            "description": t.description,
                            "inputSchema": t.input_schema,
                        })
                    })
                    .collect();
                to_result(&McpItemsResult { items })
            }),
            n if n == m::McpResources::NAME
                || n == m::McpPrompts::NAME
                || n == m::McpResourceRead::NAME
                || n == m::McpAuthStart::NAME =>
            {
                match mcp() {
                    Ok(h) => return self.mcp_async(h, conn, r),
                    Err(e) => Err(e),
                }
            }

            n if n == m::ToolsList::NAME => params::<m::ToolsList>(r).and_then(|p| {
                let rt = self.runtime.clone().ok_or_else(|| not_implemented(name))?;
                let tools = rt
                    .tool_list(&p.session_id)
                    .ok_or_else(|| RpcError::new(ErrorKind::NotFound, "session not found"))?;
                to_result(&ToolsListResult { tools })
            }),

            n if n == m::CommandList::NAME => return self.command_list(conn, r),
            n if n == m::SkillList::NAME => self
                .skills
                .as_ref()
                .ok_or_else(|| not_implemented(name))
                .and_then(|s| to_result(&s.list())),
            n if n == m::SkillSetEnabled::NAME => params::<m::SkillSetEnabled>(r).and_then(|p| {
                let s = self.skills.as_ref().ok_or_else(|| not_implemented(name))?;
                s.set_enabled(&p.id, p.enabled).map_err(skill_err)?;
                to_result(&Empty {})
            }),
            n if n == m::SkillUpload::NAME => params::<m::SkillUpload>(r).and_then(|p| {
                let s = self.skills.as_ref().ok_or_else(|| not_implemented(name))?;
                let store = self.store.as_ref().ok_or_else(|| not_implemented(name))?;
                let bytes = store
                    .read_blob(&p.blob_id)
                    .map_err(|_| RpcError::new(ErrorKind::NotFound, "blob not found"))?;
                let inst = s.upload(&bytes).map_err(skill_err)?;
                self.audit(
                    AuditEvent::SkillInstalled,
                    Attrs::new()
                        .with("skill", inst.id.clone())
                        .with("sha256", inst.sha256)
                        .with("source", "upload"),
                );
                to_result(&IdResult { id: inst.id })
            }),
            n if n == m::DirectoryList::NAME || n == m::DirectoryInstall::NAME => {
                let skills_kind = r.params.get("kind").and_then(Value::as_str) == Some("skills");
                match (&self.skills, skills_kind) {
                    (Some(s), true) => return self.directory_async(s.clone(), conn, r),
                    _ => return Handled::NotMine,
                }
            }
            _ => return Handled::NotMine,
        };
        Handled::Now(res)
    }

    /// `browser.*` (02 §6.6). Lease-gated methods check the driver lease first.
    fn browser_async(
        self: &Arc<Self>,
        b: Arc<genie_browser::Browser>,
        state: &Arc<DaemonState>,
        conn: &Arc<Conn>,
        r: &Request,
    ) -> Handled {
        let name = r.method.clone();
        if let Some(info) = m::find(&name)
            && info.needs_lease
        {
            let sid = r
                .params
                .get("session_id")
                .and_then(Value::as_str)
                .unwrap_or("");
            if let Err(e) = crate::server::with_lease(state, conn, sid) {
                return Handled::Now(Err(e));
            }
        }
        let r = r.clone();
        let c = conn.clone();
        let st = state.clone();
        let me = self.clone();
        conn.scope()
            .child("browser-call")
            .spawn_cancellable(async move {
                let berr = |e: genie_browser::BrowserError| {
                    RpcError::new(ErrorKind::InvalidState, e.to_string())
                };
                let empty = || to_result(&Empty {});
                let res: Result<Value, RpcError> = async {
                    match name.as_str() {
                        n if n == m::BrowserStatusMethod::NAME => to_result(&b.status().await),
                        n if n == m::BrowserScreencastStart::NAME => {
                            let p = params::<m::BrowserScreencastStart>(&r)?;
                            let sink = Arc::new(ConnFrames {
                                conn: c.clone(),
                                session_id: p.session_id.clone(),
                            });
                            to_result(&b.screencast_start(&p, sink).await.map_err(berr)?)
                        }
                        n if n == m::BrowserScreencastAck::NAME => {
                            b.screencast_ack(&params::<m::BrowserScreencastAck>(&r)?)
                                .map_err(berr)?;
                            empty()
                        }
                        n if n == m::BrowserScreencastStop::NAME => {
                            b.screencast_stop(&params::<m::BrowserScreencastStop>(&r)?)
                                .await;
                            empty()
                        }
                        n if n == m::BrowserInput::NAME => {
                            b.input(&params::<m::BrowserInput>(&r)?)
                                .await
                                .map_err(berr)?;
                            empty()
                        }
                        n if n == m::BrowserTakeover::NAME => {
                            b.takeover(&params::<m::BrowserTakeover>(&r)?.session_id);
                            empty()
                        }
                        n if n == m::BrowserHandback::NAME => {
                            b.handback(&params::<m::BrowserHandback>(&r)?.session_id);
                            empty()
                        }
                        n if n == m::BrowserStop::NAME => {
                            let sid = params::<m::BrowserStop>(&r)?.session_id;
                            b.stop(&sid).await;
                            // Stop also interrupts the turn (06 §10).
                            if let Some(h) = st.engine().loaded(&sid) {
                                h.interrupt();
                            }
                            empty()
                        }
                        n if n == m::BrowserPopout::NAME => {
                            b.popout(&params::<m::BrowserPopout>(&r)?.session_id)
                                .await
                                .map_err(berr)?;
                            empty()
                        }
                        n if n == m::BrowserCopy::NAME => {
                            let p = params::<m::BrowserCopy>(&r)?;
                            let text = b
                                .copy_selection(&p.session_id, &p.tab_id)
                                .await
                                .map_err(berr)?;
                            to_result(&genie_protocol::api::browser::CopyResult { text })
                        }
                        n if n == m::BrowserFileChooserAnswer::NAME => {
                            b.file_chooser_answer(&params::<m::BrowserFileChooserAnswer>(&r)?)
                                .await
                                .map_err(berr)?;
                            empty()
                        }
                        n if n == m::BrowserTabs::NAME => {
                            to_result(&b.tabs(&params::<m::BrowserTabs>(&r)?.session_id).await)
                        }
                        n if n == m::BrowserTabActivate::NAME => {
                            b.tab_activate(&params::<m::BrowserTabActivate>(&r)?)
                                .await
                                .map_err(berr)?;
                            empty()
                        }
                        n if n == m::BrowserDataClear::NAME => {
                            b.clear_data(params::<m::BrowserDataClear>(&r)?.what)
                                .await
                                .map_err(berr)?;
                            empty()
                        }
                        n if n == m::BrowserSitesList::NAME => {
                            to_result(&genie_protocol::api::browser::SitesListResult {
                                sites: b.sites.list(),
                            })
                        }
                        n if n == m::BrowserSitesSet::NAME => {
                            let p = params::<m::BrowserSitesSet>(&r)?;
                            b.sites.set(&p);
                            // Persisted in user settings (`browser.sites`).
                            if let Some(h) = &me.hub {
                                let list = serde_json::to_value(b.sites.list())
                                    .map_err(|e| RpcError::internal(e.to_string()))?;
                                h.set("browser.sites", list, ConfigScope::User, None)
                                    .map_err(config_err)?;
                            }
                            me.audit(
                                AuditEvent::BrowserSiteApproved,
                                Attrs::new()
                                    .with("origin", p.origin)
                                    .with("state", json!(p.state)),
                            );
                            empty()
                        }
                        other => Err(not_implemented(other)),
                    }
                }
                .await;
                c.respond(match res {
                    Ok(v) => Response::ok(r.id, v),
                    Err(e) => Response::err(r.id, e),
                });
            });
        Handled::Later
    }

    fn directory_async(
        self: &Arc<Self>,
        s: Arc<SkillsHub>,
        conn: &Arc<Conn>,
        r: &Request,
    ) -> Handled {
        let r = r.clone();
        let c = conn.clone();
        let me = self.clone();
        conn.scope()
            .child("directory")
            .spawn_cancellable(async move {
                let dir = genie_skills::SkillDirectory::new(UnavailableFeed);
                let res: Result<Value, RpcError> = async {
                    if r.method == m::DirectoryList::NAME {
                        let p = params::<m::DirectoryList>(&r)?;
                        return to_result(&dir.list(p.query.as_deref()).await.map_err(skill_err)?);
                    }
                    let p = params::<m::DirectoryInstall>(&r)?;
                    if p.kind != DirectoryKind::Skills {
                        return Err(not_implemented(&r.method));
                    }
                    let mut cat = s.user_catalog();
                    let out = dir.install(&mut cat, &p).await.map_err(skill_err)?;
                    if out.installed {
                        s.rescan_all();
                        me.audit(
                            AuditEvent::SkillInstalled,
                            Attrs::new()
                                .with("skill", p.id.clone())
                                .with("version", p.version.clone())
                                .with("sha256", out.manifest["sha256"].clone())
                                .with("source", "directory"),
                        );
                    }
                    to_result(&out)
                }
                .await;
                c.respond(match res {
                    Ok(v) => Response::ok(r.id, v),
                    Err(e) => Response::err(r.id, e),
                });
            });
        Handled::Later
    }

    /// `command.list` (06 §9.1, D58): built-ins, Markdown commands and skills from the
    /// skills hub, then MCP prompts from connected servers (asked concurrently per call).
    fn command_list(self: &Arc<Self>, conn: &Arc<Conn>, r: &Request) -> Handled {
        let r = r.clone();
        let c = conn.clone();
        let me = self.clone();
        conn.scope()
            .child("command-list")
            .spawn_cancellable(async move {
                let res: Result<Value, RpcError> = async {
                    let p = params::<m::CommandList>(&r)?;
                    let skills = me
                        .skills
                        .clone()
                        .ok_or_else(|| not_implemented(m::CommandList::NAME))?;
                    let (kind, workspace) = match &p.session_id {
                        Some(id) => {
                            let s = me
                                .runtime
                                .as_ref()
                                .and_then(|rt| rt.settings.settings(id))
                                .ok_or_else(|| {
                                    RpcError::new(ErrorKind::NotFound, "session not found")
                                })?;
                            (s.kind, s.workspace)
                        }
                        None => (
                            p.kind.unwrap_or_default(),
                            p.workspace_path
                                .as_deref()
                                .map(|w| PathBuf::from(canonical(w))),
                        ),
                    };
                    let mut out = skills.command_list(kind, workspace.as_deref());
                    if let Some(h) = &me.mcp {
                        let cwd = (kind == SessionKind::Code)
                            .then_some(workspace.as_deref())
                            .flatten();
                        let (prompts, diags) = h.prompt_commands(cwd).await;
                        out.diagnostics.extend(diags);
                        for pc in prompts {
                            if out.commands.iter().any(|c| c.name == pc.name) {
                                out.diagnostics.push(format!(
                                    "MCP prompt /{}: hidden, a command already uses that name",
                                    pc.name
                                ));
                            } else {
                                out.commands.push(pc);
                            }
                        }
                    }
                    to_result(&out)
                }
                .await;
                c.respond(match res {
                    Ok(v) => Response::ok(r.id, v),
                    Err(e) => Response::err(r.id, e),
                });
            });
        Handled::Later
    }

    fn mcp_async(self: &Arc<Self>, h: Arc<McpHub>, conn: &Arc<Conn>, r: &Request) -> Handled {
        let r = r.clone();
        let c = conn.clone();
        let me = self.clone();
        conn.scope()
            .child("mcp-call")
            .spawn_cancellable(async move {
                let res: Result<Value, RpcError> = async {
                    let name = r.method.as_str();
                    if name == m::McpResourceRead::NAME {
                        let p = params::<m::McpResourceRead>(&r)?;
                        let (blob_id, mime) = h
                            .read_resource(&p.name, &p.uri)
                            .await
                            .map_err(|e| RpcError::new(ErrorKind::InvalidState, e))?;
                        return to_result(&BlobMimeResult { blob_id, mime });
                    }
                    let p = params::<m::McpTools>(&r)?;
                    let s = server(&h, &p.name)?;
                    let st = |e: genie_mcp::McpError| {
                        RpcError::new(ErrorKind::InvalidState, e.to_string())
                    };
                    if name == m::McpAuthStart::NAME {
                        let auth_url = s.authorize().await.map_err(st)?;
                        me.audit(
                            AuditEvent::McpAuth,
                            Attrs::new()
                                .with("server", p.name.clone())
                                .with("step", "authorize"),
                        );
                        return to_result(&AuthUrlResult { auth_url });
                    }
                    let items = if name == m::McpPrompts::NAME {
                        s.list_prompts().await.map_err(st)?
                    } else {
                        let mut v = s.list_resources().await.map_err(st)?;
                        v.extend(s.list_resource_templates().await.unwrap_or_default());
                        v
                    };
                    to_result(&McpItemsResult { items })
                }
                .await;
                c.respond(match res {
                    Ok(v) => Response::ok(r.id, v),
                    Err(e) => Response::err(r.id, e),
                });
            });
        Handled::Later
    }

    /// Background work: hub events → MCP reconcile + `config.changed`/`policy.changed`;
    /// MCP status → `mcp.status`; settings/policy watcher and export loops (`crate::sync`).
    /// `InitializeResult.locked` (02 §2).
    pub(crate) fn locked_info(&self) -> Option<genie_protocol::handshake::LockedInfo> {
        self.providers.as_ref().and_then(|p| p.locked_info())
    }

    pub(crate) fn start(self: &Arc<Self>, state: &Arc<DaemonState>) {
        if let Some(p) = &self.providers {
            p.start(state);
        }
        let scope = state.root_scope().child("services");
        if let Some(hub) = &self.hub {
            let mut rx = hub.subscribe();
            let st = state.clone();
            let me = self.clone();
            scope.spawn_cancellable(async move {
                loop {
                    let ev = match rx.recv().await {
                        Ok(e) => e,
                        Err(broadcast::error::RecvError::Lagged(_)) => continue,
                        Err(broadcast::error::RecvError::Closed) => break,
                    };
                    if let Some(mcp) = &me.mcp {
                        mcp.reconcile_all();
                    }
                    if let Some(sk) = &me.skills
                        && !matches!(ev, HubEvent::Config { .. })
                    {
                        sk.on_change();
                    }
                    match ev {
                        HubEvent::Config { scope, keys, .. } => st
                            .workspace_send(m::ConfigChanged::NAME, &ConfigChanged { keys, scope }),
                        HubEvent::Policy { keys } => {
                            st.workspace_send(m::PolicyChanged::NAME, &PolicyChanged { keys });
                        }
                        HubEvent::Trust { .. } => st.workspace_send(
                            m::ConfigChanged::NAME,
                            &ConfigChanged {
                                keys: Vec::new(),
                                scope: ConfigScope::Project,
                            },
                        ),
                    }
                }
            });
            // Settings/policy hot reload (debounced watcher), remote policy, OTLP and
            // telemetry export (D23, 08 §5–7).
            crate::sync::start(self, &scope, hub.clone());
        }
        if let Some(sk) = &self.skills {
            let mut rx = sk.subscribe_changes();
            let st = state.clone();
            scope.spawn_cancellable(async move {
                while rx.changed().await.is_ok() {
                    st.workspace_send(
                        m::CommandsChangedNote::NAME,
                        &genie_protocol::api::mcp::CommandsChanged {},
                    );
                }
            });
        }
        if let Some(mcp) = &self.mcp {
            let mut rx = mcp.subscribe_status();
            let st = state.clone();
            let me = self.clone();
            scope.spawn_cancellable(async move {
                loop {
                    match rx.recv().await {
                        Ok(s) => {
                            if s.status == genie_mcp::McpStatus::Connected {
                                me.audit(
                                    AuditEvent::McpConnected,
                                    Attrs::new().with("server", s.name.clone()),
                                );
                            }
                            st.workspace_send(m::McpStatusNote::NAME, &s);
                            // Prompts appear/disappear with the server (06 §9.1).
                            st.workspace_send(
                                m::CommandsChangedNote::NAME,
                                &genie_protocol::api::mcp::CommandsChanged {},
                            );
                        }
                        Err(broadcast::error::RecvError::Lagged(_)) => {}
                        Err(broadcast::error::RecvError::Closed) => break,
                    }
                }
            });
        }
    }
}

/// Screencast frames for one connection, as ephemeral `browser.frame` events.
struct ConnFrames {
    conn: Arc<Conn>,
    session_id: String,
}

impl genie_browser::FrameSink for ConnFrames {
    fn frame(&self, frame: genie_protocol::event::BrowserFrame) {
        self.conn
            .notify_event(&genie_protocol::event::SessionEventEnvelope {
                session_id: self.session_id.clone(),
                seq: None,
                ref_seq: None,
                ts: genie_protocol::rfc3339_ms(genie_core::bus::now_ms()),
                turn_id: None,
                step: None,
                subagent_of: None,
                event: genie_protocol::event::SessionEvent::BrowserFrame(frame),
            });
    }
}

fn skill_err(e: genie_skills::SkillError) -> RpcError {
    use genie_skills::SkillError as E;
    let kind = match &e {
        E::NotFound(_) => ErrorKind::NotFound,
        E::Blocked(_) | E::Policy(_) => ErrorKind::PolicyBlocked,
        E::Disabled(_) | E::UserOnly(_) => ErrorKind::InvalidState,
        E::AlreadyExists(_) => ErrorKind::Conflict,
        E::Directory(_) | E::Io(_) => ErrorKind::InvalidState,
        _ => ErrorKind::InvalidParams,
    };
    RpcError::new(kind, e.to_string()).with_data(json!({"code": e.code()}))
}

/// The signed skills feed has no HTTP transport in this build (D40 feed URL + keys are
/// not pinned yet): `directory.*` for skills reports it as unavailable.
struct UnavailableFeed;

#[async_trait::async_trait]
impl genie_skills::DirectoryTransport for UnavailableFeed {
    async fn fetch_feed(&self) -> Result<genie_skills::SignedFeed, genie_skills::SkillError> {
        Err(genie_skills::SkillError::Directory(
            "the skills directory feed is not available in this build".into(),
        ))
    }
    async fn fetch_package(&self, _url: &str) -> Result<Vec<u8>, genie_skills::SkillError> {
        Err(genie_skills::SkillError::Directory(
            "the skills directory feed is not available in this build".into(),
        ))
    }
}

fn server(h: &McpHub, name: &str) -> Result<genie_mcp::McpServer, RpcError> {
    h.server(name)
        .ok_or_else(|| RpcError::new(ErrorKind::NotFound, format!("no MCP server `{name}`")))
}

fn scope_str(s: ConfigScope) -> &'static str {
    match s {
        ConfigScope::User => "user",
        ConfigScope::Project => "project",
        ConfigScope::Local => "local",
        ConfigScope::Managed => "managed",
        ConfigScope::Unknown => "unknown",
    }
}

/// Parse a `--settings` file into the session's CLI layer (D23 layer 2).
pub(crate) fn read_settings_file(path: &Path) -> Result<Value, String> {
    let bytes = std::fs::read(path).map_err(|e| format!("{}: {e}", path.display()))?;
    match serde_json::from_slice::<Value>(&bytes) {
        Ok(v @ Value::Object(_)) => Ok(v),
        _ => Err(format!("{}: not a JSON object", path.display())),
    }
}
