//! The compile step (03 §4): canonical history → what an adapter serialises.
//!
//! Deterministic for the same inputs and append-only over history already sent: messages
//! with a stored [`WireRecord`] for this route are replayed byte-for-byte, only new
//! messages are normalised (D18).

use std::collections::{HashMap, HashSet};

use sha2::{Digest, Sha256};

use crate::compat::{Compat, SchemaSanitizer, ToolIdRule};
use crate::error::ProviderError;
use crate::model::{
    AccountTag, CacheHint, CachePolicy, CacheTtl, LlmRequest, MediaSource, Message, MessageId,
    Opaque, Origin, Part, ProtocolKind, ProviderId, ReplayScope, Role, Sampling, TextPart,
    ToolCallId, ToolOutput, ToolResult, WireRecord,
};

/// The destination as the compile step sees it.
#[derive(Clone, Debug)]
pub struct RouteInfo {
    pub provider: ProviderId,
    pub protocol: ProtocolKind,
    pub replay: ReplayScope,
    pub account: Option<AccountTag>,
    /// Route compat with the model's patches already applied.
    pub compat: Compat,
    /// Protocol + endpoint + model; stored records are reused only on an exact match.
    pub route_key: String,
}

#[derive(Clone, Debug, PartialEq)]
pub enum WireMessage {
    /// Replayed byte-for-byte.
    Stored(WireRecord),
    /// Normalised by the adapter in this request.
    Fresh(Message),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CompileNote(pub String);

#[derive(Clone, Debug)]
pub struct CompiledRequest {
    pub wire_history: Vec<WireMessage>,
    /// The request with system/tool cache hints placed, sampling and effort filtered.
    pub request: LlmRequest,
    pub route_key: String,
    pub protocol: ProtocolKind,
    /// Tail cache breakpoint, applied by the adapter to the last history item at
    /// serialisation time (never stored in a wire record, so old breakpoints don't pile up).
    pub tail_cache: Option<CacheTtl>,
    /// canonical id → wire id, persisted with the wire record (03 §4 step 5).
    pub tool_id_map: Vec<(ToolCallId, String)>,
    pub diagnostics: Vec<CompileNote>,
}

impl CompiledRequest {
    pub fn wire_tool_id(&self, id: &ToolCallId) -> String {
        self.tool_id_map
            .iter()
            .find(|(c, _)| c == id)
            .map(|(_, w)| w.clone())
            .unwrap_or_else(|| id.0.clone())
    }
    pub fn note(&mut self, s: impl Into<String>) {
        self.diagnostics.push(CompileNote(s.into()));
    }
}

/// Whether an opaque payload from `origin` may be sent to the destination.
pub fn in_scope(origin: &Origin, dest: &RouteInfo, dest_model: &str) -> bool {
    if origin.protocol != dest.protocol {
        return false;
    }
    match dest.replay {
        ReplayScope::ProtocolFamilySameAccount => match (&origin.account, &dest.account) {
            (Some(a), Some(b)) => a == b,
            (Some(_), None) => false,
            (None, _) => true,
        },
        ReplayScope::ModelFamily => {
            origin.provider == dest.provider
                && model_family(&origin.model) == model_family(dest_model)
        }
        ReplayScope::SameModel => origin.provider == dest.provider && origin.model == dest_model,
        ReplayScope::Never => false,
    }
}

/// Approximate model family from a wire id: drop a `-YYYY-MM-DD` / `-latest` / `-preview`
/// suffix and keep the first two dash components (`gpt-5.1-mini-2026-01-01` → `gpt-5.1`).
pub fn model_family(id: &str) -> String {
    let parts: Vec<&str> = id
        .split('-')
        .filter(|p| !(p.len() == 4 && p.chars().all(|c| c.is_ascii_digit())))
        .filter(|p| !(p.len() == 2 && p.chars().all(|c| c.is_ascii_digit())))
        .filter(|p| !matches!(*p, "latest" | "preview"))
        .collect();
    parts.iter().take(2).copied().collect::<Vec<_>>().join("-")
}

/// Wire id for a canonical tool-call id under `rule`. Pure function of the id, so stored
/// records and fresh messages agree.
pub fn rewrite_tool_id(id: &str, rule: ToolIdRule) -> String {
    let hash = || hex::encode(Sha256::digest(id.as_bytes()));
    match rule {
        ToolIdRule::Keep | ToolIdRule::Omit => id.to_string(),
        ToolIdRule::AnthropicCharset => {
            let s: String = id
                .chars()
                .map(|c| {
                    if c.is_ascii_alphanumeric() || c == '_' || c == '-' {
                        c
                    } else {
                        '_'
                    }
                })
                .collect();
            if s.len() > 64 {
                format!("{}_{}", &s[..47], &hash()[..16])
            } else {
                s
            }
        }
        ToolIdRule::Max40 => {
            if id.len() <= 40 {
                id.to_string()
            } else {
                let mut cut = 23;
                while !id.is_char_boundary(cut) {
                    cut -= 1;
                }
                format!("{}_{}", &id[..cut], &hash()[..16])
            }
        }
        ToolIdRule::Mistral9 => {
            let h = Sha256::digest(id.as_bytes());
            const A: &[u8] = b"abcdefghijklmnopqrstuvwxyzABCDEFGHIJKLMNOPQRSTUVWXYZ0123456789";
            h.iter()
                .take(9)
                .map(|b| A[(*b as usize) % A.len()] as char)
                .collect()
        }
    }
}

/// Strip JSON-Schema keywords Gemini rejects and coerce `const` into `enum`.
pub fn sanitize_schema(v: &mut serde_json::Value, kind: SchemaSanitizer) {
    use serde_json::Value;
    match kind {
        SchemaSanitizer::ObjectRoot => {
            if v.get("type").and_then(Value::as_str) != Some("object") {
                let inner = std::mem::take(v);
                *v = serde_json::json!({"type": "object", "properties": {"value": inner}, "required": ["value"]});
            }
        }
        SchemaSanitizer::Gemini => {
            const DROP: &[&str] = &[
                "$schema",
                "$id",
                "$ref",
                "$defs",
                "definitions",
                "additionalProperties",
                "patternProperties",
                "unevaluatedProperties",
                "examples",
                "default",
                "title",
                "exclusiveMinimum",
                "exclusiveMaximum",
                "multipleOf",
                "if",
                "then",
                "else",
                "not",
                "dependentRequired",
                "dependentSchemas",
            ];
            fn walk(v: &mut Value) {
                match v {
                    Value::Object(m) => {
                        for k in DROP {
                            m.remove(*k);
                        }
                        if let Some(c) = m.remove("const") {
                            m.insert("enum".into(), Value::Array(vec![c]));
                        }
                        if let Some(Value::String(f)) = m.get("format")
                            && !matches!(f.as_str(), "enum" | "date-time")
                        {
                            m.remove("format");
                        }
                        // `type: ["string","null"]` → `type: "string", nullable: true`
                        if let Some(Value::Array(ts)) = m.get("type").cloned() {
                            let non_null: Vec<_> = ts
                                .iter()
                                .filter(|t| t.as_str() != Some("null"))
                                .cloned()
                                .collect();
                            if non_null.len() < ts.len() {
                                m.insert("nullable".into(), Value::Bool(true));
                            }
                            if let Some(first) = non_null.into_iter().next() {
                                m.insert("type".into(), first);
                            }
                        }
                        for (_, child) in m.iter_mut() {
                            walk(child);
                        }
                    }
                    Value::Array(a) => a.iter_mut().for_each(walk),
                    _ => {}
                }
            }
            walk(v);
        }
    }
}

/// Run the compile step for `req` against `route`.
pub fn compile(mut req: LlmRequest, route: &RouteInfo) -> Result<CompiledRequest, ProviderError> {
    let compat = &route.compat;
    let mut notes: Vec<CompileNote> = vec![];
    let dest_model = req.model.wire_id.clone();

    // 1. Compaction: the latest compaction message replaces everything before it.
    let mut messages = std::mem::take(&mut req.messages);
    if let Some(i) = messages
        .iter()
        .rposition(|m| m.parts.iter().any(|p| matches!(p, Part::Compaction(_))))
        && i > 0
    {
        notes.push(CompileNote(format!(
            "compaction: {i} earlier messages replaced"
        )));
        messages.drain(..i);
    }

    // 4. Tool history repair (over the whole path, before splitting stored/fresh).
    repair_tool_history(&mut messages, compat, &mut notes);

    // 2. Reuse stored wire records for the leading run of messages sent on this route.
    let records: HashMap<&MessageId, &WireRecord> = req
        .wire_records
        .iter()
        .filter(|r| r.route_key == route.route_key)
        .map(|r| (&r.message_id, r))
        .collect();
    let mut wire_history = vec![];
    let mut fresh_from = messages.len();
    for (i, m) in messages.iter().enumerate() {
        match records.get(&m.id) {
            Some(r) => wire_history.push(WireMessage::Stored((*r).clone())),
            None => {
                fresh_from = i;
                break;
            }
        }
    }
    let mut tool_ids: Vec<(ToolCallId, String)> = vec![];
    let mut dropped_reasoning = 0usize;
    for mut m in messages.into_iter().skip(fresh_from) {
        // 3. Replay filter.
        m.parts = m
            .parts
            .into_iter()
            .filter_map(|p| replay_filter(p, route, &dest_model, &mut dropped_reasoning))
            .collect();
        // 9. Attachments.
        let mut parts = Vec::with_capacity(m.parts.len());
        for p in m.parts {
            match p {
                Part::Media(media) => {
                    if let MediaSource::Blob { id } = &media.source {
                        return Err(ProviderError::invalid_request(format!(
                            "blob {id} was not resolved before compile"
                        )));
                    }
                    if media.is_image() && !req.model.input.image {
                        return Err(ProviderError::invalid_request(format!(
                            "{} can't read images. Remove the image or pick a model with vision.",
                            req.model.wire_id
                        )));
                    }
                    if media.is_pdf() && !req.model.input.pdf {
                        match &media.extracted_text {
                            Some(t) => {
                                notes.push(CompileNote("pdf sent as extracted text".into()));
                                let name = media
                                    .filename
                                    .clone()
                                    .unwrap_or_else(|| "document.pdf".into());
                                parts.push(Part::text(format!("[{name}]\n{t}")));
                            }
                            None => {
                                return Err(ProviderError::invalid_request(format!(
                                    "{} can't read PDFs and no text could be extracted.",
                                    req.model.wire_id
                                )));
                            }
                        }
                        continue;
                    }
                    parts.push(Part::Media(media));
                }
                Part::EffortMarker(_) if !compat.mid_conversation_system => {}
                other => parts.push(other),
            }
        }
        m.parts = parts;
        // 5. Tool-id rewrite.
        if compat.tool_id_rule != ToolIdRule::Keep {
            for p in &mut m.parts {
                let id = match p {
                    Part::ToolCall(c) => &mut c.id,
                    Part::ToolResult(r) => &mut r.call_id,
                    _ => continue,
                };
                let wire = rewrite_tool_id(&id.0, compat.tool_id_rule);
                if wire != id.0 && !tool_ids.iter().any(|(c, _)| c == id) {
                    tool_ids.push((id.clone(), wire.clone()));
                }
                id.0 = wire;
            }
        }
        if m.parts.is_empty() && m.role != Role::Assistant {
            continue;
        }
        if m.parts.is_empty() {
            continue;
        }
        wire_history.push(WireMessage::Fresh(m));
    }
    if dropped_reasoning > 0 {
        notes.push(CompileNote(format!(
            "dropped {dropped_reasoning} opaque payloads: out of replay scope for {}",
            dest_model
        )));
    }

    // 6. Schema sanitising.
    if let Some(s) = compat.schema_sanitizer {
        for t in &mut req.tools {
            sanitize_schema(&mut t.input_schema, s);
        }
    }

    // 7. Cache breakpoints (D32): tools, system 1, system 2, tail.
    let mut tail_cache = None;
    if req.cache == CachePolicy::Auto {
        let budget = compat.max_cache_breakpoints.min(4) as usize;
        let long = req.baseline_long_ttl && req.model.prompt_cache.ttl_1h && req.system.len() > 1;
        // 1 h entries must precede 5 m ones, so everything up to the baseline gets 1 h.
        let early_ttl = if long {
            CacheTtl::OneHour
        } else {
            CacheTtl::FiveMinutes
        };
        let mut used = 0;
        let mut place = |slot: &mut Option<CacheHint>, ttl: CacheTtl| {
            if used < budget {
                *slot = Some(CacheHint { ttl });
                used += 1;
            }
        };
        if let Some(t) = req.tools.last_mut() {
            place(&mut t.cache, early_ttl);
        }
        for s in req.system.iter_mut().take(2) {
            if !s.text.is_empty() {
                place(&mut s.cache, early_ttl);
            }
        }
        if used < budget && !wire_history.is_empty() {
            tail_cache = Some(CacheTtl::FiveMinutes);
        }
    }

    // 8. Effort and sampling.
    if let Some(e) = req.effort
        && req.model.effort_wire(e).is_none()
    {
        notes.push(CompileNote(format!(
            "effort {e:?} not supported by {dest_model}; not sent"
        )));
        req.effort = None;
    }
    req.sampling = filter_sampling(&req, compat, &mut notes);

    for n in &notes {
        tracing::debug!(target: "genie_providers::compile", "{}", n.0);
    }
    let protocol = route.protocol;
    Ok(CompiledRequest {
        wire_history,
        request: req,
        route_key: route.route_key.clone(),
        protocol,
        tail_cache,
        tool_id_map: tool_ids,
        diagnostics: notes,
    })
}

fn filter_sampling(req: &LlmRequest, compat: &Compat, notes: &mut Vec<CompileNote>) -> Sampling {
    use crate::model::SamplingParam as P;
    let s = &req.sampling;
    let ok = |p| compat.sampling_params_allowed && req.model.supports_sampling(p);
    let out = Sampling {
        temperature: s.temperature.filter(|_| ok(P::Temperature)),
        top_p: s.top_p.filter(|_| ok(P::TopP)),
        top_k: s.top_k.filter(|_| ok(P::TopK)),
        stop: if ok(P::Stop) { s.stop.clone() } else { vec![] },
    };
    if &out != s {
        notes.push(CompileNote("unsupported sampling settings not sent".into()));
    }
    out
}

fn replay_filter(
    p: Part,
    route: &RouteInfo,
    dest_model: &str,
    dropped: &mut usize,
) -> Option<Part> {
    let ok = |o: &Opaque| in_scope(&o.origin, route, dest_model);
    match p {
        Part::Reasoning(mut r) => match &r.opaque {
            Some(o) if ok(o) => Some(Part::Reasoning(r)),
            other => {
                if other.is_some() {
                    *dropped += 1;
                }
                r.opaque = None;
                // Out of scope → readable text only (nothing for empty/redacted reasoning).
                (!r.text.is_empty() && !r.redacted).then(|| Part::text(r.text))
            }
        },
        Part::Text(mut t) => {
            if t.opaque.as_ref().is_some_and(|o| !ok(o)) {
                *dropped += 1;
                t.opaque = None;
            }
            Some(Part::Text(t))
        }
        Part::ToolCall(mut c) => {
            if c.opaque.as_ref().is_some_and(|o| !ok(o)) {
                *dropped += 1;
                c.opaque = None;
                if c.provider_executed {
                    // A server tool call can only be replayed as its raw block.
                    return None;
                }
            }
            Some(Part::ToolCall(c))
        }
        Part::ToolResult(mut r) => {
            if r.opaque.as_ref().is_some_and(|o| !ok(o)) {
                *dropped += 1;
                r.opaque = None;
            }
            Some(Part::ToolResult(r))
        }
        Part::Compaction(mut c) => {
            if c.opaque.as_ref().is_some_and(|o| !ok(o)) {
                *dropped += 1;
                c.opaque = None;
            }
            match (&c.opaque, &c.summary) {
                (None, None) => None,
                (None, Some(s)) => Some(Part::Text(TextPart {
                    text: s.clone(),
                    ..Default::default()
                })),
                _ => Some(Part::Compaction(c)),
            }
        }
        Part::Unknown(u) => {
            if in_scope(&u.origin, route, dest_model) {
                Some(Part::Unknown(u))
            } else {
                *dropped += 1;
                None
            }
        }
        other => Some(other),
    }
}

/// Orphan calls get a synthetic "interrupted" error result; orphan results are dropped;
/// `require_assistant_after_tool` inserts an empty assistant turn before a user turn.
fn repair_tool_history(messages: &mut Vec<Message>, compat: &Compat, notes: &mut Vec<CompileNote>) {
    let mut out: Vec<Message> = Vec::with_capacity(messages.len());
    let all_results: HashSet<ToolCallId> = messages
        .iter()
        .flat_map(|m| m.parts.iter())
        .filter_map(|p| match p {
            Part::ToolResult(r) => Some(r.call_id.clone()),
            _ => None,
        })
        .collect();
    let mut known_calls: HashSet<ToolCallId> = HashSet::new();
    let n = messages.len();
    for (i, mut m) in std::mem::take(messages).into_iter().enumerate() {
        // Drop results whose call never appeared.
        let before = m.parts.len();
        m.parts.retain(|p| match p {
            Part::ToolResult(r) => known_calls.contains(&r.call_id),
            _ => true,
        });
        if m.parts.len() < before {
            notes.push(CompileNote(format!(
                "dropped {} orphan tool results",
                before - m.parts.len()
            )));
            if m.parts.is_empty() {
                continue;
            }
        }
        let orphans: Vec<ToolCallId> = m
            .parts
            .iter()
            .filter_map(|p| match p {
                Part::ToolCall(c) if !c.provider_executed => {
                    known_calls.insert(c.id.clone());
                    (!all_results.contains(&c.id)).then(|| c.id.clone())
                }
                _ => None,
            })
            .collect();
        let id = m.id.clone();
        // Synthetic results join the following tool message, so results stay in one turn.
        if m.role == Role::Tool
            && out
                .last()
                .is_some_and(|p| p.role == Role::Tool && p.id.0.ends_with("~repair"))
            && let Some(rep) = out.pop()
        {
            m.parts.extend(rep.parts);
        }
        out.push(m);
        // Don't invent results for the very last message: the engine is about to run them.
        if !orphans.is_empty() && i + 1 < n {
            notes.push(CompileNote(format!(
                "{} interrupted tool calls closed",
                orphans.len()
            )));
            out.push(Message::new(
                format!("{}~repair", id.0),
                Role::Tool,
                orphans
                    .into_iter()
                    .map(|call_id| {
                        Part::ToolResult(ToolResult {
                            call_id,
                            content: ToolOutput::Text("interrupted".into()),
                            is_error: true,
                            cache: None,
                            opaque: None,
                        })
                    })
                    .collect(),
            ));
        }
    }
    if compat.require_assistant_after_tool {
        let mut fixed = Vec::with_capacity(out.len());
        let mut prev_tool = false;
        for m in out {
            if prev_tool && m.role == Role::User {
                fixed.push(Message::new(
                    format!("{}~bridge", m.id.0),
                    Role::Assistant,
                    vec![Part::text("")],
                ));
            }
            prev_tool = m.role == Role::Tool;
            fixed.push(m);
        }
        out = fixed;
    }
    *messages = out;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::compat::ModelSpec;
    use crate::model::{OpaqueData, ReasoningPart, ToolCall, ToolDef};
    use serde_json::json;

    fn route(protocol: ProtocolKind, replay: ReplayScope) -> RouteInfo {
        RouteInfo {
            provider: ProviderId::new("p"),
            protocol,
            replay,
            account: None,
            compat: Compat {
                max_cache_breakpoints: 4,
                ..Compat::default()
            },
            route_key: "k".into(),
        }
    }
    fn origin(protocol: ProtocolKind, model: &str, account: Option<AccountTag>) -> Origin {
        Origin {
            provider: ProviderId::new("p"),
            model: model.into(),
            protocol,
            account,
        }
    }
    fn reasoning(o: Origin) -> Part {
        Part::Reasoning(ReasoningPart {
            text: "thought".into(),
            redacted: false,
            opaque: Some(Opaque {
                origin: o,
                data: OpaqueData::AnthropicSignature {
                    signature: "sig".into(),
                },
            }),
        })
    }

    #[test]
    fn replay_scope_rules() {
        let tag_a = AccountTag([1; 16]);
        let tag_b = AccountTag([2; 16]);
        let mut r = route(
            ProtocolKind::AnthropicMessages,
            ReplayScope::ProtocolFamilySameAccount,
        );
        r.account = Some(tag_a);
        assert!(in_scope(
            &origin(ProtocolKind::AnthropicMessages, "other-model", Some(tag_a)),
            &r,
            "m"
        ));
        assert!(!in_scope(
            &origin(ProtocolKind::AnthropicMessages, "m", Some(tag_b)),
            &r,
            "m"
        ));
        assert!(in_scope(
            &origin(ProtocolKind::AnthropicMessages, "m", None),
            &r,
            "m"
        ));
        assert!(!in_scope(&origin(ProtocolKind::Gemini, "m", None), &r, "m"));

        let r = route(ProtocolKind::OpenaiResponses, ReplayScope::ModelFamily);
        assert!(in_scope(
            &origin(ProtocolKind::OpenaiResponses, "gpt-5.1-2026-01-01", None),
            &r,
            "gpt-5.1"
        ));
        assert!(!in_scope(
            &origin(ProtocolKind::OpenaiResponses, "gpt-4o", None),
            &r,
            "gpt-5.1"
        ));

        let r = route(ProtocolKind::Gemini, ReplayScope::SameModel);
        assert!(in_scope(&origin(ProtocolKind::Gemini, "g", None), &r, "g"));
        assert!(!in_scope(&origin(ProtocolKind::Gemini, "h", None), &r, "g"));
    }

    #[test]
    fn out_of_scope_reasoning_becomes_text_and_is_noted() {
        let r = route(ProtocolKind::Gemini, ReplayScope::SameModel);
        let mut m = Message::new(
            "a1",
            Role::Assistant,
            vec![reasoning(origin(
                ProtocolKind::AnthropicMessages,
                "c",
                None,
            ))],
        );
        m.parts.push(Part::text("answer"));
        let req = LlmRequest::new(
            ModelSpec::new("p", "g", ProtocolKind::Gemini),
            vec![
                Message::user_text("u1", "hi"),
                m,
                Message::user_text("u2", "more"),
            ],
        );
        let c = compile(req, &r).unwrap();
        let WireMessage::Fresh(a) = &c.wire_history[1] else {
            panic!()
        };
        assert_eq!(a.parts[0], Part::text("thought"));
        assert!(
            c.diagnostics
                .iter()
                .any(|n| n.0.contains("dropped 1 opaque"))
        );
    }

    #[test]
    fn stored_prefix_is_reused_and_rest_is_fresh() {
        let r = route(
            ProtocolKind::AnthropicMessages,
            ReplayScope::ProtocolFamilySameAccount,
        );
        let mut req = LlmRequest::new(
            ModelSpec::new("p", "c", ProtocolKind::AnthropicMessages),
            vec![
                Message::user_text("u1", "hi"),
                Message::user_text("u2", "again"),
            ],
        );
        req.wire_records = vec![
            WireRecord {
                message_id: MessageId::new("u1"),
                route_key: "k".into(),
                items: "[{\"x\":1}]".into(),
            },
            WireRecord {
                message_id: MessageId::new("u2"),
                route_key: "other".into(),
                items: "[]".into(),
            },
        ];
        let c = compile(req, &r).unwrap();
        assert!(matches!(&c.wire_history[0], WireMessage::Stored(w) if w.items == "[{\"x\":1}]"));
        assert!(matches!(&c.wire_history[1], WireMessage::Fresh(_)));
    }

    #[test]
    fn orphan_calls_and_results_are_repaired() {
        let r = route(ProtocolKind::OpenaiChat, ReplayScope::SameModel);
        let call = |id: &str| {
            Part::ToolCall(ToolCall {
                id: ToolCallId::new(id),
                name: "t".into(),
                input: json!({}),
                provider_executed: false,
                opaque: None,
            })
        };
        let result = |id: &str| {
            Part::ToolResult(ToolResult {
                call_id: ToolCallId::new(id),
                content: ToolOutput::Text("ok".into()),
                is_error: false,
                cache: None,
                opaque: None,
            })
        };
        let msgs = vec![
            Message::user_text("u1", "go"),
            Message::new("a1", Role::Assistant, vec![call("c1"), call("c2")]),
            Message::new("t1", Role::Tool, vec![result("c1"), result("zz")]),
            Message::user_text("u2", "next"),
        ];
        let c = compile(
            LlmRequest::new(ModelSpec::new("p", "m", ProtocolKind::OpenaiChat), msgs),
            &r,
        )
        .unwrap();
        let fresh: Vec<&Message> = c
            .wire_history
            .iter()
            .filter_map(|w| {
                if let WireMessage::Fresh(m) = w {
                    Some(m)
                } else {
                    None
                }
            })
            .collect();
        assert_eq!(fresh.len(), 4);
        assert_eq!(fresh[2].id.0, "t1");
        assert_eq!(
            fresh[2].parts.len(),
            2,
            "orphan result zz dropped, synthetic c2 merged"
        );
        assert!(
            matches!(&fresh[2].parts[1], Part::ToolResult(r) if r.call_id.0 == "c2" && r.is_error)
        );

        // A trailing orphan with no following tool message gets its own repair message.
        let msgs = vec![
            Message::new("a1", Role::Assistant, vec![call("c9")]),
            Message::user_text("u2", "next"),
        ];
        let c = compile(
            LlmRequest::new(ModelSpec::new("p", "m", ProtocolKind::OpenaiChat), msgs),
            &r,
        )
        .unwrap();
        assert!(matches!(&c.wire_history[1], WireMessage::Fresh(m) if m.id.0 == "a1~repair"));
    }

    #[test]
    fn assistant_bridge_after_tool() {
        let mut r = route(ProtocolKind::OpenaiChat, ReplayScope::SameModel);
        r.compat.require_assistant_after_tool = true;
        let msgs = vec![
            Message::new(
                "a1",
                Role::Assistant,
                vec![Part::ToolCall(ToolCall {
                    id: ToolCallId::new("c1"),
                    name: "t".into(),
                    input: json!({}),
                    provider_executed: false,
                    opaque: None,
                })],
            ),
            Message::new(
                "t1",
                Role::Tool,
                vec![Part::ToolResult(ToolResult {
                    call_id: ToolCallId::new("c1"),
                    content: ToolOutput::Text("ok".into()),
                    is_error: false,
                    cache: None,
                    opaque: None,
                })],
            ),
            Message::user_text("u2", "next"),
        ];
        let c = compile(
            LlmRequest::new(ModelSpec::new("p", "m", ProtocolKind::OpenaiChat), msgs),
            &r,
        )
        .unwrap();
        assert_eq!(c.wire_history.len(), 4);
        assert!(matches!(&c.wire_history[2], WireMessage::Fresh(m) if m.role == Role::Assistant));
    }

    #[test]
    fn tool_id_rules() {
        assert_eq!(
            rewrite_tool_id("call:1/x", ToolIdRule::AnthropicCharset),
            "call_1_x"
        );
        let long = "x".repeat(60);
        let w = rewrite_tool_id(&long, ToolIdRule::Max40);
        assert!(w.len() <= 40);
        assert_eq!(w, rewrite_tool_id(&long, ToolIdRule::Max40));
        let m = rewrite_tool_id("toolu_123", ToolIdRule::Mistral9);
        assert_eq!(m.len(), 9);
        assert!(m.chars().all(|c| c.is_ascii_alphanumeric()));
    }

    #[test]
    fn gemini_schema_sanitizer() {
        let mut s = json!({
            "$schema": "x", "type": "object", "additionalProperties": false,
            "properties": {"a": {"type": ["string", "null"], "const": "k", "format": "uri"}}
        });
        sanitize_schema(&mut s, SchemaSanitizer::Gemini);
        assert_eq!(
            s,
            json!({"type": "object", "properties": {"a": {"type": "string", "nullable": true, "enum": ["k"]}}})
        );
    }

    #[test]
    fn cache_breakpoints_and_ttl_ordering() {
        let r = route(
            ProtocolKind::AnthropicMessages,
            ReplayScope::ProtocolFamilySameAccount,
        );
        let mut model = ModelSpec::new("p", "c", ProtocolKind::AnthropicMessages);
        model.prompt_cache.ttl_1h = true;
        let mut req = LlmRequest::new(model, vec![Message::user_text("u1", "hi")]);
        req.tools = vec![ToolDef {
            name: "t".into(),
            description: "d".into(),
            input_schema: json!({}),
            cache: None,
        }];
        req.system = vec![
            TextPart {
                text: "base".into(),
                ..Default::default()
            },
            TextPart {
                text: "ctx".into(),
                ..Default::default()
            },
        ];
        req.baseline_long_ttl = true;
        let c = compile(req, &r).unwrap();
        assert_eq!(
            c.request.tools[0].cache,
            Some(CacheHint {
                ttl: CacheTtl::OneHour
            })
        );
        assert_eq!(
            c.request.system[1].cache,
            Some(CacheHint {
                ttl: CacheTtl::OneHour
            })
        );
        assert_eq!(c.tail_cache, Some(CacheTtl::FiveMinutes));

        let mut r2 = r.clone();
        r2.compat.max_cache_breakpoints = 0;
        let req = LlmRequest::new(
            ModelSpec::new("p", "c", ProtocolKind::AnthropicMessages),
            vec![Message::user_text("u1", "hi")],
        );
        assert_eq!(compile(req, &r2).unwrap().tail_cache, None);
    }

    #[test]
    fn sampling_stripped_unless_supported() {
        use crate::model::SamplingParam;
        let mut r = route(
            ProtocolKind::AnthropicMessages,
            ReplayScope::ProtocolFamilySameAccount,
        );
        let mut model = ModelSpec::new("p", "c", ProtocolKind::AnthropicMessages);
        model.sampling_supported = vec![SamplingParam::Temperature];
        let mut req = LlmRequest::new(model, vec![Message::user_text("u1", "hi")]);
        req.sampling.temperature = Some(0.5);
        req.sampling.top_k = Some(3);
        let c = compile(req.clone(), &r).unwrap();
        assert_eq!(c.request.sampling.temperature, Some(0.5));
        assert_eq!(c.request.sampling.top_k, None);
        r.compat.sampling_params_allowed = false; // Anthropic 2026-10-21
        let c = compile(req, &r).unwrap();
        assert_eq!(c.request.sampling, Sampling::default());
    }

    #[test]
    fn images_refused_without_vision_and_pdf_falls_back_to_text() {
        use crate::model::MediaPart;
        let r = route(ProtocolKind::OpenaiChat, ReplayScope::SameModel);
        let img = Part::Media(MediaPart {
            mime: "image/png".into(),
            source: MediaSource::Base64 {
                data: "AA==".into(),
            },
            filename: None,
            extracted_text: None,
            cache: None,
        });
        let req = LlmRequest::new(
            ModelSpec::new("p", "m", ProtocolKind::OpenaiChat),
            vec![Message::new("u", Role::User, vec![img])],
        );
        assert!(
            compile(req, &r)
                .unwrap_err()
                .message
                .contains("can't read images")
        );
        let pdf = Part::Media(MediaPart {
            mime: "application/pdf".into(),
            source: MediaSource::Base64 {
                data: "AA==".into(),
            },
            filename: Some("a.pdf".into()),
            extracted_text: Some("hello".into()),
            cache: None,
        });
        let req = LlmRequest::new(
            ModelSpec::new("p", "m", ProtocolKind::OpenaiChat),
            vec![Message::new("u", Role::User, vec![pdf])],
        );
        let c = compile(req, &r).unwrap();
        let WireMessage::Fresh(m) = &c.wire_history[0] else {
            panic!()
        };
        assert_eq!(m.parts[0], Part::text("[a.pdf]\nhello"));
    }

    #[test]
    fn compaction_replaces_earlier_messages() {
        use crate::model::CompactionPart;
        let r = route(ProtocolKind::OpenaiChat, ReplayScope::SameModel);
        let msgs = vec![
            Message::user_text("u1", "old"),
            Message::new(
                "c",
                Role::User,
                vec![Part::Compaction(CompactionPart {
                    summary: Some("sum".into()),
                    opaque: None,
                })],
            ),
            Message::user_text("u2", "new"),
        ];
        let c = compile(
            LlmRequest::new(ModelSpec::new("p", "m", ProtocolKind::OpenaiChat), msgs),
            &r,
        )
        .unwrap();
        assert_eq!(c.wire_history.len(), 2);
    }

    #[test]
    fn deterministic() {
        let r = route(
            ProtocolKind::AnthropicMessages,
            ReplayScope::ProtocolFamilySameAccount,
        );
        let mk = || {
            LlmRequest::new(
                ModelSpec::new("p", "c", ProtocolKind::AnthropicMessages),
                vec![Message::user_text("u1", "hi")],
            )
        };
        let a = compile(mk(), &r).unwrap();
        let b = compile(mk(), &r).unwrap();
        assert_eq!(a.wire_history, b.wire_history);
        assert_eq!(a.diagnostics, b.diagnostics);
    }
}
