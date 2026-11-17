//! One handshaken connection to the daemon: request/response correlation, incoming
//! notifications and daemon → client requests, EOF detection.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use genie_protocol::handshake::{
    ClientCapabilities, ClientKind, InitializeParams, InitializeResult,
};
use genie_protocol::{
    Id, Message, Notification, Outcome, PROTOCOL_VERSION, Request, Response, RpcError, methods,
};
use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::Value;
use tokio::sync::{mpsc, oneshot};
use tokio_util::sync::CancellationToken;
use tokio_util::task::TaskTracker;

use crate::framing::{self, BoxStream, FrameError};

#[derive(Debug, thiserror::Error)]
pub enum ClientError {
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Rpc(#[from] RpcError),
    #[error("connection closed")]
    Closed,
    #[error("timed out: {0}")]
    Timeout(&'static str),
    #[error("could not start the engine: {0}")]
    Spawn(String),
    #[error("bad response: {0}")]
    Decode(String),
}

/// Who this client is (02 §2).
#[derive(Debug, Clone)]
pub struct ClientInfo {
    pub kind: ClientKind,
    pub version: String,
    pub instance_id: String,
    pub capabilities: ClientCapabilities,
}

/// Messages the daemon sends that are not responses.
#[derive(Debug)]
pub enum Incoming {
    Notification(Notification),
    /// A daemon → client request (e.g. `permission.ask`); answer with [`Connection::respond`].
    Request(Request),
}

type Pending = Arc<Mutex<HashMap<u64, oneshot::Sender<Result<Value, RpcError>>>>>;

pub struct Connection {
    out: mpsc::UnboundedSender<Message>,
    pending: Pending,
    next_id: AtomicU64,
    incoming: Mutex<Option<mpsc::UnboundedReceiver<Incoming>>>,
    closed: CancellationToken,
    tasks: TaskTracker,
    pub init: InitializeResult,
}

/// Handshake timeout for the `initialize` reply.
const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);

impl Connection {
    /// Run the `initialize` handshake over `stream`.
    pub async fn handshake(
        stream: BoxStream,
        info: &ClientInfo,
        token: String,
    ) -> Result<Connection, ClientError> {
        let (mut reader, mut writer) = framing::split(stream);
        let params = InitializeParams {
            protocol: PROTOCOL_VERSION.to_string(),
            client_kind: info.kind,
            client_version: info.version.clone(),
            client_instance_id: info.instance_id.clone(),
            token,
            capabilities: info.capabilities.clone(),
        };
        let req = Request::new(Id::Num(0), methods::INITIALIZE, to_value(&params)?);
        writer.send(&Message::Request(req)).await?;

        let init: InitializeResult = loop {
            let msg = tokio::time::timeout(HANDSHAKE_TIMEOUT, reader.next())
                .await
                .map_err(|_| ClientError::Timeout("handshake"))?;
            match msg {
                Ok(Some(Message::Response(r))) if r.id == Id::Num(0) => match r.outcome {
                    Outcome::Result(v) => break from_value(v)?,
                    Outcome::Error(e) => return Err(ClientError::Rpc(e)),
                },
                Ok(Some(_)) => continue,
                Ok(None) => return Err(ClientError::Closed),
                Err(FrameError::Io(e)) => return Err(ClientError::Io(e)),
                Err(e) => return Err(ClientError::Decode(e.to_string())),
            }
        };

        let (out_tx, mut out_rx) = mpsc::unbounded_channel::<Message>();
        let (in_tx, in_rx) = mpsc::unbounded_channel();
        let pending: Pending = Arc::default();
        let closed = CancellationToken::new();
        let tasks = TaskTracker::new();

        // Writer task.
        let c = closed.clone();
        tasks.spawn(async move {
            loop {
                tokio::select! {
                    m = out_rx.recv() => match m {
                        Some(m) => if writer.send(&m).await.is_err() { break },
                        None => break,
                    },
                    () = c.cancelled() => break,
                }
            }
            let _ = writer.shutdown().await;
            c.cancel();
        });

        // Reader task: routes responses to waiters, everything else to `incoming`.
        let c = closed.clone();
        let p = pending.clone();
        tasks.spawn(async move {
            loop {
                let msg = tokio::select! {
                    m = reader.next() => m,
                    () = c.cancelled() => break,
                };
                match msg {
                    Ok(Some(Message::Response(r))) => {
                        let Id::Num(id) = r.id else { continue };
                        let waiter = p.lock().unwrap_or_else(|e| e.into_inner()).remove(&id);
                        if let Some(w) = waiter {
                            let _ = w.send(match r.outcome {
                                Outcome::Result(v) => Ok(v),
                                Outcome::Error(e) => Err(e),
                            });
                        }
                    }
                    Ok(Some(Message::Notification(n))) => {
                        let _ = in_tx.send(Incoming::Notification(n));
                    }
                    Ok(Some(Message::Request(r))) => {
                        let _ = in_tx.send(Incoming::Request(r));
                    }
                    Err(FrameError::Bad(e)) => tracing::warn!(error = %e, "bad line from daemon"),
                    Ok(None) | Err(_) => break,
                }
            }
            c.cancel();
            // Fail everything still waiting.
            p.lock().unwrap_or_else(|e| e.into_inner()).clear();
        });
        tasks.close();

        Ok(Connection {
            out: out_tx,
            pending,
            next_id: AtomicU64::new(1),
            incoming: Mutex::new(Some(in_rx)),
            closed,
            tasks,
            init,
        })
    }

    /// Send a request and wait for its result.
    pub async fn request<P: Serialize, R: DeserializeOwned>(
        &self,
        method: &str,
        params: P,
    ) -> Result<R, ClientError> {
        let v = self.request_value(method, to_value(&params)?).await?;
        from_value(v)
    }

    pub async fn request_value(&self, method: &str, params: Value) -> Result<Value, ClientError> {
        if self.closed.is_cancelled() {
            return Err(ClientError::Closed);
        }
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = oneshot::channel();
        self.pending
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(id, tx);
        let params = if params.is_null() {
            Value::Null
        } else {
            params
        };
        if self
            .out
            .send(Message::Request(Request::new(Id::Num(id), method, params)))
            .is_err()
        {
            return Err(ClientError::Closed);
        }
        match rx.await {
            Ok(Ok(v)) => Ok(v),
            Ok(Err(e)) => Err(ClientError::Rpc(e)),
            Err(_) => Err(ClientError::Closed),
        }
    }

    pub fn notify(&self, method: &str, params: Value) -> Result<(), ClientError> {
        self.out
            .send(Message::Notification(Notification::new(method, params)))
            .map_err(|_| ClientError::Closed)
    }

    /// Answer a daemon → client request.
    pub fn respond(&self, id: Id, result: Result<Value, RpcError>) -> Result<(), ClientError> {
        let resp = match result {
            Ok(v) => Response::ok(id, v),
            Err(e) => Response::err(id, e),
        };
        self.out
            .send(Message::Response(resp))
            .map_err(|_| ClientError::Closed)
    }

    /// Take the incoming stream (once).
    pub fn take_incoming(&self) -> Option<mpsc::UnboundedReceiver<Incoming>> {
        self.incoming
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .take()
    }

    /// Resolves on EOF / broken pipe: the caller re-runs `ensure` and re-subscribes with
    /// its last `seq` (01 §4.1 Ready → Reconnecting).
    pub async fn closed(&self) {
        self.closed.cancelled().await;
    }

    pub fn is_closed(&self) -> bool {
        self.closed.is_cancelled()
    }

    pub async fn close(self) {
        self.closed.cancel();
        self.tasks.wait().await;
    }
}

impl std::fmt::Debug for Connection {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Connection")
            .field("engine_version", &self.init.engine_version)
            .field("closed", &self.is_closed())
            .finish_non_exhaustive()
    }
}

impl Drop for Connection {
    fn drop(&mut self) {
        self.closed.cancel();
    }
}

fn to_value<T: Serialize>(v: &T) -> Result<Value, ClientError> {
    serde_json::to_value(v).map_err(|e| ClientError::Decode(e.to_string()))
}

fn from_value<T: DeserializeOwned>(v: Value) -> Result<T, ClientError> {
    serde_json::from_value(v).map_err(|e| ClientError::Decode(e.to_string()))
}
