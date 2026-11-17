//! Session events (02 §3.3, §4).
//!
//! Durable events carry the session's gap-free `seq`; ephemeral events carry `ref_seq`,
//! the `seq` of the durable event that opened the item they belong to. Clients must
//! ignore unknown event types and fields (02 §10): unknown types decode as
//! [`SessionEvent::Unknown`].

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::ask::{Ask, Decision, ResolvedBy};
use crate::common::{ClientRef, Cost, Mode, SessionKind, Usage};
use crate::message::{InputPart, Part, PartKind};
use crate::session::{Delivery, LiveStatus};

/// 02 §3.3.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct SessionEventEnvelope {
    pub session_id: String,
    /// Durable only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub seq: Option<u64>,
    /// Ephemeral only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ref_seq: Option<u64>,
    /// RFC 3339.
    pub ts: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub turn_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub step: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subagent_of: Option<String>,
    pub event: SessionEvent,
}

impl SessionEventEnvelope {
    pub fn is_durable(&self) -> bool {
        self.seq.is_some()
    }
}

/// 02 §4. Discriminated by `type`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "type")]
pub enum SessionEvent {
    // ---- durable (§4.1) ----
    #[serde(rename = "session.created")]
    SessionCreated(SessionCreated),
    #[serde(rename = "session.updated")]
    SessionUpdated(SessionUpdated),
    #[serde(rename = "message.admitted")]
    MessageAdmitted(MessageAdmitted),
    #[serde(rename = "inbox.changed")]
    InboxChanged(InboxChanged),
    #[serde(rename = "turn.started")]
    TurnStarted(TurnStarted),
    #[serde(rename = "step.started")]
    StepStarted(StepStarted),
    #[serde(rename = "part.started")]
    PartStarted(PartStarted),
    #[serde(rename = "part.ended")]
    PartEnded(PartEnded),
    #[serde(rename = "tool.started")]
    ToolStarted(ToolStarted),
    #[serde(rename = "tool.ended")]
    ToolEnded(ToolEnded),
    #[serde(rename = "permission.asked")]
    PermissionAsked(PermissionAsked),
    #[serde(rename = "permission.resolved")]
    PermissionResolved(PermissionResolved),
    #[serde(rename = "step.ended")]
    StepEnded(StepEnded),
    #[serde(rename = "retry.scheduled")]
    RetryScheduled(RetryScheduled),
    #[serde(rename = "turn.settled")]
    TurnSettled(TurnSettled),
    #[serde(rename = "turn.interrupted")]
    TurnInterrupted(TurnInterrupted),
    #[serde(rename = "session.compacted")]
    SessionCompacted(SessionCompacted),
    #[serde(rename = "artifact.versioned")]
    ArtifactVersioned(ArtifactVersioned),
    #[serde(rename = "memory.changed")]
    MemoryChanged(MemoryChanged),
    #[serde(rename = "subagent.started")]
    SubagentStarted(SubagentStarted),
    #[serde(rename = "subagent.ended")]
    SubagentEnded(SubagentEnded),
    #[serde(rename = "checkpoint.created")]
    CheckpointCreated(CheckpointCreated),
    #[serde(rename = "branch.switched")]
    BranchSwitched(BranchSwitched),
    #[serde(rename = "lease.changed")]
    LeaseChanged(LeaseChanged),
    #[serde(rename = "browser.action")]
    BrowserAction(BrowserAction),
    #[serde(rename = "hook.ran")]
    HookRan(HookRan),
    #[serde(rename = "session.error")]
    SessionError(SessionError),

    // ---- ephemeral (§4.2) ----
    #[serde(rename = "part.delta")]
    PartDelta(PartDelta),
    #[serde(rename = "tool.progress")]
    ToolProgress(ToolProgress),
    #[serde(rename = "usage.updated")]
    UsageUpdated(UsageUpdated),
    #[serde(rename = "status")]
    Status(StatusEvent),
    #[serde(rename = "browser.frame")]
    BrowserFrame(BrowserFrame),
    #[serde(rename = "terminal.output")]
    TerminalOutput(TerminalOutput),

    /// An event type from a newer daemon. Skip it.
    #[serde(other)]
    Unknown,
}

impl SessionEvent {
    /// Whether this event type is stored and replayed (02 §4.1) or live only (§4.2).
    pub fn is_durable(&self) -> bool {
        !matches!(
            self,
            SessionEvent::PartDelta(_)
                | SessionEvent::ToolProgress(_)
                | SessionEvent::UsageUpdated(_)
                | SessionEvent::Status(_)
                | SessionEvent::BrowserFrame(_)
                | SessionEvent::TerminalOutput(_)
                | SessionEvent::Unknown
        )
    }

    /// The wire `type` string.
    pub fn type_name(&self) -> &'static str {
        match self {
            SessionEvent::SessionCreated(_) => "session.created",
            SessionEvent::SessionUpdated(_) => "session.updated",
            SessionEvent::MessageAdmitted(_) => "message.admitted",
            SessionEvent::InboxChanged(_) => "inbox.changed",
            SessionEvent::TurnStarted(_) => "turn.started",
            SessionEvent::StepStarted(_) => "step.started",
            SessionEvent::PartStarted(_) => "part.started",
            SessionEvent::PartEnded(_) => "part.ended",
            SessionEvent::ToolStarted(_) => "tool.started",
            SessionEvent::ToolEnded(_) => "tool.ended",
            SessionEvent::PermissionAsked(_) => "permission.asked",
            SessionEvent::PermissionResolved(_) => "permission.resolved",
            SessionEvent::StepEnded(_) => "step.ended",
            SessionEvent::RetryScheduled(_) => "retry.scheduled",
            SessionEvent::TurnSettled(_) => "turn.settled",
            SessionEvent::TurnInterrupted(_) => "turn.interrupted",
            SessionEvent::SessionCompacted(_) => "session.compacted",
            SessionEvent::ArtifactVersioned(_) => "artifact.versioned",
            SessionEvent::MemoryChanged(_) => "memory.changed",
            SessionEvent::SubagentStarted(_) => "subagent.started",
            SessionEvent::SubagentEnded(_) => "subagent.ended",
            SessionEvent::CheckpointCreated(_) => "checkpoint.created",
            SessionEvent::BranchSwitched(_) => "branch.switched",
            SessionEvent::LeaseChanged(_) => "lease.changed",
            SessionEvent::BrowserAction(_) => "browser.action",
            SessionEvent::HookRan(_) => "hook.ran",
            SessionEvent::SessionError(_) => "session.error",
            SessionEvent::PartDelta(_) => "part.delta",
            SessionEvent::ToolProgress(_) => "tool.progress",
            SessionEvent::UsageUpdated(_) => "usage.updated",
            SessionEvent::Status(_) => "status",
            SessionEvent::BrowserFrame(_) => "browser.frame",
            SessionEvent::TerminalOutput(_) => "terminal.output",
            SessionEvent::Unknown => "unknown",
        }
    }
}

// ---------------------------------------------------------------- durable payloads

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct SessionCreated {
    pub kind: SessionKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace_id: Option<String>,
    pub model: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effort: Option<String>,
    pub mode: Mode,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_session_id: Option<String>,
}

/// Only the changed fields are present.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct SessionUpdated {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effort: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mode: Option<Mode>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pinned: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub archived: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active_leaf_id: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum MessageSource {
    User,
    Synthetic,
    #[serde(other)]
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct MessageAdmitted {
    pub message_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_id: Option<String>,
    /// Always `user`.
    pub role: crate::message::Role,
    pub parts: Vec<InputPart>,
    pub delivery: Delivery,
    pub source: MessageSource,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct InboxItemView {
    pub id: String,
    pub delivery: Delivery,
    pub preview: String,
}

/// The queue shown above the composer: the full current list.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct InboxChanged {
    pub items: Vec<InboxItemView>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct TurnStarted {
    pub turn_id: String,
    /// The admitted user message (or inbox item id until the store assigns messages).
    pub message_id: String,
    #[serde(default)]
    pub resume: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct StepStarted {
    pub step: u32,
    pub model: String,
    /// Provider route, e.g. `anthropic/default`.
    pub route: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct PartStarted {
    pub message_id: String,
    pub part_id: String,
    pub kind: PartKind,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct PartEnded {
    pub part_id: String,
    pub part: Part,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ToolStarted {
    pub call_id: String,
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resource: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ToolOutcome {
    Ok,
    Error,
    Declined,
    Blocked,
    Cancelled,
    #[serde(other)]
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ToolEnded {
    pub call_id: String,
    pub outcome: ToolOutcome,
    pub result_part_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub spill_ref: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct PermissionAsked {
    pub ask: Box<Ask>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct PermissionResolved {
    pub ask_id: String,
    pub decision: Decision,
    pub by: ResolvedBy,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub saved_rule: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum StepFinish {
    Stop,
    ToolUse,
    Length,
    ContentFilter,
    Error,
    Cancelled,
    #[serde(other)]
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct StepEnded {
    pub finish: StepFinish,
    pub usage: Usage,
    pub cost: Cost,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct RetryScheduled {
    pub attempt: u32,
    pub after_ms: u64,
    pub error_class: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum TurnStatus {
    Completed,
    Failed,
    Cancelled,
    #[serde(other)]
    Unknown,
}

/// A user-facing error: stable code, message, optional fix link (01 §7).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ErrorInfo {
    pub code: String,
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub link: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct TurnSettled {
    pub turn_id: String,
    pub status: TurnStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<ErrorInfo>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum InterruptReason {
    DaemonRestart,
    Crash,
    #[serde(other)]
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct TurnInterrupted {
    pub turn_id: String,
    pub reason: InterruptReason,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct SessionCompacted {
    pub compaction_message_id: String,
    /// `auto` | `manual` | `overflow`.
    pub trigger: String,
    pub tokens_before: u64,
    pub tokens_after: u64,
    pub native: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ArtifactVersioned {
    pub artifact_id: String,
    pub n: u32,
    pub message_id: String,
    #[serde(rename = "type")]
    pub artifact_type: String,
    pub title: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct MemoryChanged {
    pub memory_id: String,
    /// `add` | `update` | `delete`.
    pub op: String,
    pub scope: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct SubagentStarted {
    pub child_session_id: String,
    pub agent: String,
    pub description: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct SubagentEnded {
    pub child_session_id: String,
    pub status: TurnStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
    pub cost: Cost,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct CheckpointCreated {
    pub step_ref: String,
    pub before_tree: String,
    pub after_tree: String,
    #[serde(default)]
    pub skipped: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct BranchSwitched {
    pub active_leaf_id: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct LeaseChanged {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub driver: Option<ClientRef>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct BrowserAction {
    pub tab_id: String,
    pub tool: String,
    pub origin: String,
    pub decision: String,
    pub status: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub thumbnail_ref: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct HookRan {
    pub event: String,
    pub hook_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub decision: Option<String>,
    pub duration_ms: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct SessionError {
    pub code: String,
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub link: Option<String>,
}

// ---------------------------------------------------------------- ephemeral payloads

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct PartDelta {
    pub part_id: String,
    /// Text / reasoning delta.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub text: Option<String>,
    /// Tool-input JSON fragment.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub json: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ToolProgress {
    pub call_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fraction: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output_tail: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct UsageUpdated {
    pub session: Usage,
    pub cost: Cost,
    /// Context meter (D32).
    pub context_tokens: u64,
    pub context_window: u64,
}

/// What the turn is doing right now (spinner text).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Activity {
    Thinking,
    Retrying,
    Compacting,
    Interrupting,
    WaitingForChildren,
    #[serde(other)]
    Unknown,
}

/// Live status. `state` is the session state machine (01 §5.1); `activity` the finer
/// spinner state of 02 §4.2.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct StatusEvent {
    pub state: LiveStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub activity: Option<Activity>,
}

/// Screencast frame (02 §6.6).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct BrowserFrame {
    pub stream_id: String,
    pub frame_seq: u64,
    pub tab_id: String,
    pub jpeg_b64: String,
    pub w: u32,
    pub h: u32,
    pub device_scale: f64,
    pub ts: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct TerminalOutput {
    pub terminal_id: String,
    pub start: u64,
    pub end: u64,
    pub data_b64: String,
}

// ---------------------------------------------------------------- stream notifications

/// `session.synced`: replay done, live from here (02 §3.1).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct SessionSynced {
    pub session_id: String,
    pub seq: u64,
}

/// `session.resync_required` (02 §3.1). With `snapshot`, reload with `session.get`
/// and subscribe from its `head_seq`; otherwise re-subscribe with `after_seq = from_seq`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ResyncRequired {
    pub session_id: String,
    pub from_seq: u64,
    #[serde(default)]
    pub snapshot: bool,
}

/// `lease.lost` (02 §5): sent to the old driver on `take_over` or when the lease lapses.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct LeaseLost {
    pub session_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub new_driver: Option<ClientRef>,
}

/// `$/cancelRequest`: the daemon withdraws a request it sent (e.g. an ask answered
/// elsewhere, or the lease moved).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct CancelRequest {
    pub id: crate::jsonrpc::Id,
}
