//! Per-connection protocol handling and daemon-wide state.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use genie_client::framing::{self, BoxStream, FrameError};
use genie_core::{Engine, TaskScope};
use genie_protocol::handshake::{DaemonCapabilities, InitializeParams, InitializeResult};
use genie_protocol::methods::{
    self, ClientRef, DaemonRestartParams, DaemonRestartingParams, DaemonStatus,
};
use genie_protocol::version::{SkewDecision, decide_skew};
use genie_protocol::{
    ErrorKind, Id, Message, Notification, PROTOCOL_VERSION, Request, Response, RpcError,
};
use serde_json::{Value, json};
use tokio::sync::{broadcast, watch};

use crate::DaemonConfig;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShutdownReason {
    /// No clients and no work for the idle period (01 §4.3).
    Idle,
    /// Version skew or `daemon.restart`: drain, exit, a client re-runs ensure (01 §4.2).
    Restart,
    Signal,
    /// Standalone parent closed our stdin (01 §4.4).
    LeaseEnded,
}

impl ShutdownReason {
    fn as_str(self) -> &'static str {
        match self {
            ShutdownReason::Idle => "idle",
            ShutdownReason::Restart => "restart",
            ShutdownReason::Signal => "signal",
            ShutdownReason::LeaseEnded => "lease_ended",
        }
    }
}

pub struct DaemonState {
    cfg: DaemonConfig,
    engine: Engine,
    token: String,
    root: TaskScope,
    started: Instant,
    clients: Mutex<HashMap<u64, ClientRef>>,
    next_conn: AtomicU64,
    shutdown: watch::Sender<Option<ShutdownReason>>,
    restarting: broadcast::Sender<ShutdownReason>,
}

/// Time a new connection has to send `initialize`.
const INIT_TIMEOUT: Duration = Duration::from_secs(10);

impl DaemonState {
    pub fn new(cfg: DaemonConfig, engine: Engine, token: String, root: TaskScope) -> Self {
        DaemonState {
            cfg,
            engine,
            token,
            root,
            started: Instant::now(),
            clients: Mutex::new(HashMap::new()),
            next_conn: AtomicU64::new(1),
            shutdown: watch::channel(None).0,
            restarting: broadcast::channel(4).0,
        }
    }

    pub fn engine(&self) -> &Engine {
        &self.engine
    }

    pub fn request_shutdown(&self, reason: ShutdownReason) {
        self.shutdown.send_if_modified(|cur| {
            if cur.is_none() {
                *cur = Some(reason);
                true
            } else {
                false
            }
        });
    }

    pub async fn wait_shutdown(&self) -> ShutdownReason {
        let mut rx = self.shutdown.subscribe();
        match rx.wait_for(Option::is_some).await {
            Ok(v) => v.unwrap_or(ShutdownReason::Signal),
            Err(_) => ShutdownReason::Signal,
        }
    }

    /// Closes subscriptions with `daemon.restarting` before the drain (01 §4.2).
    pub fn broadcast_restarting(&self, reason: ShutdownReason) {
        let _ = self.restarting.send(reason);
    }

    pub fn client_count(&self) -> usize {
        self.clients.lock().unwrap_or_else(|e| e.into_inner()).len()
    }

    /// Idle exit: no clients, no running turns, no paused asks, no background shell jobs
    /// and no open terminals, for the idle period (01 §4.3).
    pub(crate) async fn idle_watch(&self) {
        let idle = self.cfg.idle_timeout;
        let check = (idle / 10).clamp(Duration::from_millis(10), Duration::from_secs(5));
        let mut since: Option<Instant> = None;
        loop {
            tokio::time::sleep(check).await;
            if self.client_count() == 0 && !self.engine.busy().keeps_alive() {
                if since.get_or_insert_with(Instant::now).elapsed() >= idle {
                    self.request_shutdown(ShutdownReason::Idle);
                    return;
                }
            } else {
                since = None;
            }
        }
    }

    fn status(&self) -> DaemonStatus {
        let mut clients: Vec<ClientRef> = self
            .clients
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .values()
            .cloned()
            .collect();
        clients.sort_by_key(|c| c.connection_id);
        DaemonStatus {
            version: self.cfg.engine_version.clone(),
            channel: self.cfg.endpoint.channel,
            uptime_s: self.started.elapsed().as_secs(),
            pid: std::process::id(),
            socket: self.cfg.endpoint.address.to_string(),
            sessions_running: self.engine.sessions_running(),
            clients,
        }
    }

    fn init_result(&self, restart_pending: bool) -> InitializeResult {
        InitializeResult {
            engine_version: self.cfg.engine_version.clone(),
            build: self.cfg.build.clone(),
            channel: self.cfg.endpoint.channel,
            protocol: PROTOCOL_VERSION.to_string(),
            capabilities: DaemonCapabilities { features: vec![] },
            restart_pending: restart_pending.then_some(true),
            locked: None,
        }
    }
}

struct ClientGuard<'a> {
    state: &'a DaemonState,
    id: u64,
}

impl Drop for ClientGuard<'_> {
    fn drop(&mut self) {
        self.state
            .clients
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&self.id);
        tracing::debug!(connection_id = self.id, "client disconnected");
    }
}

/// Serve one client connection over any byte stream (socket, pipe or in-memory).
pub async fn serve_stream(state: Arc<DaemonState>, stream: BoxStream, _scope: TaskScope) {
    // Connections hang off the root, not the accept loop, so they can still receive
    // `daemon.restarting` after accepting stops.
    let scope = state.root.child("connection");
    let (mut reader, mut writer) = framing::split(stream);
    let conn_id = state.next_conn.fetch_add(1, Ordering::Relaxed);

    // 1. `initialize` must be the first request.
    let first = tokio::select! {
        r = tokio::time::timeout(INIT_TIMEOUT, reader.next()) => r,
        () = scope.token().cancelled() => return,
    };
    let req = match first {
        Ok(Ok(Some(Message::Request(r)))) if r.method == methods::INITIALIZE => r,
        Ok(Ok(Some(Message::Request(r)))) => {
            let e = RpcError::invalid_request("initialize must be the first request");
            let _ = writer
                .send(&Message::Response(Response::err(r.id, e)))
                .await;
            return;
        }
        _ => return,
    };
    let params: InitializeParams = match serde_json::from_value(req.params.clone()) {
        Ok(p) => p,
        Err(e) => {
            let _ = writer
                .send(&Message::Response(Response::err(
                    req.id,
                    RpcError::invalid_params(e.to_string()),
                )))
                .await;
            return;
        }
    };
    if !crate::token::matches(&state.token, &params.token) {
        tracing::warn!(connection_id = conn_id, "initialize with a bad token");
        let e = RpcError::new(ErrorKind::Unauthorized, "bad or missing token");
        let _ = writer
            .send(&Message::Response(Response::err(req.id, e)))
            .await;
        return;
    }

    // 2. Version skew (01 §4.2).
    let busy = state.engine.busy();
    let decision = decide_skew(
        &state.cfg.engine_version,
        PROTOCOL_VERSION,
        &params.client_version,
        &params.protocol,
        busy.has_turns(),
    );
    tracing::info!(connection_id = conn_id, ?params, ?decision, "initialize");
    let degraded = match decision {
        SkewDecision::Proceed => false,
        SkewDecision::Degraded => true,
        SkewDecision::RestartPending => {
            let res = serde_json::to_value(state.init_result(true)).unwrap_or(Value::Null);
            let _ = writer
                .send(&Message::Response(Response::ok(req.id, res)))
                .await;
            state.request_shutdown(ShutdownReason::Restart);
            return;
        }
        SkewDecision::BusyMismatch => {
            let e = RpcError::new(ErrorKind::Busy, "daemon is finishing tasks before updating")
                .with_data(json!({ "running": busy.running_turns + busy.pending_asks }));
            let _ = writer
                .send(&Message::Response(Response::err(req.id, e)))
                .await;
            return;
        }
        SkewDecision::ClientTooOld => {
            let e = RpcError::new(
                ErrorKind::ProtocolMismatch,
                "update Genie to connect to this engine",
            )
            .with_data(
                json!({ "daemon_protocol": PROTOCOL_VERSION, "client_protocol": params.protocol }),
            );
            let _ = writer
                .send(&Message::Response(Response::err(req.id, e)))
                .await;
            return;
        }
        SkewDecision::Invalid(msg) => {
            let _ = writer
                .send(&Message::Response(Response::err(
                    req.id,
                    RpcError::invalid_params(msg),
                )))
                .await;
            return;
        }
    };

    let res = serde_json::to_value(state.init_result(false)).unwrap_or(Value::Null);
    if writer
        .send(&Message::Response(Response::ok(req.id, res)))
        .await
        .is_err()
    {
        return;
    }
    state
        .clients
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .insert(
            conn_id,
            ClientRef {
                connection_id: conn_id,
                client_kind: params.client_kind,
                client_version: params.client_version.clone(),
                client_instance_id: params.client_instance_id.clone(),
                degraded,
            },
        );
    let _guard = ClientGuard {
        state: &state,
        id: conn_id,
    };
    let mut restarting = state.restarting.subscribe();

    // 3. Request loop.
    loop {
        tokio::select! {
            biased;
            r = restarting.recv() => {
                if let Ok(reason) = r {
                    let p = DaemonRestartingParams { reason: reason.as_str().into() };
                    let n = Notification::new(methods::DAEMON_RESTARTING, serde_json::to_value(p).unwrap_or(Value::Null));
                    let _ = writer.send(&Message::Notification(n)).await;
                }
            }
            () = scope.token().cancelled() => break,
            msg = reader.next() => match msg {
                Ok(Some(Message::Request(r))) => {
                    let resp = dispatch(&state, r);
                    if writer.send(&Message::Response(resp)).await.is_err() {
                        break;
                    }
                }
                Ok(Some(Message::Notification(_) | Message::Response(_))) => {}
                Err(FrameError::Bad(e)) => {
                    // Unparseable line: JSON-RPC says reply with id null; we use 0.
                    let _ = writer.send(&Message::Response(Response::err(Id::Num(0), e))).await;
                }
                Ok(None) | Err(_) => break,
            },
        }
    }
    let _ = writer.shutdown().await;
}

fn dispatch(state: &DaemonState, r: Request) -> Response {
    let result: Result<Value, RpcError> = match r.method.as_str() {
        methods::PING => Ok(json!({})),
        methods::DAEMON_STATUS => {
            serde_json::to_value(state.status()).map_err(|e| RpcError::internal(e.to_string()))
        }
        methods::DAEMON_RESTART => {
            let p: DaemonRestartParams = if r.params.is_null() {
                DaemonRestartParams::default()
            } else {
                match serde_json::from_value(r.params.clone()) {
                    Ok(p) => p,
                    Err(e) => return Response::err(r.id, RpcError::invalid_params(e.to_string())),
                }
            };
            let busy = state.engine.busy();
            if busy.has_turns() && !p.stop_running {
                Err(RpcError::new(ErrorKind::Busy, "sessions are running")
                    .with_data(json!({ "running": busy.running_turns + busy.pending_asks })))
            } else {
                state.request_shutdown(ShutdownReason::Restart);
                Ok(json!({}))
            }
        }
        methods::INITIALIZE => Err(RpcError::invalid_request("already initialized")),
        other => Err(RpcError::method_not_found(other)),
    };
    match result {
        Ok(v) => Response::ok(r.id, v),
        Err(e) => Response::err(r.id, e),
    }
}
