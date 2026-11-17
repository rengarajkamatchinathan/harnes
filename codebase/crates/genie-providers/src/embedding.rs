//! Embeddings for project retrieval (03 §9, D32).
//!
//! [`EmbeddingClient`] sits next to [`crate::LlmClient`]. [`OpenAiEmbeddings`] calls the
//! OpenAI-compatible `/v1/embeddings` endpoint (OpenAI, Azure v1, Ollama, vLLM …) through the
//! same [`Transport`] the [`crate::Executor`] uses, so cassettes replay it in tests and
//! errors are classified with [`crate::error::classify`]. [`HashEmbedder`] is a
//! deterministic, offline embedder for tests and for development builds without a
//! provider.

use std::sync::Arc;

use async_trait::async_trait;
use bytes::Bytes;
use futures::StreamExt;
use http::header::{ACCEPT, CONTENT_TYPE, HeaderValue};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use tokio_util::sync::CancellationToken;

use crate::auth::Auth;
use crate::error::{ErrorKind, HttpFailure, ProtocolHint, ProviderError, classify};
use crate::executor::{HttpRequest, Transport};

/// Turns texts into vectors. Implementations are cheap to clone behind an `Arc`.
#[async_trait]
pub trait EmbeddingClient: Send + Sync {
    /// Cache key for the vectors (`kembedding.model`): provider and model, e.g.
    /// `openai/text-embedding-3-small`. Vectors of different models never mix.
    fn model(&self) -> &str;

    /// One vector per input text, in order.
    async fn embed(
        &self,
        texts: &[String],
        cancel: &CancellationToken,
    ) -> Result<Vec<Vec<f32>>, ProviderError>;
}

/// Inputs per request (OpenAI accepts up to 2048; smaller batches keep bodies small).
pub const EMBED_BATCH: usize = 64;

/// `POST {url}` with `{"model", "input": [...]}` → `data[i].embedding`.
pub struct OpenAiEmbeddings {
    transport: Arc<dyn Transport>,
    auth: Arc<dyn Auth>,
    url: String,
    wire_model: String,
    key: String,
    dimensions: Option<u32>,
}

impl OpenAiEmbeddings {
    /// `provider` names the cache key (`{provider}/{wire_model}`).
    pub fn new(
        transport: Arc<dyn Transport>,
        auth: Arc<dyn Auth>,
        url: impl Into<String>,
        provider: &str,
        wire_model: impl Into<String>,
    ) -> Self {
        let wire_model = wire_model.into();
        OpenAiEmbeddings {
            transport,
            auth,
            url: url.into(),
            key: format!("{provider}/{wire_model}"),
            wire_model,
            dimensions: None,
        }
    }

    /// Ask for shorter vectors (`text-embedding-3-*` support it).
    pub fn with_dimensions(mut self, d: u32) -> Self {
        self.dimensions = Some(d);
        self
    }

    async fn call(&self, texts: &[String]) -> Result<Vec<Vec<f32>>, ProviderError> {
        let mut body =
            json!({ "model": self.wire_model, "input": texts, "encoding_format": "float" });
        if let Some(d) = self.dimensions {
            body["dimensions"] = json!(d);
        }
        let body = Bytes::from(
            serde_json::to_vec(&body)
                .map_err(|e| ProviderError::invalid_request(format!("embedding request: {e}")))?,
        );
        let mut parts = http::Request::builder()
            .method(http::Method::POST)
            .uri(&self.url)
            .body(())
            .map_err(|e| ProviderError::invalid_request(format!("bad url {}: {e}", self.url)))?
            .into_parts()
            .0;
        parts
            .headers
            .insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
        parts
            .headers
            .insert(ACCEPT, HeaderValue::from_static("application/json"));
        self.auth.apply(&mut parts, &body).await?;
        let resp = self
            .transport
            .send(HttpRequest {
                method: parts.method,
                url: self.url.clone(),
                headers: parts.headers,
                body,
            })
            .await?;
        let status = resp.status;
        let headers: Vec<(String, String)> = resp
            .headers
            .iter()
            .filter_map(|(k, v)| {
                v.to_str()
                    .ok()
                    .map(|v| (k.as_str().to_string(), v.to_string()))
            })
            .collect();
        let mut stream = resp.body;
        let mut buf = Vec::new();
        while let Some(chunk) = stream.next().await {
            buf.extend_from_slice(&chunk?);
        }
        if !(200..300).contains(&status) {
            return Err(classify(
                &HttpFailure {
                    status,
                    headers,
                    body: String::from_utf8_lossy(&buf).into_owned(),
                },
                ProtocolHint::default(),
            ));
        }
        parse_response(&buf, texts.len())
    }
}

fn parse_response(buf: &[u8], expected: usize) -> Result<Vec<Vec<f32>>, ProviderError> {
    let bad =
        |m: &str| ProviderError::new(ErrorKind::InvalidProviderOutput, format!("embeddings: {m}"));
    let v: Value = serde_json::from_slice(buf).map_err(|e| bad(&e.to_string()))?;
    let data = v
        .get("data")
        .and_then(Value::as_array)
        .ok_or_else(|| bad("no data array"))?;
    let mut out: Vec<Option<Vec<f32>>> = vec![None; expected];
    for (i, item) in data.iter().enumerate() {
        let idx = item
            .get("index")
            .and_then(Value::as_u64)
            .and_then(|n| usize::try_from(n).ok())
            .unwrap_or(i);
        let vec = item
            .get("embedding")
            .and_then(Value::as_array)
            .ok_or_else(|| bad("item without embedding"))?
            .iter()
            .map(|x| x.as_f64().map(|f| f as f32))
            .collect::<Option<Vec<f32>>>()
            .ok_or_else(|| bad("non-numeric embedding"))?;
        if let Some(slot) = out.get_mut(idx) {
            *slot = Some(vec);
        }
    }
    out.into_iter()
        .map(|v| v.ok_or_else(|| bad("missing vector")))
        .collect()
}

#[async_trait]
impl EmbeddingClient for OpenAiEmbeddings {
    fn model(&self) -> &str {
        &self.key
    }

    async fn embed(
        &self,
        texts: &[String],
        cancel: &CancellationToken,
    ) -> Result<Vec<Vec<f32>>, ProviderError> {
        let mut out = Vec::with_capacity(texts.len());
        for batch in texts.chunks(EMBED_BATCH) {
            let r = tokio::select! {
                () = cancel.cancelled() => return Err(ProviderError::cancelled()),
                r = self.call(batch) => r?,
            };
            out.extend(r);
        }
        Ok(out)
    }
}

/// Deterministic feature-hashing embedder: each lower-cased word (and word bigram) adds to
/// a bucket picked by its SHA-256, then the vector is L2-normalised. Texts that share
/// words get a high cosine. No network; for tests and offline development.
pub struct HashEmbedder {
    dim: usize,
    key: String,
}

impl HashEmbedder {
    pub fn new(dim: usize) -> Self {
        HashEmbedder {
            dim: dim.max(8),
            key: format!("genie/hash-{}", dim.max(8)),
        }
    }

    pub fn vector(&self, text: &str) -> Vec<f32> {
        let mut v = vec![0f32; self.dim];
        let words: Vec<String> = text
            .split(|c: char| !c.is_alphanumeric())
            .filter(|w| !w.is_empty())
            .map(str::to_lowercase)
            .collect();
        let mut add = |s: &str, w: f32| {
            let h = Sha256::digest(s.as_bytes());
            let idx = usize::from(h[0]) << 8 | usize::from(h[1]);
            let sign = if h[2] & 1 == 0 { 1.0 } else { -1.0 };
            v[idx % self.dim] += sign * w;
        };
        for w in &words {
            add(w, 1.0);
        }
        for pair in words.windows(2) {
            add(&format!("{} {}", pair[0], pair[1]), 0.5);
        }
        let norm = v.iter().map(|x| x * x).sum::<f32>().sqrt();
        if norm > 0.0 {
            for x in &mut v {
                *x /= norm;
            }
        }
        v
    }
}

impl Default for HashEmbedder {
    fn default() -> Self {
        Self::new(256)
    }
}

#[async_trait]
impl EmbeddingClient for HashEmbedder {
    fn model(&self) -> &str {
        &self.key
    }

    async fn embed(
        &self,
        texts: &[String],
        _cancel: &CancellationToken,
    ) -> Result<Vec<Vec<f32>>, ProviderError> {
        Ok(texts.iter().map(|t| self.vector(t)).collect())
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;
    use crate::auth::NoAuth;
    use crate::cassette::{CassetteTransport, RecordedResponse};

    fn cos(a: &[f32], b: &[f32]) -> f32 {
        a.iter().zip(b).map(|(x, y)| x * y).sum()
    }

    #[tokio::test]
    async fn hash_embedder_is_deterministic_and_topical() {
        let e = HashEmbedder::default();
        let t = CancellationToken::new();
        let v = e
            .embed(
                &[
                    "the billing invoice due date".into(),
                    "invoice due date for billing".into(),
                    "kubernetes pod scheduling".into(),
                ],
                &t,
            )
            .await
            .unwrap();
        assert_eq!(v[0], e.vector("the billing invoice due date"));
        assert!(cos(&v[0], &v[1]) > cos(&v[0], &v[2]));
        assert_eq!(e.model(), "genie/hash-256");
    }

    #[tokio::test]
    async fn openai_embeddings_over_a_cassette() {
        let body = json!({"object":"list","data":[
            {"object":"embedding","index":1,"embedding":[0.0, 1.0]},
            {"object":"embedding","index":0,"embedding":[1.0, 0.5]}
        ],"model":"text-embedding-3-small"})
        .to_string();
        let t = Arc::new(CassetteTransport::new(vec![
            RecordedResponse::json(200, &body, vec![]),
            RecordedResponse::json(
                429,
                r#"{"error":{"message":"Rate limit reached","type":"requests"}}"#,
                vec![],
            ),
        ]));
        let e = OpenAiEmbeddings::new(
            t.clone(),
            Arc::new(NoAuth),
            "https://api.openai.com/v1/embeddings",
            "openai",
            "text-embedding-3-small",
        );
        assert_eq!(e.model(), "openai/text-embedding-3-small");
        let c = CancellationToken::new();
        let v = e.embed(&["a".into(), "b".into()], &c).await.unwrap();
        assert_eq!(v, vec![vec![1.0, 0.5], vec![0.0, 1.0]]);
        let sent: Value = serde_json::from_str(&t.recorded()[0].body).unwrap();
        assert_eq!(sent["model"], "text-embedding-3-small");
        assert_eq!(sent["input"], json!(["a", "b"]));
        let err = e.embed(&["c".into()], &c).await.unwrap_err();
        assert!(matches!(err.kind, ErrorKind::RateLimited), "{err:?}");
    }
}
