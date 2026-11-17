//! OAuth sign-in and refresh for Azure AI Foundry with Entra ID (03 §10.2, D26).
//!
//! * **Auth code + PKCE** in the system browser with a loopback redirect
//!   (`http://localhost:<port>`, bound on 127.0.0.1 only, one request, `state` checked).
//! * **Device code** for SSH / CLI.
//! * **Refresh**: [`OAuthTokenSource`] is a [`TokenSource`]; [`crate::auth::TokenAuth`]
//!   makes it single-flight per credential, and the refresh itself runs under a
//!   cross-process file lock: the grant is re-read under the lock and our refresh is
//!   dropped when another process already rotated the token (pick-list #44).
//!
//! The OAuth client is the `oauth2` crate driving our own rustls `reqwest` client with
//! redirects disabled. Tokens and refresh tokens never reach logs: [`OAuthGrant`]'s
//! `Debug` redacts them. C2: our own public client id only, never a borrowed one.

use std::path::PathBuf;
use std::pin::Pin;
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use async_trait::async_trait;
use genie_secrets::lock::FileLock;
use genie_secrets::{ExposeSecret, SecretString};
use oauth2::basic::BasicClient;
use oauth2::{
    AuthUrl, AuthorizationCode, ClientId, CsrfToken, DeviceAuthorizationUrl, PkceCodeChallenge,
    RedirectUrl, RefreshToken, Scope, StandardDeviceAuthorizationResponse, TokenResponse, TokenUrl,
};
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

use crate::auth::{AccessToken, TokenSource};
use crate::error::{ErrorKind, ProviderError};

pub const ENTRA_AUTHORITY: &str = "https://login.microsoftonline.com";
/// 03 §10.2.
pub const FOUNDRY_SCOPE: &str = "https://ai.azure.com/.default";
/// Multi-tenant work/school accounts.
pub const DEFAULT_TENANT: &str = "organizations";
/// How long a sign-in may wait for the browser / the user's code entry.
pub const LOGIN_TIMEOUT: Duration = Duration::from_secs(10 * 60);

/// Entra app and endpoint settings for one provider.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct EntraConfig {
    pub client_id: String,
    pub tenant: String,
    /// `https://login.microsoftonline.com` (sovereign clouds / tests override it).
    pub authority: String,
    pub scope: String,
}

impl EntraConfig {
    pub fn new(client_id: impl Into<String>) -> Self {
        Self {
            client_id: client_id.into(),
            tenant: DEFAULT_TENANT.into(),
            authority: ENTRA_AUTHORITY.into(),
            scope: FOUNDRY_SCOPE.into(),
        }
    }
    fn base(&self) -> String {
        format!(
            "{}/{}/oauth2/v2.0",
            self.authority.trim_end_matches('/'),
            self.tenant
        )
    }
    pub fn authorize_url(&self) -> String {
        format!("{}/authorize", self.base())
    }
    pub fn token_url(&self) -> String {
        format!("{}/token", self.base())
    }
    pub fn device_url(&self) -> String {
        format!("{}/devicecode", self.base())
    }
    fn scopes(&self) -> Vec<Scope> {
        vec![
            Scope::new(self.scope.clone()),
            Scope::new("offline_access".into()),
        ]
    }
}

/// What an OAuth credential stores (as its secret, JSON).
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OAuthGrant {
    /// `entra`.
    pub issuer: String,
    pub config: EntraConfig,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub refresh_token: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub access_token: Option<String>,
    /// Unix seconds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expires_at: Option<u64>,
}

impl std::fmt::Debug for OAuthGrant {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OAuthGrant")
            .field("issuer", &self.issuer)
            .field("config", &self.config)
            .field(
                "refresh_token",
                &self.refresh_token.as_ref().map(|_| "[REDACTED]"),
            )
            .field(
                "access_token",
                &self.access_token.as_ref().map(|_| "[REDACTED]"),
            )
            .field("expires_at", &self.expires_at)
            .finish()
    }
}

fn now_unix() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

impl OAuthGrant {
    pub fn to_secret(&self) -> Result<SecretString, String> {
        serde_json::to_string(self)
            .map(SecretString::from)
            .map_err(|e| e.to_string())
    }
    pub fn from_secret(s: &SecretString) -> Result<Self, String> {
        serde_json::from_str(s.expose_secret()).map_err(|_| "stored sign-in is unreadable".into())
    }
    /// Access token still good for at least `skew`.
    pub fn valid_for(&self, skew: Duration) -> bool {
        self.access_token.is_some()
            && self
                .expires_at
                .is_some_and(|e| e > now_unix() + skew.as_secs())
    }
    fn access(&self) -> Option<AccessToken> {
        let tok = self.access_token.clone()?;
        let left = self.expires_at.map(|e| e.saturating_sub(now_unix()));
        Some(AccessToken {
            token: SecretString::from(tok),
            expires_at: left.map(|l| Instant::now() + Duration::from_secs(l)),
        })
    }
}

/// The HTTP client for token endpoints: rustls, no redirects (RFC 6749 §10.x), 30 s.
pub fn http_client() -> Result<reqwest::Client, String> {
    reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .timeout(Duration::from_secs(30))
        .build()
        .map_err(|e| e.to_string())
}

fn auth_err(m: impl Into<String>) -> ProviderError {
    ProviderError::new(ErrorKind::Authentication, m)
}

fn grant_from<T: TokenResponse>(
    cfg: &EntraConfig,
    t: &T,
    old_refresh: Option<String>,
) -> OAuthGrant {
    OAuthGrant {
        issuer: "entra".into(),
        config: cfg.clone(),
        refresh_token: t
            .refresh_token()
            .map(|r| r.secret().clone())
            .or(old_refresh),
        access_token: Some(t.access_token().secret().clone()),
        expires_at: t.expires_in().map(|d| now_unix() + d.as_secs()),
    }
}

fn token_error<E: std::error::Error>(e: E) -> String {
    // `RequestTokenError` prints the server's error code/description, never the request.
    let mut s = e.to_string();
    let mut src = e.source();
    while let Some(x) = src {
        s.push_str(": ");
        s.push_str(&x.to_string());
        src = x.source();
    }
    s
}

pub type LoginFuture =
    Pin<Box<dyn std::future::Future<Output = Result<OAuthGrant, String>> + Send>>;

/// A started browser sign-in: open `auth_url`, then await `wait`.
pub struct PkceLogin {
    pub auth_url: String,
    pub redirect_uri: String,
    pub wait: LoginFuture,
}

/// Bind the loopback listener and build the authorize URL (auth code + PKCE S256).
pub async fn start_pkce(
    cfg: &EntraConfig,
    http: reqwest::Client,
    timeout: Duration,
) -> Result<PkceLogin, String> {
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
        .await
        .map_err(|e| format!("loopback listener: {e}"))?;
    let port = listener.local_addr().map_err(|e| e.to_string())?.port();
    let redirect = format!("http://localhost:{port}");
    let client = BasicClient::new(ClientId::new(cfg.client_id.clone()))
        .set_auth_uri(AuthUrl::new(cfg.authorize_url()).map_err(|e| e.to_string())?)
        .set_token_uri(TokenUrl::new(cfg.token_url()).map_err(|e| e.to_string())?)
        .set_redirect_uri(RedirectUrl::new(redirect.clone()).map_err(|e| e.to_string())?);
    let (challenge, verifier) = PkceCodeChallenge::new_random_sha256();
    let (url, csrf) = client
        .authorize_url(CsrfToken::new_random)
        .add_scopes(cfg.scopes())
        .set_pkce_challenge(challenge)
        .url();
    let cfg = cfg.clone();
    let wait: LoginFuture = Box::pin(async move {
        let code = tokio::time::timeout(timeout, receive_code(listener, csrf.secret()))
            .await
            .map_err(|_| "sign-in timed out".to_string())??;
        let t = client
            .exchange_code(AuthorizationCode::new(code))
            .set_pkce_verifier(verifier)
            .request_async(&http)
            .await
            .map_err(token_error)?;
        Ok(grant_from(&cfg, &t, None))
    });
    Ok(PkceLogin {
        auth_url: url.to_string(),
        redirect_uri: redirect,
        wait,
    })
}

const DONE_PAGE: &str = "<!doctype html><meta charset=utf-8><title>Genie</title>\
<p style=\"font-family:system-ui\">You're signed in. You can close this window and return to Genie.</p>";
const FAIL_PAGE: &str = "<!doctype html><meta charset=utf-8><title>Genie</title>\
<p style=\"font-family:system-ui\">Sign-in failed. Return to Genie and try again.</p>";

/// Accept loopback requests until one carries `code` (or `error`) for our `state`.
async fn receive_code(listener: tokio::net::TcpListener, state: &str) -> Result<String, String> {
    loop {
        let (mut sock, _) = listener.accept().await.map_err(|e| e.to_string())?;
        let mut buf = Vec::with_capacity(2048);
        let mut chunk = [0u8; 1024];
        while !buf.windows(4).any(|w| w == b"\r\n\r\n") && buf.len() < 16 * 1024 {
            match tokio::time::timeout(Duration::from_secs(5), sock.read(&mut chunk)).await {
                Ok(Ok(n)) if n > 0 => buf.extend_from_slice(&chunk[..n]),
                _ => break,
            }
        }
        let head = String::from_utf8_lossy(&buf);
        let target = head
            .lines()
            .next()
            .and_then(|l| l.strip_prefix("GET "))
            .and_then(|l| l.split(' ').next())
            .unwrap_or("");
        let parsed = url::Url::parse(&format!("http://localhost{target}")).ok();
        let q = |k: &str| {
            parsed.as_ref().and_then(|u| {
                u.query_pairs()
                    .find(|(n, _)| n == k)
                    .map(|(_, v)| v.into_owned())
            })
        };
        let (code, err, st) = (q("code"), q("error"), q("state"));
        if code.is_none() && err.is_none() {
            let _ = sock
                .write_all(
                    b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                )
                .await;
            continue;
        }
        let ok = st.as_deref() == Some(state) && code.is_some();
        let page = if ok { DONE_PAGE } else { FAIL_PAGE };
        let resp = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{page}",
            page.len()
        );
        let _ = sock.write_all(resp.as_bytes()).await;
        let _ = sock.shutdown().await;
        if st.as_deref() != Some(state) {
            return Err("sign-in response did not match this request (state)".into());
        }
        if let Some(e) = err {
            let desc = q("error_description").unwrap_or_default();
            return Err(format!("sign-in failed: {e} {desc}").trim().to_string());
        }
        return code.ok_or_else(|| "no authorization code".into());
    }
}

/// A started device-code sign-in: show `user_code` + `verification_uri`, await `wait`.
pub struct DeviceLogin {
    pub user_code: String,
    pub verification_uri: String,
    pub expires_in: Duration,
    pub wait: LoginFuture,
}

pub async fn start_device_code(
    cfg: &EntraConfig,
    http: reqwest::Client,
) -> Result<DeviceLogin, String> {
    let client = BasicClient::new(ClientId::new(cfg.client_id.clone()))
        .set_device_authorization_url(
            DeviceAuthorizationUrl::new(cfg.device_url()).map_err(|e| e.to_string())?,
        )
        .set_token_uri(TokenUrl::new(cfg.token_url()).map_err(|e| e.to_string())?);
    let details: StandardDeviceAuthorizationResponse = client
        .exchange_device_code()
        .add_scopes(cfg.scopes())
        .request_async(&http)
        .await
        .map_err(token_error)?;
    let user_code = details.user_code().secret().clone();
    let verification_uri = details.verification_uri().to_string();
    let expires_in = details.expires_in();
    let cfg = cfg.clone();
    let wait: LoginFuture = Box::pin(async move {
        let t = client
            .exchange_device_access_token(&details)
            .request_async(&http, tokio::time::sleep, Some(expires_in))
            .await
            .map_err(token_error)?;
        Ok(grant_from(&cfg, &t, None))
    });
    Ok(DeviceLogin {
        user_code,
        verification_uri,
        expires_in,
        wait,
    })
}

/// Exchange the refresh token for a new access (and maybe refresh) token.
pub async fn refresh(grant: &OAuthGrant, http: &reqwest::Client) -> Result<OAuthGrant, String> {
    let rt = grant
        .refresh_token
        .clone()
        .ok_or_else(|| "sign-in expired; sign in again".to_string())?;
    let cfg = &grant.config;
    let client = BasicClient::new(ClientId::new(cfg.client_id.clone()))
        .set_token_uri(TokenUrl::new(cfg.token_url()).map_err(|e| e.to_string())?);
    let t = client
        .exchange_refresh_token(&RefreshToken::new(rt.clone()))
        .add_scopes(cfg.scopes())
        .request_async(http)
        .await
        .map_err(token_error)?;
    Ok(grant_from(cfg, &t, Some(rt)))
}

/// Where a grant is persisted (the credential store, behind the daemon).
pub trait GrantStore: Send + Sync {
    fn load(&self) -> Result<Option<OAuthGrant>, String>;
    fn save(&self, grant: &OAuthGrant) -> Result<(), String>;
}

/// Tokens for one OAuth credential, refreshed under a cross-process lock.
pub struct OAuthTokenSource {
    store: Arc<dyn GrantStore>,
    http: reqwest::Client,
    lock_path: PathBuf,
    /// Refresh this long before expiry.
    pub skew: Duration,
    pub lock_timeout: Duration,
}

impl OAuthTokenSource {
    pub fn new(store: Arc<dyn GrantStore>, http: reqwest::Client, lock_path: PathBuf) -> Self {
        Self {
            store,
            http,
            lock_path,
            skew: Duration::from_secs(120),
            lock_timeout: Duration::from_secs(30),
        }
    }
}

#[async_trait]
impl TokenSource for OAuthTokenSource {
    async fn fetch(&self, force: bool) -> Result<AccessToken, ProviderError> {
        let seen = self
            .store
            .load()
            .map_err(auth_err)?
            .ok_or_else(|| auth_err("not signed in"))?;
        if !force
            && seen.valid_for(self.skew)
            && let Some(t) = seen.access()
        {
            return Ok(t);
        }
        let _lock = FileLock::acquire(&self.lock_path, self.lock_timeout)
            .await
            .map_err(|e| auth_err(format!("token refresh lock: {e}")))?;
        // Re-read under the lock: another process may have rotated the token already.
        let current = self
            .store
            .load()
            .map_err(auth_err)?
            .ok_or_else(|| auth_err("not signed in"))?;
        if current.access_token != seen.access_token
            && current.valid_for(self.skew)
            && let Some(t) = current.access()
        {
            tracing::debug!("token already refreshed by another process");
            return Ok(t);
        }
        let fresh = refresh(&current, &self.http).await.map_err(auth_err)?;
        self.store.save(&fresh).map_err(auth_err)?;
        fresh
            .access()
            .ok_or_else(|| auth_err("token endpoint returned no access token"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn grant_debug_redacts_and_roundtrips() {
        let g = OAuthGrant {
            issuer: "entra".into(),
            config: EntraConfig::new("cid"),
            refresh_token: Some("rt-secret".into()),
            access_token: Some("at-secret".into()),
            expires_at: Some(now_unix() + 3600),
        };
        let d = format!("{g:?}");
        assert!(!d.contains("rt-secret") && !d.contains("at-secret"));
        let s = g.to_secret().unwrap();
        assert!(!format!("{s:?}").contains("secret\""));
        assert_eq!(OAuthGrant::from_secret(&s).unwrap(), g);
        assert!(g.valid_for(Duration::from_secs(120)));
        assert_eq!(
            g.config.token_url(),
            "https://login.microsoftonline.com/organizations/oauth2/v2.0/token"
        );
    }
}
