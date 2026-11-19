//! Workspace feed (02 §3.2): a snapshot, then delta notifications. No `seq`; on
//! reconnect the client takes a fresh snapshot.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::common::{ClientRef, ConfigScope};
use crate::message::SessionSummary;

/// `workspace.snapshot`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct WorkspaceSnapshot {
    pub sessions: Vec<SessionSummary>,
    /// Asks waiting across all sessions (badge count).
    pub pending_asks: u32,
    pub clients: Vec<ClientRef>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ListedChange {
    Created,
    Updated,
    Deleted,
    #[serde(other)]
    Unknown,
}

/// `session.listed`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct SessionListed {
    pub change: ListedChange,
    pub session: SessionSummary,
}

/// `config.changed`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ConfigChanged {
    pub keys: Vec<String>,
    pub scope: ConfigScope,
}

/// `policy.changed`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct PolicyChanged {
    pub keys: Vec<String>,
}

/// `mcp.status`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct McpStatusChanged {
    pub name: String,
    pub status: crate::api::mcp::McpStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// `catalog.updated`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct CatalogUpdated {
    pub source: crate::api::provider::CatalogSource,
    pub fetched_at: String,
}

/// `provider.status`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ProviderStatusChanged {
    pub provider: String,
    pub status: crate::api::provider::ProviderStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// `notification`: something the desktop app should show as an OS notification
/// (e.g. "Genie needs your approval", 02 §7 step 2).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct UserNotification {
    pub title: String,
    pub body: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ask_id: Option<String>,
    /// `genie://…` deep link.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub link: Option<String>,
}

/// `terminal.exited` (02 §6.7).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct TerminalExited {
    pub terminal_id: String,
    pub code: Option<i32>,
}

/// `preview.changed`: a session's dev servers changed (ticket 47); the full list.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct PreviewChanged {
    pub session_id: String,
    pub servers: Vec<crate::api::browser::PreviewServer>,
}
