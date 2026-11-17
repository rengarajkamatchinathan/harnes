//! The one HTTP `Executor` (03 §3): auth, timeouts, retries, framing, decoding and
//! cancellation for every provider request. Adapters never talk to `reqwest`.
//!
//! The network sits behind [`Transport`]; [`ReqwestTransport`] is the live one and
//! [`crate::cassette::CassetteTransport`] replays recorded traffic in tests (D41).

use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use bytes::Bytes;
use futures::stream::{BoxStream, Stream, StreamExt};
use http::header::{ACCEPT, CONTENT_TYPE, HeaderName, HeaderValue};
use tokio_util::sync::CancellationToken;

use crate::compile::{CompiledRequest, compile};
use crate::error::{ErrorKind, HttpFailure, ProviderError, TimeoutPhase, TransportPhase};
use crate::event::StreamEvent;
use crate::model::{LlmRequest, Origin};
use crate::protocol::{Framing, RawFrame, StreamContext};
use crate::retry::{RetryDecision, RetryPolicy, RetryState};
use crate::route::{Route, VersionPlacement};
use crate::sse::SseParser;

pub struct HttpRequest {
    pub method: http::Method,
    pub url: String,
    pub headers: http::HeaderMap,
    pub body: Bytes,
}

pub type BodyStream = BoxStream<'static, Result<Bytes, TransportError>>;

pub struct HttpResponse {
    pub status: u16,
    pub headers: http::HeaderMap,
    pub body: BodyStream,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TransportError {
    pub phase: TransportPhase,
    /// DNS failure / network unreachable.
    pub offline: bool,
    pub message: String,
}

impl From<TransportError> for ProviderError {
    fn from(e: TransportError) -> Self {
        ProviderError::new(
            ErrorKind::Transport {
                phase: e.phase,
                offline: e.offline,
            },
            e.message,
        )
    }
}

#[async_trait]
pub trait Transport: Send + Sync {
    async fn send(&self, req: HttpRequest) -> Result<HttpResponse, TransportError>;
}

/// Live transport: `reqwest` with rustls (D18).
pub struct ReqwestTransport {
    client: reqwest::Client,
}

impl ReqwestTransport {
    pub fn new() -> Result<Self, ProviderError> {
        let client = reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(30))
            .user_agent(concat!("genie/", env!("CARGO_PKG_VERSION")))
            .build()
            .map_err(|e| {
                ProviderError::new(
                    ErrorKind::Transport {
                        phase: TransportPhase::Connect,
                        offline: false,
                    },
                    e.to_string(),
                )
            })?;
        Ok(Self { client })
    }
}

fn looks_offline(msg: &str) -> bool {
    let m = msg.to_ascii_lowercase();
    [
        "dns",
        "failed to lookup",
        "name or service not known",
        "nodename nor servname",
        "network is unreachable",
        "no route to host",
    ]
    .iter()
    .any(|p| m.contains(p))
}

fn error_chain(e: &dyn std::error::Error) -> String {
    let mut s = e.to_string();
    let mut cur = e.source();
    while let Some(c) = cur {
        s.push_str(": ");
        s.push_str(&c.to_string());
        cur = c.source();
    }
    s
}

#[async_trait]
impl Transport for ReqwestTransport {
    async fn send(&self, req: HttpRequest) -> Result<HttpResponse, TransportError> {
        let resp = self
            .client
            .request(req.method, &req.url)
            .headers(req.headers)
            .body(req.body)
            .send()
            .await
            .map_err(|e| {
                let msg = error_chain(&e);
                let phase = if e.is_connect() {
                    TransportPhase::Connect
                } else {
                    TransportPhase::Send
                };
                TransportError {
                    phase,
                    offline: e.is_connect() && looks_offline(&msg),
                    message: msg,
                }
            })?;
        let status = resp.status().as_u16();
        let headers = resp.headers().clone();
        let body = resp
            .bytes_stream()
            .map(|r| {
                r.map_err(|e| TransportError {
                    phase: TransportPhase::Body,
                    offline: false,
                    message: error_chain(&e),
                })
            })
            .boxed();
        Ok(HttpResponse {
            status,
            headers,
            body,
        })
    }
}

pub type EventStream = Pin<Box<dyn Stream<Item = Result<StreamEvent, ProviderError>> + Send>>;

pub trait LlmClient {
    fn stream(&self, route: &Route, req: LlmRequest, cancel: CancellationToken) -> EventStream;
}

#[derive(Clone)]
pub struct Executor {
    transport: Arc<dyn Transport>,
    pub policy: RetryPolicy,
    /// Cap on how much of an error body is read.
    pub max_error_body: usize,
}

impl Executor {
    pub fn new(transport: Arc<dyn Transport>, policy: RetryPolicy) -> Self {
        Self {
            transport,
            policy,
            max_error_body: 64 * 1024,
        }
    }

    /// Stream an already compiled request.
    pub fn stream_compiled(
        &self,
        route: &Route,
        compiled: CompiledRequest,
        cancel: CancellationToken,
    ) -> EventStream {
        if !matches!(
            route.protocol.framing(),
            Framing::Sse | Framing::SseWithDone
        ) {
            let e = ProviderError::invalid_request("only SSE framing is implemented in v1");
            return futures::stream::once(async move { Err(e) }).boxed();
        }
        let built = match route.protocol.build(&compiled, &route.compat) {
            Ok(b) => b,
            Err(e) => return futures::stream::once(async move { Err(e) }).boxed(),
        };
        let route = route.clone();
        let transport = self.transport.clone();
        let policy = self.policy.clone();
        let max_err = self.max_error_body;
        let timeouts = compiled.request.timeouts;
        let include_raw = compiled.request.include_raw;
        let url = route.endpoint.url(&route.model.wire_id);
        let body = Bytes::from(built.body);
        let ctx = StreamContext {
            origin: Origin {
                provider: route.provider.clone(),
                model: route.model.wire_id.clone(),
                protocol: route.protocol.kind(),
                account: route.account,
            },
            compat: route.compat.clone(),
        };

        let s = async_stream::stream! {
            if !built.records.is_empty() {
                yield Ok(StreamEvent::WireRecorded { records: built.records });
            }
            let mut st = RetryState::default();
            let mut refreshed = false;
            loop {
                let mut parts = match http::Request::builder().method(http::Method::POST).uri(&url).body(()) {
                    Ok(r) => r.into_parts().0,
                    Err(e) => { yield Err(ProviderError::invalid_request(format!("bad url {url}: {e}"))); return; }
                };
                parts.headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
                parts.headers.insert(ACCEPT, HeaderValue::from_static("text/event-stream"));
                for (k, v) in &route.headers {
                    if let (Ok(k), Ok(v)) = (HeaderName::from_bytes(k.as_bytes()), HeaderValue::from_str(v)) {
                        parts.headers.insert(k, v);
                    }
                }
                if let (VersionPlacement::Header(h), Some(v)) = (&route.endpoint.version, &route.compat.anthropic_version)
                    && let (Ok(k), Ok(v)) = (HeaderName::from_bytes(h.as_bytes()), HeaderValue::from_str(v)) {
                        parts.headers.insert(k, v);
                    }
                if let Err(e) = route.auth.apply(&mut parts, &body).await {
                    yield Err(e);
                    return;
                }
                let req = HttpRequest { method: parts.method.clone(), url: url.clone(), headers: parts.headers, body: body.clone() };

                let sent = tokio::select! {
                    _ = cancel.cancelled() => { yield Err(ProviderError::cancelled()); return; }
                    r = tokio::time::timeout(timeouts.headers, transport.send(req)) => r,
                };
                let err: ProviderError = match sent {
                    Err(_) => ProviderError::new(ErrorKind::Timeout { phase: TimeoutPhase::Headers }, "no response headers in time"),
                    Ok(Err(te)) => te.into(),
                    Ok(Ok(resp)) if !(200..300).contains(&resp.status) => {
                        let headers: Vec<(String, String)> = resp
                            .headers
                            .iter()
                            .filter_map(|(k, v)| v.to_str().ok().map(|v| (k.as_str().to_string(), v.to_string())))
                            .collect();
                        let mut body = resp.body;
                        let mut buf = Vec::new();
                        loop {
                            let next = tokio::select! {
                                _ = cancel.cancelled() => { yield Err(ProviderError::cancelled()); return; }
                                c = tokio::time::timeout(timeouts.chunk_gap, body.next()) => c,
                            };
                            match next {
                                Ok(Some(Ok(b))) => {
                                    buf.extend_from_slice(&b);
                                    if buf.len() >= max_err { buf.truncate(max_err); break; }
                                }
                                _ => break,
                            }
                        }
                        let f = HttpFailure { status: resp.status, headers, body: String::from_utf8_lossy(&buf).into_owned() };
                        route.protocol.classify(&f)
                    }
                    Ok(Ok(resp)) => {
                        let mut body = resp.body;
                        let mut parser = SseParser::new();
                        let mut dec = Some(route.protocol.decoder(&ctx));
                        let mut frames: Vec<RawFrame> = Vec::new();
                        let mut events: Vec<StreamEvent> = Vec::new();
                        let mut pending: Vec<StreamEvent> = Vec::new();
                        let mut output_started = false;
                        let outcome: Result<(), ProviderError> = loop {
                            let next = tokio::select! {
                                _ = cancel.cancelled() => { yield Err(ProviderError::cancelled()); return; }
                                c = tokio::time::timeout(timeouts.chunk_gap, body.next()) => c,
                            };
                            let mut end = false;
                            match next {
                                Err(_) => break Err(ProviderError::new(ErrorKind::Timeout { phase: TimeoutPhase::ChunkGap }, "stream stalled")),
                                Ok(Some(Err(te))) => break Err(te.into()),
                                Ok(Some(Ok(b))) => parser.push(&b, &mut frames),
                                Ok(None) => { parser.finish(&mut frames); end = true; }
                            }
                            let mut failed = None;
                            for f in frames.drain(..) {
                                if include_raw {
                                    events.push(StreamEvent::Raw { event: f.event.clone(), data: f.data.clone() });
                                }
                                let Some(d) = dec.as_mut() else { break };
                                if let Err(e) = d.feed(&f, &mut events) { failed = Some(e); break; }
                            }
                            for ev in events.drain(..) {
                                if output_started {
                                    yield Ok(ev);
                                } else if ev.is_output() {
                                    output_started = true;
                                    for p in pending.drain(..) { yield Ok(p); }
                                    yield Ok(ev);
                                } else {
                                    pending.push(ev);
                                }
                            }
                            if let Some(e) = failed { break Err(e); }
                            if end {
                                let mut tail = Vec::new();
                                let r = match dec.take() { Some(d) => d.finish(&mut tail), None => Ok(()) };
                                for ev in tail.into_iter() {
                                    if ev.is_output() || output_started { output_started = true; }
                                    pending.push(ev);
                                }
                                if r.is_ok() || output_started {
                                    for p in pending.drain(..) { yield Ok(p); }
                                }
                                break r;
                            }
                        };
                        match outcome {
                            Ok(()) => return,
                            Err(e) if output_started => { yield Err(e); return; }
                            Err(e) => e,
                        }
                    }
                };

                // Expired Entra/OAuth token: one forced refresh, then a single retry (D26).
                if err.kind == ErrorKind::Authentication && !refreshed && route.auth.invalidate() {
                    refreshed = true;
                    continue;
                }
                match policy.decide(&err, &st, false) {
                    RetryDecision::Retry { after } => {
                        policy.note(&err, after, &mut st);
                        tracing::info!(target: "genie_providers::retry", attempt = st.retries, delay_ms = after.as_millis() as u64, kind = ?err.kind, "retry scheduled");
                        yield Ok(StreamEvent::RetryScheduled {
                            attempt: st.retries,
                            delay_ms: after.as_millis() as u64,
                            kind: err.kind.clone(),
                            message: err.message.clone(),
                        });
                        tokio::select! {
                            _ = cancel.cancelled() => { yield Err(ProviderError::cancelled()); return; }
                            _ = tokio::time::sleep(after) => {}
                        }
                    }
                    RetryDecision::GiveUp => { yield Err(err); return; }
                }
            }
        };
        s.boxed()
    }
}

impl LlmClient for Executor {
    fn stream(&self, route: &Route, req: LlmRequest, cancel: CancellationToken) -> EventStream {
        match compile(req, &route.info()) {
            Ok(c) => self.stream_compiled(route, c, cancel),
            Err(e) => futures::stream::once(async move { Err(e) }).boxed(),
        }
    }
}
