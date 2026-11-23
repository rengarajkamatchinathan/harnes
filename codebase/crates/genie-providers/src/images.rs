//! Image generation (OpenAI-compatible `/v1/images/generations`).
//!
//! [`OpenAiImages`] sits next to [`crate::embedding::OpenAiEmbeddings`]: one JSON `POST`
//! through the same [`Transport`] the [`crate::Executor`] uses, so cassettes replay it in
//! tests and errors are classified with [`crate::error::classify`]. It serves OpenAI
//! (`gpt-image-*`, `dall-e-*`), Azure AI Foundry v1 (the model is the deployment name) and
//! OpenAI-compatible servers that implement the endpoint.
//!
//! `gpt-image-*` models always answer with `b64_json`; `dall-e-*` models are asked for
//! `b64_json` explicitly. A server that still answers with a `url` is followed with one
//! unauthenticated `GET` (capped at [`MAX_IMAGE_BYTES`]).

use std::sync::Arc;

use base64::Engine as _;
use bytes::Bytes;
use futures::StreamExt;
use http::header::{ACCEPT, CONTENT_TYPE, HeaderValue};
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;

use crate::auth::Auth;
use crate::error::{ErrorKind, HttpFailure, ProtocolHint, ProviderError, classify};
use crate::executor::{HttpRequest, Transport};

/// Largest image accepted from a provider (decoded bytes).
pub const MAX_IMAGE_BYTES: usize = 32 * 1024 * 1024;

/// One image request. `None` fields are left to the provider's defaults.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ImageRequest {
    pub prompt: String,
    /// `1024x1024`, `1536x1024`, `1024x1536`, `auto` …
    pub size: Option<String>,
    /// `low` | `medium` | `high` | `auto` (gpt-image), `standard` | `hd` (dall-e-3).
    pub quality: Option<String>,
    /// `png` | `jpeg` | `webp` (gpt-image only).
    pub output_format: Option<String>,
    /// `transparent` | `opaque` | `auto` (gpt-image only).
    pub background: Option<String>,
    /// Number of images (1–4 here; providers cap lower for some models).
    pub n: u32,
}

/// One generated image.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GeneratedImage {
    pub bytes: Vec<u8>,
    pub mime: String,
    /// The prompt the provider actually used (dall-e-3 rewrites prompts).
    pub revised_prompt: Option<String>,
}

/// `POST {url}` with `{"model", "prompt", …}` → `data[i].b64_json | url`.
pub struct OpenAiImages {
    transport: Arc<dyn Transport>,
    auth: Arc<dyn Auth>,
    url: String,
    wire_model: String,
    headers: Vec<(String, String)>,
}

impl OpenAiImages {
    /// `url` is the full `…/images/generations` URL.
    pub fn new(
        transport: Arc<dyn Transport>,
        auth: Arc<dyn Auth>,
        url: impl Into<String>,
        wire_model: impl Into<String>,
    ) -> Self {
        OpenAiImages {
            transport,
            auth,
            url: url.into(),
            wire_model: wire_model.into(),
            headers: vec![],
        }
    }

    /// Extra headers from the route (e.g. an org or project header).
    pub fn with_headers(mut self, headers: Vec<(String, String)>) -> Self {
        self.headers = headers;
        self
    }

    pub fn wire_model(&self) -> &str {
        &self.wire_model
    }

    fn is_dalle(&self) -> bool {
        self.wire_model.to_ascii_lowercase().starts_with("dall-e")
    }

    /// The request body for `req`.
    pub fn body(&self, req: &ImageRequest) -> Value {
        let mut body = json!({
            "model": self.wire_model,
            "prompt": req.prompt,
            "n": req.n.max(1),
        });
        let mut set = |k: &str, v: &Option<String>| {
            if let Some(v) = v.as_deref().filter(|v| !v.is_empty()) {
                body[k] = json!(v);
            }
        };
        set("size", &req.size);
        set("quality", &req.quality);
        if self.is_dalle() {
            // gpt-image models reject `response_format`; dall-e defaults to short-lived URLs.
            body["response_format"] = json!("b64_json");
        } else {
            set("output_format", &req.output_format);
            set("background", &req.background);
        }
        body
    }

    /// Generate images; cancelled requests return [`ProviderError::cancelled`].
    pub async fn generate(
        &self,
        req: &ImageRequest,
        cancel: &CancellationToken,
    ) -> Result<Vec<GeneratedImage>, ProviderError> {
        tokio::select! {
            () = cancel.cancelled() => Err(ProviderError::cancelled()),
            r = self.call(req) => r,
        }
    }

    async fn call(&self, req: &ImageRequest) -> Result<Vec<GeneratedImage>, ProviderError> {
        let body = Bytes::from(
            serde_json::to_vec(&self.body(req))
                .map_err(|e| ProviderError::invalid_request(format!("image request: {e}")))?,
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
        for (k, v) in &self.headers {
            if let (Ok(k), Ok(v)) = (
                http::HeaderName::from_bytes(k.as_bytes()),
                HeaderValue::from_str(v),
            ) {
                parts.headers.insert(k, v);
            }
        }
        self.auth.apply(&mut parts, &body).await?;
        let (status, headers, buf) = self
            .send(HttpRequest {
                method: parts.method,
                url: self.url.clone(),
                headers: parts.headers,
                body,
            })
            .await?;
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
        let fallback = mime_of_format(req.output_format.as_deref());
        let mut out = vec![];
        for item in parse_response(&buf)? {
            let (bytes, mime) = match item.data {
                ImageData::Base64(b) => (b, None),
                ImageData::Url(u) => self.download(&u).await?,
            };
            let mime = sniff_mime(&bytes)
                .map(str::to_string)
                .or(mime)
                .unwrap_or_else(|| fallback.to_string());
            out.push(GeneratedImage {
                bytes,
                mime,
                revised_prompt: item.revised_prompt,
            });
        }
        if out.is_empty() {
            return Err(bad("no images in the response"));
        }
        Ok(out)
    }

    /// Send and read the whole body (capped).
    async fn send(
        &self,
        req: HttpRequest,
    ) -> Result<(u16, Vec<(String, String)>, Vec<u8>), ProviderError> {
        let resp = self.transport.send(req).await?;
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
        // base64 inflates by 4/3; leave room for the JSON around it.
        let cap = MAX_IMAGE_BYTES * 2;
        while let Some(chunk) = stream.next().await {
            buf.extend_from_slice(&chunk?);
            if buf.len() > cap {
                return Err(bad("the response is too large"));
            }
        }
        Ok((status, headers, buf))
    }

    async fn download(&self, url: &str) -> Result<(Vec<u8>, Option<String>), ProviderError> {
        if !url.starts_with("https://") {
            return Err(bad("the provider returned a non-https image URL"));
        }
        let (status, headers, buf) = self
            .send(HttpRequest {
                method: http::Method::GET,
                url: url.to_string(),
                headers: http::HeaderMap::new(),
                body: Bytes::new(),
            })
            .await?;
        if !(200..300).contains(&status) {
            return Err(bad(&format!("downloading the image failed ({status})")));
        }
        if buf.len() > MAX_IMAGE_BYTES {
            return Err(bad("the image is too large"));
        }
        let mime = headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case("content-type"))
            .map(|(_, v)| v.split(';').next().unwrap_or(v).trim().to_string())
            .filter(|m| m.starts_with("image/"));
        Ok((buf, mime))
    }
}

fn bad(m: &str) -> ProviderError {
    ProviderError::new(ErrorKind::InvalidProviderOutput, format!("images: {m}"))
}

enum ImageData {
    Base64(Vec<u8>),
    Url(String),
}

struct Item {
    data: ImageData,
    revised_prompt: Option<String>,
}

fn parse_response(buf: &[u8]) -> Result<Vec<Item>, ProviderError> {
    let v: Value = serde_json::from_slice(buf).map_err(|e| bad(&e.to_string()))?;
    let data = v
        .get("data")
        .and_then(Value::as_array)
        .ok_or_else(|| bad("no data array"))?;
    let mut out = Vec::with_capacity(data.len());
    for item in data {
        let revised_prompt = item
            .get("revised_prompt")
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
            .map(str::to_string);
        let data = if let Some(b) = item.get("b64_json").and_then(Value::as_str) {
            let bytes = base64::engine::general_purpose::STANDARD
                .decode(b.trim())
                .map_err(|e| bad(&format!("bad base64: {e}")))?;
            if bytes.len() > MAX_IMAGE_BYTES {
                return Err(bad("the image is too large"));
            }
            ImageData::Base64(bytes)
        } else if let Some(u) = item.get("url").and_then(Value::as_str) {
            ImageData::Url(u.to_string())
        } else {
            return Err(bad("item without b64_json or url"));
        };
        out.push(Item {
            data,
            revised_prompt,
        });
    }
    Ok(out)
}

fn mime_of_format(f: Option<&str>) -> &'static str {
    match f.map(str::to_ascii_lowercase).as_deref() {
        Some("jpeg" | "jpg") => "image/jpeg",
        Some("webp") => "image/webp",
        _ => "image/png",
    }
}

/// The image type from its magic bytes.
pub fn sniff_mime(b: &[u8]) -> Option<&'static str> {
    if b.starts_with(b"\x89PNG\r\n\x1a\n") {
        Some("image/png")
    } else if b.starts_with(&[0xFF, 0xD8, 0xFF]) {
        Some("image/jpeg")
    } else if b.len() >= 12 && &b[..4] == b"RIFF" && &b[8..12] == b"WEBP" {
        Some("image/webp")
    } else if b.starts_with(b"GIF87a") || b.starts_with(b"GIF89a") {
        Some("image/gif")
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;
    use crate::auth::NoAuth;
    use crate::cassette::{CassetteTransport, RecordedResponse};

    const PNG: &[u8] = b"\x89PNG\r\n\x1a\n0000";

    fn b64(b: &[u8]) -> String {
        base64::engine::general_purpose::STANDARD.encode(b)
    }

    fn client(t: Arc<CassetteTransport>, model: &str) -> OpenAiImages {
        OpenAiImages::new(
            t,
            Arc::new(NoAuth),
            "https://api.openai.com/v1/images/generations",
            model,
        )
    }

    #[tokio::test]
    async fn gpt_image_over_a_cassette() {
        let body = json!({"created": 1, "data": [{"b64_json": b64(PNG)}]}).to_string();
        let t = Arc::new(CassetteTransport::new(vec![
            RecordedResponse::json(200, &body, vec![]),
            RecordedResponse::json(
                400,
                r#"{"error":{"message":"Your request was rejected by the safety system.","type":"image_generation_user_error","code":"moderation_blocked"}}"#,
                vec![],
            ),
        ]));
        let c = client(t.clone(), "gpt-image-1");
        let req = ImageRequest {
            prompt: "a red fox".into(),
            size: Some("1024x1024".into()),
            output_format: Some("png".into()),
            n: 1,
            ..Default::default()
        };
        let cancel = CancellationToken::new();
        let out = c.generate(&req, &cancel).await.unwrap();
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].bytes, PNG);
        assert_eq!(out[0].mime, "image/png");
        let sent: Value = serde_json::from_str(&t.recorded()[0].body).unwrap();
        assert_eq!(sent["model"], "gpt-image-1");
        assert_eq!(sent["prompt"], "a red fox");
        assert_eq!(sent["size"], "1024x1024");
        assert_eq!(sent["output_format"], "png");
        assert!(sent.get("response_format").is_none());
        let err = c.generate(&req, &cancel).await.unwrap_err();
        assert!(!matches!(err.kind, ErrorKind::RateLimited), "{err:?}");
        assert!(err.message.contains("safety"), "{err:?}");
    }

    #[test]
    fn dalle_asks_for_base64_and_drops_gpt_image_fields() {
        let c = client(Arc::new(CassetteTransport::default()), "dall-e-3");
        let b = c.body(&ImageRequest {
            prompt: "x".into(),
            output_format: Some("webp".into()),
            background: Some("transparent".into()),
            n: 0,
            ..Default::default()
        });
        assert_eq!(b["response_format"], "b64_json");
        assert_eq!(b["n"], 1);
        assert!(b.get("output_format").is_none() && b.get("background").is_none());
    }

    #[tokio::test]
    async fn revised_prompt_and_errors() {
        let body =
            json!({"data": [{"b64_json": b64(PNG), "revised_prompt": "a red fox, watercolor"}]})
                .to_string();
        let t = Arc::new(CassetteTransport::new(vec![
            RecordedResponse::json(200, &body, vec![]),
            RecordedResponse::json(200, r#"{"data": []}"#, vec![]),
        ]));
        let c = client(t, "dall-e-3");
        let req = ImageRequest {
            prompt: "fox".into(),
            n: 1,
            ..Default::default()
        };
        let cancel = CancellationToken::new();
        let out = c.generate(&req, &cancel).await.unwrap();
        assert_eq!(
            out[0].revised_prompt.as_deref(),
            Some("a red fox, watercolor")
        );
        let err = c.generate(&req, &cancel).await.unwrap_err();
        assert!(matches!(err.kind, ErrorKind::InvalidProviderOutput));
    }

    #[test]
    fn sniffing() {
        assert_eq!(sniff_mime(PNG), Some("image/png"));
        assert_eq!(sniff_mime(&[0xFF, 0xD8, 0xFF, 0xE0]), Some("image/jpeg"));
        assert_eq!(sniff_mime(b"RIFF\0\0\0\0WEBPVP8 "), Some("image/webp"));
        assert_eq!(sniff_mime(b"nope"), None);
    }
}
