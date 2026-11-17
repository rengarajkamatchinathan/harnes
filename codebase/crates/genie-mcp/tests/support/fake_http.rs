//! In-process Streamable HTTP server behind the `HttpClient` port (no network).

use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use bytes::Bytes;
use futures::StreamExt;
use genie_mcp::transport::http::{HttpClient, HttpMethod, HttpRequest, HttpResponse};
use serde_json::Value;
use tokio::sync::mpsc;

use super::fake_server::{Fake, Options};

pub struct FakeHttp {
    pub fake: Mutex<Arc<Fake>>,
    pub opts: Options,
    /// Required bearer token, if any.
    pub token: Mutex<Option<String>>,
    pub sessions: AtomicU32,
    /// Answer the next session-bearing request with 404 (session expired).
    pub expire: AtomicBool,
    pub deletes: AtomicU32,
    /// Answer requests with JSON bodies instead of SSE streams.
    pub json_mode: bool,
    pub seen_protocol_header: Mutex<Option<String>>,
}

impl FakeHttp {
    pub fn new(opts: Options, json_mode: bool) -> Arc<FakeHttp> {
        Arc::new(FakeHttp {
            fake: Mutex::new(Fake::new(opts.clone())),
            opts,
            token: Mutex::new(None),
            sessions: AtomicU32::new(0),
            expire: AtomicBool::new(false),
            deletes: AtomicU32::new(0),
            json_mode,
            seen_protocol_header: Mutex::new(None),
        })
    }
}

fn resp(status: u16, headers: Vec<(&str, String)>, body: Vec<u8>) -> HttpResponse {
    HttpResponse {
        status,
        headers: headers
            .into_iter()
            .map(|(k, v)| (k.to_string(), v))
            .collect(),
        body: futures::stream::once(async move { Ok(Bytes::from(body)) }).boxed(),
    }
}

#[async_trait]
impl HttpClient for FakeHttp {
    async fn send(&self, req: HttpRequest) -> Result<HttpResponse, String> {
        if let Some(t) = self.token.lock().unwrap().clone()
            && req.header("authorization") != Some(&format!("Bearer {t}"))
        {
            return Ok(resp(
                401,
                vec![(
                    "WWW-Authenticate",
                    "Bearer resource_metadata=\"https://x/.well-known\"".into(),
                )],
                vec![],
            ));
        }
        match req.method {
            HttpMethod::Get => return Ok(resp(405, vec![], vec![])),
            HttpMethod::Delete => {
                self.deletes.fetch_add(1, Ordering::SeqCst);
                return Ok(resp(200, vec![], vec![]));
            }
            HttpMethod::Post => {}
        }
        let msg: Value = serde_json::from_slice(req.body.as_deref().unwrap_or_default())
            .map_err(|e| e.to_string())?;
        let is_init = msg["method"] == "initialize";
        if is_init {
            // New session: fresh server state.
            *self.fake.lock().unwrap() = Fake::new(self.opts.clone());
            self.sessions.fetch_add(1, Ordering::SeqCst);
        } else {
            if req.header("mcp-session-id").is_none() {
                return Ok(resp(400, vec![], b"missing session".to_vec()));
            }
            *self.seen_protocol_header.lock().unwrap() =
                req.header("mcp-protocol-version").map(String::from);
            if self.expire.swap(false, Ordering::SeqCst) {
                return Ok(resp(404, vec![], vec![]));
            }
        }
        let fake = self.fake.lock().unwrap().clone();
        let is_request = msg.get("method").is_some() && msg.get("id").is_some();
        let (tx, mut rx) = mpsc::unbounded_channel::<Value>();
        let id = msg.get("id").cloned();
        fake.on_message(msg, tx);
        let session = vec![("Mcp-Session-Id", "sess-1".to_string())];
        if !is_request {
            return Ok(resp(202, session, vec![]));
        }
        if self.json_mode {
            // Wait for the response; drop anything else.
            while let Some(m) = rx.recv().await {
                if m.get("id") == id.as_ref() && m.get("method").is_none() {
                    let mut h = session;
                    h.push(("Content-Type", "application/json".into()));
                    return Ok(resp(200, h, m.to_string().into_bytes()));
                }
            }
            return Ok(resp(500, vec![], b"no response".to_vec()));
        }
        let body = async_stream_body(rx, id);
        let mut h = session;
        h.push(("Content-Type", "text/event-stream".into()));
        Ok(HttpResponse {
            status: 200,
            headers: h.into_iter().map(|(k, v)| (k.to_string(), v)).collect(),
            body,
        })
    }
}

/// SSE body: every message the fake emits for this request, ending after its response.
fn async_stream_body(
    rx: mpsc::UnboundedReceiver<Value>,
    id: Option<Value>,
) -> futures::stream::BoxStream<'static, Result<Bytes, String>> {
    futures::stream::unfold((rx, false), move |(mut rx, done)| {
        let id = id.clone();
        async move {
            if done {
                return None;
            }
            let m = rx.recv().await?;
            let last = m.get("id") == id.as_ref() && m.get("method").is_none();
            let chunk = Bytes::from(format!("event: message\ndata: {m}\n\n"));
            Some((Ok(chunk), (rx, last)))
        }
    })
    .boxed()
}
