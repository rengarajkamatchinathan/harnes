//! `initialize` handshake (02 §2).

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::channel::Channel;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum ClientKind {
    Desktop,
    Cli,
    Headless,
}

/// Ask kinds a client can render (02 §7).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum AskKind {
    Permission,
    Elicitation,
    Sampling,
    Question,
    BrowserConfirm,
    DefinitionHash,
    #[serde(other)]
    Unknown,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ClientCapabilities {
    #[serde(default)]
    pub asks: Vec<AskKind>,
    #[serde(default)]
    pub notifications: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub screencast: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub terminal: Option<bool>,
}

/// Client → daemon; must be the first request on a connection.
#[derive(Clone, Serialize, Deserialize, JsonSchema)]
pub struct InitializeParams {
    /// Semver of `genie-protocol` the client was built with.
    pub protocol: String,
    pub client_kind: ClientKind,
    /// Build version of the client.
    pub client_version: String,
    /// Stable per app install/window; used for lease reclaim (02 §5).
    pub client_instance_id: String,
    /// Contents of `daemon.token` (01 §1.2). Never logged.
    pub token: String,
    #[serde(default)]
    pub capabilities: ClientCapabilities,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct DaemonCapabilities {
    #[serde(default)]
    pub features: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct LockedInfo {
    pub reason: String,
}

/// The daemon could not open its database, so nothing is being saved (04 §2). Clients
/// must say so plainly and offer `storage.recover` / `storage.start_fresh`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct StorageProblem {
    /// `key_missing` (the DB exists but its key is in no reachable key store) or
    /// `open_failed` (anything else; see `detail`).
    pub reason: String,
    pub detail: String,
    /// A `recovery.bin` exists, so `storage.recover` with the recovery key can work.
    #[serde(default)]
    pub recovery_available: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct InitializeResult {
    pub engine_version: String,
    pub build: String,
    pub channel: Channel,
    pub protocol: String,
    #[serde(default)]
    pub capabilities: DaemonCapabilities,
    /// Version-skew path (01 §4.2): the daemon will drain and exit; re-run ensure.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub restart_pending: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub locked: Option<LockedInfo>,
    /// Set when sessions are NOT being persisted.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub storage: Option<StorageProblem>,
}

/// `Debug` never prints the token (01 §7 rule 2).
impl std::fmt::Debug for InitializeParams {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("InitializeParams")
            .field("protocol", &self.protocol)
            .field("client_kind", &self.client_kind)
            .field("client_version", &self.client_version)
            .field("client_instance_id", &self.client_instance_id)
            .field("token", &"[redacted]")
            .field("capabilities", &self.capabilities)
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn debug_redacts_token() {
        let p = InitializeParams {
            protocol: "1.0.0".into(),
            client_kind: ClientKind::Cli,
            client_version: "0.1.0".into(),
            client_instance_id: "cli_1".into(),
            token: "s3cr3t".into(),
            capabilities: ClientCapabilities::default(),
        };
        let s = format!("{p:?}");
        assert!(!s.contains("s3cr3t"));
        assert!(s.contains("[redacted]"));
    }
}
