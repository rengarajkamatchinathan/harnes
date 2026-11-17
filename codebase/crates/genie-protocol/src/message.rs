//! Messages, parts and session records as they appear on the wire (02 §4, §6.2).
//!
//! The canonical provider model (03 §2) is richer; these are the client-facing shapes.
//! Provider-specific opaque data (signatures, encrypted reasoning) never leaves the daemon.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::common::{ClientRef, Cost, Mode, SessionKind, Usage};
use crate::session::LiveStatus;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    User,
    Assistant,
    System,
    Tool,
    #[serde(other)]
    Unknown,
}

/// What a client sends in `session.prompt` (02 §6.2).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum InputPart {
    Text {
        text: String,
    },
    Attachment {
        blob_id: String,
        name: String,
        mime: String,
    },
    /// An MCP resource (`@server:uri`).
    Resource {
        server: String,
        uri: String,
    },
    Skill {
        id: String,
    },
    FileRef {
        path: String,
    },
}

impl InputPart {
    pub fn text(t: impl Into<String>) -> Self {
        InputPart::Text { text: t.into() }
    }
}

/// A citation span on a text part (D36).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Citation {
    pub start: u32,
    pub end: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// Chat citation (`chat_search`): the cited session/message.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message_id: Option<String>,
}

/// A stored message part, final value (`part.ended`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Part {
    Text {
        id: String,
        text: String,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        citations: Vec<Citation>,
    },
    Reasoning {
        id: String,
        /// Readable summary; may be empty when the provider only returns opaque data.
        #[serde(default)]
        text: String,
        #[serde(default)]
        redacted: bool,
    },
    ToolCall {
        id: String,
        call_id: String,
        name: String,
        input: Value,
        /// Server-side tool (web search, code execution) run by the provider (D36).
        #[serde(default)]
        provider_executed: bool,
    },
    ToolResult {
        id: String,
        call_id: String,
        /// Text, JSON or rich parts; spilled results carry `spill_ref` instead (D32).
        content: Value,
        #[serde(default)]
        is_error: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        spill_ref: Option<String>,
    },
    Media {
        id: String,
        blob_id: String,
        mime: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        name: Option<String>,
    },
    /// Compaction boundary (D33): our summary text, or a provider checkpoint.
    Compaction {
        id: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        summary: Option<String>,
        #[serde(default)]
        native: bool,
    },
    /// A part type this client does not know (02 §10).
    #[serde(other)]
    Unknown,
}

/// Which kind of streamed part a `part.started` opens.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum PartKind {
    Text,
    Reasoning,
    ToolCall,
    ToolResult,
    Media,
    Compaction,
    #[serde(other)]
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct Message {
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_id: Option<String>,
    pub role: Role,
    pub parts: Vec<Part>,
    /// RFC 3339.
    pub created_at: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub turn_id: Option<String>,
    /// Assistant messages: the tokens of the step that produced this message.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage: Option<Usage>,
    /// Assistant messages: estimated cost of that step (D27).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cost: Option<Cost>,
}

/// One row in the session list (02 §6.2, workspace feed).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct SessionSummary {
    pub session_id: String,
    pub kind: SessionKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    pub status: LiveStatus,
    #[serde(default)]
    pub pinned: bool,
    #[serde(default)]
    pub archived: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace_id: Option<String>,
    /// RFC 3339.
    pub updated_at: String,
}

/// Full session record (`session.get`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct SessionInfo {
    #[serde(flatten)]
    pub summary: SessionSummary,
    pub model: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effort: Option<String>,
    pub mode: Mode,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_session_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active_leaf_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub driver: Option<ClientRef>,
    pub head_seq: u64,
    #[serde(default)]
    pub incognito: bool,
    /// RFC 3339.
    pub created_at: String,
    /// Per-chat provider web search toggle (06 §8).
    #[serde(default = "yes")]
    pub web_search: bool,
    /// Per-chat provider code execution toggle (D35).
    #[serde(default)]
    pub code_execution: bool,
}

fn yes() -> bool {
    true
}
