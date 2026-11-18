//! The `Tool` trait and what a tool gets to run (06 §4).
//!
//! Dependency rule (01 §2.1 rule 3): this crate sees `genie-protocol` only. Everything a
//! tool needs from another subsystem (permissions, blobs, memory, events) is a port
//! defined here and implemented by `genie-core`, which injects it per call.

use std::borrow::Cow;
use std::sync::Arc;

use async_trait::async_trait;
use genie_protocol::ask::{Preview, SaveSuggestion};
use genie_protocol::common::{Mode, SessionKind};
use genie_protocol::event::{SessionEvent, ToolProgress};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio_util::sync::CancellationToken;

use crate::output::OutputLimits;

/// Max issues reported for invalid tool input (01 §7).
pub const MAX_INPUT_ISSUES: usize = 5;

/// What a tool does to the world. The default authorizer asks before anything that is
/// not `ReadOnly` (06 §5 is evaluated by `genie-permissions`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolKind {
    ReadOnly,
    Mutating,
    /// Talks to an external service (MCP, web fetch).
    External,
    /// Declared to the provider and run there (web search, code execution).
    ProviderHosted,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Availability {
    Available,
    /// Hidden from the model (session kind, mode, model capability, policy).
    Unavailable(Cow<'static, str>),
}

impl Availability {
    pub fn is_available(&self) -> bool {
        matches!(self, Availability::Available)
    }
}

/// Session features that switch tools and prompt fragments on (06 §2.6.3, §4.1).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Features {
    /// Memory on (not paused, allowed by policy).
    pub memory: bool,
    pub incognito: bool,
    pub chat_search: bool,
    pub artifacts: bool,
    /// The project this session belongs to (project-scoped memory).
    pub project_id: Option<String>,
}

/// Session-level environment shared by every call of one turn.
pub struct ToolEnv {
    pub session_id: String,
    pub kind: SessionKind,
    pub mode: Mode,
    pub features: Features,
    pub output_limits: OutputLimits,
    pub services: Services,
}

impl ToolEnv {
    pub fn new(session_id: impl Into<String>, kind: SessionKind) -> Self {
        ToolEnv {
            session_id: session_id.into(),
            kind,
            mode: Mode::Default,
            features: Features::default(),
            output_limits: OutputLimits::default(),
            services: Services::default(),
        }
    }
}

/// Ports implemented by `genie-core` (01 §2.1 rule 3).
#[derive(Clone, Default)]
pub struct Services {
    /// Spilled tool output (§2.4). Without it, long output is cut, not spilled.
    pub spill: Option<Arc<dyn SpillStore>>,
    pub memory: Option<Arc<dyn MemoryPort>>,
}

/// What one tool call gets.
#[derive(Clone)]
pub struct ToolCtx {
    pub env: Arc<ToolEnv>,
    pub turn_id: String,
    pub call_id: String,
    /// The canonical tool name (after alias repair).
    pub tool_name: String,
    /// The call's token: a child of the turn token (01 §6).
    pub cancel: CancellationToken,
    /// Cancelled when the user asks to move this call out of the foreground (ticket 43,
    /// `session.background_tool`). Only `shell` acts on it; it never stops the call.
    pub detach: CancellationToken,
    pub permissions: Arc<dyn PermissionGate>,
    pub events: Arc<dyn EventSink>,
}

impl ToolCtx {
    /// The tool owns authorisation (06 §4): call this at the right moment (e.g. `edit`
    /// after the match succeeded). `Err` is `Declined` or `Blocked`.
    pub async fn authorize(&self, req: PermissionRequest) -> Result<(), ToolError> {
        self.permissions.assert(req).await
    }

    /// Ephemeral `tool.progress`.
    pub fn progress(&self, message: impl Into<String>, fraction: Option<f64>) {
        self.events.progress(ToolProgress {
            call_id: self.call_id.clone(),
            message: Some(message.into()),
            fraction,
            output_tail: None,
        });
    }
}

/// One authorisation request from a tool. The gate fills in the tool name, kind and
/// session mode.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct PermissionRequest {
    /// What is touched: a path, a domain, `server/tool` …
    pub resource: Option<String>,
    pub preview: Option<Preview>,
    pub save_suggestions: Vec<SaveSuggestion>,
}

/// Permission port (`ctx.permissions.assert`, 06 §4). Implemented in `genie-core` on its
/// `ToolAuthorizer`; rule evaluation lives in `genie-permissions` (06 §5).
#[async_trait]
pub trait PermissionGate: Send + Sync {
    async fn assert(&self, req: PermissionRequest) -> Result<(), ToolError>;
}

/// Allows everything (tests, read-only contexts).
pub struct AllowAll;

#[async_trait]
impl PermissionGate for AllowAll {
    async fn assert(&self, _req: PermissionRequest) -> Result<(), ToolError> {
        Ok(())
    }
}

/// Session events a tool may publish (`memory.changed`, `artifact.versioned`, progress).
pub trait EventSink: Send + Sync {
    /// A durable event.
    fn publish(&self, event: SessionEvent);
    /// Ephemeral `tool.progress`.
    fn progress(&self, progress: ToolProgress);
}

/// Drops everything (tests).
pub struct NullSink;

impl EventSink for NullSink {
    fn publish(&self, _event: SessionEvent) {}
    fn progress(&self, _progress: ToolProgress) {}
}

/// Spilled output storage (06 §2.4): a blob in the store, paged by `tool_output_read`.
pub trait SpillStore: Send + Sync {
    /// Store `text`; returns the reference shown to the model.
    fn put(&self, session_id: &str, text: &str) -> Result<String, String>;
    /// Read back the full text of a reference made in this session.
    fn read(&self, session_id: &str, reference: &str) -> Result<String, String>;
}

/// One memory entry as tools see it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MemoryEntry {
    pub id: String,
    pub scope: String,
    pub text: String,
}

/// Memory port (D31): implemented on `genie-store` by `genie-core`.
pub trait MemoryPort: Send + Sync {
    fn save(
        &self,
        scope: &str,
        text: &str,
        source_session: Option<&str>,
    ) -> Result<MemoryEntry, String>;
    fn update(&self, id: &str, text: &str) -> Result<MemoryEntry, String>;
    /// `Ok(None)` when no entry has this id.
    fn delete(&self, id: &str) -> Result<Option<MemoryEntry>, String>;
    fn search(&self, scopes: &[&str], query: &str, limit: u32) -> Result<Vec<MemoryEntry>, String>;
}

/// A successful tool result.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "value", rename_all = "snake_case")]
pub enum ToolOutput {
    Text(String),
    Json(Value),
}

impl ToolOutput {
    pub fn text(t: impl Into<String>) -> Self {
        ToolOutput::Text(t.into())
    }

    /// Model-readable text (JSON is pretty-printed so line limits mean something).
    pub fn to_text(&self) -> String {
        match self {
            ToolOutput::Text(t) => t.clone(),
            ToolOutput::Json(v) => {
                serde_json::to_string_pretty(v).unwrap_or_else(|_| v.to_string())
            }
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BlockSource {
    Policy,
    Rule,
}

/// A tool call failed (mirrors `genie_core::ToolError`, 01 §7; core converts).
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum ToolError {
    #[error("invalid input: {}", issues.join("; "))]
    InvalidInput { issues: Vec<String> },
    #[error("{message}")]
    Failed { message: String, may_have_run: bool },
    /// The user declined. The model sees only the user's own message (D20, D25).
    #[error("declined")]
    Declined { message: Option<String> },
    #[error("blocked by rule {rule}")]
    Blocked { rule: String, origin: BlockSource },
    #[error("timed out after {after_s}s")]
    Timeout { after_s: u64 },
    /// The turn was interrupted while the tool ran.
    #[error("cancelled")]
    Cancelled,
}

impl ToolError {
    pub fn invalid_input(mut issues: Vec<String>) -> Self {
        issues.truncate(MAX_INPUT_ISSUES);
        ToolError::InvalidInput { issues }
    }

    /// A failure before anything ran.
    pub fn failed(message: impl Into<String>) -> Self {
        ToolError::Failed {
            message: message.into(),
            may_have_run: false,
        }
    }
}

/// A tool (06 §4). Registries per session kind share this framework.
#[async_trait]
pub trait Tool: Send + Sync {
    fn name(&self) -> &str;
    /// May depend on the session (active shell, bundled artifact libraries …).
    fn description(&self, env: &ToolEnv) -> Cow<'_, str>;
    /// JSON Schema of the input (generated with schemars).
    fn input_schema(&self) -> &Value;
    fn kind(&self) -> ToolKind;
    fn availability(&self, _env: &ToolEnv) -> Availability {
        Availability::Available
    }
    /// Whether the tool calls [`ToolCtx::authorize`] before acting. Tools that are always
    /// allowed (read-only tools, memory, D31) return `false`; `tool.started` is then
    /// published as the call starts.
    fn authorizes(&self) -> bool {
        self.kind() != ToolKind::ReadOnly
    }
    async fn execute(&self, input: Value, ctx: ToolCtx) -> Result<ToolOutput, ToolError>;
}
