//! Dev servers (ticket 47 phase 2): `preview.list/start/stop/logs` for the Browser tab's
//! **Dev servers** menu and the chat's server cards. The user's own start/stop never
//! prompts. Changes go out as `preview.changed` on the workspace feed (wired in `lib.rs`).

use std::sync::Arc;

use genie_protocol::api::browser::{PreviewListResult, PreviewLogsResult};
use genie_protocol::common::Empty;
use genie_protocol::methods::{self as m, Method};
use genie_protocol::{ErrorKind, Request, Response, RpcError};
use genie_tools::preview::DevServers;
use serde::Serialize;
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;

use crate::server::{Conn, DaemonState};
use crate::services::{Handled, not_implemented};

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

fn known_session(state: &DaemonState, sid: &str) -> Result<(), RpcError> {
    if state.registry().exists(sid) {
        Ok(())
    } else {
        Err(RpcError::new(ErrorKind::NotFound, "session not found"))
    }
}

fn state_err(e: String) -> RpcError {
    RpcError::new(ErrorKind::InvalidState, e)
}

/// `preview.*`; `None` for anything else.
pub(crate) fn dispatch(
    dev: Option<&Arc<DevServers>>,
    state: &Arc<DaemonState>,
    conn: &Arc<Conn>,
    r: &Request,
) -> Option<Handled> {
    let name = r.method.as_str();
    if !name.starts_with("preview.") {
        return None;
    }
    let Some(dev) = dev.cloned() else {
        return Some(Handled::Now(Err(not_implemented(name))));
    };
    let res = match name {
        n if n == m::PreviewList::NAME => params::<m::PreviewList>(r).and_then(|p| {
            known_session(state, &p.session_id)?;
            let (configs, auto_verify, config_error) = match dev.launch_file(&p.session_id) {
                Ok(f) => (
                    f.configurations.iter().map(|c| c.name.clone()).collect(),
                    f.auto_verify(),
                    None,
                ),
                // No folder (Chat session): an empty menu, not an error.
                Err(_) if dev.root(&p.session_id).is_err() => (vec![], true, None),
                Err(e) => (vec![], true, Some(e)),
            };
            to_result(&PreviewListResult {
                servers: dev.list(&p.session_id),
                configs,
                auto_verify,
                config_error,
            })
        }),
        n if n == m::PreviewStop::NAME => params::<m::PreviewStop>(r).and_then(|p| {
            known_session(state, &p.session_id)?;
            match p.name {
                Some(n) => {
                    if !dev.stop(&p.session_id, &n) {
                        return Err(RpcError::new(ErrorKind::NotFound, "dev server not found"));
                    }
                }
                None => {
                    dev.stop_all(&p.session_id);
                }
            }
            to_result(&Empty {})
        }),
        n if n == m::PreviewLogs::NAME => params::<m::PreviewLogs>(r).and_then(|p| {
            let lines = p.lines.unwrap_or(200).clamp(1, 5000) as usize;
            let text = dev
                .logs(&p.session_id, &p.name, lines)
                .ok_or_else(|| RpcError::new(ErrorKind::NotFound, "dev server not found"))?;
            to_result(&PreviewLogsResult { text })
        }),
        n if n == m::PreviewStart::NAME => {
            // Waits for the port: answered from a task.
            let p = match params::<m::PreviewStart>(r)
                .and_then(|p| known_session(state, &p.session_id).map(|()| p))
            {
                Ok(p) => p,
                Err(e) => return Some(Handled::Now(Err(e))),
            };
            let id = r.id.clone();
            let c = conn.clone();
            let scope = conn.scope().child("preview-start");
            let cancel = CancellationToken::new();
            scope.spawn_cancellable(async move {
                let res = dev
                    .start(&p.session_id, &p.name, &cancel)
                    .await
                    .map_err(state_err)
                    .and_then(|s| to_result(&s));
                c.respond(match res {
                    Ok(v) => Response::ok(id, v),
                    Err(e) => Response::err(id, e),
                });
            });
            return Some(Handled::Later);
        }
        other => Err(RpcError::method_not_found(other)),
    };
    Some(Handled::Now(res))
}

/// `session.delete`: stop the session's servers.
pub(crate) fn session_deleted(state: &DaemonState, sid: &str) {
    if let Some(d) = state.services().and_then(|s| s.preview.as_ref()) {
        d.end_session(sid);
    }
}
