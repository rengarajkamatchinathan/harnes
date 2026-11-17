//! One MCP connection: JSON-RPC over a transport, the initialize handshake, typed
//! requests with timeouts, progress and cancellation, and server → client requests.
//!
//! Protocol version: Genie speaks MCP **2025-06-18** (the first revision with elicitation)
//! and accepts servers that answer 2025-03-26 or 2024-11-05. Spec assumption: 06 §7 names
//! no revision; see README "Spec 06 §7 gaps".

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use genie_protocol::jsonrpc::Outcome;
use genie_protocol::{Id, Message, Notification, Request, Response, RpcError};
use serde_json::{Map, Value, json};
use tokio::sync::{mpsc, oneshot};
use tokio_util::sync::CancellationToken;
use tokio_util::task::TaskTracker;

use crate::error::McpError;
use crate::handler::{
    ClientHandler, ElicitRequest, SamplingBudget, SamplingRefusal, SamplingRequest,
};
use crate::logs::LogRing;
use crate::transport::{CloseReason, Inbound, TransportPair};

pub const LATEST_PROTOCOL_VERSION: &str = "2025-06-18";
pub const SUPPORTED_PROTOCOL_VERSIONS: &[&str] = &["2025-06-18", "2025-03-26", "2024-11-05"];

/// Pages fetched per list call before giving up on a looping cursor.
const MAX_PAGES: usize = 100;

/// Notifications the server layer reacts to.
#[derive(Debug, Clone, PartialEq)]
pub enum ServerNotification {
    ToolsListChanged,
    ResourcesListChanged,
    PromptsListChanged,
    ResourceUpdated { uri: String },
}

/// `notifications/progress` for one of our requests.
#[derive(Debug, Clone, PartialEq)]
pub struct Progress {
    pub progress: f64,
    pub total: Option<f64>,
    pub message: Option<String>,
}

/// Per-request options.
#[derive(Clone, Default)]
pub struct RequestOpts {
    pub timeout: Option<Duration>,
    pub cancel: Option<CancellationToken>,
    /// Ask the server for progress (`_meta.progressToken`) and deliver it here.
    pub progress: Option<mpsc::UnboundedSender<Progress>>,
}

/// What the server said in `initialize`.
#[derive(Debug, Clone, PartialEq)]
pub struct ServerInit {
    pub protocol_version: String,
    pub capabilities: Value,
    pub server_info: Value,
    pub instructions: Option<String>,
}

impl ServerInit {
    pub fn has(&self, cap: &str) -> bool {
        self.capabilities.get(cap).is_some_and(|v| !v.is_null())
    }

    pub fn resources_subscribe(&self) -> bool {
        self.capabilities
            .pointer("/resources/subscribe")
            .and_then(Value::as_bool)
            .unwrap_or(false)
    }
}

/// What a connection needs from its server.
#[derive(Clone)]
pub struct ClientCtx {
    pub server: String,
    pub tasks: TaskTracker,
    pub logs: LogRing,
    pub handler: Arc<dyn ClientHandler>,
    pub notify: mpsc::UnboundedSender<ServerNotification>,
    /// `false` when policy bans sampling for this server (D30).
    pub sampling_allowed: bool,
    pub sampling_budget: Arc<SamplingBudget>,
    pub client_name: String,
    pub client_version: String,
}

struct Pending {
    tx: oneshot::Sender<Result<Value, RpcError>>,
    progress: Option<mpsc::UnboundedSender<Progress>>,
}

struct Shared {
    out: mpsc::Sender<Message>,
    pending: Mutex<HashMap<u64, Pending>>,
    /// Server → client requests in flight, cancellable by `notifications/cancelled`.
    incoming: Mutex<HashMap<String, CancellationToken>>,
    next_id: AtomicU64,
    closed: CancellationToken,
    close_reason: Mutex<Option<CloseReason>>,
}

impl Shared {
    fn close(&self, reason: CloseReason) {
        if let Ok(mut r) = self.close_reason.lock()
            && r.is_none()
        {
            *r = Some(reason);
        }
        self.closed.cancel();
        // Dropping the senders fails every waiting request with "closed".
        if let Ok(mut p) = self.pending.lock() {
            p.clear();
        }
        if let Ok(mut i) = self.incoming.lock() {
            for (_, t) in i.drain() {
                t.cancel();
            }
        }
    }

    fn closed_err(&self) -> McpError {
        match self.reason() {
            CloseReason::Unauthorized { www_authenticate } => {
                McpError::NeedsAuth { www_authenticate }
            }
            r => McpError::Closed(r.to_string()),
        }
    }

    fn reason(&self) -> CloseReason {
        self.close_reason
            .lock()
            .ok()
            .and_then(|r| r.clone())
            .unwrap_or(CloseReason::Local)
    }
}

/// A live MCP connection. Dropping it closes the transport (and kills a stdio server).
pub struct McpClient {
    shared: Arc<Shared>,
    transport_close: CancellationToken,
    init: ServerInit,
}

impl Drop for McpClient {
    fn drop(&mut self) {
        self.transport_close.cancel();
    }
}

fn id_key(id: &Id) -> String {
    match id {
        Id::Num(n) => n.to_string(),
        Id::Str(s) => format!("s:{s}"),
    }
}

fn value_key(v: &Value) -> Option<String> {
    match v {
        Value::Number(n) => n.as_u64().map(|n| n.to_string()),
        Value::String(s) => Some(format!("s:{s}")),
        _ => None,
    }
}

impl McpClient {
    /// Start the dispatcher and run the initialize handshake within `timeout`.
    pub async fn connect(
        pair: TransportPair,
        ctx: ClientCtx,
        timeout: Duration,
    ) -> Result<McpClient, McpError> {
        let TransportPair { tx, rx, close } = pair;
        let shared = Arc::new(Shared {
            out: tx,
            pending: Mutex::new(HashMap::new()),
            incoming: Mutex::new(HashMap::new()),
            next_id: AtomicU64::new(1),
            closed: CancellationToken::new(),
            close_reason: Mutex::new(None),
        });
        ctx.tasks
            .spawn(dispatch(shared.clone(), rx, ctx.clone(), close.clone()));
        let mut client = McpClient {
            shared,
            transport_close: close,
            init: ServerInit {
                protocol_version: String::new(),
                capabilities: Value::Null,
                server_info: Value::Null,
                instructions: None,
            },
        };
        let params = json!({
            "protocolVersion": LATEST_PROTOCOL_VERSION,
            "capabilities": {
                "elicitation": {},
                "sampling": {},
            },
            "clientInfo": {"name": ctx.client_name, "version": ctx.client_version},
        });
        let res = client
            .request(
                "initialize",
                params,
                RequestOpts {
                    timeout: Some(timeout),
                    ..RequestOpts::default()
                },
            )
            .await?;
        let version = res
            .get("protocolVersion")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        if !SUPPORTED_PROTOCOL_VERSIONS.contains(&version.as_str()) {
            return Err(McpError::Version {
                server: version,
                supported: SUPPORTED_PROTOCOL_VERSIONS
                    .iter()
                    .map(|s| (*s).to_string())
                    .collect(),
            });
        }
        client.init = ServerInit {
            protocol_version: version,
            capabilities: res.get("capabilities").cloned().unwrap_or(json!({})),
            server_info: res.get("serverInfo").cloned().unwrap_or(Value::Null),
            instructions: res
                .get("instructions")
                .and_then(Value::as_str)
                .map(String::from),
        };
        client
            .notify("notifications/initialized", Value::Null)
            .await?;
        Ok(client)
    }

    pub fn init(&self) -> &ServerInit {
        &self.init
    }

    /// Cancelled when the connection is gone.
    pub fn closed(&self) -> CancellationToken {
        self.shared.closed.clone()
    }

    pub fn close_reason(&self) -> Option<CloseReason> {
        if self.shared.closed.is_cancelled() {
            Some(self.shared.reason())
        } else {
            None
        }
    }

    pub fn close(&self) {
        self.transport_close.cancel();
        self.shared.close(CloseReason::Local);
    }

    pub async fn notify(&self, method: &str, params: Value) -> Result<(), McpError> {
        self.shared
            .out
            .send(Message::Notification(Notification::new(method, params)))
            .await
            .map_err(|_| self.shared.closed_err())
    }

    /// Send a request and wait for its result. On timeout or cancel the server gets
    /// `notifications/cancelled` with the request id.
    pub async fn request(
        &self,
        method: &str,
        params: Value,
        opts: RequestOpts,
    ) -> Result<Value, McpError> {
        if self.shared.closed.is_cancelled() {
            return Err(self.shared.closed_err());
        }
        let id = self.shared.next_id.fetch_add(1, Ordering::Relaxed);
        let mut params = params;
        if opts.progress.is_some() {
            if !params.is_object() {
                params = json!({});
            }
            if let Some(obj) = params.as_object_mut() {
                let meta = obj
                    .entry("_meta")
                    .or_insert_with(|| Value::Object(Map::new()));
                if let Value::Object(m) = meta {
                    m.insert("progressToken".into(), json!(id));
                }
            }
        }
        let (tx, rx) = oneshot::channel();
        if let Ok(mut p) = self.shared.pending.lock() {
            p.insert(
                id,
                Pending {
                    tx,
                    progress: opts.progress.clone(),
                },
            );
        }
        let req = Message::Request(Request::new(Id::Num(id), method, params));
        if self.shared.out.send(req).await.is_err() {
            self.forget(id);
            return Err(self.shared.closed_err());
        }
        let cancel = opts.cancel.clone().unwrap_or_default();
        let sleep = async {
            match opts.timeout {
                Some(t) => tokio::time::sleep(t).await,
                None => std::future::pending().await,
            }
        };
        tokio::select! {
            r = rx => match r {
                Ok(Ok(v)) => Ok(v),
                Ok(Err(e)) => Err(McpError::Rpc(e)),
                Err(_) => Err(self.shared.closed_err()),
            },
            () = sleep => {
                self.forget(id);
                self.send_cancel(id, "timeout").await;
                Err(McpError::Timeout {
                    method: method.to_string(),
                    secs: opts.timeout.map_or(0, |t| t.as_secs()),
                })
            }
            () = cancel.cancelled() => {
                self.forget(id);
                self.send_cancel(id, "cancelled by user").await;
                Err(McpError::Cancelled)
            }
        }
    }

    fn forget(&self, id: u64) {
        if let Ok(mut p) = self.shared.pending.lock() {
            p.remove(&id);
        }
    }

    async fn send_cancel(&self, id: u64, reason: &str) {
        let _ = self
            .notify(
                "notifications/cancelled",
                json!({"requestId": id, "reason": reason}),
            )
            .await;
    }

    /// Fetch every page of a `*/list` method and concatenate `key`.
    pub async fn list_all(
        &self,
        method: &str,
        key: &str,
        timeout: Duration,
    ) -> Result<Vec<Value>, McpError> {
        let mut out = Vec::new();
        let mut cursor: Option<String> = None;
        for _ in 0..MAX_PAGES {
            let params = match &cursor {
                Some(c) => json!({"cursor": c}),
                None => json!({}),
            };
            let res = self
                .request(
                    method,
                    params,
                    RequestOpts {
                        timeout: Some(timeout),
                        ..RequestOpts::default()
                    },
                )
                .await?;
            out.extend(
                res.get(key)
                    .and_then(Value::as_array)
                    .cloned()
                    .unwrap_or_default(),
            );
            cursor = res
                .get("nextCursor")
                .and_then(Value::as_str)
                .map(String::from);
            if cursor.is_none() {
                return Ok(out);
            }
        }
        Err(McpError::Protocol(format!("{method}: too many pages")))
    }

    pub async fn ping(&self, timeout: Duration) -> Result<(), McpError> {
        self.request(
            "ping",
            json!({}),
            RequestOpts {
                timeout: Some(timeout),
                ..RequestOpts::default()
            },
        )
        .await
        .map(|_| ())
    }
}

async fn dispatch(
    shared: Arc<Shared>,
    mut rx: mpsc::Receiver<Inbound>,
    ctx: ClientCtx,
    transport_close: CancellationToken,
) {
    loop {
        let inbound = tokio::select! {
            () = shared.closed.cancelled() => break,
            m = rx.recv() => m,
        };
        match inbound {
            None => {
                shared.close(CloseReason::Eof("transport ended".into()));
                break;
            }
            Some(Inbound::Closed(r)) => {
                shared.close(r);
                break;
            }
            Some(Inbound::Message(m)) => handle(&shared, &ctx, m).await,
        }
    }
    transport_close.cancel();
}

async fn handle(shared: &Arc<Shared>, ctx: &ClientCtx, msg: Message) {
    match msg {
        Message::Response(r) => {
            let Id::Num(id) = r.id else { return };
            let p = shared.pending.lock().ok().and_then(|mut p| p.remove(&id));
            if let Some(p) = p {
                let _ = p.tx.send(match r.outcome {
                    Outcome::Result(v) => Ok(v),
                    Outcome::Error(e) => Err(e),
                });
            }
        }
        Message::Notification(n) => on_notification(shared, ctx, n),
        Message::Request(r) => on_request(shared, ctx, r).await,
    }
}

fn on_notification(shared: &Arc<Shared>, ctx: &ClientCtx, n: Notification) {
    let p = &n.params;
    match n.method.as_str() {
        "notifications/progress" => {
            let Some(token) = p.get("progressToken").and_then(Value::as_u64) else {
                return;
            };
            let tx = shared
                .pending
                .lock()
                .ok()
                .and_then(|g| g.get(&token).and_then(|p| p.progress.clone()));
            if let Some(tx) = tx {
                let _ = tx.send(Progress {
                    progress: p.get("progress").and_then(Value::as_f64).unwrap_or(0.0),
                    total: p.get("total").and_then(Value::as_f64),
                    message: p.get("message").and_then(Value::as_str).map(String::from),
                });
            }
        }
        "notifications/cancelled" => {
            let key = p.get("requestId").and_then(value_key);
            if let Some(k) = key
                && let Some(t) = shared.incoming.lock().ok().and_then(|mut i| i.remove(&k))
            {
                t.cancel();
            }
        }
        "notifications/tools/list_changed" => {
            let _ = ctx.notify.send(ServerNotification::ToolsListChanged);
        }
        "notifications/resources/list_changed" => {
            let _ = ctx.notify.send(ServerNotification::ResourcesListChanged);
        }
        "notifications/prompts/list_changed" => {
            let _ = ctx.notify.send(ServerNotification::PromptsListChanged);
        }
        "notifications/resources/updated" => {
            if let Some(uri) = p.get("uri").and_then(Value::as_str) {
                let _ = ctx.notify.send(ServerNotification::ResourceUpdated {
                    uri: uri.to_string(),
                });
            }
        }
        "notifications/message" => {
            let level = p.get("level").and_then(Value::as_str).unwrap_or("info");
            let data = match p.get("data") {
                Some(Value::String(s)) => s.clone(),
                Some(v) => v.to_string(),
                None => String::new(),
            };
            ctx.logs.push(&format!("server:{level}"), &data);
        }
        other => tracing::debug!(server = %ctx.server, method = other, "ignored MCP notification"),
    }
}

async fn reply(shared: &Shared, id: Id, result: Result<Value, RpcError>) {
    let resp = match result {
        Ok(v) => Response::ok(id, v),
        Err(e) => Response::err(id, e),
    };
    let _ = shared.out.send(Message::Response(resp)).await;
}

fn rpc_err(code: i64, message: impl Into<String>) -> RpcError {
    RpcError {
        code,
        message: message.into(),
        data: None,
    }
}

async fn on_request(shared: &Arc<Shared>, ctx: &ClientCtx, r: Request) {
    match r.method.as_str() {
        "ping" => reply(shared, r.id, Ok(json!({}))).await,
        "elicitation/create" | "sampling/createMessage" => {
            let cancel = shared.closed.child_token();
            let key = id_key(&r.id);
            if let Ok(mut i) = shared.incoming.lock() {
                i.insert(key.clone(), cancel.clone());
            }
            let shared = shared.clone();
            let ctx2 = ctx.clone();
            ctx.tasks.spawn(async move {
                let result = if r.method == "elicitation/create" {
                    let req = ElicitRequest::from_params(&r.params);
                    Ok(ctx2
                        .handler
                        .elicit(&ctx2.server, req, cancel.clone())
                        .await
                        .to_result())
                } else {
                    sample(&ctx2, &r.params, cancel.clone()).await
                };
                let still_wanted = shared
                    .incoming
                    .lock()
                    .ok()
                    .and_then(|mut i| i.remove(&key))
                    .is_some();
                // A request the server cancelled gets no response (MCP cancellation rules).
                if still_wanted && !cancel.is_cancelled() {
                    reply(&shared, r.id, result).await;
                }
            });
        }
        // We don't declare `roots`; everything else is unknown.
        _ => {
            reply(
                shared,
                r.id,
                Err(rpc_err(-32601, format!("method not found: {}", r.method))),
            )
            .await;
        }
    }
}

async fn sample(
    ctx: &ClientCtx,
    params: &Value,
    cancel: CancellationToken,
) -> Result<Value, RpcError> {
    let req = SamplingRequest::from_params(params).map_err(|e| rpc_err(-32602, e))?;
    let refuse = |r: SamplingRefusal| {
        ctx.logs.push("sampling", &r.to_string());
        rpc_err(-1, r.to_string())
    };
    if !ctx.sampling_allowed {
        return Err(refuse(SamplingRefusal::PolicyBanned));
    }
    ctx.sampling_budget.check(req.max_tokens).map_err(refuse)?;
    let left = ctx.sampling_budget.left();
    let max = req.max_tokens;
    match ctx.handler.sample(&ctx.server, req, left, cancel).await {
        Ok(resp) => {
            ctx.sampling_budget.charge(if resp.tokens_used == 0 {
                max
            } else {
                resp.tokens_used
            });
            Ok(resp.to_result())
        }
        Err(r) => Err(refuse(r)),
    }
}
