//! Test doubles: an in-process CDP endpoint that replays recorded responses and events,
//! a backend on top of it, and a recording [`BrowserHost`]. Used by this crate's tests
//! and available to the daemon's contract tests (no real browser needed).

use std::collections::{HashMap, VecDeque};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use genie_protocol::api::browser::BrowserStatus;
use genie_protocol::event::BrowserFrame;
use serde_json::{Value, json};
use tokio::sync::mpsc;
use tokio_util::task::TaskTracker;

use crate::backend::{BrowserBackend, BrowserError};
use crate::cdp::{CdpClient, Wire};
use crate::host::{BrowserHost, ConfirmDecision, ConfirmRequest, Control, FrameSink};

/// One recorded reply to a method.
#[derive(Debug, Clone)]
pub struct Reply {
    pub result: Result<Value, (i64, String)>,
    /// Event messages (`{method, params, sessionId?}`) sent right after the response.
    pub events: Vec<Value>,
    /// Never answer (the client times out: a lost result).
    pub drop: bool,
}

impl Reply {
    pub fn ok(v: Value) -> Reply {
        Reply {
            result: Ok(v),
            events: vec![],
            drop: false,
        }
    }

    pub fn with_events(mut self, events: Vec<Value>) -> Reply {
        self.events = events;
        self
    }

    pub fn error(code: i64, message: &str) -> Reply {
        Reply {
            result: Err((code, message.to_string())),
            events: vec![],
            drop: false,
        }
    }

    pub fn dropped() -> Reply {
        Reply {
            result: Ok(Value::Null),
            events: vec![],
            drop: true,
        }
    }
}

/// A call the fake received.
#[derive(Debug, Clone, PartialEq)]
pub struct Call {
    pub method: String,
    pub params: Value,
    pub session_id: Option<String>,
}

#[derive(Default)]
struct State {
    /// Per method: replies in order; the last one repeats.
    replies: HashMap<String, VecDeque<Reply>>,
    calls: Vec<Call>,
}

/// In-process CDP endpoint. Unscripted methods answer `{}`.
#[derive(Clone)]
pub struct FakeCdp {
    state: Arc<Mutex<State>>,
    out: mpsc::UnboundedSender<String>,
}

impl FakeCdp {
    /// The fake and the client-side wire.
    pub fn new(tasks: &TaskTracker) -> (FakeCdp, Wire) {
        let (client, mut server) = Wire::pair();
        let fake = FakeCdp {
            state: Arc::new(Mutex::new(State::default())),
            out: server.tx.clone(),
        };
        let f = fake.clone();
        tasks.spawn(async move {
            while let Some(msg) = server.rx.recv().await {
                f.handle(&msg);
            }
        });
        (fake, client)
    }

    fn handle(&self, msg: &str) {
        let Ok(v) = serde_json::from_str::<Value>(msg) else {
            return;
        };
        let method = v["method"].as_str().unwrap_or("").to_string();
        let session_id = v["sessionId"].as_str().map(String::from);
        let reply = {
            let Ok(mut s) = self.state.lock() else { return };
            s.calls.push(Call {
                method: method.clone(),
                params: v["params"].clone(),
                session_id: session_id.clone(),
            });
            match s.replies.get_mut(&method) {
                Some(q) if q.len() > 1 => q.pop_front(),
                Some(q) => q.front().cloned(),
                None => None,
            }
        }
        .unwrap_or_else(|| Reply::ok(json!({})));
        if reply.drop {
            return;
        }
        let mut resp = json!({"id": v["id"]});
        match reply.result {
            Ok(r) => resp["result"] = r,
            Err((code, message)) => resp["error"] = json!({"code": code, "message": message}),
        }
        if let Some(s) = &session_id {
            resp["sessionId"] = json!(s);
        }
        let _ = self.out.send(resp.to_string());
        for e in reply.events {
            let _ = self.out.send(e.to_string());
        }
    }

    /// Queue a reply for `method`.
    pub fn on(&self, method: &str, reply: Reply) -> &FakeCdp {
        if let Ok(mut s) = self.state.lock() {
            s.replies
                .entry(method.to_string())
                .or_default()
                .push_back(reply);
        }
        self
    }

    pub fn ok(&self, method: &str, result: Value) -> &FakeCdp {
        self.on(method, Reply::ok(result))
    }

    /// Replace every queued reply of `method`.
    pub fn reset(&self, method: &str) {
        if let Ok(mut s) = self.state.lock() {
            s.replies.remove(method);
        }
    }

    /// Load a recording: `[{"method", "result" | "error", "events"?, "drop"?}, …]`.
    pub fn load(&self, recording: &Value) {
        for r in recording.as_array().into_iter().flatten() {
            let method = r["method"].as_str().unwrap_or("");
            let mut reply = match r.get("error") {
                Some(e) => Reply::error(
                    e["code"].as_i64().unwrap_or(-32000),
                    e["message"].as_str().unwrap_or(""),
                ),
                None => Reply::ok(r.get("result").cloned().unwrap_or_else(|| json!({}))),
            };
            reply.events = r["events"].as_array().cloned().unwrap_or_default();
            reply.drop = r["drop"].as_bool().unwrap_or(false);
            self.on(method, reply);
        }
    }

    /// Send an event now.
    pub fn emit(&self, event: Value) {
        let _ = self.out.send(event.to_string());
    }

    pub fn calls(&self) -> Vec<Call> {
        self.state
            .lock()
            .map(|s| s.calls.clone())
            .unwrap_or_default()
    }

    pub fn calls_to(&self, method: &str) -> Vec<Call> {
        self.calls()
            .into_iter()
            .filter(|c| c.method == method)
            .collect()
    }
}

/// A backend whose browser is a [`FakeCdp`].
pub struct FakeBackend {
    pub client: CdpClient,
}

impl FakeBackend {
    pub fn new(client: CdpClient) -> FakeBackend {
        FakeBackend { client }
    }
}

#[async_trait]
impl BrowserBackend for FakeBackend {
    async fn status(&self) -> BrowserStatus {
        BrowserStatus {
            installed: vec![],
            running: !self.client.is_closed(),
            profile_path: String::new(),
        }
    }

    async fn connect(&self) -> Result<CdpClient, BrowserError> {
        if self.client.is_closed() {
            return Err(BrowserError::Launch("fake browser closed".into()));
        }
        Ok(self.client.clone())
    }

    async fn shutdown(&self) {}
}

/// Records everything the browser asks of the daemon; confirmations follow a script
/// (default: approve).
#[derive(Default)]
pub struct RecordingHost {
    pub decisions: Mutex<VecDeque<ConfirmDecision>>,
    pub confirms: Mutex<Vec<ConfirmRequest>>,
    pub taints: Mutex<Vec<(String, String)>>,
    pub images: Mutex<Vec<usize>>,
    pub controls: Mutex<Vec<Control>>,
    pub handoffs: Mutex<Vec<String>>,
    pub user_urls: Mutex<Vec<String>>,
    pub attachments: Mutex<HashMap<String, PathBuf>>,
    pub vision: bool,
}

impl RecordingHost {
    pub fn decide(&self, d: ConfirmDecision) {
        if let Ok(mut g) = self.decisions.lock() {
            g.push_back(d);
        }
    }

    pub fn confirm_actions(&self) -> Vec<String> {
        self.confirms
            .lock()
            .map(|g| g.iter().map(|c| c.confirm.action.clone()).collect())
            .unwrap_or_default()
    }
}

#[async_trait]
impl BrowserHost for RecordingHost {
    async fn confirm(&self, req: ConfirmRequest) -> ConfirmDecision {
        if let Ok(mut g) = self.confirms.lock() {
            g.push(req);
        }
        self.decisions
            .lock()
            .ok()
            .and_then(|mut g| g.pop_front())
            .unwrap_or(ConfirmDecision::Approve)
    }

    fn taint(&self, _session_id: &str, turn_id: &str, origin: &str) {
        if let Ok(mut g) = self.taints.lock() {
            g.push((turn_id.to_string(), origin.to_string()));
        }
    }

    fn vision(&self, _session_id: &str) -> bool {
        self.vision
    }

    fn store_image(&self, _session_id: &str, bytes: &[u8], _mime: &str) -> Option<String> {
        let mut g = self.images.lock().ok()?;
        g.push(bytes.len());
        Some(format!("img{}", g.len()))
    }

    fn attachment_path(&self, _session_id: &str, id: &str) -> Option<PathBuf> {
        self.attachments.lock().ok()?.get(id).cloned()
    }

    fn user_supplied_url(&self, _session_id: &str, url: &str) -> bool {
        self.user_urls
            .lock()
            .map(|g| g.iter().any(|u| u == url))
            .unwrap_or(false)
    }

    fn control_changed(&self, _session_id: &str, c: Control) {
        if let Ok(mut g) = self.controls.lock() {
            g.push(c);
        }
    }

    fn handoff(&self, _session_id: &str, reason: &str) {
        if let Ok(mut g) = self.handoffs.lock() {
            g.push(reason.to_string());
        }
    }
}

/// Collects screencast frames.
#[derive(Default)]
pub struct FrameLog(pub Mutex<Vec<BrowserFrame>>);

impl FrameSink for FrameLog {
    fn frame(&self, frame: BrowserFrame) {
        if let Ok(mut g) = self.0.lock() {
            g.push(frame);
        }
    }
}
