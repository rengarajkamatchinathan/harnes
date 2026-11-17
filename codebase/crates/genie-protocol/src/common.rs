//! Small shared wire types (02 §5, §6).

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::handshake::ClientKind;

/// A client as named in leases, ask resolutions and `daemon.status` (02 §5).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct ClientRef {
    pub client_instance_id: String,
    pub client_kind: ClientKind,
    /// Human label shown on the "Take over" banner, e.g. "Desktop" or "CLI (pid 4211)".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
}

/// `{}` params or result. Unknown fields are ignored (02 §10).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Empty {}

/// `{ok: true}`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct OkResult {
    pub ok: bool,
}

/// Config write scopes (D23). `policy` is only ever a *source*, never a target.
/// `managed` labels definitions delivered by managed policy (e.g. MCP servers); it is
/// never a write target either.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ConfigScope {
    User,
    Project,
    Local,
    Managed,
    #[serde(other)]
    Unknown,
}

/// Where a config value came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ConfigSource {
    Default,
    User,
    Project,
    Local,
    Policy,
    // Session-only CLI layer (`--settings`, `--allow`/`--deny`, D23 layer 2).
    Cli,
    #[serde(other)]
    Unknown,
}

/// Scopes a saved permission rule can go to (t13).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum RuleScope {
    Session,
    Local,
    User,
    #[serde(other)]
    Unknown,
}

/// Session kind: Chat or Code mode (D54).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema, Default)]
#[serde(rename_all = "snake_case")]
pub enum SessionKind {
    #[default]
    Chat,
    Code,
    #[serde(other)]
    Unknown,
}

/// Permission mode (D25, 06 §4).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema, Default)]
#[serde(rename_all = "snake_case")]
pub enum Mode {
    #[default]
    Default,
    AcceptEdits,
    Plan,
    Bypass,
    #[serde(other)]
    Unknown,
}

/// Token usage of a step or session.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
pub struct Usage {
    #[serde(default)]
    pub input_tokens: u64,
    #[serde(default)]
    pub output_tokens: u64,
    #[serde(default)]
    pub cache_read_tokens: u64,
    #[serde(default)]
    pub cache_write_tokens: u64,
    #[serde(default)]
    pub reasoning_tokens: u64,
}

/// Cost in US dollars, as estimated from the catalog prices (D27).
#[derive(Debug, Clone, Copy, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Cost {
    pub usd: f64,
}

/// A config / trust diagnostic.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Diagnostic {
    pub severity: Severity,
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub key: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Severity {
    Error,
    Warning,
    Info,
    #[serde(other)]
    Unknown,
}
