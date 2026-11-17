//! Streamable HTTP transport (MCP 2025-06-18 "Streamable HTTP").
//!
//! * Every client message is a `POST` to the endpoint with
//!   `Accept: application/json, text/event-stream`. Notifications and responses expect
//!   `202`; requests get either one JSON body or an SSE stream carrying notifications,
//!   server requests and finally the response.
//! * `Mcp-Session-Id` from the initialize response is sent on every later request;
//!   `MCP-Protocol-Version` after the version is negotiated. `404` with a session id means
//!   the session expired → the transport closes with [`CloseReason::SessionExpired`].
//! * After `notifications/initialized` a `GET` opens the optional server→client SSE stream
//!   (`405` = not offered).
//! * `401` asks the [`AuthProvider`] for a refreshed token once (refresh only if the stored
//!   token is unchanged, 06 §7); otherwise the transport closes with `Unauthorized` and the
//!   server goes to `needs_auth`.
//! * Closing sends `DELETE` with the session id.
//!
//! HTTP itself is behind the [`HttpClient`] port: [`ReqwestClient`] in production, an
//! in-process fake in tests (no network).

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use bytes::Bytes;
use futures::StreamExt;
use futures::stream::BoxStream;
use genie_protocol::jsonrpc::Outcome;
use genie_protocol::{Id, Message, Response, RpcError};
use genie_secrets::{ExposeSecret, SecretString};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

use super::sse::SseParser;
use super::{CHANNEL, CloseReason, ConnectEnv, Inbound, TransportPair};
use crate::auth::AuthKey;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HttpMethod {
    Get,
    Post,
    Delete,
}

/// An HTTP request. Header values may carry secrets: there is deliberately no `Debug`.
pub struct HttpRequest {
    pub method: HttpMethod,
    pub url: String,
    pub headers: Vec<(String, String)>,
    pub body: Option<Vec<u8>>,
}

impl HttpRequest {
    pub fn header(&self, name: &str) -> Option<&str> {
        find_header(&self.headers, name)
    }
}

pub struct HttpResponse {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub body: BoxStream<'static, Result<Bytes, String>>,
}

impl HttpResponse {
    pub fn header(&self, name: &str) -> Option<&str> {
        find_header(&self.headers, name)
    }
}

fn find_header<'a>(headers: &'a [(String, String)], name: &str) -> Option<&'a str> {
    headers
        .iter()
        .find(|(k, _)| k.eq_ignore_ascii_case(name))
        .map(|(_, v)| v.as_str())
}

/// HTTP port. Errors are connection-level failures (DNS, refused, TLS); HTTP statuses are
/// responses.
#[async_trait]
pub trait HttpClient: Send + Sync {
    async fn send(&self, req: HttpRequest) -> Result<HttpResponse, String>;
}

/// `reqwest` (rustls) implementation.
pub struct ReqwestClient {
    client: reqwest::Client,
}

impl ReqwestClient {
    pub fn new() -> Result<ReqwestClient, String> {
        let client = reqwest::Client::builder()
            .connect_timeout(std::time::Duration::from_secs(30))
            .build()
            .map_err(|e| e.to_string())?;
        Ok(ReqwestClient { client })
    }
}

#[async_trait]
impl HttpClient for ReqwestClient {
    async fn send(&self, req: HttpRequest) -> Result<HttpResponse, String> {
        let mut b = match req.method {
            HttpMethod::Get => self.client.get(&req.url),
            HttpMethod::Post => self.client.post(&req.url),
            HttpMethod::Delete => self.client.delete(&req.url),
        };
        for (k, v) in &req.headers {
            b = b.header(k, v);
        }
        if let Some(body) = req.body {
            b = b.body(body);
        }
        let resp = b.send().await.map_err(|e| e.without_url().to_string())?;
        let status = resp.status().as_u16();
        let headers = resp
            .headers()
            .iter()
            .filter_map(|(k, v)| Some((k.as_str().to_string(), v.to_str().ok()?.to_string())))
            .collect();
        let body = resp
            .bytes_stream()
            .map(|r| r.map_err(|e| e.without_url().to_string()))
            .boxed();
        Ok(HttpResponse {
            status,
            headers,
            body,
        })
    }
}

/// Largest JSON (non-SSE) response body accepted.
const MAX_JSON_BODY: usize = 16 * 1024 * 1024;

struct State {
    session_id: Option<String>,
    protocol_version: Option<String>,
    init_request: Option<Id>,
    token: Option<SecretString>,
    get_stream_opened: bool,
}

struct Http {
    client: Arc<dyn HttpClient>,
    url: String,
    /// Resolved static headers from the config (may hold secrets).
    headers: Vec<(String, String)>,
    key: AuthKey,
    env: ConnectEnv,
    state: Mutex<State>,
    inbound: mpsc::Sender<Inbound>,
    close: CancellationToken,
}

/// Start a Streamable HTTP transport. Header values go through the secret resolver; a
/// resolution failure closes the transport with that error.
pub fn connect(
    client: Arc<dyn HttpClient>,
    url: &str,
    headers: &BTreeMap<String, String>,
    env: &ConnectEnv,
) -> TransportPair {
    let (out_tx, mut out_rx) = mpsc::channel::<Message>(CHANNEL);
    let (in_tx, in_rx) = mpsc::channel::<Inbound>(CHANNEL);
    let close = env.parent.child_token();
    let mut resolved = Vec::new();
    let mut bad = None;
    for (k, v) in headers {
        match env.secrets.resolve(v) {
            Ok(s) => resolved.push((k.clone(), s.expose_secret().to_string())),
            Err(e) => bad = Some(format!("header `{k}`: {e}")),
        }
    }
    let http = Arc::new(Http {
        client,
        url: url.to_string(),
        headers: resolved,
        key: env.auth_key(url),
        env: env.clone(),
        state: Mutex::new(State {
            session_id: None,
            protocol_version: None,
            init_request: None,
            token: None,
            get_stream_opened: false,
        }),
        inbound: in_tx,
        close: close.clone(),
    });
    let h = http.clone();
    env.tasks.spawn(async move {
        if let Some(e) = bad {
            let _ = h.inbound.send(Inbound::Closed(CloseReason::Error(e))).await;
            return;
        }
        let token = h.env.auth.token(&h.key).await;
        if let Ok(mut s) = h.state.lock() {
            s.token = token;
        }
        loop {
            let msg = tokio::select! {
                () = h.close.cancelled() => break,
                m = out_rx.recv() => match m { Some(m) => m, None => break },
            };
            let is_request = matches!(msg, Message::Request(_));
            let initialized = h.state.lock().map(|s| s.protocol_version.is_some()).unwrap_or(false);
            if is_request && initialized {
                // Requests run concurrently so a long tool call doesn't hold back
                // cancellations or other calls.
                let h2 = h.clone();
                h.env.tasks.spawn(async move { h2.post(msg).await });
            } else {
                let opens_stream = matches!(&msg, Message::Notification(n) if n.method == "notifications/initialized");
                h.post(msg).await;
                if opens_stream {
                    let h2 = h.clone();
                    h.env.tasks.spawn(async move { h2.get_stream().await });
                }
            }
        }
        h.delete_session().await;
    });
    TransportPair {
        tx: out_tx,
        rx: in_rx,
        close,
    }
}

impl Http {
    fn base_headers(&self, accept: &str) -> Vec<(String, String)> {
        let mut h = self.headers.clone();
        h.push(("Accept".into(), accept.into()));
        let Ok(s) = self.state.lock() else { return h };
        if let Some(id) = &s.session_id {
            h.push(("Mcp-Session-Id".into(), id.clone()));
        }
        if let Some(v) = &s.protocol_version {
            h.push(("MCP-Protocol-Version".into(), v.clone()));
        }
        if find_header(&self.headers, "authorization").is_none()
            && let Some(t) = &s.token
        {
            h.push((
                "Authorization".into(),
                format!("Bearer {}", t.expose_secret()),
            ));
        }
        h
    }

    async fn closed(&self, reason: CloseReason) {
        let _ = self.inbound.send(Inbound::Closed(reason)).await;
        self.close.cancel();
    }

    /// Answer a failed request locally so the caller gets an error instead of a hang.
    async fn fail_request(&self, msg: &Message, text: String) {
        if let Message::Request(r) = msg {
            let resp = Response::err(
                r.id.clone(),
                RpcError {
                    code: -32603,
                    message: text,
                    data: None,
                },
            );
            let _ = self
                .inbound
                .send(Inbound::Message(Message::Response(resp)))
                .await;
        } else {
            self.env.logs.push("http", &text);
        }
    }

    async fn post(&self, msg: Message) {
        if let (Message::Request(r), Ok(mut s)) = (&msg, self.state.lock())
            && r.method == "initialize"
        {
            s.init_request = Some(r.id.clone());
        }
        let body = msg.to_line().into_bytes();
        let mut refreshed = false;
        loop {
            let mut headers = self.base_headers("application/json, text/event-stream");
            headers.push(("Content-Type".into(), "application/json".into()));
            let req = HttpRequest {
                method: HttpMethod::Post,
                url: self.url.clone(),
                headers,
                body: Some(body.clone()),
            };
            let resp = match self.client.send(req).await {
                Ok(r) => r,
                Err(e) => {
                    self.closed(CloseReason::Error(format!("HTTP: {e}"))).await;
                    return;
                }
            };
            if let Some(sid) = resp.header("mcp-session-id")
                && let Ok(mut s) = self.state.lock()
            {
                s.session_id = Some(sid.to_string());
            }
            match resp.status {
                401 if !refreshed => {
                    refreshed = true;
                    let challenge = resp.header("www-authenticate").map(String::from);
                    let current = self.state.lock().ok().and_then(|s| s.token.clone());
                    match self
                        .env
                        .auth
                        .refresh(&self.key, current.as_ref(), challenge.as_deref())
                        .await
                    {
                        Some(t) => {
                            if let Ok(mut s) = self.state.lock() {
                                s.token = Some(t);
                            }
                            continue;
                        }
                        None => {
                            self.closed(CloseReason::Unauthorized {
                                www_authenticate: challenge,
                            })
                            .await;
                            return;
                        }
                    }
                }
                401 => {
                    let challenge = resp.header("www-authenticate").map(String::from);
                    self.closed(CloseReason::Unauthorized {
                        www_authenticate: challenge,
                    })
                    .await;
                    return;
                }
                404 if self
                    .state
                    .lock()
                    .map(|s| s.session_id.is_some())
                    .unwrap_or(false) =>
                {
                    self.closed(CloseReason::SessionExpired).await;
                    return;
                }
                202 | 204 => return,
                s if (200..300).contains(&s) => {
                    self.read_body(resp).await;
                    return;
                }
                s => {
                    let text = read_text(resp.body, 2048).await;
                    self.fail_request(&msg, format!("HTTP {s}: {text}")).await;
                    return;
                }
            }
        }
    }

    async fn read_body(&self, resp: HttpResponse) {
        let ct = resp
            .header("content-type")
            .unwrap_or("")
            .to_ascii_lowercase();
        if ct.starts_with("text/event-stream") {
            self.read_sse(resp.body).await;
        } else {
            let mut body = resp.body;
            let mut buf = Vec::new();
            while let Some(chunk) = body.next().await {
                match chunk {
                    Ok(b) => {
                        buf.extend_from_slice(&b);
                        if buf.len() > MAX_JSON_BODY {
                            self.env.logs.push("http", "response body exceeds 16 MiB");
                            return;
                        }
                    }
                    Err(e) => {
                        self.env.logs.push("http", &format!("reading body: {e}"));
                        return;
                    }
                }
            }
            if buf.iter().all(u8::is_ascii_whitespace) {
                return;
            }
            self.deliver_json(&buf).await;
        }
    }

    async fn deliver_json(&self, bytes: &[u8]) {
        // A body may be one message or a batch array.
        let text = String::from_utf8_lossy(bytes);
        match serde_json::from_str::<serde_json::Value>(&text) {
            Ok(serde_json::Value::Array(items)) => {
                for item in items {
                    self.deliver_line(&item.to_string()).await;
                }
            }
            Ok(_) => self.deliver_line(text.trim()).await,
            Err(e) => self.env.logs.push("http", &format!("bad JSON body: {e}")),
        }
    }

    async fn deliver_line(&self, line: &str) {
        match Message::parse(line) {
            Ok(m) => {
                self.observe(&m);
                let _ = self.inbound.send(Inbound::Message(m)).await;
            }
            Err(e) => self
                .env
                .logs
                .push("http", &format!("bad message: {}", e.message)),
        }
    }

    /// Learn the negotiated protocol version from the initialize response.
    fn observe(&self, m: &Message) {
        let Message::Response(r) = m else { return };
        let Ok(mut s) = self.state.lock() else { return };
        if s.init_request.as_ref() == Some(&r.id)
            && let Outcome::Result(v) = &r.outcome
        {
            s.protocol_version = v
                .get("protocolVersion")
                .and_then(|p| p.as_str())
                .map(String::from);
            s.init_request = None;
        }
    }

    async fn read_sse(&self, mut body: BoxStream<'static, Result<Bytes, String>>) {
        let mut parser = SseParser::default();
        loop {
            let chunk = tokio::select! {
                () = self.close.cancelled() => return,
                c = body.next() => c,
            };
            match chunk {
                None => return,
                Some(Err(e)) => {
                    self.env
                        .logs
                        .push("http", &format!("SSE stream error: {e}"));
                    return;
                }
                Some(Ok(b)) => match parser.push(&b) {
                    Ok(events) => {
                        for ev in events {
                            if ev.event.as_deref().is_none_or(|e| e == "message") {
                                self.deliver_json(ev.data.as_bytes()).await;
                            }
                        }
                    }
                    Err(e) => {
                        self.env.logs.push("http", &e);
                        return;
                    }
                },
            }
        }
    }

    async fn get_stream(&self) {
        {
            let Ok(mut s) = self.state.lock() else { return };
            if s.get_stream_opened {
                return;
            }
            s.get_stream_opened = true;
        }
        let req = HttpRequest {
            method: HttpMethod::Get,
            url: self.url.clone(),
            headers: self.base_headers("text/event-stream"),
            body: None,
        };
        let resp = tokio::select! {
            () = self.close.cancelled() => return,
            r = self.client.send(req) => r,
        };
        match resp {
            Ok(r) if (200..300).contains(&r.status) => self.read_sse(r.body).await,
            Ok(r) if r.status == 405 => {}
            Ok(r) => self
                .env
                .logs
                .push("http", &format!("GET stream refused: HTTP {}", r.status)),
            Err(e) => self.env.logs.push("http", &format!("GET stream: {e}")),
        }
    }

    async fn delete_session(&self) {
        let has_session = self
            .state
            .lock()
            .map(|s| s.session_id.is_some())
            .unwrap_or(false);
        if !has_session {
            return;
        }
        let req = HttpRequest {
            method: HttpMethod::Delete,
            url: self.url.clone(),
            headers: self.base_headers("application/json"),
            body: None,
        };
        let _ =
            tokio::time::timeout(std::time::Duration::from_secs(5), self.client.send(req)).await;
    }
}

async fn read_text(mut body: BoxStream<'static, Result<Bytes, String>>, cap: usize) -> String {
    let mut buf = Vec::new();
    while let Some(Ok(b)) = body.next().await {
        buf.extend_from_slice(&b);
        if buf.len() >= cap {
            buf.truncate(cap);
            break;
        }
    }
    String::from_utf8_lossy(&buf).into_owned()
}
