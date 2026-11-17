//! Cassettes (D41, 03 §11 layer 1): recorded request/response traffic replayed through the
//! [`crate::Executor`] with no network. The recorder scrubs credentials.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use bytes::Bytes;
use futures::StreamExt;
use serde::{Deserialize, Serialize};

use crate::error::TransportPhase;
use crate::executor::{HttpRequest, HttpResponse, Transport, TransportError};

/// Headers whose values never reach a fixture.
pub const SECRET_HEADERS: &[&str] = &[
    "authorization",
    "x-api-key",
    "api-key",
    "x-goog-api-key",
    "cookie",
    "set-cookie",
    "openai-organization",
    "openai-project",
];

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Cassette {
    pub interactions: Vec<Interaction>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Interaction {
    pub request: RecordedRequest,
    pub response: RecordedResponse,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct RecordedRequest {
    pub method: String,
    pub url: String,
    pub headers: Vec<(String, String)>,
    pub body: String,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct RecordedResponse {
    pub status: u16,
    #[serde(default)]
    pub headers: Vec<(String, String)>,
    /// Body chunks as they arrived (chunk boundaries matter for the SSE parser).
    pub chunks: Vec<String>,
    /// Simulate a transport failure after the chunks (mid-stream disconnect).
    #[serde(default)]
    pub disconnect: bool,
}

impl RecordedResponse {
    /// A response whose body is `sse`, split into `chunk`-byte pieces (on char boundaries).
    pub fn sse(status: u16, sse: &str, chunk: usize) -> Self {
        let mut chunks = vec![];
        let mut start = 0;
        while start < sse.len() {
            let mut end = (start + chunk.max(1)).min(sse.len());
            while !sse.is_char_boundary(end) {
                end += 1;
            }
            chunks.push(sse[start..end].to_string());
            start = end;
        }
        Self {
            status,
            headers: vec![("content-type".into(), "text/event-stream".into())],
            chunks,
            disconnect: false,
        }
    }
    pub fn json(status: u16, body: &str, headers: Vec<(String, String)>) -> Self {
        Self {
            status,
            headers,
            chunks: vec![body.to_string()],
            disconnect: false,
        }
    }
}

fn scrub_headers(h: &http::HeaderMap) -> Vec<(String, String)> {
    let mut v: Vec<(String, String)> = h
        .iter()
        .map(|(k, v)| {
            let val = if SECRET_HEADERS.contains(&k.as_str()) {
                "[REDACTED]".to_string()
            } else {
                v.to_str().unwrap_or("").to_string()
            };
            (k.as_str().to_string(), val)
        })
        .collect();
    v.sort();
    v
}

fn record_request(r: &HttpRequest) -> RecordedRequest {
    RecordedRequest {
        method: r.method.to_string(),
        url: r.url.clone(),
        headers: scrub_headers(&r.headers),
        body: String::from_utf8_lossy(&r.body).into_owned(),
    }
}

fn to_response(r: RecordedResponse) -> HttpResponse {
    let mut headers = http::HeaderMap::new();
    for (k, v) in &r.headers {
        if let (Ok(k), Ok(v)) = (
            http::header::HeaderName::from_bytes(k.as_bytes()),
            http::HeaderValue::from_str(v),
        ) {
            headers.append(k, v);
        }
    }
    let mut items: Vec<Result<Bytes, TransportError>> =
        r.chunks.into_iter().map(|c| Ok(Bytes::from(c))).collect();
    if r.disconnect {
        items.push(Err(TransportError {
            phase: TransportPhase::Body,
            offline: false,
            message: "connection reset".into(),
        }));
    }
    HttpResponse {
        status: r.status,
        headers,
        body: futures::stream::iter(items).boxed(),
    }
}

/// Replays responses in order and records the (scrubbed) requests it received.
#[derive(Default)]
pub struct CassetteTransport {
    responses: Mutex<VecDeque<Result<RecordedResponse, TransportError>>>,
    pub requests: Arc<Mutex<Vec<RecordedRequest>>>,
}

impl CassetteTransport {
    pub fn new(responses: impl IntoIterator<Item = RecordedResponse>) -> Self {
        Self {
            responses: Mutex::new(responses.into_iter().map(Ok).collect()),
            requests: Default::default(),
        }
    }
    pub fn from_cassette(c: Cassette) -> Self {
        Self::new(c.interactions.into_iter().map(|i| i.response))
    }
    /// Queue a connect-level failure.
    pub fn push_error(&self, e: TransportError) {
        if let Ok(mut q) = self.responses.lock() {
            q.push_back(Err(e));
        }
    }
    pub fn recorded(&self) -> Vec<RecordedRequest> {
        self.requests.lock().map(|r| r.clone()).unwrap_or_default()
    }
}

#[async_trait]
impl Transport for CassetteTransport {
    async fn send(&self, req: HttpRequest) -> Result<HttpResponse, TransportError> {
        if let Ok(mut r) = self.requests.lock() {
            r.push(record_request(&req));
        }
        let next = self.responses.lock().ok().and_then(|mut q| q.pop_front());
        match next {
            Some(Ok(r)) => Ok(to_response(r)),
            Some(Err(e)) => Err(e),
            None => Err(TransportError {
                phase: TransportPhase::Connect,
                offline: false,
                message: "cassette exhausted".into(),
            }),
        }
    }
}

/// Wraps a live transport and records each interaction (live smoke re-recording, 03 §11
/// layer 4). Bodies are buffered so the recording keeps the original chunk boundaries.
pub struct RecordingTransport<T> {
    inner: T,
    pub cassette: Arc<Mutex<Cassette>>,
}

impl<T: Transport> RecordingTransport<T> {
    pub fn new(inner: T) -> Self {
        Self {
            inner,
            cassette: Default::default(),
        }
    }
}

#[async_trait]
impl<T: Transport> Transport for RecordingTransport<T> {
    async fn send(&self, req: HttpRequest) -> Result<HttpResponse, TransportError> {
        let rec_req = record_request(&req);
        let resp = self.inner.send(req).await?;
        let status = resp.status;
        let headers = scrub_headers(&resp.headers);
        let mut chunks = vec![];
        let mut body = resp.body;
        let mut disconnect = false;
        while let Some(c) = body.next().await {
            match c {
                Ok(b) => chunks.push(String::from_utf8_lossy(&b).into_owned()),
                Err(_) => {
                    disconnect = true;
                    break;
                }
            }
        }
        let rec = RecordedResponse {
            status,
            headers,
            chunks,
            disconnect,
        };
        if let Ok(mut c) = self.cassette.lock() {
            c.interactions.push(Interaction {
                request: rec_req,
                response: rec.clone(),
            });
        }
        Ok(to_response(rec))
    }
}

/// CI check: does a fixture contain something that looks like a credential?
pub fn contains_secret(text: &str) -> bool {
    crate::error::looks_like_secret(text)
}
