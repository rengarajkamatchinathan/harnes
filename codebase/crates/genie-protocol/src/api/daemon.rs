//! Daemon and secrets (02 §6.1).

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::channel::Channel;
use crate::handshake::ClientKind;

/// One connected client as `daemon.status` reports it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ConnectedClient {
    pub connection_id: u64,
    pub client_kind: ClientKind,
    pub client_version: String,
    pub client_instance_id: String,
    /// Served in degraded mode because the daemon is older and busy (01 §4.2).
    #[serde(default)]
    pub degraded: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct DaemonStatus {
    pub version: String,
    pub channel: Channel,
    pub uptime_s: u64,
    pub pid: u32,
    pub socket: String,
    pub sessions_running: u32,
    pub clients: Vec<ConnectedClient>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct DaemonRestartParams {
    #[serde(default)]
    pub stop_running: bool,
}

/// `daemon.restarting` notification (01 §4.2).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct DaemonRestartingParams {
    pub reason: String,
}

#[derive(Clone, Serialize, Deserialize, JsonSchema)]
pub struct UnlockParams {
    pub passphrase: String,
}

impl std::fmt::Debug for UnlockParams {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("UnlockParams { passphrase: [redacted] }")
    }
}

/// `storage.recover`: the recovery key shown when it was created (04 §2, SA-16).
#[derive(Clone, Serialize, Deserialize, JsonSchema)]
pub struct StorageRecoverParams {
    pub recovery_key: String,
}

impl std::fmt::Debug for StorageRecoverParams {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("StorageRecoverParams { recovery_key: [redacted] }")
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum CheckStatus {
    Ok,
    Warn,
    Fail,
    #[serde(other)]
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct DoctorCheck {
    pub id: String,
    pub status: CheckStatus,
    pub detail: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fix_link: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct DoctorResult {
    pub checks: Vec<DoctorCheck>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct DiagnosticsParams {
    #[serde(default)]
    pub include_logs: bool,
}

/// Sanitised; never contains prompt content (D34).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct DiagnosticsResult {
    pub text: String,
}
