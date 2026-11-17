//! The relay between the daemon and the webview (01 §1.3, D22, D57).
//!
//! The Tauri side owns one `genie-client` [`Connection`] (via `ensure`) and:
//! - turns webview `invoke`s into JSON-RPC requests ([`Relay::request`]);
//! - queues daemon notifications in a [`Batcher`] and flushes them to the webview's IPC
//!   Channel every ~8 ms ([`Relay::flush`]);
//! - relays daemon → client requests (`permission.ask`) as [`HostMessage::ServerRequest`]
//!   and sends the webview's answer back ([`Relay::respond`]); `$/cancelRequest` becomes
//!   [`HostMessage::RequestCancelled`];
//! - keeps a [`SessionTracker`] per subscribed session, so the webview never sees
//!   duplicates or stale deltas, resubscribes with the cursor on overflow, and after a
//!   reconnect re-subscribes every open session with `after_seq = last_seq` (02 §3.1).
//!
//! It knows nothing about Tauri: the webview end is a [`Sink`], so tests drive it with
//! an in-memory daemon.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use genie_client::{
    Action, ClientError, Connection, EnsureOptions, EnsureOutcome, EnsureState, Incoming,
    SessionTracker, ensure,
};
use genie_protocol::{ErrorKind, Id, Notification, Request, RpcError};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::runtime::Handle;
use tokio::sync::Notify;
use tokio_util::sync::CancellationToken;
use tokio_util::task::TaskTracker;

use crate::batch::Batcher;

/// Connection state shown by the app ("Starting engine…", error card with Retry, 01 §4.1).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ConnState {
    Connecting,
    /// Spawn / WaitReady: "Starting engine…".
    Starting,
    /// An older daemon drains and restarts (01 §4.2).
    Restarting,
    Ready,
    /// Older busy daemon, same major: "Engine will update when tasks finish".
    Degraded,
    /// Older busy daemon, other major: "Genie is finishing N tasks before updating".
    BusyUpdating,
    /// EOF; reconnecting and resubscribing.
    Reconnecting,
    /// Gave up: error card with **Retry** and **Open logs**.
    Failed,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct HostStatus {
    pub state: ConnState,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub engine_version: Option<String>,
    /// `initialize.locked.reason`: the secrets store needs `secrets.unlock` (D26).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub locked: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub running: Option<u32>,
}

impl HostStatus {
    fn new(state: ConnState) -> Self {
        HostStatus {
            state,
            detail: None,
            engine_version: None,
            locked: None,
            running: None,
        }
    }
}

/// Everything the host pushes to the webview over the IPC Channel.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum HostMessage {
    Status(HostStatus),
    /// Daemon notifications (`{method, params}`), in order, deltas coalesced.
    Batch {
        messages: Vec<Value>,
    },
    /// A daemon → client request (e.g. `permission.ask`); answer with `host_respond`.
    ServerRequest {
        req_id: u64,
        method: String,
        params: Value,
    },
    /// The daemon withdrew the request (answered elsewhere, lease moved, reconnect).
    RequestCancelled {
        req_id: u64,
    },
    /// The cursor fell below the retained log: `session.get`, then subscribe from its
    /// `head_seq` (02 §3.1).
    Reload {
        session_id: String,
    },
}

/// What `host_connect` returns.
#[derive(Debug, Clone, Serialize)]
pub struct HostInfo {
    pub client_instance_id: String,
    pub os: &'static str,
    pub app_version: &'static str,
    pub batch_ms: u64,
    /// The OS account's full name (or login), for the greeting and profile row
    /// until the user sets a display name. Empty when unknown.
    pub user_name: String,
}

/// Best-effort full name of the signed-in OS user: macOS `id -F`, the passwd GECOS field
/// on Linux, then the login name from the environment.
pub fn os_user_name() -> String {
    #[cfg(target_os = "macos")]
    if let Ok(out) = std::process::Command::new("/usr/bin/id").arg("-F").output() {
        let name = String::from_utf8_lossy(&out.stdout).trim().to_string();
        if out.status.success() && !name.is_empty() {
            return name;
        }
    }
    let login = std::env::var("USER").or_else(|_| std::env::var("USERNAME")).unwrap_or_default();
    #[cfg(target_os = "linux")]
    if let Ok(passwd) = std::fs::read_to_string("/etc/passwd") {
        let gecos = passwd
            .lines()
            .map(|l| l.split(':').collect::<Vec<_>>())
            .find(|f| f.len() > 4 && f[0] == login)
            .map(|f| f[4].split(',').next().unwrap_or("").trim().to_string());
        if let Some(g) = gecos.filter(|g| !g.is_empty()) {
            return g;
        }
    }
    login
}

/// The webview end of the relay.
pub trait Sink: Send + Sync + 'static {
    fn send(&self, msg: HostMessage);
}

/// A JSON-RPC style error as the webview sees it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, thiserror::Error)]
#[error("{message}")]
pub struct WireError {
    pub code: i64,
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data: Option<Value>,
}

impl WireError {
    pub fn kind(kind: ErrorKind, message: impl Into<String>) -> Self {
        RpcError::new(kind, message).into()
    }
}

impl From<RpcError> for WireError {
    fn from(e: RpcError) -> Self {
        WireError {
            code: e.code,
            message: e.message,
            data: e.data,
        }
    }
}

impl From<WireError> for RpcError {
    fn from(e: WireError) -> Self {
        RpcError {
            code: e.code,
            message: e.message,
            data: e.data,
        }
    }
}

impl From<ClientError> for WireError {
    fn from(e: ClientError) -> Self {
        match e {
            ClientError::Rpc(r) => r.into(),
            ClientError::Closed => {
                WireError::kind(ErrorKind::InvalidState, "engine connection lost")
            }
            other => WireError::kind(ErrorKind::InvalidState, other.to_string()),
        }
    }
}

struct ServerReq {
    generation: u64,
    id: Id,
    method: String,
    params: Value,
}

pub struct Relay {
    runtime: Handle,
    tasks: TaskTracker,
    shutdown: CancellationToken,
    instance_id: String,
    user_name: String,
    conn: Mutex<Option<(u64, Arc<Connection>)>>,
    generation: AtomicU64,
    sink: Mutex<Option<Arc<dyn Sink>>>,
    status: Mutex<HostStatus>,
    batch: Mutex<Batcher>,
    trackers: Mutex<HashMap<String, SessionTracker>>,
    workspace: AtomicBool,
    server_reqs: Mutex<HashMap<u64, ServerReq>>,
    next_req: AtomicU64,
    retry: Notify,
}

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

impl Relay {
    pub fn new(runtime: Handle, instance_id: impl Into<String>) -> Arc<Relay> {
        Arc::new(Relay {
            runtime,
            tasks: TaskTracker::new(),
            shutdown: CancellationToken::new(),
            instance_id: instance_id.into(),
            user_name: os_user_name(),
            conn: Mutex::new(None),
            generation: AtomicU64::new(0),
            sink: Mutex::new(None),
            status: Mutex::new(HostStatus::new(ConnState::Connecting)),
            batch: Mutex::new(Batcher::default()),
            trackers: Mutex::new(HashMap::new()),
            workspace: AtomicBool::new(false),
            server_reqs: Mutex::new(HashMap::new()),
            next_req: AtomicU64::new(1),
            retry: Notify::new(),
        })
    }

    /// Spawn on the relay's task tracker (01 §2.1 rule 4).
    pub fn spawn<F>(&self, fut: F)
    where
        F: std::future::Future<Output = ()> + Send + 'static,
    {
        self.tasks.spawn_on(fut, &self.runtime);
    }

    pub fn instance_id(&self) -> &str {
        &self.instance_id
    }

    /// Attach (or replace, after a webview reload) the webview end. The current status
    /// is sent at once. The webview re-subscribes its sessions itself after a reload.
    pub fn attach(&self, sink: Arc<dyn Sink>) -> HostInfo {
        sink.send(HostMessage::Status(self.status()));
        // A reloaded webview lost its view of pending asks: send them again.
        let pending: Vec<HostMessage> = lock(&self.server_reqs)
            .iter()
            .map(|(req_id, r)| HostMessage::ServerRequest {
                req_id: *req_id,
                method: r.method.clone(),
                params: r.params.clone(),
            })
            .collect();
        for m in pending {
            sink.send(m);
        }
        *lock(&self.sink) = Some(sink);
        HostInfo {
            client_instance_id: self.instance_id.clone(),
            os: std::env::consts::OS,
            app_version: env!("CARGO_PKG_VERSION"),
            batch_ms: crate::batch::BATCH_MS,
            user_name: self.user_name.clone(),
        }
    }

    pub fn status(&self) -> HostStatus {
        lock(&self.status).clone()
    }

    fn set_status(&self, st: HostStatus) {
        let changed = {
            let mut cur = lock(&self.status);
            let changed = *cur != st;
            *cur = st.clone();
            changed
        };
        if changed {
            tracing::info!(state = ?st.state, detail = ?st.detail, "engine connection");
            self.flush();
            self.emit(HostMessage::Status(st));
        }
    }

    fn emit(&self, msg: HostMessage) {
        let sink = lock(&self.sink).clone();
        if let Some(s) = sink {
            s.send(msg);
        }
    }

    /// Send queued notifications as one Channel message.
    pub fn flush(&self) {
        let items = {
            let mut b = lock(&self.batch);
            if b.is_empty() {
                return;
            }
            b.take()
        };
        self.emit(HostMessage::Batch { messages: items });
    }

    /// Flush every `every` until shutdown (D57: ~8 ms).
    pub fn start_flusher(self: &Arc<Self>, every: Duration) {
        let me = self.clone();
        self.spawn(async move {
            let mut iv = tokio::time::interval(every);
            iv.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            loop {
                tokio::select! {
                    _ = iv.tick() => me.flush(),
                    () = me.shutdown.cancelled() => break,
                }
            }
        });
    }

    fn current(&self) -> Option<(u64, Arc<Connection>)> {
        lock(&self.conn).clone()
    }

    /// A webview request → JSON-RPC request. Subscriptions are tracked so they survive
    /// reconnects.
    pub async fn request(&self, method: &str, params: Value) -> Result<Value, WireError> {
        let Some((_, conn)) = self.current() else {
            return Err(WireError::kind(
                ErrorKind::InvalidState,
                "the engine is not connected",
            ));
        };
        match method {
            "session.subscribe" => {
                if let Some(sid) = params["session_id"].as_str() {
                    let after = params["after_seq"].as_u64().unwrap_or(0);
                    lock(&self.trackers).insert(sid.to_string(), SessionTracker::new(sid, after));
                }
            }
            "workspace.subscribe" => self.workspace.store(true, Ordering::SeqCst),
            _ => {}
        }
        let res = conn.request_value(method, params.clone()).await;
        if method == "session.unsubscribe"
            && let Some(sid) = params["session_id"].as_str()
        {
            lock(&self.trackers).remove(sid);
        }
        res.map_err(WireError::from)
    }

    /// The webview's answer to a [`HostMessage::ServerRequest`].
    pub fn respond(&self, req_id: u64, result: Result<Value, WireError>) -> Result<(), WireError> {
        let req = lock(&self.server_reqs).remove(&req_id);
        let Some(req) = req else {
            return Err(WireError::kind(
                ErrorKind::InvalidParams,
                "that request is no longer pending",
            ));
        };
        match self.current() {
            Some((generation, conn)) if generation == req.generation => conn
                .respond(req.id, result.map_err(RpcError::from))
                .map_err(WireError::from),
            _ => Err(WireError::kind(
                ErrorKind::InvalidState,
                "the engine connection changed; the ask will be asked again",
            )),
        }
    }

    /// The user clicked **Retry** on the error card.
    pub fn retry_now(&self) {
        self.retry.notify_one();
    }

    pub fn shutdown(&self) {
        self.shutdown.cancel();
    }

    /// The manager loop: ensure → serve → reconnect, forever (01 §4.1).
    pub fn start(self: &Arc<Self>, opts: EnsureOptions) {
        let me = self.clone();
        self.spawn(async move { me.run(opts).await });
    }

    async fn run(self: Arc<Self>, opts: EnsureOptions) {
        loop {
            if self.shutdown.is_cancelled() {
                return;
            }
            let outcome = ensure(&opts, |s| {
                let state = match s {
                    EnsureState::Connect | EnsureState::Handshake => ConnState::Connecting,
                    EnsureState::Spawn | EnsureState::WaitReady => ConnState::Starting,
                    EnsureState::Restarting => ConnState::Restarting,
                    EnsureState::Ready => return,
                };
                let mut st = HostStatus::new(state);
                if state == ConnState::Starting {
                    st.detail = Some("Starting engine…".into());
                }
                self.set_status(st);
            })
            .await;
            match outcome {
                Ok(EnsureOutcome::Ready(conn)) => self.serve(conn, false).await,
                Ok(EnsureOutcome::Degraded(conn)) => self.serve(conn, true).await,
                Ok(EnsureOutcome::BusyUpdating { running }) => {
                    let mut st = HostStatus::new(ConnState::BusyUpdating);
                    st.running = Some(running);
                    self.set_status(st);
                    self.wait_retry(Some(Duration::from_secs(5))).await;
                    continue;
                }
                Err(e) => {
                    let mut st = HostStatus::new(ConnState::Failed);
                    st.detail = Some(e.to_string());
                    self.set_status(st);
                    self.wait_retry(None).await;
                    continue;
                }
            }
            if self.shutdown.is_cancelled() {
                return;
            }
            let mut st = HostStatus::new(ConnState::Reconnecting);
            st.detail = Some("Engine connection lost; reconnecting…".into());
            self.set_status(st);
        }
    }

    async fn wait_retry(&self, max: Option<Duration>) {
        let wait = self.retry.notified();
        match max {
            Some(d) => {
                tokio::select! {
                    () = wait => {},
                    () = tokio::time::sleep(d) => {},
                    () = self.shutdown.cancelled() => {},
                }
            }
            None => {
                tokio::select! {
                    () = wait => {},
                    () = self.shutdown.cancelled() => {},
                }
            }
        }
    }

    /// Serve one handshaken connection until EOF. Public for tests.
    pub async fn serve(&self, conn: Connection, degraded: bool) {
        let Some(mut incoming) = conn.take_incoming() else {
            return;
        };
        let generation = self.generation.fetch_add(1, Ordering::SeqCst) + 1;
        let mut st = HostStatus::new(if degraded {
            ConnState::Degraded
        } else {
            ConnState::Ready
        });
        st.engine_version = Some(conn.init.engine_version.clone());
        st.locked = conn.init.locked.as_ref().map(|l| l.reason.clone());
        let conn = Arc::new(conn);
        *lock(&self.conn) = Some((generation, conn.clone()));
        self.resubscribe_all(&conn).await;
        self.set_status(st);

        loop {
            tokio::select! {
                m = incoming.recv() => match m {
                    Some(Incoming::Notification(n)) => self.on_notification(&conn, generation, n).await,
                    Some(Incoming::Request(r)) => self.on_server_request(generation, r),
                    None => break,
                },
                () = conn.closed() => {
                    // Drain whatever the reader already queued.
                    while let Ok(m) = incoming.try_recv() {
                        if let Incoming::Notification(n) = m {
                            self.on_notification(&conn, generation, n).await;
                        }
                    }
                    break;
                }
                () = self.shutdown.cancelled() => break,
            }
        }

        *lock(&self.conn) = None;
        self.flush();
        let stale: Vec<u64> = {
            let mut reqs = lock(&self.server_reqs);
            let ids: Vec<u64> = reqs
                .iter()
                .filter(|(_, r)| r.generation == generation)
                .map(|(k, _)| *k)
                .collect();
            for k in &ids {
                reqs.remove(k);
            }
            ids
        };
        for req_id in stale {
            self.emit(HostMessage::RequestCancelled { req_id });
        }
    }

    /// After (re)connecting: the workspace feed and every tracked session, from the
    /// last applied `seq` (02 §3.1 resume).
    async fn resubscribe_all(&self, conn: &Connection) {
        if self.workspace.load(Ordering::SeqCst)
            && let Err(e) = conn.request_value("workspace.subscribe", json!({})).await
        {
            tracing::warn!(error = %e, "workspace resubscribe failed");
        }
        let cursors: Vec<(String, u64)> = {
            let mut t = lock(&self.trackers);
            t.values_mut()
                .map(|tr| {
                    let seq = tr.last_seq();
                    tr.reset_to(seq);
                    (tr.session_id().to_string(), seq)
                })
                .collect()
        };
        for (sid, after) in cursors {
            if let Err(e) = conn
                .request_value(
                    "session.subscribe",
                    json!({"session_id": sid, "after_seq": after}),
                )
                .await
            {
                tracing::warn!(session = %sid, error = %e, "resubscribe failed");
            }
        }
    }

    async fn on_notification(&self, conn: &Connection, generation: u64, n: Notification) {
        match n.method.as_str() {
            "event" | "session.synced" | "session.resync_required" => {
                let sid = n.params["session_id"].as_str().map(str::to_string);
                let action = sid.as_ref().and_then(|sid| {
                    lock(&self.trackers)
                        .get_mut(sid)
                        .map(|t| t.on_notification(&n))
                });
                match action {
                    // Untracked session (e.g. a subagent child): pass through.
                    None => lock(&self.batch).push(&n.method, n.params),
                    Some(Action::Event(_) | Action::Synced) => {
                        lock(&self.batch).push(&n.method, n.params);
                    }
                    Some(Action::Ignore) => {}
                    Some(Action::Resubscribe { after_seq }) => {
                        let sid = sid.unwrap_or_default();
                        if let Err(e) = conn
                            .request_value(
                                "session.subscribe",
                                json!({"session_id": sid, "after_seq": after_seq}),
                            )
                            .await
                        {
                            tracing::warn!(session = %sid, error = %e, "resubscribe failed");
                        }
                    }
                    Some(Action::Reload) => {
                        let session_id = sid.unwrap_or_default();
                        self.flush();
                        self.emit(HostMessage::Reload { session_id });
                    }
                }
            }
            "$/cancelRequest" => {
                let Ok(id) = serde_json::from_value::<Id>(n.params["id"].clone()) else {
                    return;
                };
                let found = {
                    let mut reqs = lock(&self.server_reqs);
                    let k = reqs
                        .iter()
                        .find(|(_, r)| r.generation == generation && r.id == id)
                        .map(|(k, _)| *k);
                    if let Some(k) = k {
                        reqs.remove(&k);
                    }
                    k
                };
                if let Some(req_id) = found {
                    self.flush();
                    self.emit(HostMessage::RequestCancelled { req_id });
                }
            }
            _ => lock(&self.batch).push(&n.method, n.params),
        }
    }

    fn on_server_request(&self, generation: u64, r: Request) {
        let req_id = self.next_req.fetch_add(1, Ordering::SeqCst);
        lock(&self.server_reqs).insert(
            req_id,
            ServerReq {
                generation,
                id: r.id,
                method: r.method.clone(),
                params: r.params.clone(),
            },
        );
        self.flush();
        self.emit(HostMessage::ServerRequest {
            req_id,
            method: r.method,
            params: r.params,
        });
    }

    /// The resume cursor of a tracked session (tests, diagnostics).
    pub fn cursor(&self, session_id: &str) -> Option<u64> {
        lock(&self.trackers)
            .get(session_id)
            .map(SessionTracker::last_seq)
    }
}
