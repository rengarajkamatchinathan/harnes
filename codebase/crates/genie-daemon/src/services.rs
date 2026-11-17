//! Daemon methods backed by the engine's subsystems (02 §6.4, §6.8): config, policy,
//! trust, MCP and `tools.list`, plus the background tasks that turn their changes into
//! workspace-feed notifications (`config.changed`, `policy.changed`, `mcp.status`).

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use genie_audit::{Attrs, AuditEvent, Auditor};
use genie_config::{ConfigError, PolicyManager, TrustError};
use genie_core::config::{ConfigHub, HubEvent};
use genie_core::runtime::AgentRuntime;
use genie_core::runtime::mcp::McpHub;
use genie_protocol::api::config::{ApproveDecision, PendingDefinitionsResult, TrustGetResult};
use genie_protocol::api::mcp::{
    AuthUrlResult, BlobMimeResult, LinesResult, McpItemsResult, McpListResult,
};
use genie_protocol::api::session::ToolsListResult;
use genie_protocol::common::{ConfigScope, Empty};
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

fn config_err(e: ConfigError) -> RpcError {
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

    fn policy_problems(&self) -> Vec<genie_protocol::common::Diagnostic> {
        self.policy
            .as_ref()
            .map(|p| p.lock().unwrap_or_else(|e| e.into_inner()).problems())
            .unwrap_or_default()
    }

    /// Methods served here. `NotMine` lets the server fall through to its own table.
    pub(crate) fn dispatch(self: &Arc<Self>, conn: &Arc<Conn>, r: &Request) -> Handled {
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

            _ => return Handled::NotMine,
        };
        Handled::Now(res)
    }

    fn mcp_async(self: &Arc<Self>, h: Arc<McpHub>, conn: &Arc<Conn>, r: &Request) -> Handled {
        let r = r.clone();
        let c = conn.clone();
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
    /// MCP status → `mcp.status`; settings files polled (content-hash suppressed) and
    /// managed policy re-read.
    pub(crate) fn start(self: &Arc<Self>, state: &Arc<DaemonState>) {
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
            // Settings hot reload (D23): poll; `refresh` reports only real changes.
            let h = hub.clone();
            let me = self.clone();
            scope.spawn_cancellable(async move {
                let mut tick = 0u64;
                loop {
                    tokio::time::sleep(Duration::from_secs(2)).await;
                    h.refresh();
                    tick += 1;
                    if tick % 30 == 0 {
                        if let Some(pm) = &me.policy {
                            let eff = {
                                let mut g = pm.lock().unwrap_or_else(|e| e.into_inner());
                                g.refresh_local();
                                g.effective()
                            };
                            h.set_policy(eff);
                        }
                    }
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
                        }
                        Err(broadcast::error::RecvError::Lagged(_)) => {}
                        Err(broadcast::error::RecvError::Closed) => break,
                    }
                }
            });
        }
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
