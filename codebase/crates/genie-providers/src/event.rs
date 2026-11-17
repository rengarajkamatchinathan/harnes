//! The normalised stream event model (03 §3; research "Type sketch").
//!
//! Every adapter turns provider frames into this sequence. `part` indices are dense per
//! response, assigned in the order parts start.

use serde::{Deserialize, Serialize};

use crate::model::{
    Citation, FinishDetails, FinishReason, Opaque, Part, ToolCall, ToolCallId, ToolResult, Usage,
    WireRecord,
};

pub type PartIdx = u32;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum StreamEvent {
    StepStart {
        step: u32,
    },
    TextStart {
        part: PartIdx,
    },
    TextDelta {
        part: PartIdx,
        delta: String,
    },
    TextEnd {
        part: PartIdx,
        citations: Vec<Citation>,
        opaque: Option<Opaque>,
    },
    ReasoningStart {
        part: PartIdx,
    },
    ReasoningDelta {
        part: PartIdx,
        delta: String,
    },
    ReasoningEnd {
        part: PartIdx,
        final_text: Option<String>,
        opaque: Option<Opaque>,
    },
    ToolInputStart {
        part: PartIdx,
        call_id: ToolCallId,
        name: String,
        provider_executed: bool,
    },
    ToolInputDelta {
        part: PartIdx,
        json_fragment: String,
    },
    ToolInputEnd {
        part: PartIdx,
        call: ToolCall,
    },
    ToolInputError {
        part: PartIdx,
        call_id: ToolCallId,
        raw: String,
        error: String,
    },
    ProviderToolResult {
        part: PartIdx,
        result: ToolResult,
    },
    Citation {
        part: PartIdx,
        citation: Citation,
    },
    /// Whole-arriving blocks (redacted thinking, Anthropic `fallback`, unknown blocks).
    OpaqueBlock {
        part: PartIdx,
        value: Part,
    },
    /// Server-side fallback boundary.
    ModelSwitched {
        from: String,
        to: String,
    },
    StepFinish {
        reason: FinishReason,
        details: FinishDetails,
        usage: Usage,
    },
    Finish {
        reason: FinishReason,
        usage: Usage,
    },
    Warning {
        message: String,
    },
    /// The executor is about to retry (the engine publishes durable `retry.scheduled`,
    /// 02 §4.1; the UI shows a countdown with Retry now / Stop).
    RetryScheduled {
        attempt: u32,
        delay_ms: u64,
        kind: crate::error::ErrorKind,
        message: String,
    },
    /// Wire records produced while building the request; the engine persists them (D18).
    WireRecorded {
        records: Vec<WireRecord>,
    },
    /// Only when `include_raw`.
    Raw {
        event: Option<String>,
        data: String,
    },
}

impl StreamEvent {
    /// Whether the event carries model output (used by the executor to decide whether a
    /// failure can still be retried transparently).
    pub fn is_output(&self) -> bool {
        !matches!(
            self,
            StreamEvent::StepStart { .. }
                | StreamEvent::Raw { .. }
                | StreamEvent::Warning { .. }
                | StreamEvent::RetryScheduled { .. }
                | StreamEvent::WireRecorded { .. }
        )
    }
}
