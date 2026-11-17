//! Method names and the params/results the architecture layer needs (02 §6.1).
//! Other areas are added by their own specs.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::channel::Channel;
use crate::handshake::ClientKind;

pub const INITIALIZE: &str = "initialize";
pub const DAEMON_STATUS: &str = "daemon.status";
pub const DAEMON_RESTART: &str = "daemon.restart";
/// Liveness probe; no params, result `{}`.
pub const PING: &str = "$/ping";
/// Daemon → client notification sent before a drain (01 §4.2).
pub const DAEMON_RESTARTING: &str = "daemon.restarting";

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ClientRef {
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
    pub clients: Vec<ClientRef>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct DaemonRestartParams {
    #[serde(default)]
    pub stop_running: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct DaemonRestartingParams {
    pub reason: String,
}
