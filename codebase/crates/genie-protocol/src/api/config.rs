//! Config, policy and trust (02 §6.4).

use std::collections::BTreeMap;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::common::{ConfigScope, ConfigSource, Diagnostic};

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ConfigGetParams {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub keys: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scope: Option<ConfigScope>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ConfigGetResult {
    pub values: BTreeMap<String, Value>,
    pub sources: BTreeMap<String, ConfigSource>,
    /// Keys locked by managed policy (D38).
    pub locked: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ConfigSetParams {
    pub key: String,
    pub value: Value,
    pub scope: ConfigScope,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
}

/// JSON Schema of the config plus Settings UI metadata (D23, D34).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ConfigSchemaResult {
    pub schema: Value,
    #[serde(default)]
    pub ui: Value,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct CwdParams {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ConfigDiagnosticsResult {
    pub problems: Vec<Diagnostic>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct PolicyEffectiveResult {
    pub values: BTreeMap<String, Value>,
    pub source_per_key: BTreeMap<String, String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct PathParams {
    pub path: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum TrustDecision {
    Trust,
    Once,
    Deny,
    Revoke,
    #[serde(other)]
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum TrustState {
    Trusted,
    Once,
    Denied,
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct TrustGetResult {
    pub path: String,
    pub state: TrustState,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct TrustSetParams {
    pub path: String,
    pub decision: TrustDecision,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum DefinitionKind {
    Mcp,
    Hook,
    Setup,
    #[serde(other)]
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct PendingDefinition {
    pub kind: DefinitionKind,
    pub name: String,
    pub hash: String,
    pub diff: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct PendingDefinitionsResult {
    pub items: Vec<PendingDefinition>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ApproveDecision {
    Approve,
    Skip,
    #[serde(other)]
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ApproveDefinitionParams {
    pub path: String,
    pub kind: DefinitionKind,
    pub name: String,
    pub hash: String,
    pub decision: ApproveDecision,
}

// ---------------------------------------------------------------- trust list

/// One stored folder decision (`trust.json`, 08 §4). Revoke with
/// `trust.set {path, decision: "revoke"}`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct TrustedFolder {
    /// Canonical folder path.
    pub path: String,
    /// `trust` or `deny`.
    pub decision: TrustDecision,
    /// Repository identity the decision is bound to, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub identity: Option<String>,
    /// RFC 3339.
    pub decided_at: String,
    /// Approved project definitions (MCP servers, hooks) under this decision.
    #[serde(default)]
    pub approvals: u32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct TrustListResult {
    pub items: Vec<TrustedFolder>,
}

// ---------------------------------------------------------------- permission rules

/// Where a permission rule lives (06 §5.4, D23).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum PermissionRuleScope {
    /// `~/.genie/settings.json`.
    User,
    /// `<project>/.genie/settings.json`.
    Project,
    /// `<project>/.genie/settings.local.json`.
    Local,
    /// Managed policy (always locked).
    Managed,
    /// Remembered for one session only ("always" with session scope).
    Session,
    /// The session's CLI layer (`--allow`/`--deny`/`--settings`; locked).
    Cli,
    #[serde(other)]
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum PermissionRuleClass {
    Allow,
    Ask,
    Deny,
    #[serde(other)]
    Unknown,
}

impl PermissionRuleClass {
    /// The `permissions.<class>` key segment.
    pub fn as_str(self) -> &'static str {
        match self {
            PermissionRuleClass::Allow => "allow",
            PermissionRuleClass::Ask => "ask",
            PermissionRuleClass::Deny => "deny",
            PermissionRuleClass::Unknown => "unknown",
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct PermissionRulesListParams {
    /// Only this scope; omit for every scope.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scope: Option<PermissionRuleScope>,
    /// Folder whose project/local layers to read (Code workspaces).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    /// Session whose session-scoped and CLI rules to include.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct PermissionRule {
    /// Rule text, e.g. `Bash(npm test:*)`.
    pub rule: String,
    pub class: PermissionRuleClass,
    pub scope: PermissionRuleScope,
    /// The settings file the rule is in, or `policy` / `session`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    /// Cannot be removed with `permissions.rules.remove` (managed policy, CLI layer).
    pub locked: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct PermissionRulesListResult {
    pub rules: Vec<PermissionRule>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct PermissionRuleRemoveParams {
    pub rule: String,
    pub class: PermissionRuleClass,
    /// `user`, `project`, `local` or `session`.
    pub scope: PermissionRuleScope,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    /// Required for `session` scope.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
}

// ---------------------------------------------------------------- delete all data

/// What "Delete all data" will remove (04 §8), and the token that confirms it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct DeleteAllBeginResult {
    /// Pass to `data.delete_all.confirm` within `expires_in_s` seconds; single use.
    pub token: String,
    pub expires_in_s: u32,
    pub sessions: u64,
    pub projects: u64,
    pub memories: u64,
    pub artifacts: u64,
    pub blob_bytes: u64,
    /// Whether the local audit log is removed too (kept under a managed audit policy).
    pub clears_audit: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct DeleteAllParams {
    /// From `data.delete_all.begin`.
    pub token: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct DeleteAllResult {
    pub sessions_deleted: u64,
    pub audit_cleared: bool,
}
