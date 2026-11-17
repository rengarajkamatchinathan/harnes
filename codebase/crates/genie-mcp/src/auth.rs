//! OAuth for remote servers, as a port (06 §7, D26).
//!
//! The flow itself (client id order static → Genie-hosted CIMD → dynamic registration,
//! loopback redirect on `127.0.0.1` and `localhost`, token storage, refresh 30 s before
//! expiry) lives behind [`AuthProvider`]; the transport only asks for the current token,
//! asks for a refresh once on `401`, and otherwise reports `needs_auth`.

use async_trait::async_trait;
use genie_secrets::SecretString;
use sha2::{Digest, Sha256};

/// Identifies a server's tokens: keyed by hash(name, url) (06 §7).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct AuthKey {
    pub server: String,
    pub url: String,
}

impl AuthKey {
    pub fn new(server: &str, url: &str) -> AuthKey {
        AuthKey {
            server: server.to_string(),
            url: url.to_string(),
        }
    }

    /// The token-store key: hex SHA-256 of `name \0 url`.
    pub fn storage_key(&self) -> String {
        let mut h = Sha256::new();
        h.update(self.server.as_bytes());
        h.update([0]);
        h.update(self.url.as_bytes());
        hex::encode(h.finalize())
    }
}

#[async_trait]
pub trait AuthProvider: Send + Sync {
    /// The current access token, if one is stored (refreshed ahead of expiry by the
    /// provider).
    async fn token(&self, _key: &AuthKey) -> Option<SecretString> {
        None
    }

    /// The server rejected `rejected` with 401. Return a new token only if a refresh
    /// succeeded (and only refresh if the stored token is still `rejected`, so concurrent
    /// refreshes don't race); `None` puts the server in `needs_auth`.
    async fn refresh(
        &self,
        _key: &AuthKey,
        _rejected: Option<&SecretString>,
        _www_authenticate: Option<&str>,
    ) -> Option<SecretString> {
        None
    }

    /// Start an interactive authorisation (`mcp.auth`): returns the URL to open. When it
    /// completes, the caller restarts the server.
    async fn begin(
        &self,
        _key: &AuthKey,
        _www_authenticate: Option<&str>,
    ) -> Result<String, String> {
        Err("OAuth is not available".into())
    }
}

/// No OAuth: servers that answer 401 go to `needs_auth`.
pub struct NoAuth;

impl AuthProvider for NoAuth {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn storage_key_depends_on_both() {
        let a = AuthKey::new("s", "https://a").storage_key();
        assert_eq!(a.len(), 64);
        assert_ne!(a, AuthKey::new("s", "https://b").storage_key());
        assert_ne!(a, AuthKey::new("t", "https://a").storage_key());
    }
}
