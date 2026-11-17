//! Canonical (03 §2) ↔ wire (02 §4) conversions.

use genie_protocol::common::Usage as WireUsage;
use genie_protocol::event::StepFinish;
use genie_protocol::message::{self as wm, InputPart, PartKind};
use genie_providers::model::{FinishReason, Part, Role, ToolOutput, Usage};
use serde_json::Value;

use crate::runtime::conversation::StoredMessage;

/// Part ids are `{message_id}.p{ordinal}`, so streamed events and `session.get` agree.
pub fn part_id(message_id: &str, ordinal: usize) -> String {
    format!("{message_id}.p{ordinal}")
}

pub fn part_kind(p: &Part) -> PartKind {
    match p {
        Part::Text(_) => PartKind::Text,
        Part::Reasoning(_) => PartKind::Reasoning,
        Part::ToolCall(_) => PartKind::ToolCall,
        Part::ToolResult(_) => PartKind::ToolResult,
        Part::Media(_) => PartKind::Media,
        Part::Compaction(_) => PartKind::Compaction,
        Part::EffortMarker(_) | Part::Unknown(_) => PartKind::Unknown,
    }
}

fn tool_content(o: &ToolOutput) -> Value {
    match o {
        ToolOutput::Text(t) => Value::String(t.clone()),
        ToolOutput::Json(v) => v.clone(),
        ToolOutput::Rich(_) => Value::String(o.to_text()),
    }
}

/// The client-facing part (opaque provider data never leaves the daemon).
pub fn wire_part(id: String, p: &Part, spill_ref: Option<String>) -> wm::Part {
    match p {
        Part::Text(t) => wm::Part::Text {
            id,
            text: t.text.clone(),
            citations: t
                .citations
                .iter()
                .map(|c| wm::Citation {
                    start: c.span.map_or(0, |s| u32::try_from(s.0).unwrap_or(u32::MAX)),
                    end: c.span.map_or(0, |s| u32::try_from(s.1).unwrap_or(u32::MAX)),
                    url: c.source.url.clone(),
                    title: c.source.title.clone(),
                    session_id: None,
                    message_id: None,
                })
                .collect(),
        },
        Part::Reasoning(r) => wm::Part::Reasoning {
            id,
            text: r.text.clone(),
            redacted: r.redacted,
        },
        Part::ToolCall(c) => wm::Part::ToolCall {
            id,
            call_id: c.id.0.clone(),
            name: c.name.clone(),
            input: c.input.clone(),
            provider_executed: c.provider_executed,
        },
        Part::ToolResult(r) => wm::Part::ToolResult {
            id,
            call_id: r.call_id.0.clone(),
            content: tool_content(&r.content),
            is_error: r.is_error,
            spill_ref,
        },
        Part::Media(m) => wm::Part::Media {
            id,
            blob_id: match &m.source {
                genie_providers::model::MediaSource::Blob { id } => id.0.clone(),
                _ => String::new(),
            },
            mime: m.mime.clone(),
            name: m.filename.clone(),
        },
        Part::Compaction(c) => wm::Part::Compaction {
            id,
            summary: c.summary.clone(),
            native: c.opaque.is_some(),
        },
        Part::EffortMarker(_) | Part::Unknown(_) => wm::Part::Unknown,
    }
}

/// `session.prompt` parts → canonical parts. Attachments, MCP resources, skills and file
/// references are placeholders until their subsystems resolve them (06 §2.5, §7, §9).
pub fn input_parts(parts: &[InputPart]) -> Vec<Part> {
    parts
        .iter()
        .map(|p| match p {
            InputPart::Text { text } => Part::text(text.clone()),
            InputPart::Attachment { name, mime, .. } => Part::text(format!(
                "[The user attached {name} ({mime}); attachments are not readable in this build.]"
            )),
            InputPart::Resource { server, uri } => Part::text(format!(
                "[MCP resource {server}:{uri} is not available in this build.]"
            )),
            InputPart::Skill { id } => Part::text(format!("[Skill {id} requested.]")),
            InputPart::FileRef { path } => Part::text(format!("[File reference: {path}]")),
        })
        .collect()
}

pub fn usage(u: &Usage) -> WireUsage {
    WireUsage {
        input_tokens: u.input_total,
        output_tokens: u.output_total,
        cache_read_tokens: u.cache_read,
        cache_write_tokens: u.cache_write_5m + u.cache_write_1h,
        reasoning_tokens: u.reasoning.unwrap_or(0),
    }
}

pub fn step_finish(r: FinishReason) -> StepFinish {
    match r {
        FinishReason::Stop | FinishReason::Paused => StepFinish::Stop,
        FinishReason::ToolCalls => StepFinish::ToolUse,
        FinishReason::Length => StepFinish::Length,
        FinishReason::ContentFilter | FinishReason::Refusal => StepFinish::ContentFilter,
        FinishReason::Error => StepFinish::Error,
        FinishReason::Unknown => StepFinish::Unknown,
    }
}

/// A stored message as `session.get` returns it.
pub fn message(m: &StoredMessage) -> wm::Message {
    let id = m.id().to_string();
    wm::Message {
        parent_id: m.parent_id.clone(),
        role: match m.message.role {
            Role::User => wm::Role::User,
            Role::Assistant => wm::Role::Assistant,
            Role::System => wm::Role::System,
            Role::Tool => wm::Role::Tool,
        },
        parts: m
            .message
            .parts
            .iter()
            .enumerate()
            .map(|(i, p)| wire_part(part_id(&id, i), p, None))
            .collect(),
        created_at: genie_protocol::rfc3339_ms(m.created_ms),
        model: m.model.clone(),
        turn_id: m.turn_id.clone(),
        id,
    }
}
