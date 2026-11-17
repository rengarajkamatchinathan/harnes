//! MCP, skills and directories (02 §6.8).

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::common::ConfigScope;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum McpStatus {
    Pending,
    Connected,
    Disabled,
    Failed,
    NeedsAuth,
    #[serde(other)]
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct McpServerInfo {
    pub name: String,
    pub status: McpStatus,
    pub scope: ConfigScope,
    pub tools: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct McpListResult {
    pub servers: Vec<McpServerInfo>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct McpAddParams {
    pub scope: ConfigScope,
    pub name: String,
    /// Server definition as in `settings.json` `mcpServers` (D30).
    pub config: Value,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct McpRemoveParams {
    pub scope: ConfigScope,
    pub name: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct McpSetEnabledParams {
    pub name: String,
    pub enabled: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct NameParams {
    pub name: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct AuthUrlResult {
    pub auth_url: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct McpLogsParams {
    pub name: String,
    pub tail: u32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct LinesResult {
    pub lines: Vec<String>,
}

/// `mcp.tools` / `mcp.resources` / `mcp.prompts`: MCP's own list items, passed through.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct McpItemsResult {
    pub items: Vec<Value>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct McpResourceReadParams {
    pub name: String,
    pub uri: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct BlobMimeResult {
    pub blob_id: String,
    pub mime: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct SkillInfo {
    pub id: String,
    pub name: String,
    pub description: String,
    pub enabled: bool,
    pub source: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct SkillListResult {
    pub skills: Vec<SkillInfo>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct SkillSetEnabledParams {
    pub id: String,
    pub enabled: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct BlobIdParams {
    pub blob_id: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct IdResult {
    pub id: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum DirectoryKind {
    Connectors,
    Skills,
    #[serde(other)]
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct DirectoryListParams {
    pub kind: DirectoryKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub query: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct DirectoryEntry {
    pub id: String,
    pub version: String,
    pub name: String,
    pub description: String,
    #[serde(default)]
    pub extra: Value,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct DirectoryListResult {
    pub entries: Vec<DirectoryEntry>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct DirectoryInstallParams {
    pub kind: DirectoryKind,
    pub id: String,
    pub version: String,
    /// First call without it returns the "what will run" manifest; second call installs.
    #[serde(default)]
    pub confirm: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct DirectoryInstallResult {
    pub installed: bool,
    /// Commands, URLs and permissions the entry will use.
    pub manifest: Value,
}
