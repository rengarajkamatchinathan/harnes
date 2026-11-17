//! Per-connection protocol handling and daemon-wide state (spec 01 §4, spec 02).
//!
//! Each connection has one writer task fed by an unbounded channel of pre-serialised
//! lines. Session subscriptions run as forwarder tasks that account their queued bytes
//! and messages; past the bound (SA-10) the subscription is dropped with
//! `session.resync_required` instead of losing events (02 §3.1).

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use genie_client::framing::{self, BoxStream, FrameError};
use genie_core::bus::SubItem;
use genie_core::session::AttachGuard;
use genie_core::{Engine, SessionHandle, TaskScope};
use genie_protocol::api::session::{self as sp, PromptResult};
use genie_protocol::ask::{Ask, AskAnswer, ResolvedBy};
use genie_protocol::common::{ClientRef, Empty};
use genie_protocol::event::{
    CancelRequest, LeaseChanged, LeaseLost, ResyncRequired, SessionEvent, SessionSynced,
    SessionUpdated,
};
use genie_protocol::handshake::{
    ClientCapabilities, DaemonCapabilities, InitializeParams, InitializeResult,
};
use genie_protocol::methods::{
    self as m, ConnectedClient, DaemonRestartParams, DaemonRestartingParams, DaemonStatus, Method,
};
use genie_protocol::version::{SkewDecision, decide_skew};
use genie_protocol::workspace::{ListedChange, SessionListed, UserNotification, WorkspaceSnapshot};
use genie_protocol::{
    ErrorKind, Id, Message, Notification, Outcome, PROTOCOL_VERSION, Request, Response, RpcError,
};
use serde::Serialize;
use serde_json::{Value, json};
use tokio::sync::{broadcast, mpsc, watch};

use genie_core::runtime::ConversationStore;

use crate::DaemonConfig;
use crate::lease::{LeaseCheck, Leases};
use crate::registry::{Extra, Registry};
use crate::services::{Handled, Services};

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

/// Max remembered `client_msg_id`s for prompt idempotency.
const IDEMPOTENCY_CAP: usize = 10_000;

pub struct DaemonState {
    cfg: DaemonConfig,
    engine: Engine,
    token: String,
    root: TaskScope,
    started: Instant,
    clients: Mutex<HashMap<u64, ConnectedClient>>,
    conns: Mutex<HashMap<u64, Arc<Conn>>>,
    next_conn: AtomicU64,
    shutdown: watch::Sender<Option<ShutdownReason>>,
    restarting: broadcast::Sender<ShutdownReason>,
    leases: Leases,
    registry: Arc<Registry>,
    /// The message tree (`session.get` path, `session.regenerate` checks). `None` with a
    /// fake runner in tests.
    conversation: Option<Arc<dyn ConversationStore>>,
    prompts: Mutex<HashMap<(String, String), PromptResult>>,
    workspace: broadcast::Sender<Arc<str>>,
    pump_started: AtomicBool,
    services: Option<Arc<Services>>,
    background_started: AtomicBool,
}

/// Time a new connection has to send `initialize`.
const INIT_TIMEOUT: Duration = Duration::from_secs(10);

impl DaemonState {
    pub fn new(cfg: DaemonConfig, engine: Engine, token: String, root: TaskScope) -> Self {
        Self::with_runtime(
            cfg,
            engine,
            token,
            root,
            Arc::new(Registry::default()),
            None,
        )
    }

    /// With the session registry the turn loop reads settings from, and the message tree.
    pub fn with_runtime(
        cfg: DaemonConfig,
        engine: Engine,
        token: String,
        root: TaskScope,
        registry: Arc<Registry>,
        conversation: Option<Arc<dyn ConversationStore>>,
    ) -> Self {
        Self::with_services(cfg, engine, token, root, registry, conversation, None)
    }

    /// Fully wired: config/policy/trust, MCP, `tools.list` (and audit) from `services`.
    pub fn with_services(
        cfg: DaemonConfig,
        engine: Engine,
        token: String,
        root: TaskScope,
        registry: Arc<Registry>,
        conversation: Option<Arc<dyn ConversationStore>>,
        services: Option<Arc<Services>>,
    ) -> Self {
        let leases = Leases::new(cfg.lease_grace);
        DaemonState {
            cfg,
            engine,
            token,
            root,
            started: Instant::now(),
            clients: Mutex::new(HashMap::new()),
            conns: Mutex::new(HashMap::new()),
            next_conn: AtomicU64::new(1),
            shutdown: watch::channel(None).0,
            restarting: broadcast::channel(4).0,
            leases,
            registry,
            conversation,
            prompts: Mutex::new(HashMap::new()),
            workspace: broadcast::channel(1024).0,
            pump_started: AtomicBool::new(false),
            services,
            background_started: AtomicBool::new(false),
        }
    }

    pub(crate) fn root_scope(&self) -> &TaskScope {
        &self.root
    }

    /// Start the services' background tasks once (first connection or daemon start).
    pub fn start_background(self: &Arc<Self>) {
        if self.background_started.swap(true, Ordering::SeqCst) {
            return;
        }
        if let Some(s) = &self.services {
            s.start(self);
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
        let mut clients: Vec<ConnectedClient> = self
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

    /// Whether a client subscribed to `session` can show asks of `kind`.
    fn can_render(&self, session: &str, kind: genie_protocol::ask::AskKind) -> bool {
        self.conns
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .values()
            .any(|c| {
                c.caps.asks.contains(&kind)
                    && c.subs
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .contains_key(session)
            })
    }

    fn conn(&self, id: u64) -> Option<Arc<Conn>> {
        self.conns
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(&id)
            .cloned()
    }

    /// The summary with the live status of a loaded actor.
    fn live_summary(&self, id: &str) -> Option<genie_protocol::message::SessionSummary> {
        let mut s = self.registry.get(id)?.summary;
        if let Some(h) = self.engine.loaded(id) {
            s.status = h.status();
        }
        Some(s)
    }

    pub(crate) fn workspace_send<P: Serialize>(&self, method: &str, params: &P) {
        if self.workspace.receiver_count() > 0
            && let Some(line) = notification_line(method, params)
        {
            let _ = self.workspace.send(line.into());
        }
    }

    fn listed(&self, change: ListedChange, session: genie_protocol::message::SessionSummary) {
        self.workspace_send(m::SessionListed::NAME, &SessionListed { change, session });
    }

    /// Start the bus → workspace-feed pump once (02 §3.2, §7 step 2).
    fn ensure_pump(self: &Arc<Self>) {
        if self.pump_started.swap(true, Ordering::SeqCst) {
            return;
        }
        let st = self.clone();
        let mut rx = self.engine.bus().subscribe_all();
        self.root
            .child("workspace-pump")
            .spawn_cancellable(async move {
                loop {
                    let env = match rx.recv().await {
                        Ok(e) => e,
                        Err(broadcast::error::RecvError::Lagged(_)) => continue,
                        Err(broadcast::error::RecvError::Closed) => break,
                    };
                    let (event, _) = env.event.to_wire();
                    match event {
                        SessionEvent::Status(_) | SessionEvent::SessionUpdated(_) => {
                            if let Some(s) = st.live_summary(&env.session_id) {
                                st.listed(ListedChange::Updated, s);
                            }
                        }
                        SessionEvent::PermissionAsked(a)
                            // Notify unless a client subscribed to this session can
                            // render the ask kind (e.g. a headless client can't): 02 §7
                            // step 2.
                            if !st.can_render(&env.session_id, a.ask.kind) => {
                                st.workspace_send(
                                    m::UserNotification::NAME,
                                    &UserNotification {
                                        title: "Genie needs your approval".into(),
                                        body: a
                                            .ask
                                            .tool
                                            .clone()
                                            .unwrap_or_else(|| "A session is waiting".into()),
                                        session_id: Some(env.session_id.clone()),
                                        ask_id: Some(a.ask.id.clone()),
                                        link: Some(format!("genie://session/{}", env.session_id)),
                                    },
                                );
                            }
                        _ => {}
                    }
                }
            });
    }
}

// ------------------------------------------------------------------ connection

/// Byte/message accounting of one subscription's queued lines (SA-10).
#[derive(Default)]
struct SubQueue {
    bytes: AtomicUsize,
    msgs: AtomicUsize,
}

enum Out {
    Line(Arc<str>, Option<Arc<SubQueue>>),
    Close,
}

struct SubEntry {
    id: u64,
    scope: TaskScope,
    _attach: AttachGuard,
}

/// One handshaken connection.
pub(crate) struct Conn {
    id: u64,
    client: ClientRef,
    caps: ClientCapabilities,
    out: mpsc::UnboundedSender<Out>,
    scope: TaskScope,
    subs: Mutex<HashMap<String, SubEntry>>,
    /// Open daemon → client `permission.ask` requests: request id → (session, ask id).
    server_reqs: Mutex<HashMap<u64, (String, String)>>,
    asks_sent: Mutex<HashSet<String>>,
    next_req: AtomicU64,
}

impl Conn {
    fn send_line(&self, line: impl Into<Arc<str>>) {
        let _ = self.out.send(Out::Line(line.into(), None));
    }

    fn notify<P: Serialize>(&self, method: &str, params: &P) {
        if let Some(l) = notification_line(method, params) {
            self.send_line(l);
        }
    }

    pub(crate) fn scope(&self) -> &TaskScope {
        &self.scope
    }

    pub(crate) fn respond(&self, resp: Response) {
        self.send_line(Message::Response(resp).to_line());
    }

    /// Send `permission.ask` if this client can render the kind and has not had it yet.
    fn send_ask(&self, ask: &Ask) {
        if !self.caps.asks.contains(&ask.kind) {
            return;
        }
        if !self
            .asks_sent
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(ask.id.clone())
        {
            return;
        }
        let id = self.next_req.fetch_add(1, Ordering::Relaxed);
        self.server_reqs
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(id, (ask.session_id.clone(), ask.id.clone()));
        let params = sp::AskParams { ask: ask.clone() };
        let Ok(p) = serde_json::to_value(&params) else {
            return;
        };
        let req = Request::new(Id::Num(id), m::PermissionAsk::NAME, p);
        self.send_line(Message::Request(req).to_line());
    }

    /// Withdraw open asks matching `pred` with `$/cancelRequest` (02 §7 step 7).
    fn cancel_asks(&self, pred: impl Fn(&str, &str) -> bool) {
        let ids: Vec<u64> = {
            let mut reqs = self.server_reqs.lock().unwrap_or_else(|e| e.into_inner());
            let ids: Vec<u64> = reqs
                .iter()
                .filter(|(_, (s, a))| pred(s, a))
                .map(|(k, _)| *k)
                .collect();
            for id in &ids {
                if let Some((_, a)) = reqs.remove(id) {
                    self.asks_sent
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .remove(&a);
                }
            }
            ids
        };
        for id in ids {
            self.notify(
                m::CancelRequestNote::NAME,
                &CancelRequest { id: Id::Num(id) },
            );
        }
    }
}

fn notification_line<P: Serialize>(method: &str, params: &P) -> Option<String> {
    let v = serde_json::to_value(params).ok()?;
    Some(Message::Notification(Notification::new(method, v)).to_line())
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
        self.state
            .conns
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&self.id);
        self.state.leases.disconnected(self.id);
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
        Ok(Ok(Some(Message::Request(r)))) if r.method == m::INITIALIZE => r,
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

    // 3. Writer task: the only place that writes to the stream from here on.
    let (out_tx, mut out_rx) = mpsc::unbounded_channel::<Out>();
    let writer_task = {
        let token = scope.token().clone();
        scope.spawn(async move {
            loop {
                let next = tokio::select! {
                    biased;
                    o = out_rx.recv() => o,
                    () = token.cancelled() => {
                        // Flush what is already queued (e.g. `daemon.restarting`), briefly.
                        let flush = async {
                            while let Ok(Out::Line(line, _)) = out_rx.try_recv() {
                                if writer.send_raw_line(&line).await.is_err() {
                                    break;
                                }
                            }
                        };
                        let _ = tokio::time::timeout(Duration::from_millis(500), flush).await;
                        None
                    }
                };
                match next {
                    Some(Out::Line(line, q)) => {
                        let r = writer.send_raw_line(&line).await;
                        if let Some(q) = q {
                            q.bytes.fetch_sub(line.len(), Ordering::SeqCst);
                            q.msgs.fetch_sub(1, Ordering::SeqCst);
                        }
                        if r.is_err() {
                            token.cancel();
                            break;
                        }
                    }
                    Some(Out::Close) | None => break,
                }
            }
            let _ = writer.shutdown().await;
        })
    };

    let client = ClientRef {
        client_instance_id: params.client_instance_id.clone(),
        client_kind: params.client_kind,
        label: Some(label_for(params.client_kind)),
    };
    let conn = Arc::new(Conn {
        id: conn_id,
        client,
        caps: params.capabilities.clone(),
        out: out_tx,
        scope: scope.clone(),
        subs: Mutex::new(HashMap::new()),
        server_reqs: Mutex::new(HashMap::new()),
        asks_sent: Mutex::new(HashSet::new()),
        next_req: AtomicU64::new(1),
    });
    state
        .clients
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .insert(
            conn_id,
            ConnectedClient {
                connection_id: conn_id,
                client_kind: params.client_kind,
                client_version: params.client_version.clone(),
                client_instance_id: params.client_instance_id.clone(),
                degraded,
            },
        );
    state
        .conns
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .insert(conn_id, conn.clone());
    let guard = ClientGuard {
        state: &state,
        id: conn_id,
    };
    state.start_background();
    let mut restarting = state.restarting.subscribe();

    // 4. Request loop.
    loop {
        tokio::select! {
            biased;
            r = restarting.recv() => {
                if let Ok(reason) = r {
                    let p = DaemonRestartingParams { reason: reason.as_str().into() };
                    conn.notify(m::DAEMON_RESTARTING, &p);
                }
            }
            () = scope.token().cancelled() => break,
            msg = reader.next() => match msg {
                Ok(Some(Message::Request(r))) => {
                    if let Some(resp) = dispatch(&state, &conn, r) {
                        conn.respond(resp);
                    }
                }
                Ok(Some(Message::Response(r))) => on_client_response(&state, &conn, r),
                Ok(Some(Message::Notification(_))) => {}
                Err(FrameError::Bad(e)) => {
                    // Unparseable line: JSON-RPC says reply with id null; we use 0.
                    conn.respond(Response::err(Id::Num(0), e));
                }
                Ok(None) | Err(_) => break,
            },
        }
    }
    // Drain what is queued (e.g. `daemon.restarting`), then stop the subscriptions.
    let _ = conn.out.send(Out::Close);
    let _ = tokio::time::timeout(Duration::from_secs(1), writer_task).await;
    scope.cancel();
    conn.subs.lock().unwrap_or_else(|e| e.into_inner()).clear();
    drop(guard);
}

fn label_for(kind: genie_protocol::ClientKind) -> String {
    match kind {
        genie_protocol::ClientKind::Desktop => "Desktop".into(),
        genie_protocol::ClientKind::Cli => "CLI".into(),
        genie_protocol::ClientKind::Headless => "Headless".into(),
    }
}

/// A client answered one of our requests (`permission.ask`).
fn on_client_response(state: &DaemonState, conn: &Conn, r: Response) {
    let Id::Num(id) = r.id else { return };
    let Some((session, ask_id)) = conn
        .server_reqs
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .remove(&id)
    else {
        return;
    };
    let answer = match r.outcome {
        Outcome::Result(v) => match serde_json::from_value::<AskAnswer>(v) {
            Ok(a) => a,
            Err(e) => {
                tracing::warn!(ask_id, error = %e, "bad permission.ask answer");
                return;
            }
        },
        // The client could not show it; the ask stays pending (02 §7).
        Outcome::Error(_) => return,
    };
    if let Some(h) = state.engine.loaded(&session)
        && let Err(e) = h.answer(&ask_id, answer, ResolvedBy::Client(conn.client.clone()))
    {
        tracing::debug!(ask_id, error = %e, "ask answer not applied");
    }
}

// ------------------------------------------------------------------ dispatch

fn params<M: Method>(r: &Request) -> Result<M::Params, RpcError> {
    let v = if r.params.is_null() {
        json!({})
    } else {
        r.params.clone()
    };
    serde_json::from_value(v).map_err(|e| RpcError::invalid_params(e.to_string()))
}

fn to_result<T: Serialize>(v: &T) -> Result<Value, RpcError> {
    serde_json::to_value(v).map_err(|e| RpcError::internal(e.to_string()))
}

fn empty() -> Result<Value, RpcError> {
    to_result(&Empty {})
}

fn not_found(what: &str) -> RpcError {
    RpcError::new(ErrorKind::NotFound, format!("{what} not found"))
}

/// Methods in the table whose subsystem is not wired yet (owned by other specs).
fn not_implemented(method: &str) -> RpcError {
    RpcError::new(
        ErrorKind::MethodNotFound,
        format!("{method} is not available in this build"),
    )
    .with_data(json!({ "reason": "not_implemented" }))
}

/// Handle one request. `None` when the handler already sent the response (subscribe
/// must answer before replaying).
fn dispatch(state: &Arc<DaemonState>, conn: &Arc<Conn>, r: Request) -> Option<Response> {
    if let Some(s) = &state.services {
        match s.dispatch(conn, &r) {
            Handled::Now(res) => {
                return Some(match res {
                    Ok(v) => Response::ok(r.id, v),
                    Err(e) => Response::err(r.id, e),
                });
            }
            Handled::Later => return None,
            Handled::NotMine => {}
        }
    }
    let result: Result<Value, RpcError> = match r.method.as_str() {
        m::PING => empty(),
        m::DAEMON_STATUS => to_result(&state.status()),
        m::DAEMON_RESTART => daemon_restart(state, &r),
        m::INITIALIZE => Err(RpcError::invalid_request("already initialized")),

        n if n == m::SessionSubscribe::NAME => match params::<m::SessionSubscribe>(&r) {
            Ok(p) => return session_subscribe(state, conn, r.id, p),
            Err(e) => Err(e),
        },
        n if n == m::SessionUnsubscribe::NAME => {
            params::<m::SessionUnsubscribe>(&r).and_then(|p| {
                conn.subs
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .remove(&p.session_id);
                empty()
            })
        }
        n if n == m::WorkspaceSubscribe::NAME => {
            workspace_subscribe(state, conn);
            empty()
        }
        n if n == m::SessionList::NAME => params::<m::SessionList>(&r).and_then(|p| {
            let mut items = state.registry.list(&p);
            for s in &mut items {
                if let Some(h) = state.engine.loaded(&s.session_id) {
                    s.status = h.status();
                }
            }
            let limit = p.limit.map_or(usize::MAX, |l| l as usize);
            items.truncate(limit);
            to_result(&sp::ListResult {
                items,
                next_cursor: None,
            })
        }),
        n if n == m::SessionGet::NAME => {
            params::<m::SessionGet>(&r).and_then(|p| session_get(state, &p.session_id))
        }
        n if n == m::SessionCreate::NAME => {
            params::<m::SessionCreate>(&r).and_then(|p| session_create(state, &p, None))
        }
        n if n == m::SessionUpdate::NAME => {
            params::<m::SessionUpdate>(&r).and_then(|p| session_update(state, conn, p))
        }
        n if n == m::SessionDelete::NAME => params::<m::SessionDelete>(&r).and_then(|p| {
            with_lease(state, conn, &p.session_id)?;
            let Some(info) = state.registry.remove(&p.session_id) else {
                return Err(not_found("session"));
            };
            state.engine.close_session(&p.session_id);
            state.leases.forget(&p.session_id);
            state.listed(ListedChange::Deleted, info.summary);
            empty()
        }),
        n if n == m::SessionPrompt::NAME => {
            params::<m::SessionPrompt>(&r).and_then(|p| session_prompt(state, conn, p))
        }
        n if n == m::SessionInterrupt::NAME => params::<m::SessionInterrupt>(&r).and_then(|p| {
            let h = leased_handle(state, conn, &p.session_id)?;
            to_result(&sp::InterruptResult {
                interrupted: h.interrupt(),
            })
        }),
        n if n == m::SessionResume::NAME => params::<m::SessionResume>(&r).and_then(|p| {
            let h = leased_handle(state, conn, &p.session_id)?;
            h.resume().map_err(|e| e.to_rpc())?;
            empty()
        }),
        n if n == m::SessionCompact::NAME => params::<m::SessionCompact>(&r).and_then(|p| {
            let h = leased_handle(state, conn, &p.session_id)?;
            h.compact(p.focus.filter(|f| !f.trim().is_empty()))
                .map_err(|e| e.to_rpc())?;
            empty()
        }),
        n if n == m::SessionRegenerate::NAME => {
            params::<m::SessionRegenerate>(&r).and_then(|p| session_regenerate(state, conn, &p))
        }
        n if n == m::SessionInboxRemove::NAME => {
            params::<m::SessionInboxRemove>(&r).and_then(|p| {
                let h = leased_handle(state, conn, &p.session_id)?;
                h.remove_queued(&p.inbox_id).map_err(|e| e.to_rpc())?;
                empty()
            })
        }
        n if n == m::SessionTakeOver::NAME => {
            params::<m::SessionTakeOver>(&r).and_then(|p| take_over(state, conn, &p.session_id))
        }
        n if n == m::SessionRelease::NAME => params::<m::SessionRelease>(&r).and_then(|p| {
            if !state.registry.exists(&p.session_id) {
                return Err(not_found("session"));
            }
            if state.leases.release(&p.session_id, &conn.client) {
                publish_lease(state, &p.session_id, None)?;
            }
            empty()
        }),
        n if n == m::SessionFork::NAME => params::<m::SessionFork>(&r).and_then(|p| {
            let Some(src) = state.registry.get(&p.session_id) else {
                return Err(not_found("session"));
            };
            // Copying the message path needs the store (spec 04); the fork starts empty
            // with the same settings and records its parent.
            let x = state.registry.extra(&p.session_id);
            let cp = sp::CreateParams {
                kind: src.summary.kind,
                project_id: src.summary.project_id.clone(),
                workspace_path: x.workspace.map(|w| w.display().to_string()),
                model: src.model.clone(),
                effort: src.effort.clone(),
                agent: src.agent.clone(),
                mode: Some(src.mode),
                incognito: src.incognito,
                ..Default::default()
            };
            session_create(state, &cp, Some(p.session_id))
        }),

        n if n == m::PermissionPending::NAME => params::<m::PermissionPending>(&r).and_then(|p| {
            let asks: Vec<Ask> = match p.session_id {
                Some(id) => state
                    .engine
                    .loaded(&id)
                    .map(|h| h.pending_ask_list())
                    .unwrap_or_default(),
                None => state
                    .registry
                    .all_summaries()
                    .iter()
                    .filter_map(|s| state.engine.loaded(&s.session_id))
                    .flat_map(|h| h.pending_ask_list())
                    .collect(),
            };
            to_result(&sp::PendingResult { asks })
        }),
        n if n == m::PermissionRespond::NAME => {
            params::<m::PermissionRespond>(&r).and_then(|p| permission_respond(state, conn, p))
        }

        other => match m::find(other) {
            Some(_) => Err(not_implemented(other)),
            None => Err(RpcError::method_not_found(other)),
        },
    };
    Some(match result {
        Ok(v) => Response::ok(r.id, v),
        Err(e) => Response::err(r.id, e),
    })
}

fn daemon_restart(state: &DaemonState, r: &Request) -> Result<Value, RpcError> {
    let p: DaemonRestartParams = params::<m::DaemonRestart>(r)?;
    let busy = state.engine.busy();
    if busy.has_turns() && !p.stop_running {
        Err(RpcError::new(ErrorKind::Busy, "sessions are running")
            .with_data(json!({ "running": busy.running_turns + busy.pending_asks })))
    } else {
        state.request_shutdown(ShutdownReason::Restart);
        empty()
    }
}

fn handle(state: &DaemonState, session: &str) -> Result<SessionHandle, RpcError> {
    if !state.registry.exists(session) {
        return Err(not_found("session"));
    }
    state.engine.session(session).map_err(|d| d.to_rpc())
}

/// Check the driver lease for a mutating command; acquire it if vacant (02 §5).
fn with_lease(state: &Arc<DaemonState>, conn: &Arc<Conn>, session: &str) -> Result<(), RpcError> {
    if !state.registry.exists(session) {
        return Err(not_found("session"));
    }
    match state.leases.check(session, &conn.client, conn.id) {
        LeaseCheck::Held => Ok(()),
        LeaseCheck::Acquired => {
            publish_lease(state, session, Some(conn.client.clone()))?;
            push_pending_asks(state, conn, session);
            Ok(())
        }
        LeaseCheck::NotDriver(driver) => Err(RpcError::new(
            ErrorKind::NotDriver,
            "another client is driving this session",
        )
        .with_data(json!({ "driver": driver }))),
    }
}

fn leased_handle(
    state: &Arc<DaemonState>,
    conn: &Arc<Conn>,
    session: &str,
) -> Result<SessionHandle, RpcError> {
    with_lease(state, conn, session)?;
    handle(state, session)
}

fn publish_lease(
    state: &DaemonState,
    session: &str,
    driver: Option<ClientRef>,
) -> Result<(), RpcError> {
    let h = handle(state, session)?;
    h.publish_wire(SessionEvent::LeaseChanged(LeaseChanged { driver }))
        .map_err(|e| e.to_rpc())?;
    Ok(())
}

/// A new driver gets `permission.ask` for asks already waiting (02 §7 step 1).
fn push_pending_asks(state: &DaemonState, conn: &Conn, session: &str) {
    if let Some(h) = state.engine.loaded(session) {
        for a in h.pending_ask_list() {
            conn.send_ask(&a);
        }
    }
}

fn take_over(state: &Arc<DaemonState>, conn: &Arc<Conn>, session: &str) -> Result<Value, RpcError> {
    if !state.registry.exists(session) {
        return Err(not_found("session"));
    }
    let t = state.leases.take_over(session, &conn.client, conn.id);
    if let Some(old) = t.old_conn.and_then(|id| state.conn(id)) {
        old.cancel_asks(|s, _| s == session);
        old.notify(
            m::LeaseLostNote::NAME,
            &LeaseLost {
                session_id: session.to_string(),
                new_driver: Some(conn.client.clone()),
            },
        );
    }
    if t.changed {
        publish_lease(state, session, Some(conn.client.clone()))?;
    }
    push_pending_asks(state, conn, session);
    empty()
}

fn session_create(
    state: &Arc<DaemonState>,
    p: &sp::CreateParams,
    parent: Option<String>,
) -> Result<Value, RpcError> {
    if p.model.trim().is_empty() {
        return Err(RpcError::invalid_params("model is required"));
    }
    let overlay = match &p.settings {
        Some(o) => Some(overlay(o, p.workspace_path.as_deref())?),
        None => None,
    };
    let mut p = p.clone();
    if p.mode.is_none() {
        p.mode = overlay.as_ref().and_then(|o| o.default_mode);
    }
    let p = &p;
    let extra = Extra {
        workspace: p
            .workspace_path
            .as_deref()
            .filter(|w| !w.is_empty())
            .map(|w| std::path::PathBuf::from(crate::services::canonical(w))),
        overlay,
        worktree_name: p.worktree_name.clone(),
    };
    let h = state.engine.create_session();
    let info = state.registry.create_with(h.id(), p, parent.clone(), extra);
    h.publish_wire(SessionEvent::SessionCreated(
        genie_protocol::event::SessionCreated {
            kind: p.kind,
            project_id: p.project_id.clone(),
            workspace_id: None,
            model: p.model.clone(),
            effort: p.effort.clone(),
            mode: info.mode,
            agent: p.agent.clone(),
            parent_session_id: parent,
        },
    ))
    .map_err(|e| e.to_rpc())?;
    if !info.incognito {
        state.listed(ListedChange::Created, info.summary.clone());
    }
    to_result(&sp::CreateResult {
        session_id: h.id().to_string(),
    })
}

/// The session-only CLI layer from `session.create.settings` (D23 layer 2): the
/// `--settings` file's `permissions` block plus `--allow`/`--deny`/`--add-dir`.
fn overlay(
    o: &sp::SessionSettingsOverlay,
    workspace: Option<&str>,
) -> Result<genie_core::runtime::SessionOverlay, RpcError> {
    let mut out = genie_core::runtime::SessionOverlay {
        allow: o.allow.clone(),
        deny: o.deny.clone(),
        add_dirs: o.add_dirs.clone(),
        ..Default::default()
    };
    if let Some(path) = o.settings_path.as_deref().filter(|p| !p.is_empty()) {
        let mut pb = std::path::PathBuf::from(path);
        if pb.is_relative()
            && let Some(w) = workspace
        {
            pb = std::path::Path::new(w).join(pb);
        }
        let v = crate::services::read_settings_file(&pb).map_err(RpcError::invalid_params)?;
        let perms = &v["permissions"];
        let list = |k: &str| -> Vec<String> {
            perms[k]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|x| x.as_str().map(String::from))
                .collect()
        };
        out.allow.extend(list("allow"));
        out.ask.extend(list("ask"));
        out.deny.extend(list("deny"));
        out.add_dirs.extend(list("additionalDirectories"));
        out.default_mode = perms["defaultMode"]
            .as_str()
            .and_then(genie_config::ModeSetting::parse)
            .map(genie_config::ModeSetting::to_mode);
    }
    for r in out.allow.iter().chain(&out.ask).chain(&out.deny) {
        genie_permissions::Rule::parse(r)
            .map_err(|e| RpcError::invalid_params(format!("rule `{r}`: {e}")))?;
    }
    Ok(out)
}

fn session_get(state: &DaemonState, id: &str) -> Result<Value, RpcError> {
    let Some(mut info) = state.registry.get(id) else {
        return Err(not_found("session"));
    };
    if let Some(h) = state.engine.loaded(id) {
        info.summary.status = h.status();
    }
    info.driver = state.leases.driver(id);
    info.head_seq = state.engine.bus().head_seq(id).map_err(|d| d.to_rpc())?;
    // The active branch (D24). Sibling lists need the store's tree queries (gap).
    let path = match &state.conversation {
        Some(c) => c
            .path(id)
            .map_err(|d| d.to_rpc())?
            .iter()
            .map(genie_core::runtime::wire::message)
            .collect(),
        None => Vec::new(),
    };
    to_result(&sp::GetResult {
        session: info,
        path,
        siblings: Default::default(),
    })
}

/// `session.regenerate` (D24): a sibling reply, queued like a prompt.
fn session_regenerate(
    state: &Arc<DaemonState>,
    conn: &Arc<Conn>,
    p: &sp::RegenerateParams,
) -> Result<Value, RpcError> {
    let h = leased_handle(state, conn, &p.session_id)?;
    if let Some(c) = &state.conversation {
        let m = c
            .get(&p.session_id, &p.message_id)
            .map_err(|d| d.to_rpc())?
            .ok_or_else(|| not_found("message"))?;
        if !m.is_assistant() {
            return Err(RpcError::invalid_params(
                "message_id must be an assistant message",
            ));
        }
    }
    h.regenerate(p.message_id.clone()).map_err(|e| e.to_rpc())?;
    empty()
}

fn session_update(
    state: &Arc<DaemonState>,
    conn: &Arc<Conn>,
    p: sp::UpdateParams,
) -> Result<Value, RpcError> {
    if p.is_mutating() {
        with_lease(state, conn, &p.session_id)?;
    }
    let h = handle(state, &p.session_id)?;
    if state.registry.update(&p).is_none() {
        return Err(not_found("session"));
    }
    h.publish_wire(SessionEvent::SessionUpdated(SessionUpdated {
        title: p.title,
        model: p.model,
        effort: p.effort,
        mode: p.mode,
        pinned: p.pinned,
        archived: p.archived,
        active_leaf_id: p.active_leaf_id,
    }))
    .map_err(|e| e.to_rpc())?;
    empty()
}

fn session_prompt(
    state: &Arc<DaemonState>,
    conn: &Arc<Conn>,
    p: sp::PromptParams,
) -> Result<Value, RpcError> {
    if p.client_msg_id.is_empty() {
        return Err(RpcError::invalid_params("client_msg_id is required"));
    }
    if p.parts.is_empty() {
        return Err(RpcError::invalid_params("parts must not be empty"));
    }
    let key = (p.session_id.clone(), p.client_msg_id.clone());
    if let Some(prev) = state
        .prompts
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .get(&key)
    {
        return to_result(prev);
    }
    let h = leased_handle(state, conn, &p.session_id)?;
    // `parent_id` (edit → sibling branch, D24) needs the store's message tree.
    if p.parent_id.is_some() {
        return Err(not_implemented("session.prompt with parent_id"));
    }
    let (inbox_id, message_id) = h
        .prompt_parts(p.parts, p.delivery)
        .map_err(|e| e.to_rpc())?;
    let res = PromptResult {
        message_id,
        inbox_id,
    };
    {
        let mut prompts = state.prompts.lock().unwrap_or_else(|e| e.into_inner());
        if prompts.len() >= IDEMPOTENCY_CAP {
            prompts.clear();
        }
        prompts.insert(key, res.clone());
    }
    state.registry.touch(&p.session_id);
    to_result(&res)
}

fn permission_respond(
    state: &DaemonState,
    conn: &Conn,
    p: sp::RespondParams,
) -> Result<Value, RpcError> {
    // Not lease-gated: any authorised client may answer (02 §5).
    let owner = state
        .registry
        .all_summaries()
        .into_iter()
        .filter_map(|s| state.engine.loaded(&s.session_id))
        .find(|h| h.has_ask(&p.ask_id));
    let Some(h) = owner else {
        return Err(RpcError::new(
            ErrorKind::NotFound,
            "ask not found or already answered",
        ));
    };
    // If this client had the request open, it is answered now.
    conn.server_reqs
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .retain(|_, (_, a)| *a != p.ask_id);
    h.answer(&p.ask_id, p.answer, ResolvedBy::Client(conn.client.clone()))
        .map_err(|e| match e {
            genie_core::error::CoreError::NotFound(_) => {
                RpcError::new(ErrorKind::Conflict, "ask already answered")
            }
            other => other.to_rpc(),
        })?;
    empty()
}

// ------------------------------------------------------------------ subscriptions

fn session_subscribe(
    state: &Arc<DaemonState>,
    conn: &Arc<Conn>,
    id: Id,
    p: sp::SubscribeParams,
) -> Option<Response> {
    let h = match handle(state, &p.session_id) {
        Ok(h) => h,
        Err(e) => return Some(Response::err(id, e)),
    };
    let bus = state.engine.bus();
    let floor = match bus.log_floor(&p.session_id) {
        Ok(f) => f,
        Err(d) => return Some(Response::err(id, d.to_rpc())),
    };
    // Below the retained log: reload with session.get, then subscribe from head (§3.1).
    let below_floor = p.after_seq.is_some_and(|s| s < floor);
    let after = p.after_seq.unwrap_or(floor);
    let sub = match bus.subscribe(&p.session_id, if below_floor { u64::MAX } else { after }) {
        Ok(s) => s,
        Err(d) => return Some(Response::err(id, d.to_rpc())),
    };
    let head_seq = match bus.head_seq(&p.session_id) {
        Ok(s) => s,
        Err(d) => return Some(Response::err(id, d.to_rpc())),
    };
    let result = sp::SubscribeResult {
        head_seq,
        driver: state.leases.driver(&p.session_id),
        status: h.status(),
    };
    match to_result(&result) {
        Ok(v) => conn.respond(Response::ok(id, v)),
        Err(e) => return Some(Response::err(id, e)),
    }
    if below_floor {
        conn.notify(
            m::SessionResyncRequired::NAME,
            &ResyncRequired {
                session_id: p.session_id.clone(),
                from_seq: 0,
                snapshot: true,
            },
        );
        return None;
    }

    let scope = conn.scope.child("subscription");
    let sub_id = conn.next_req.fetch_add(1, Ordering::Relaxed);
    let entry = SubEntry {
        id: sub_id,
        scope: scope.clone(),
        _attach: h.attach(),
    };
    // Re-subscribing replaces the old subscription (resync path).
    if let Some(old) = conn
        .subs
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .insert(p.session_id.clone(), entry)
    {
        old.scope.cancel();
    }
    let st = state.clone();
    let c = conn.clone();
    let session = p.session_id;
    scope.spawn_cancellable(forward(st, c, session, sub_id, sub, after, h));
    None
}

impl Drop for SubEntry {
    fn drop(&mut self) {
        self.scope.cancel();
    }
}

async fn forward(
    state: Arc<DaemonState>,
    conn: Arc<Conn>,
    session: String,
    sub_id: u64,
    mut sub: genie_core::bus::Subscription,
    after: u64,
    h: SessionHandle,
) {
    let q = Arc::new(SubQueue::default());
    let max_bytes = state.cfg.sub_queue_max_bytes;
    let max_msgs = state.cfg.sub_queue_max_msgs;
    let mut last_seq = after;
    while let Some(item) = sub.next().await {
        match item {
            SubItem::Event(env) => {
                let wire = env.to_wire();
                // Live asks: forward to the driver as a request (02 §7 step 1).
                match &wire.event {
                    SessionEvent::PermissionAsked(a)
                        if state.leases.is_driver(&session, conn.id) && h.has_ask(&a.ask.id) =>
                    {
                        conn.send_ask(&a.ask);
                    }
                    SessionEvent::PermissionResolved(r) => {
                        let ask_id = r.ask_id.clone();
                        conn.cancel_asks(|_, a| a == ask_id);
                    }
                    _ => {}
                }
                let Some(line) = notification_line(m::EVENT, &wire) else {
                    continue;
                };
                let bytes = q.bytes.load(Ordering::SeqCst);
                let msgs = q.msgs.load(Ordering::SeqCst);
                if bytes + line.len() > max_bytes || msgs + 1 > max_msgs {
                    // No lossy streams: drop this subscription, tell the client where
                    // to resume (02 §3.1).
                    tracing::debug!(session_id = %session, last_seq, "subscription overflow");
                    conn.notify(
                        m::SessionResyncRequired::NAME,
                        &ResyncRequired {
                            session_id: session.clone(),
                            from_seq: last_seq,
                            snapshot: false,
                        },
                    );
                    break;
                }
                if let Some(s) = wire.seq {
                    last_seq = s;
                }
                q.bytes.fetch_add(line.len(), Ordering::SeqCst);
                q.msgs.fetch_add(1, Ordering::SeqCst);
                if conn
                    .out
                    .send(Out::Line(line.into(), Some(q.clone())))
                    .is_err()
                {
                    break;
                }
            }
            SubItem::Synced => conn.notify(
                m::SessionSyncedNote::NAME,
                &SessionSynced {
                    session_id: session.clone(),
                    seq: last_seq,
                },
            ),
            SubItem::ResyncRequired { from_seq } => {
                conn.notify(
                    m::SessionResyncRequired::NAME,
                    &ResyncRequired {
                        session_id: session.clone(),
                        from_seq,
                        snapshot: false,
                    },
                );
                break;
            }
        }
    }
    // Detach this subscription unless a re-subscribe already replaced it.
    let mut subs = conn.subs.lock().unwrap_or_else(|e| e.into_inner());
    if subs.get(&session).is_some_and(|e| e.id == sub_id) {
        subs.remove(&session);
    }
}

fn workspace_subscribe(state: &Arc<DaemonState>, conn: &Arc<Conn>) {
    state.ensure_pump();
    let mut rx = state.workspace.subscribe();
    let st = state.clone();
    let c = conn.clone();
    conn.scope.child("workspace").spawn_cancellable(async move {
        send_snapshot(&st, &c);
        loop {
            match rx.recv().await {
                Ok(line) => {
                    if c.out.send(Out::Line(line, None)).is_err() {
                        break;
                    }
                }
                // The feed has no seq: a fresh snapshot is the recovery (02 §3.2).
                Err(broadcast::error::RecvError::Lagged(_)) => send_snapshot(&st, &c),
                Err(broadcast::error::RecvError::Closed) => break,
            }
        }
    });
}

fn send_snapshot(state: &DaemonState, conn: &Conn) {
    let mut sessions = state.registry.all_summaries();
    let mut pending = 0u32;
    for s in &mut sessions {
        if let Some(h) = state.engine.loaded(&s.session_id) {
            s.status = h.status();
            pending += u32::try_from(h.pending_asks()).unwrap_or(u32::MAX);
        }
    }
    let mut clients: Vec<ClientRef> = state
        .conns
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .values()
        .map(|c| c.client.clone())
        .collect();
    clients.sort_by(|a, b| a.client_instance_id.cmp(&b.client_instance_id));
    conn.notify(
        m::WorkspaceSnapshot::NAME,
        &WorkspaceSnapshot {
            sessions,
            pending_asks: pending,
            clients,
        },
    );
}
