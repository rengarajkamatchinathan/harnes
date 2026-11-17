//! Google Application Default Credentials for Vertex AI (03 §10.2).
//!
//! Sources, in order: an explicit file path (Settings), `GOOGLE_APPLICATION_CREDENTIALS`,
//! then gcloud's well-known file (`gcloud auth application-default login`):
//! `~/.config/gcloud/application_default_credentials.json` (Windows:
//! `%APPDATA%\gcloud\application_default_credentials.json`).
//!
//! File types: `authorized_user` (refresh-token grant) and `service_account` (RS256 JWT
//! bearer grant, signed with `ring`). Spec deviation: implemented here instead of with
//! the `google-cloud-auth` crate (keeps rustls-only HTTP and our own Executor setup);
//! external-account / workload identity federation is not supported (not in v1).

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use async_trait::async_trait;
use base64::Engine as _;
use base64::engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD};
use genie_secrets::SecretString;
use serde::Deserialize;
use zeroize::Zeroizing;

use crate::auth::{AccessToken, TokenSource};
use crate::error::{ErrorKind, ProviderError};

pub const CLOUD_PLATFORM_SCOPE: &str = "https://www.googleapis.com/auth/cloud-platform";
pub const GOOGLE_TOKEN_URI: &str = "https://oauth2.googleapis.com/token";

/// Locate the ADC file. `env` is the environment lookup (injected for tests).
pub fn adc_path(explicit: Option<&Path>, env: &dyn Fn(&str) -> Option<String>) -> Option<PathBuf> {
    if let Some(p) = explicit {
        return Some(p.to_path_buf());
    }
    if let Some(p) = env("GOOGLE_APPLICATION_CREDENTIALS").filter(|s| !s.is_empty()) {
        return Some(PathBuf::from(p));
    }
    let well_known = if cfg!(windows) {
        env("APPDATA").map(|a| PathBuf::from(a).join("gcloud"))
    } else {
        env("CLOUDSDK_CONFIG")
            .map(PathBuf::from)
            .or_else(|| env("HOME").map(|h| PathBuf::from(h).join(".config").join("gcloud")))
    }?
    .join("application_default_credentials.json");
    well_known.exists().then_some(well_known)
}

#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum AdcFile {
    AuthorizedUser {
        client_id: String,
        client_secret: String,
        refresh_token: String,
        #[serde(default)]
        token_uri: Option<String>,
    },
    ServiceAccount {
        client_email: String,
        private_key: String,
        #[serde(default)]
        token_uri: Option<String>,
    },
}

#[derive(Deserialize)]
struct TokenReply {
    access_token: String,
    #[serde(default)]
    expires_in: Option<u64>,
}

fn auth_err(m: impl Into<String>) -> ProviderError {
    ProviderError::new(ErrorKind::Authentication, m)
}

fn form(pairs: &[(&str, &str)]) -> String {
    url::form_urlencoded::Serializer::new(String::new())
        .extend_pairs(pairs)
        .finish()
}

/// `-----BEGIN PRIVATE KEY-----` (PKCS#8) → DER.
fn pem_to_der(pem: &str) -> Result<Zeroizing<Vec<u8>>, String> {
    let body: Zeroizing<String> = Zeroizing::new(
        pem.lines()
            .filter(|l| !l.starts_with("-----"))
            .collect::<Vec<_>>()
            .concat(),
    );
    STANDARD
        .decode(body.trim())
        .map(Zeroizing::new)
        .map_err(|_| "service account private_key is not PEM".into())
}

/// RS256 JWT for the JWT-bearer grant (RFC 7523).
pub fn service_account_assertion(
    email: &str,
    private_key_pem: &str,
    audience: &str,
    scope: &str,
    now: u64,
) -> Result<String, String> {
    let header = URL_SAFE_NO_PAD.encode(br#"{"alg":"RS256","typ":"JWT"}"#);
    let claims = serde_json::json!({
        "iss": email, "scope": scope, "aud": audience, "iat": now, "exp": now + 3600,
    });
    let claims = URL_SAFE_NO_PAD.encode(serde_json::to_vec(&claims).map_err(|e| e.to_string())?);
    let signing_input = format!("{header}.{claims}");
    let der = pem_to_der(private_key_pem)?;
    let key = ring::signature::RsaKeyPair::from_pkcs8(&der)
        .map_err(|e| format!("service account key: {e}"))?;
    let mut sig = vec![0u8; key.public().modulus_len()];
    key.sign(
        &ring::signature::RSA_PKCS1_SHA256,
        &ring::rand::SystemRandom::new(),
        signing_input.as_bytes(),
        &mut sig,
    )
    .map_err(|_| "signing the service account assertion failed".to_string())?;
    Ok(format!("{signing_input}.{}", URL_SAFE_NO_PAD.encode(sig)))
}

/// Access tokens from an ADC file (re-read on every refresh, so `gcloud` re-logins apply).
pub struct AdcTokenSource {
    path: PathBuf,
    http: reqwest::Client,
    pub scope: String,
}

impl AdcTokenSource {
    pub fn new(path: PathBuf, http: reqwest::Client) -> Self {
        Self {
            path,
            http,
            scope: CLOUD_PLATFORM_SCOPE.into(),
        }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    async fn post(&self, uri: &str, body: String) -> Result<TokenReply, ProviderError> {
        let resp = self
            .http
            .post(uri)
            .header("content-type", "application/x-www-form-urlencoded")
            .body(body)
            .send()
            .await
            .map_err(|e| auth_err(format!("Google token endpoint: {e}")))?;
        let status = resp.status();
        let text = resp
            .text()
            .await
            .map_err(|e| auth_err(format!("Google token endpoint: {e}")))?;
        if !status.is_success() {
            // Error bodies are `{error, error_description}`; no secrets echo back.
            let short: String = text.chars().take(300).collect();
            return Err(auth_err(format!(
                "Google token endpoint returned {status}: {short}"
            )));
        }
        serde_json::from_str(&text).map_err(|_| auth_err("unreadable Google token response"))
    }
}

#[async_trait]
impl TokenSource for AdcTokenSource {
    async fn fetch(&self, _force: bool) -> Result<AccessToken, ProviderError> {
        let raw = Zeroizing::new(std::fs::read(&self.path).map_err(|e| {
            auth_err(format!(
                "Google credentials {} unreadable: {e}; run `gcloud auth application-default login`",
                self.path.display()
            ))
        })?);
        let file: AdcFile = serde_json::from_slice(&raw)
            .map_err(|e| auth_err(format!("Google credentials file: {e}")))?;
        let reply = match file {
            AdcFile::AuthorizedUser {
                client_id,
                client_secret,
                refresh_token,
                token_uri,
            } => {
                let client_secret = Zeroizing::new(client_secret);
                let refresh_token = Zeroizing::new(refresh_token);
                let body = Zeroizing::new(form(&[
                    ("grant_type", "refresh_token"),
                    ("client_id", &client_id),
                    ("client_secret", &client_secret),
                    ("refresh_token", &refresh_token),
                ]));
                let uri = token_uri.unwrap_or_else(|| GOOGLE_TOKEN_URI.into());
                self.post(&uri, body.to_string()).await?
            }
            AdcFile::ServiceAccount {
                client_email,
                private_key,
                token_uri,
            } => {
                let private_key = Zeroizing::new(private_key);
                let uri = token_uri.unwrap_or_else(|| GOOGLE_TOKEN_URI.into());
                let now = SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .map(|d| d.as_secs())
                    .unwrap_or(0);
                let jwt =
                    service_account_assertion(&client_email, &private_key, &uri, &self.scope, now)
                        .map_err(auth_err)?;
                let body = form(&[
                    ("grant_type", "urn:ietf:params:oauth:grant-type:jwt-bearer"),
                    ("assertion", &jwt),
                ]);
                self.post(&uri, body).await?
            }
        };
        Ok(AccessToken {
            token: SecretString::from(reply.access_token),
            expires_at: reply
                .expires_in
                .map(|s| Instant::now() + Duration::from_secs(s)),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn path_order() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().to_string_lossy().to_string();
        let env_none = |_: &str| None;
        assert_eq!(
            adc_path(Some(Path::new("/x.json")), &env_none),
            Some(PathBuf::from("/x.json"))
        );
        let env_var = |k: &str| (k == "GOOGLE_APPLICATION_CREDENTIALS").then(|| "/y.json".into());
        assert_eq!(adc_path(None, &env_var), Some(PathBuf::from("/y.json")));
        let h = home.clone();
        let env_home = move |k: &str| (k == "HOME" || k == "APPDATA").then(|| h.clone());
        assert_eq!(adc_path(None, &env_home), None);
        let gdir = if cfg!(windows) {
            dir.path().join("gcloud")
        } else {
            dir.path().join(".config").join("gcloud")
        };
        std::fs::create_dir_all(&gdir).unwrap();
        std::fs::write(gdir.join("application_default_credentials.json"), "{}").unwrap();
        assert!(adc_path(None, &env_home).is_some());
    }
}
