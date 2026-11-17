//! Request authentication (03 §3, §10).
//!
//! Static keys and bearer tokens are implemented here. Token-based flows (Entra PKCE /
//! device code, Google ADC) plug in as a [`TokenSource`]; [`TokenAuth`] caches the token,
//! refreshes single-flight and supports one forced refresh after a 401 (D26).

use std::sync::Arc;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use genie_secrets::{ExposeSecret, SecretString};
use http::HeaderValue;
use http::header::{AUTHORIZATION, HeaderName};

use crate::error::{ErrorKind, ProviderError};

#[async_trait]
pub trait Auth: Send + Sync {
    /// Add credentials to the request. `body` is available for signing schemes (SigV4, v2).
    async fn apply(&self, req: &mut http::request::Parts, body: &[u8])
    -> Result<(), ProviderError>;
    /// Drop any cached token so the next `apply` refreshes. Returns whether this auth can
    /// refresh at all (the executor then retries a 401 once).
    fn invalidate(&self) -> bool {
        false
    }
}

/// Data description of a route's auth (presets, Settings).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AuthKind {
    ApiKeyHeader {
        header: String,
    },
    Bearer,
    /// v2 (Bedrock); unused in v1.
    SigV4 {
        service: String,
        region: String,
    },
    Entra {
        scope: String,
    },
    GoogleAdc,
    None,
}

fn header_value(secret: &str, prefix: &str) -> Result<HeaderValue, ProviderError> {
    let mut v = HeaderValue::from_str(&format!("{prefix}{secret}")).map_err(|_| {
        ProviderError::new(
            ErrorKind::Authentication,
            "credential contains invalid header characters",
        )
    })?;
    v.set_sensitive(true);
    Ok(v)
}

/// `x-api-key: …` (Anthropic), `api-key: …` (Azure), `x-goog-api-key: …` (Gemini).
pub struct ApiKeyHeader {
    pub header: HeaderName,
    pub key: SecretString,
}

#[async_trait]
impl Auth for ApiKeyHeader {
    async fn apply(
        &self,
        req: &mut http::request::Parts,
        _body: &[u8],
    ) -> Result<(), ProviderError> {
        req.headers.insert(
            self.header.clone(),
            header_value(self.key.expose_secret(), "")?,
        );
        Ok(())
    }
}

/// `Authorization: Bearer <key>` with a static key.
pub struct BearerKey(pub SecretString);

#[async_trait]
impl Auth for BearerKey {
    async fn apply(
        &self,
        req: &mut http::request::Parts,
        _body: &[u8],
    ) -> Result<(), ProviderError> {
        req.headers.insert(
            AUTHORIZATION,
            header_value(self.0.expose_secret(), "Bearer ")?,
        );
        Ok(())
    }
}

pub struct NoAuth;

#[async_trait]
impl Auth for NoAuth {
    async fn apply(
        &self,
        _req: &mut http::request::Parts,
        _body: &[u8],
    ) -> Result<(), ProviderError> {
        Ok(())
    }
}

#[derive(Clone)]
pub struct AccessToken {
    pub token: SecretString,
    pub expires_at: Option<Instant>,
}

impl std::fmt::Debug for AccessToken {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AccessToken")
            .field("token", &"[REDACTED]")
            .field("expires_at", &self.expires_at)
            .finish()
    }
}

/// Where OAuth-style tokens come from: Entra (PKCE / device code), Google ADC, … The
/// implementations land with the auth flows (deferred; see README "Known gaps").
#[async_trait]
pub trait TokenSource: Send + Sync {
    /// Fetch a fresh token. `force` = the cached one was rejected (401).
    async fn fetch(&self, force: bool) -> Result<AccessToken, ProviderError>;
}

/// Bearer auth from a [`TokenSource`], cached until shortly before expiry.
pub struct TokenAuth {
    source: Arc<dyn TokenSource>,
    cache: tokio::sync::Mutex<Option<AccessToken>>,
    forced: std::sync::atomic::AtomicBool,
    /// Refresh this long before expiry.
    pub skew: Duration,
}

impl TokenAuth {
    pub fn new(source: Arc<dyn TokenSource>) -> Self {
        Self {
            source,
            cache: tokio::sync::Mutex::new(None),
            forced: std::sync::atomic::AtomicBool::new(false),
            skew: Duration::from_secs(120),
        }
    }

    /// Current token, refreshing single-flight: concurrent callers wait on the same lock
    /// and reuse the token the first one fetched.
    pub async fn token(&self) -> Result<AccessToken, ProviderError> {
        let mut guard = self.cache.lock().await;
        let force = self.forced.swap(false, std::sync::atomic::Ordering::SeqCst);
        let fresh = guard
            .as_ref()
            .filter(|t| !force && t.expires_at.is_none_or(|e| e > Instant::now() + self.skew))
            .cloned();
        if let Some(t) = fresh {
            return Ok(t);
        }
        let t = self.source.fetch(force).await?;
        *guard = Some(t.clone());
        Ok(t)
    }
}

#[async_trait]
impl Auth for TokenAuth {
    async fn apply(
        &self,
        req: &mut http::request::Parts,
        _body: &[u8],
    ) -> Result<(), ProviderError> {
        let t = self.token().await?;
        req.headers.insert(
            AUTHORIZATION,
            header_value(t.token.expose_secret(), "Bearer ")?,
        );
        Ok(())
    }
    fn invalidate(&self) -> bool {
        self.forced.store(true, std::sync::atomic::Ordering::SeqCst);
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU32, Ordering};

    struct Counting(AtomicU32);
    #[async_trait]
    impl TokenSource for Counting {
        async fn fetch(&self, _force: bool) -> Result<AccessToken, ProviderError> {
            let n = self.0.fetch_add(1, Ordering::SeqCst);
            Ok(AccessToken {
                token: SecretString::from(format!("tok{n}")),
                expires_at: Some(Instant::now() + Duration::from_secs(3600)),
            })
        }
    }

    fn parts() -> http::request::Parts {
        http::Request::builder()
            .uri("https://x/")
            .body(())
            .unwrap()
            .into_parts()
            .0
    }

    #[tokio::test]
    async fn token_cached_then_forced_refresh() {
        let src = Arc::new(Counting(AtomicU32::new(0)));
        let auth = TokenAuth::new(src.clone());
        let mut p = parts();
        auth.apply(&mut p, b"").await.unwrap();
        auth.apply(&mut p, b"").await.unwrap();
        assert_eq!(src.0.load(Ordering::SeqCst), 1);
        assert_eq!(p.headers[AUTHORIZATION], "Bearer tok0");
        assert!(p.headers[AUTHORIZATION].is_sensitive());
        assert!(auth.invalidate());
        auth.apply(&mut p, b"").await.unwrap();
        assert_eq!(p.headers[AUTHORIZATION], "Bearer tok1");
    }

    #[tokio::test]
    async fn single_flight_under_concurrency() {
        let src = Arc::new(Counting(AtomicU32::new(0)));
        let auth = Arc::new(TokenAuth::new(src.clone()));
        let futs = (0..8).map(|_| {
            let a = auth.clone();
            async move { a.token().await.map(|t| t.token.expose_secret().to_string()) }
        });
        let toks = futures::future::join_all(futs).await;
        assert!(toks.iter().all(|t| t.as_deref().ok() == Some("tok0")));
        assert_eq!(src.0.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn api_key_header() {
        let a = ApiKeyHeader {
            header: HeaderName::from_static("x-api-key"),
            key: SecretString::from("k"),
        };
        let mut p = parts();
        a.apply(&mut p, b"").await.unwrap();
        assert_eq!(p.headers["x-api-key"], "k");
        assert!(!a.invalidate());
    }
}
