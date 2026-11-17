//! Sessions (02 §3.1, §6.2) and asks (§6.3).

use std::collections::BTreeMap;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::ask::{Ask, AskAnswer};
use crate::common::{ClientRef, Mode, SessionKind};
use crate::message::{InputPart, Message, SessionInfo, SessionSummary};
use crate::session::{Delivery, LiveStatus};

// ---------------------------------------------------------------- subscribe

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct SubscribeParams {
    pub session_id: String,
    /// Omit to replay from the start of the retained log.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub after_seq: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct SubscribeResult {
    pub head_seq: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub driver: Option<ClientRef>,
    pub status: LiveStatus,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct SessionIdParams {
    pub session_id: String,
}

// ---------------------------------------------------------------- list / get

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ListParams {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<SessionKind>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub archived: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub query: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cursor: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ListResult {
    pub items: Vec<SessionSummary>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_cursor: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct GetParams {
    pub session_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub leaf_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct GetResult {
    pub session: SessionInfo,
    /// Root → leaf.
    pub path: Vec<Message>,
    /// For each message on the path with siblings: the sibling ids, in order.
    pub siblings: BTreeMap<String, Vec<String>>,
}

// ---------------------------------------------------------------- create / update

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum WorktreeChoice {
    Auto,
    New,
    None,
    #[serde(other)]
    Unknown,
}

/// Session-only settings (the CLI layer, D23 layer 2): `--allow`/`--deny` rules,
/// `--settings <file>` and `--add-dir`. They apply to this session only.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct SessionSettingsOverlay {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub allow: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub deny: Vec<String>,
    /// A settings file read by the daemon (same user) as the session's CLI layer.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub settings_path: Option<String>,
    /// Extra directories the session may read/edit without asking (`--add-dir`).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub add_dirs: Vec<String>,
    /// `--enable-browser`: browser tools in a headless session (SA-28c).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub enable_browser: bool,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct CreateParams {
    pub kind: SessionKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace_path: Option<String>,
    pub model: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effort: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mode: Option<Mode>,
    #[serde(default)]
    pub incognito: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub worktree: Option<WorktreeChoice>,
    /// Name for a new worktree (`--worktree <name>`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub worktree_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub settings: Option<SessionSettingsOverlay>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct CreateResult {
    pub session_id: String,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct UpdateParams {
    pub session_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pinned: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub archived: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effort: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mode: Option<Mode>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active_leaf_id: Option<String>,
}

impl UpdateParams {
    /// `model`, `effort`, `mode` and `active_leaf_id` change how the session runs and
    /// need the driver lease; title/pinned/archived are list metadata (02 §5).
    pub fn is_mutating(&self) -> bool {
        self.model.is_some()
            || self.effort.is_some()
            || self.mode.is_some()
            || self.active_leaf_id.is_some()
    }
}

// ---------------------------------------------------------------- prompt / inbox

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct PromptParams {
    pub session_id: String,
    pub parts: Vec<InputPart>,
    pub delivery: Delivery,
    /// Edit = prompt with `parent_id` set to the edited message's parent (D24).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_id: Option<String>,
    /// Idempotency key: a retry with the same id returns the first result.
    pub client_msg_id: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct PromptResult {
    pub message_id: String,
    pub inbox_id: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct RegenerateParams {
    pub session_id: String,
    pub message_id: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct InterruptParams {
    pub session_id: String,
    #[serde(default)]
    pub stop_subagents: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct InterruptResult {
    /// False when no turn was running.
    pub interrupted: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ForkParams {
    pub session_id: String,
    pub message_id: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct CompactParams {
    pub session_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub focus: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct InboxRemoveParams {
    pub session_id: String,
    pub inbox_id: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ExportFormat {
    Json,
    Markdown,
    #[serde(other)]
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ExportParams {
    pub session_id: String,
    pub format: ExportFormat,
    #[serde(default)]
    pub sanitize: bool,
}

/// Either a blob (large exports) or inline text.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ExportResult {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub blob_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum SearchScope {
    All,
    Project,
    #[serde(other)]
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct SearchParams {
    pub query: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scope: Option<SearchScope>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct SearchHit {
    pub session_id: String,
    pub message_id: String,
    pub snippet: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct SearchResult {
    pub hits: Vec<SearchHit>,
}

// ---------------------------------------------------------------- asks (§6.3)

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct PendingParams {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct PendingResult {
    pub asks: Vec<Ask>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct RespondParams {
    pub ask_id: String,
    #[serde(flatten)]
    pub answer: AskAnswer,
}

/// Daemon → client request `permission.ask`; the result is an [`AskAnswer`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct AskParams {
    pub ask: Ask,
}

// ---------------------------------------------------------------- tools

/// Where a tool comes from (`tools.list`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ToolSource {
    // Built into the engine.
    Builtin,
    // An MCP server tool (`mcp__<server>__<tool>`).
    Mcp,
    #[serde(other)]
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ToolInfo {
    pub name: String,
    pub description: String,
    pub source: ToolSource,
    /// The MCP server, for `source: "mcp"`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub server: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ToolsListResult {
    pub tools: Vec<ToolInfo>,
}
