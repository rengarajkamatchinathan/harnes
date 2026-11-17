//! OpenAI Chat Completions (03 §5): Azure Foundry non-OpenAI models, Ollama, vLLM,
//! OpenRouter, generic OpenAI-compatible. Dialects are `Compat` flags.
//!
//! Stop reasons are derived, not copied: tool calls with a missing/`stop` finish reason
//! become `ToolCalls` (KB ch.07).

use std::collections::BTreeMap;

use serde_json::{Value, json};

use super::openai_responses::openai_hint;
use super::util::{WireBody, WireItem, assemble, data_url, parse_tool_input, str_at, u64_at};
use super::{BuiltBody, Framing, Protocol, RawFrame, StreamContext};
use crate::compat::{CacheFormat, Compat, EffortWire, MaxTokensField, ReasoningField};
use crate::compile::CompiledRequest;
use crate::error::{HttpFailure, ProviderError, classify};
use crate::event::{PartIdx, StreamEvent};
use crate::model::{
    CacheHint, CacheTtl, FinishDetails, FinishReason, MediaSource, Message, Opaque, OpaqueData,
    Origin, Part, ProtocolKind, ProviderId, ReplayScope, Role, ToolCall, ToolCallId, ToolChoice,
    Usage,
};

pub struct OpenAiChat;

fn cache_part(v: Value, h: &Option<CacheHint>, compat: &Compat) -> Value {
    match (h, compat.cache_format) {
        (Some(h), Some(CacheFormat::Anthropic)) => {
            let mut v = v;
            v["cache_control"] = match h.ttl {
                CacheTtl::FiveMinutes => json!({"type": "ephemeral"}),
                CacheTtl::OneHour => json!({"type": "ephemeral", "ttl": "1h"}),
            };
            v
        }
        _ => v,
    }
}

fn user_content(parts: &[Part], compat: &Compat) -> Result<Value, ProviderError> {
    let simple = parts
        .iter()
        .all(|p| matches!(p, Part::Text(t) if t.cache.is_none()));
    if simple {
        let text: Vec<&str> = parts
            .iter()
            .filter_map(|p| {
                if let Part::Text(t) = p {
                    Some(t.text.as_str())
                } else {
                    None
                }
            })
            .collect();
        return Ok(json!(text.join("\n")));
    }
    let mut arr = vec![];
    for p in parts {
        match p {
            Part::Text(t) => arr.push(cache_part(
                json!({"type": "text", "text": t.text}),
                &t.cache,
                compat,
            )),
            Part::Media(m) if m.is_image() => {
                let url = match &m.source {
                    MediaSource::Base64 { data } => data_url(&m.mime, data),
                    MediaSource::Url { url } => url.clone(),
                    other => {
                        return Err(ProviderError::invalid_request(format!(
                            "chat: unsupported media source {other:?}"
                        )));
                    }
                };
                arr.push(json!({"type": "image_url", "image_url": {"url": url}}));
            }
            Part::Media(m) => {
                let MediaSource::Base64 { data } = &m.source else {
                    return Err(ProviderError::invalid_request("chat: files must be inline"));
                };
                arr.push(json!({"type": "file", "file": {"filename": m.filename.clone().unwrap_or_default(), "file_data": data_url(&m.mime, data)}}));
            }
            _ => {}
        }
    }
    Ok(Value::Array(arr))
}

fn normalise(
    m: &Message,
    req: &CompiledRequest,
    compat: &Compat,
    items: &mut Vec<Value>,
) -> Result<(), ProviderError> {
    match m.role {
        Role::Assistant => {
            let mut texts: Vec<&str> = vec![];
            let mut reasoning = String::new();
            let mut details: Vec<Value> = vec![];
            let mut calls = vec![];
            for p in &m.parts {
                match p {
                    Part::Text(t) if !t.text.is_empty() => texts.push(&t.text),
                    Part::Reasoning(r) => {
                        if let Some(OpaqueData::ChatReasoningDetails { value }) =
                            r.opaque.as_ref().map(|o| &o.data)
                        {
                            match value {
                                Value::Array(a) => details.extend(a.iter().cloned()),
                                v => details.push(v.clone()),
                            }
                        }
                        reasoning.push_str(&r.text);
                    }
                    Part::ToolCall(c) if !c.provider_executed => calls.push(json!({
                        "id": req.wire_tool_id(&c.id),
                        "type": "function",
                        "function": {"name": c.name, "arguments": c.input.to_string()},
                    })),
                    Part::Compaction(c) => {
                        if let Some(s) = &c.summary {
                            texts.push(s);
                        }
                    }
                    _ => {}
                }
            }
            let text = texts.join("\n\n");
            let mut msg = json!({"role": "assistant", "content": if text.is_empty() && !calls.is_empty() { Value::Null } else { json!(text) }});
            if !calls.is_empty() {
                msg["tool_calls"] = Value::Array(calls);
            }
            match compat.reasoning_field {
                Some(ReasoningField::ReasoningContent) if !reasoning.is_empty() => {
                    msg["reasoning_content"] = json!(reasoning)
                }
                Some(ReasoningField::Reasoning) if !reasoning.is_empty() => {
                    msg["reasoning"] = json!(reasoning)
                }
                Some(ReasoningField::ReasoningDetails) if !details.is_empty() => {
                    msg["reasoning_details"] = Value::Array(details)
                }
                _ => {}
            }
            items.push(msg);
        }
        Role::System => {
            let text: Vec<String> = m
                .parts
                .iter()
                .filter_map(|p| {
                    if let Part::Text(t) = p {
                        Some(t.text.clone())
                    } else {
                        None
                    }
                })
                .collect();
            items.push(json!({"role": "system", "content": text.join("\n")}));
        }
        Role::User | Role::Tool => {
            let mut rest = vec![];
            for p in &m.parts {
                match p {
                    Part::ToolResult(r) => items.push(json!({
                        "role": "tool",
                        "tool_call_id": req.wire_tool_id(&r.call_id),
                        "content": r.content.to_text(),
                    })),
                    Part::EffortMarker(e) => items.push(json!({"role": "system", "content": format!("Reasoning effort is now {:?}.", e.effort)})),
                    Part::Compaction(c) => {
                        if let Some(s) = &c.summary {
                            rest.push(Part::text(s.clone()));
                        }
                    }
                    other => rest.push(other.clone()),
                }
            }
            if !rest.is_empty() {
                items.push(json!({"role": "user", "content": user_content(&rest, compat)?}));
            }
        }
    }
    Ok(())
}

impl Protocol for OpenAiChat {
    type Body = WireBody;
    type Frame = Frame;
    type State = State;
    const KIND: ProtocolKind = ProtocolKind::OpenaiChat;
    const FRAMING: Framing = Framing::SseWithDone;
    const REPLAY: ReplayScope = ReplayScope::SameModel;

    fn build_body(
        &self,
        req: &CompiledRequest,
        compat: &Compat,
    ) -> Result<BuiltBody<WireBody>, ProviderError> {
        let r = &req.request;
        let mut body = WireBody {
            list_key: "messages",
            ..Default::default()
        };
        let system: Vec<_> = r.system.iter().filter(|s| !s.text.is_empty()).collect();
        let (sys, baseline) = if compat.baseline_as_user && system.len() > 1 {
            (&system[..1], Some(&system[1..]))
        } else {
            (&system[..], None)
        };
        for s in sys {
            let content = if compat.cache_format.is_some() && s.cache.is_some() {
                json!([cache_part(
                    json!({"type": "text", "text": s.text}),
                    &s.cache,
                    compat
                )])
            } else {
                json!(s.text)
            };
            body.list.push(WireItem::Json(
                json!({"role": "system", "content": content}),
            ));
        }
        if let Some(b) = baseline {
            let text: Vec<&str> = b.iter().map(|s| s.text.as_str()).collect();
            body.list.push(WireItem::Json(json!({"role": "user", "content": format!("<context>\n{}\n</context>", text.join("\n\n"))})));
        }
        let records = assemble(req, &mut body.list, |m, items| {
            normalise(m, req, compat, items)
        })?;
        body.head.insert("model".into(), json!(r.model.wire_id));
        let t = &mut body.tail;
        if !r.tools.is_empty() {
            t.insert(
                "tools".into(),
                Value::Array(
                    r.tools
                        .iter()
                        .map(|d| cache_part(json!({"type": "function", "function": {"name": d.name, "description": d.description, "parameters": d.input_schema}}), &d.cache, compat))
                        .collect(),
                ),
            );
            t.insert(
                "tool_choice".into(),
                match &r.tool_choice {
                    ToolChoice::Auto => json!("auto"),
                    ToolChoice::Required => json!("required"),
                    ToolChoice::None => json!("none"),
                    ToolChoice::Tool { name } => {
                        json!({"type": "function", "function": {"name": name}})
                    }
                },
            );
        }
        if let Some(n) = r.max_output_tokens {
            let field = match compat.max_tokens_field {
                MaxTokensField::MaxTokens => "max_tokens",
                MaxTokensField::MaxCompletionTokens => "max_completion_tokens",
            };
            t.insert(field.into(), json!(n));
        }
        if let Some(e) = r.effort
            && let Some(EffortWire::Effort(level)) = r.model.effort_wire(e)
        {
            t.insert("reasoning_effort".into(), json!(level));
        }
        if let Some(v) = r.sampling.temperature {
            t.insert("temperature".into(), json!(v));
        }
        if let Some(v) = r.sampling.top_p {
            t.insert("top_p".into(), json!(v));
        }
        if let Some(v) = r.sampling.top_k {
            t.insert("top_k".into(), json!(v));
        }
        if !r.sampling.stop.is_empty() {
            t.insert("stop".into(), json!(r.sampling.stop));
        }
        t.insert("stream".into(), json!(true));
        if compat.stream_usage {
            t.insert("stream_options".into(), json!({"include_usage": true}));
        }
        Ok(BuiltBody { body, records })
    }

    fn decode(&self, frame: &RawFrame) -> Result<Option<Frame>, ProviderError> {
        let d = frame.data.trim();
        if d == "[DONE]" {
            return Ok(Some(Frame::Done));
        }
        if d.is_empty() || d == "null" {
            return Ok(None);
        }
        serde_json::from_str::<Value>(d)
            .map(|v| Some(Frame::Chunk(v)))
            .map_err(|e| ProviderError::invalid_output(format!("chat: bad JSON chunk: {e}")))
    }

    fn begin(&self, st: &mut State, ctx: &StreamContext) {
        st.origin = Some(ctx.origin.clone());
        st.require_finish_reason = ctx.compat.require_finish_reason;
    }

    fn step(
        &self,
        st: &mut State,
        f: Frame,
        out: &mut Vec<StreamEvent>,
    ) -> Result<(), ProviderError> {
        match f {
            Frame::Done => {
                st.saw_done = true;
                st.finish(out)
            }
            Frame::Chunk(v) => st.chunk(v, out),
        }
    }

    fn on_halt(&self, mut st: State, out: &mut Vec<StreamEvent>) -> Result<(), ProviderError> {
        if st.finished {
            return Ok(());
        }
        if st.finish_reason.is_some() {
            return st.finish(out);
        }
        Err(ProviderError::incomplete(
            "chat: stream ended without finish_reason or [DONE]",
        ))
    }

    fn classify(&self, f: &HttpFailure) -> ProviderError {
        classify(f, openai_hint(f.json().as_ref()))
    }
}

#[derive(Debug)]
pub enum Frame {
    Chunk(Value),
    Done,
}

#[derive(Debug)]
struct PendingCall {
    part: PartIdx,
    id: String,
    name: String,
    args: String,
}

#[derive(Debug, Default)]
pub struct State {
    origin: Option<Origin>,
    require_finish_reason: bool,
    next_part: PartIdx,
    started: bool,
    finished: bool,
    saw_done: bool,
    text: Option<PartIdx>,
    reasoning: Option<(PartIdx, String, Vec<Value>)>,
    calls: BTreeMap<u64, PendingCall>,
    last_call_key: Option<u64>,
    finish_reason: Option<String>,
    usage: Usage,
}

pub fn read_usage(u: &Value) -> Usage {
    let prompt = u64_at(u, &["prompt_tokens"]);
    let cached = u64_at(u, &["prompt_tokens_details", "cached_tokens"]);
    Usage {
        non_cached_input: prompt.saturating_sub(cached),
        cache_read: cached,
        output_total: u64_at(u, &["completion_tokens"]),
        reasoning: u
            .get("completion_tokens_details")
            .map(|_| u64_at(u, &["completion_tokens_details", "reasoning_tokens"])),
        ..Default::default()
    }
    .finalize()
}

impl State {
    fn part(&mut self) -> PartIdx {
        let p = self.next_part;
        self.next_part += 1;
        p
    }

    fn origin(&self) -> Origin {
        self.origin.clone().unwrap_or(Origin {
            provider: ProviderId::new("openai-compatible"),
            model: String::new(),
            protocol: ProtocolKind::OpenaiChat,
            account: None,
        })
    }

    fn close_reasoning(&mut self, out: &mut Vec<StreamEvent>) {
        if let Some((part, text, details)) = self.reasoning.take() {
            let opaque = (!details.is_empty()).then(|| Opaque {
                origin: self.origin(),
                data: OpaqueData::ChatReasoningDetails {
                    value: Value::Array(details),
                },
            });
            out.push(StreamEvent::ReasoningEnd {
                part,
                final_text: Some(text),
                opaque,
            });
        }
    }

    fn close_text(&mut self, out: &mut Vec<StreamEvent>) {
        if let Some(part) = self.text.take() {
            out.push(StreamEvent::TextEnd {
                part,
                citations: vec![],
                opaque: None,
            });
        }
    }

    fn chunk(&mut self, v: Value, out: &mut Vec<StreamEvent>) -> Result<(), ProviderError> {
        if let Some(err) = v.get("error") {
            let f = HttpFailure {
                status: 500,
                headers: vec![],
                body: json!({"error": err}).to_string(),
            };
            let mut e = classify(&f, openai_hint(f.json().as_ref()));
            e.status = None;
            return Err(e);
        }
        if !self.started {
            self.started = true;
            out.push(StreamEvent::StepStart { step: 0 });
        }
        if let (Some(m), Some(o)) = (str_at(&v, "model"), self.origin.as_mut()) {
            o.model = m.to_string();
        }
        if let Some(u) = v.get("usage").filter(|u| u.is_object()) {
            self.usage = read_usage(u);
        }
        let Some(choice) = v
            .get("choices")
            .and_then(Value::as_array)
            .and_then(|c| c.first())
        else {
            return Ok(());
        };
        let delta = choice.get("delta").cloned().unwrap_or(Value::Null);
        // Reasoning: any dialect, whatever the route's compat says (tolerant).
        let r_text = str_at(&delta, "reasoning_content")
            .or_else(|| str_at(&delta, "reasoning"))
            .map(String::from);
        let r_details = delta
            .get("reasoning_details")
            .and_then(Value::as_array)
            .cloned();
        if r_text.as_deref().is_some_and(|t| !t.is_empty())
            || r_details.as_ref().is_some_and(|d| !d.is_empty())
        {
            if self.reasoning.is_none() {
                let part = self.part();
                out.push(StreamEvent::ReasoningStart { part });
                self.reasoning = Some((part, String::new(), vec![]));
            }
            if let Some((part, text, details)) = self.reasoning.as_mut() {
                let mut d = r_text.unwrap_or_default();
                if let Some(ds) = r_details {
                    if d.is_empty() {
                        d = ds
                            .iter()
                            .filter_map(|x| str_at(x, "text").or_else(|| str_at(x, "summary")))
                            .collect();
                    }
                    details.extend(ds);
                }
                if !d.is_empty() {
                    text.push_str(&d);
                    out.push(StreamEvent::ReasoningDelta {
                        part: *part,
                        delta: d,
                    });
                }
            }
        }
        if let Some(c) = str_at(&delta, "content").filter(|c| !c.is_empty()) {
            self.close_reasoning(out);
            let part = match self.text {
                Some(p) => p,
                None => {
                    let p = self.part();
                    out.push(StreamEvent::TextStart { part: p });
                    self.text = Some(p);
                    p
                }
            };
            out.push(StreamEvent::TextDelta {
                part,
                delta: c.to_string(),
            });
        }
        if let Some(calls) = delta.get("tool_calls").and_then(Value::as_array) {
            self.close_reasoning(out);
            self.close_text(out);
            for (n, tc) in calls.iter().enumerate() {
                // Some servers omit `index`: fall back to an id match, then the last call.
                let id = str_at(tc, "id").filter(|s| !s.is_empty());
                let key = tc
                    .get("index")
                    .and_then(Value::as_u64)
                    .or_else(|| {
                        id.and_then(|id| {
                            self.calls.iter().find(|(_, c)| c.id == id).map(|(k, _)| *k)
                        })
                    })
                    .or_else(|| {
                        if id.is_some() {
                            None
                        } else {
                            self.last_call_key
                        }
                    })
                    .unwrap_or(self.calls.len() as u64 + n as u64);
                let f = tc.get("function").cloned().unwrap_or(Value::Null);
                if !self.calls.contains_key(&key) {
                    let part = self.part();
                    let id = id
                        .map(String::from)
                        .unwrap_or_else(|| format!("call_{part}"));
                    let name = str_at(&f, "name").unwrap_or("").to_string();
                    out.push(StreamEvent::ToolInputStart {
                        part,
                        call_id: ToolCallId::new(id.clone()),
                        name: name.clone(),
                        provider_executed: false,
                    });
                    self.calls.insert(
                        key,
                        PendingCall {
                            part,
                            id,
                            name,
                            args: String::new(),
                        },
                    );
                }
                self.last_call_key = Some(key);
                if let Some(c) = self.calls.get_mut(&key) {
                    if c.name.is_empty()
                        && let Some(n) = str_at(&f, "name")
                    {
                        c.name = n.to_string();
                    }
                    if let Some(a) = str_at(&f, "arguments").filter(|a| !a.is_empty()) {
                        c.args.push_str(a);
                        out.push(StreamEvent::ToolInputDelta {
                            part: c.part,
                            json_fragment: a.to_string(),
                        });
                    }
                }
            }
        }
        if let Some(fr) = str_at(choice, "finish_reason") {
            self.finish_reason = Some(fr.to_string());
        }
        Ok(())
    }

    fn finish(&mut self, out: &mut Vec<StreamEvent>) -> Result<(), ProviderError> {
        if self.finished {
            return Ok(());
        }
        if self.finish_reason.is_none() && self.require_finish_reason && !self.saw_done {
            return Err(ProviderError::incomplete("chat: no finish_reason"));
        }
        self.close_reasoning(out);
        self.close_text(out);
        let had_calls = !self.calls.is_empty();
        for (_, c) in std::mem::take(&mut self.calls) {
            match parse_tool_input(&c.args) {
                Ok(input) => out.push(StreamEvent::ToolInputEnd {
                    part: c.part,
                    call: ToolCall {
                        id: ToolCallId::new(c.id),
                        name: c.name,
                        input,
                        provider_executed: false,
                        opaque: None,
                    },
                }),
                Err(error) => out.push(StreamEvent::ToolInputError {
                    part: c.part,
                    call_id: ToolCallId::new(c.id),
                    raw: c.args,
                    error,
                }),
            }
        }
        let reason = match self.finish_reason.as_deref() {
            _ if had_calls
                && !matches!(
                    self.finish_reason.as_deref(),
                    Some("length" | "content_filter")
                ) =>
            {
                FinishReason::ToolCalls
            }
            Some("stop") | None => FinishReason::Stop,
            Some("length") => FinishReason::Length,
            Some("tool_calls" | "function_call") => FinishReason::ToolCalls,
            Some("content_filter") => FinishReason::ContentFilter,
            Some(_) => FinishReason::Unknown,
        };
        let usage = self.usage;
        out.push(StreamEvent::StepFinish {
            reason,
            details: FinishDetails {
                raw: self.finish_reason.clone(),
                ..Default::default()
            },
            usage,
        });
        out.push(StreamEvent::Finish { reason, usage });
        self.finished = true;
        Ok(())
    }
}
