//! Our own thin async CDP client (06 §10, research/browser.md §3).
//!
//! The client speaks JSON messages over a [`Wire`]: two channels of whole messages, fed
//! by the `--remote-debugging-pipe` bridge ([`crate::transport`]) or by the in-process
//! fake ([`crate::testing::FakeCdp`]). One reader task (on the caller's `TaskTracker`)
//! routes responses to their waiting request by `id` and fans events out to subscribers.
//! Sessions are flattened (`Target.attachToTarget {flatten: true}`): a tab's commands and
//! events carry its `sessionId`.
//!
//! A request whose response doesn't come back within its timeout, or whose connection
//! closes first, fails with [`CdpError::Timeout`] / [`CdpError::Closed`]: callers report
//! the action as `unknown` and never retry it (research §3).

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::{Value, json};
use tokio::sync::{broadcast, mpsc, oneshot};
use tokio_util::task::TaskTracker;

/// Default per-request timeout.
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(30);

/// Capacity of the event fan-out. Slow subscribers lose the oldest events
/// (`RecvError::Lagged`); the screencast tolerates that by design.
const EVENT_CAPACITY: usize = 1024;

/// Whole CDP messages in both directions.
pub struct Wire {
    pub tx: mpsc::UnboundedSender<String>,
    pub rx: mpsc::UnboundedReceiver<String>,
}

impl Wire {
    /// A connected pair: what one side sends, the other receives.
    pub fn pair() -> (Wire, Wire) {
        let (a_tx, a_rx) = mpsc::unbounded_channel();
        let (b_tx, b_rx) = mpsc::unbounded_channel();
        (Wire { tx: a_tx, rx: b_rx }, Wire { tx: b_tx, rx: a_rx })
    }
}

#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum CdpError {
    /// No response in time. The command may still have run.
    #[error("`{method}` timed out after {after_ms} ms")]
    Timeout { method: String, after_ms: u64 },
    /// The connection closed before the response. The command may have run.
    #[error("browser connection closed")]
    Closed,
    /// The browser answered with an error. The command did not run.
    #[error("CDP error {code} on `{method}`: {message}")]
    Protocol {
        method: String,
        code: i64,
        message: String,
    },
    #[error("unexpected CDP response to `{method}`: {detail}")]
    Decode { method: String, detail: String },
}

impl CdpError {
    /// The command may have taken effect: report `unknown`, never retry.
    pub fn result_lost(&self) -> bool {
        matches!(self, CdpError::Timeout { .. } | CdpError::Closed)
    }
}

/// An event from the browser.
#[derive(Debug, Clone, PartialEq)]
pub struct CdpEvent {
    pub method: String,
    pub params: Value,
    /// Flattened session (tab) id; `None` for browser-level events.
    pub session_id: Option<String>,
}

type Pending = HashMap<u64, (String, oneshot::Sender<Result<Value, CdpError>>)>;

struct Inner {
    tx: mpsc::UnboundedSender<String>,
    pending: Mutex<Pending>,
    next_id: AtomicU64,
    events: broadcast::Sender<CdpEvent>,
    closed: AtomicBool,
    timeout: Duration,
}

/// A CDP connection. Cheap to clone.
#[derive(Clone)]
pub struct CdpClient {
    inner: Arc<Inner>,
}

impl CdpClient {
    /// Start the reader on `tasks`. The client is closed when the wire's receiver ends.
    pub fn connect(wire: Wire, tasks: &TaskTracker) -> CdpClient {
        Self::connect_with_timeout(wire, tasks, DEFAULT_TIMEOUT)
    }

    pub fn connect_with_timeout(wire: Wire, tasks: &TaskTracker, timeout: Duration) -> CdpClient {
        let (events, _) = broadcast::channel(EVENT_CAPACITY);
        let inner = Arc::new(Inner {
            tx: wire.tx,
            pending: Mutex::new(HashMap::new()),
            next_id: AtomicU64::new(1),
            events,
            closed: AtomicBool::new(false),
            timeout,
        });
        let reader = inner.clone();
        let mut rx = wire.rx;
        tasks.spawn(async move {
            while let Some(msg) = rx.recv().await {
                reader.route(&msg);
            }
            reader.close();
        });
        CdpClient { inner }
    }

    /// Same underlying connection (a relaunched browser is a new one).
    pub fn same_connection(&self, other: &CdpClient) -> bool {
        Arc::ptr_eq(&self.inner, &other.inner)
    }

    pub fn is_closed(&self) -> bool {
        self.inner.closed.load(Ordering::SeqCst)
    }

    /// Subscribe to every event from now on.
    pub fn subscribe(&self) -> broadcast::Receiver<CdpEvent> {
        self.inner.events.subscribe()
    }

    /// A browser-level command.
    pub async fn call(&self, method: &str, params: Value) -> Result<Value, CdpError> {
        self.call_in(None, method, params).await
    }

    /// A command, optionally inside a flattened session.
    pub async fn call_in(
        &self,
        session: Option<&str>,
        method: &str,
        params: Value,
    ) -> Result<Value, CdpError> {
        self.call_timeout(session, method, params, self.inner.timeout)
            .await
    }

    pub async fn call_timeout(
        &self,
        session: Option<&str>,
        method: &str,
        params: Value,
        timeout: Duration,
    ) -> Result<Value, CdpError> {
        if self.is_closed() {
            return Err(CdpError::Closed);
        }
        let id = self.inner.next_id.fetch_add(1, Ordering::SeqCst);
        let mut msg = json!({ "id": id, "method": method, "params": params });
        if let Some(s) = session {
            msg["sessionId"] = Value::String(s.to_string());
        }
        let (tx, rx) = oneshot::channel();
        if let Ok(mut p) = self.inner.pending.lock() {
            p.insert(id, (method.to_string(), tx));
        }
        if self.inner.tx.send(msg.to_string()).is_err() {
            self.inner.take(id);
            return Err(CdpError::Closed);
        }
        match tokio::time::timeout(timeout, rx).await {
            Ok(Ok(r)) => r,
            Ok(Err(_)) => Err(CdpError::Closed),
            Err(_) => {
                self.inner.take(id);
                Err(CdpError::Timeout {
                    method: method.to_string(),
                    after_ms: u64::try_from(timeout.as_millis()).unwrap_or(u64::MAX),
                })
            }
        }
    }

    /// Fire and forget (acks, input during take-over). Errors are logged.
    pub fn send(&self, session: Option<&str>, method: &str, params: Value) {
        let id = self.inner.next_id.fetch_add(1, Ordering::SeqCst);
        let mut msg = json!({ "id": id, "method": method, "params": params });
        if let Some(s) = session {
            msg["sessionId"] = Value::String(s.to_string());
        }
        if self.inner.tx.send(msg.to_string()).is_err() {
            tracing::debug!(method, "CDP send on a closed connection");
        }
    }
}

impl Inner {
    fn take(&self, id: u64) -> Option<(String, oneshot::Sender<Result<Value, CdpError>>)> {
        self.pending.lock().ok().and_then(|mut p| p.remove(&id))
    }

    fn route(&self, msg: &str) {
        let Ok(v) = serde_json::from_str::<Value>(msg) else {
            tracing::warn!(len = msg.len(), "undecodable CDP message");
            return;
        };
        if let Some(id) = v.get("id").and_then(Value::as_u64) {
            // Responses to fire-and-forget sends have no waiter.
            let Some((method, tx)) = self.take(id) else {
                if let Some(e) = v.get("error") {
                    tracing::debug!(id, error = %e, "CDP error on a fire-and-forget command");
                }
                return;
            };
            let r = if let Some(e) = v.get("error") {
                Err(CdpError::Protocol {
                    method,
                    code: e.get("code").and_then(Value::as_i64).unwrap_or(0),
                    message: e
                        .get("message")
                        .and_then(Value::as_str)
                        .unwrap_or("")
                        .to_string(),
                })
            } else {
                Ok(v.get("result").cloned().unwrap_or(Value::Null))
            };
            let _ = tx.send(r);
        } else if let Some(method) = v.get("method").and_then(Value::as_str) {
            let _ = self.events.send(CdpEvent {
                method: method.to_string(),
                params: v.get("params").cloned().unwrap_or(Value::Null),
                session_id: v.get("sessionId").and_then(Value::as_str).map(String::from),
            });
        }
    }

    fn close(&self) {
        self.closed.store(true, Ordering::SeqCst);
        let drained: Vec<_> = self
            .pending
            .lock()
            .map(|mut p| p.drain().collect())
            .unwrap_or_default();
        for (_, (_, tx)) in drained {
            let _ = tx.send(Err(CdpError::Closed));
        }
    }
}

/// Wait for the first event matching `pred`, up to `timeout`. Subscribe *before* sending
/// the command that triggers it.
pub async fn wait_event(
    rx: &mut broadcast::Receiver<CdpEvent>,
    timeout: Duration,
    mut pred: impl FnMut(&CdpEvent) -> bool,
) -> Option<CdpEvent> {
    let fut = async {
        loop {
            match rx.recv().await {
                Ok(e) if pred(&e) => return Some(e),
                Ok(_) | Err(broadcast::error::RecvError::Lagged(_)) => {}
                Err(broadcast::error::RecvError::Closed) => return None,
            }
        }
    };
    tokio::time::timeout(timeout, fut).await.ok().flatten()
}

/// Typed accessors on CDP results.
pub(crate) fn str_at<'a>(v: &'a Value, path: &[&str]) -> Option<&'a str> {
    let mut cur = v;
    for p in path {
        cur = cur.get(p)?;
    }
    cur.as_str()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn routes_responses_and_events() {
        let tasks = TaskTracker::new();
        let (ours, mut theirs) = Wire::pair();
        let client = CdpClient::connect(ours, &tasks);
        let mut events = client.subscribe();
        let c2 = client.clone();
        let call = tokio::spawn(async move {
            c2.call_in(Some("S1"), "Page.navigate", json!({"url": "x"}))
                .await
        });
        let req: Value = serde_json::from_str(&theirs.rx.recv().await.unwrap()).unwrap();
        assert_eq!(req["method"], "Page.navigate");
        assert_eq!(req["sessionId"], "S1");
        theirs
            .tx
            .send(
                json!({"method": "Page.loadEventFired", "params": {}, "sessionId": "S1"})
                    .to_string(),
            )
            .unwrap();
        theirs
            .tx
            .send(json!({"id": req["id"], "result": {"frameId": "F"}}).to_string())
            .unwrap();
        assert_eq!(call.await.unwrap().unwrap()["frameId"], "F");
        let e = events.recv().await.unwrap();
        assert_eq!(e.method, "Page.loadEventFired");
        assert_eq!(e.session_id.as_deref(), Some("S1"));
    }

    #[tokio::test]
    async fn errors_timeouts_and_close() {
        let tasks = TaskTracker::new();
        let (ours, mut theirs) = Wire::pair();
        let client = CdpClient::connect_with_timeout(ours, &tasks, Duration::from_millis(50));
        let c2 = client.clone();
        let call = tokio::spawn(async move { c2.call("DOM.focus", json!({})).await });
        let req: Value = serde_json::from_str(&theirs.rx.recv().await.unwrap()).unwrap();
        theirs
            .tx
            .send(
                json!({"id": req["id"], "error": {"code": -32000, "message": "no node"}})
                    .to_string(),
            )
            .unwrap();
        let err = call.await.unwrap().unwrap_err();
        assert!(matches!(err, CdpError::Protocol { code: -32000, .. }));
        assert!(!err.result_lost());

        let err = client.call("Page.reload", json!({})).await.unwrap_err();
        assert!(matches!(err, CdpError::Timeout { .. }));
        assert!(err.result_lost());

        let c3 = client.clone();
        let call = tokio::spawn(async move {
            c3.call_timeout(None, "X.y", json!({}), Duration::from_secs(5))
                .await
        });
        let _ = theirs.rx.recv().await; // the timed-out one
        let _ = theirs.rx.recv().await;
        drop(theirs);
        assert_eq!(call.await.unwrap().unwrap_err(), CdpError::Closed);
        assert!(client.is_closed());
    }
}
