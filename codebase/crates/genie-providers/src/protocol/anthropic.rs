//! Anthropic Messages (03 §5): Anthropic API, Claude in Foundry, Claude on Vertex.
//!
//! Thinking / redacted-thinking blocks and signatures are kept byte-exact; unknown stream
//! events and block types are tolerated (`fallback` → `ModelSwitched` + opaque block).

use std::collections::HashMap;

use serde::Deserialize;
use serde_json::{Value, json};

use super::util::{WireBody, WireItem, assemble, parse_tool_input, str_at, u64_at};
use super::{BuiltBody, Framing, Protocol, RawFrame, StreamContext};
use crate::compat::{Compat, EffortWire};
use crate::compile::CompiledRequest;
use crate::error::{ErrorKind, HttpFailure, ProtocolHint, ProviderError, classify};
use crate::event::{PartIdx, StreamEvent};
use crate::model::{
    CacheHint, CacheTtl, Citation, CitationKind, CitationSource, FinishDetails, FinishReason,
    MediaSource, Message, Opaque, OpaqueData, Origin, Part, ProtocolKind, ReasoningPart,
    ReplayScope, Role, ToolCall, ToolCallId, ToolChoice, ToolOutput, ToolResult, UnknownPart,
    Usage,
};

pub struct AnthropicMessages;

/// Server tool versions (D36, D35). Availability per route comes from the catalog (t15).
pub const WEB_SEARCH_TOOL: &str = "web_search_20250305";
pub const CODE_EXECUTION_TOOL: &str = "code_execution_20250825";

fn cache_json(h: &Option<CacheHint>) -> Option<Value> {
    h.map(|h| match h.ttl {
        CacheTtl::FiveMinutes => json!({"type": "ephemeral"}),
        CacheTtl::OneHour => json!({"type": "ephemeral", "ttl": "1h"}),
    })
}

fn with_cache(mut v: Value, h: &Option<CacheHint>) -> Value {
    if let (Some(c), Some(o)) = (cache_json(h), v.as_object_mut()) {
        o.insert("cache_control".into(), c);
    }
    v
}

fn media_block(m: &crate::model::MediaPart) -> Result<Value, ProviderError> {
    let kind = if m.is_pdf() { "document" } else { "image" };
    let source = match &m.source {
        MediaSource::Base64 { data } => {
            json!({"type": "base64", "media_type": m.mime, "data": data})
        }
        MediaSource::Url { url } => json!({"type": "url", "url": url}),
        MediaSource::ProviderFile { id } => json!({"type": "file", "file_id": id}),
        MediaSource::Blob { id } => {
            return Err(ProviderError::invalid_request(format!(
                "unresolved blob {id}"
            )));
        }
    };
    Ok(with_cache(
        json!({"type": kind, "source": source}),
        &m.cache,
    ))
}

fn tool_result_block(r: &ToolResult, req: &CompiledRequest) -> Result<Value, ProviderError> {
    let content = match &r.content {
        ToolOutput::Text(t) => json!(t),
        ToolOutput::Json(v) => json!(v.to_string()),
        ToolOutput::Rich(parts) => {
            let mut blocks = vec![];
            for p in parts {
                match p {
                    Part::Text(t) => blocks.push(json!({"type": "text", "text": t.text})),
                    Part::Media(m) => blocks.push(media_block(m)?),
                    _ => {}
                }
            }
            Value::Array(blocks)
        }
    };
    let mut b = json!({"type": "tool_result", "tool_use_id": req.wire_tool_id(&r.call_id), "content": content});
    if r.is_error {
        b["is_error"] = json!(true);
    }
    Ok(with_cache(b, &r.cache))
}

fn normalise(
    m: &Message,
    req: &CompiledRequest,
    items: &mut Vec<Value>,
) -> Result<(), ProviderError> {
    let assistant = m.role == Role::Assistant;
    let mut content = vec![];
    for p in &m.parts {
        match p {
            Part::Text(t) => {
                if t.text.is_empty() {
                    continue; // empty text blocks are rejected
                }
                content.push(with_cache(
                    json!({"type": "text", "text": t.text}),
                    &t.cache,
                ));
            }
            Part::Media(media) => {
                if assistant {
                    continue; // `document` in assistant messages → 400 on 2026-10-21
                }
                content.push(media_block(media)?);
            }
            Part::Reasoning(r) => match r.opaque.as_ref().map(|o| &o.data) {
                Some(OpaqueData::AnthropicSignature { signature }) => content
                    .push(json!({"type": "thinking", "thinking": r.text, "signature": signature})),
                Some(OpaqueData::AnthropicRedacted { data }) => {
                    content.push(json!({"type": "redacted_thinking", "data": data}))
                }
                // Reasoning without an in-scope signature can't be sent as thinking.
                _ => {}
            },
            Part::ToolCall(c) => {
                if c.provider_executed {
                    if let Some(Opaque {
                        data: OpaqueData::Raw { value },
                        ..
                    }) = &c.opaque
                    {
                        content.push(value.clone());
                    }
                    continue;
                }
                content.push(json!({
                    "type": "tool_use", "id": req.wire_tool_id(&c.id), "name": c.name, "input": c.input,
                }));
            }
            Part::ToolResult(r) => {
                if let Some(Opaque {
                    data: OpaqueData::Raw { value },
                    ..
                }) = &r.opaque
                {
                    content.push(value.clone()); // server tool result block, replayed verbatim
                } else {
                    content.push(tool_result_block(r, req)?);
                }
            }
            Part::Compaction(c) => match (&c.opaque, &c.summary) {
                (
                    Some(Opaque {
                        data: OpaqueData::Raw { value },
                        ..
                    }),
                    _,
                ) => content.push(value.clone()),
                (_, Some(s)) => content.push(json!({"type": "text", "text": s})),
                _ => {}
            },
            Part::EffortMarker(e) => {
                // Mid-conversation system message (cache-friendly effort change).
                items.push(json!({"role": "system", "content": format!("Reasoning effort is now {:?}.", e.effort)}));
            }
            Part::Unknown(u) => content.push(u.raw.clone()),
        }
    }
    if content.is_empty() {
        return Ok(());
    }
    let role = if assistant { "assistant" } else { "user" };
    items.push(json!({"role": role, "content": content}));
    Ok(())
}

/// Put the tail breakpoint on the last cacheable block of the last message.
fn apply_tail_cache(list: &mut [WireItem], ttl: CacheTtl) {
    let Some(last) = list.last_mut() else { return };
    let mut v = match last {
        WireItem::Json(v) => v.clone(),
        WireItem::Raw(r) => match serde_json::from_str::<Value>(r.get()) {
            Ok(v) => v,
            Err(_) => return,
        },
    };
    let Some(blocks) = v.get_mut("content").and_then(Value::as_array_mut) else {
        return;
    };
    let hint = Some(CacheHint { ttl });
    if let Some(b) = blocks.iter_mut().rev().find(|b| {
        let t = str_at(b, "type").unwrap_or("");
        !matches!(t, "thinking" | "redacted_thinking")
            && !(t == "text" && str_at(b, "text") == Some(""))
    }) && b.get("cache_control").is_none()
    {
        *b = with_cache(b.take(), &hint);
    }
    *last = WireItem::Json(v);
}

impl Protocol for AnthropicMessages {
    type Body = WireBody;
    type Frame = Frame;
    type State = State;
    const KIND: ProtocolKind = ProtocolKind::AnthropicMessages;
    const FRAMING: Framing = Framing::Sse;
    const REPLAY: ReplayScope = ReplayScope::ProtocolFamilySameAccount;

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
        let records = assemble(req, &mut body.list, |m, items| normalise(m, req, items))?;
        if let Some(ttl) = req.tail_cache {
            apply_tail_cache(&mut body.list, ttl);
        }
        let h = &mut body.head;
        h.insert("model".into(), json!(r.model.wire_id));
        let max = r.max_output_tokens.or(r.model.max_output).unwrap_or(8192);
        h.insert("max_tokens".into(), json!(max));
        let t = &mut body.tail;
        let system: Vec<Value> = r
            .system
            .iter()
            .filter(|s| !s.text.is_empty())
            .map(|s| with_cache(json!({"type": "text", "text": s.text}), &s.cache))
            .collect();
        if !system.is_empty() {
            t.insert("system".into(), Value::Array(system));
        }
        let mut tools: Vec<Value> = r
            .tools
            .iter()
            .map(|d| {
                with_cache(json!({"name": d.name, "description": d.description, "input_schema": d.input_schema}), &d.cache)
            })
            .collect();
        if let Some(ws) = &r.server_tools.web_search {
            let mut w = json!({"type": WEB_SEARCH_TOOL, "name": "web_search"});
            if !ws.allowed_domains.is_empty() {
                w["allowed_domains"] = json!(ws.allowed_domains);
            }
            if !ws.blocked_domains.is_empty() {
                w["blocked_domains"] = json!(ws.blocked_domains);
            }
            if let Some(n) = ws.max_uses {
                w["max_uses"] = json!(n);
            }
            tools.push(w);
        }
        if r.server_tools.code_execution {
            tools.push(json!({"type": CODE_EXECUTION_TOOL, "name": "code_execution"}));
        }
        if !tools.is_empty() {
            t.insert("tools".into(), Value::Array(tools));
            let tc = match &r.tool_choice {
                ToolChoice::Auto => json!({"type": "auto"}),
                ToolChoice::Required => json!({"type": "any"}),
                ToolChoice::None => json!({"type": "none"}),
                ToolChoice::Tool { name } => json!({"type": "tool", "name": name}),
            };
            t.insert("tool_choice".into(), tc);
        }
        if let Some(e) = r.effort {
            match r.model.effort_wire(e) {
                Some(EffortWire::Effort(level)) => {
                    t.insert("output_config".into(), json!({"effort": level}));
                }
                Some(EffortWire::Budget(n)) => {
                    t.insert(
                        "thinking".into(),
                        json!({"type": "enabled", "budget_tokens": n}),
                    );
                }
                Some(EffortWire::Off) => {
                    t.insert("thinking".into(), json!({"type": "disabled"}));
                }
                Some(EffortWire::ThinkingLevel(_)) | None => {}
            }
        }
        let s = &r.sampling;
        if let Some(v) = s.temperature {
            t.insert("temperature".into(), json!(v));
        }
        if let Some(v) = s.top_p {
            t.insert("top_p".into(), json!(v));
        }
        if let Some(v) = s.top_k {
            t.insert("top_k".into(), json!(v));
        }
        if !s.stop.is_empty() {
            t.insert("stop_sequences".into(), json!(s.stop));
        }
        t.insert("stream".into(), json!(true));
        // Vertex: version goes in the body, model in the URL (route decides via Endpoint).
        if compat.version_in_body {
            body.head.remove("model");
            let v = compat
                .anthropic_version
                .clone()
                .unwrap_or_else(|| "vertex-2023-10-16".into());
            body.head.insert("anthropic_version".into(), json!(v));
        }
        Ok(BuiltBody { body, records })
    }

    fn decode(&self, frame: &RawFrame) -> Result<Option<Frame>, ProviderError> {
        let v: Value = match serde_json::from_str(&frame.data) {
            Ok(v) => v,
            Err(e) => {
                return Err(ProviderError::invalid_output(format!(
                    "anthropic: bad JSON event: {e}"
                )));
            }
        };
        match serde_json::from_value::<Frame>(v) {
            Ok(Frame::Unknown) => Ok(None),
            Ok(f) => Ok(Some(f)),
            Err(e) => Err(ProviderError::invalid_output(format!(
                "anthropic: bad event shape: {e}"
            ))),
        }
    }

    fn begin(&self, st: &mut State, ctx: &StreamContext) {
        st.origin = Some(ctx.origin.clone());
    }

    fn step(
        &self,
        st: &mut State,
        f: Frame,
        out: &mut Vec<StreamEvent>,
    ) -> Result<(), ProviderError> {
        st.step(f, out)
    }

    fn on_halt(&self, st: State, _out: &mut Vec<StreamEvent>) -> Result<(), ProviderError> {
        if st.done {
            Ok(())
        } else {
            Err(ProviderError::incomplete(
                "anthropic: stream ended before message_stop",
            ))
        }
    }

    fn classify(&self, f: &HttpFailure) -> ProviderError {
        let ty = f.json().and_then(|v| {
            v.get("error")
                .and_then(|e| str_at(e, "type"))
                .map(String::from)
        });
        classify(f, hint_for_type(ty.as_deref()))
    }
}

fn hint_for_type(ty: Option<&str>) -> ProtocolHint {
    let kind = match ty {
        Some("overloaded_error") => Some(ErrorKind::Overloaded),
        Some("rate_limit_error") => Some(ErrorKind::RateLimited),
        Some("authentication_error" | "permission_error") => Some(ErrorKind::Authentication),
        Some("api_error") => Some(ErrorKind::ProviderInternal),
        Some("request_too_large") => Some(ErrorKind::PayloadTooLarge),
        Some("billing_error") => Some(ErrorKind::QuotaExceeded),
        Some("timeout_error") => Some(ErrorKind::ProviderInternal),
        _ => None, // invalid_request_error / not_found_error → generic rules (overflow, replay…)
    };
    ProtocolHint {
        kind,
        code: ty.map(String::from),
        message: None,
    }
}

// ---------- stream ----------

#[derive(Debug, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Frame {
    MessageStart {
        message: Value,
    },
    ContentBlockStart {
        index: u32,
        content_block: Value,
    },
    ContentBlockDelta {
        index: u32,
        delta: Value,
    },
    ContentBlockStop {
        index: u32,
    },
    MessageDelta {
        delta: Value,
        #[serde(default)]
        usage: Value,
    },
    MessageStop,
    Ping,
    Error {
        error: Value,
    },
    #[serde(other)]
    Unknown,
}

#[derive(Debug)]
enum Block {
    Text {
        part: PartIdx,
        citations: Vec<Citation>,
    },
    Thinking {
        part: PartIdx,
        text: String,
        signature: Option<String>,
    },
    Tool {
        part: PartIdx,
        id: String,
        name: String,
        json: String,
        server: bool,
        raw: Value,
    },
    Whole,
}

#[derive(Debug, Default)]
pub struct State {
    origin: Option<Origin>,
    blocks: HashMap<u32, Block>,
    next_part: PartIdx,
    started: bool,
    done: bool,
    usage: Usage,
    stop: Option<String>,
    stop_details: Option<Value>,
}

fn origin_or_default(o: &Option<Origin>) -> Origin {
    o.clone().unwrap_or(Origin {
        provider: crate::model::ProviderId::new("anthropic"),
        model: String::new(),
        protocol: ProtocolKind::AnthropicMessages,
        account: None,
    })
}

fn read_usage(u: &mut Usage, v: &Value) {
    if v.is_null() {
        return;
    }
    // message_delta usage is cumulative: overwrite fields that are present.
    if v.get("input_tokens").is_some() {
        u.non_cached_input = u64_at(v, &["input_tokens"]);
    }
    if v.get("cache_read_input_tokens").is_some() {
        u.cache_read = u64_at(v, &["cache_read_input_tokens"]);
    }
    if let Some(cc) = v.get("cache_creation").filter(|c| c.is_object()) {
        u.cache_write_5m = u64_at(cc, &["ephemeral_5m_input_tokens"]);
        u.cache_write_1h = u64_at(cc, &["ephemeral_1h_input_tokens"]);
    } else if v.get("cache_creation_input_tokens").is_some() {
        u.cache_write_5m = u64_at(v, &["cache_creation_input_tokens"]);
    }
    if v.get("output_tokens").is_some() {
        u.output_total = u64_at(v, &["output_tokens"]);
    }
}

fn citation_from(v: &Value) -> Citation {
    let kind = match str_at(v, "type") {
        Some("char_location") => CitationKind::CharRange,
        Some("page_location") => CitationKind::PageRange,
        Some("content_block_location") => CitationKind::ContentBlock,
        Some("web_search_result_location") => CitationKind::WebUrl,
        Some("search_result_location") => CitationKind::SearchResult,
        _ => CitationKind::Unknown,
    };
    Citation {
        kind,
        cited_text: str_at(v, "cited_text").map(String::from),
        source: CitationSource {
            url: str_at(v, "url").map(String::from),
            title: str_at(v, "title")
                .or_else(|| str_at(v, "document_title"))
                .map(String::from),
            document_index: v
                .get("document_index")
                .and_then(Value::as_u64)
                .map(|n| n as u32),
        },
        span: None,
        opaque: None,
    }
}

pub fn finish_reason(s: Option<&str>) -> FinishReason {
    match s {
        Some("end_turn" | "stop_sequence") => FinishReason::Stop,
        Some("max_tokens" | "model_context_window_exceeded") => FinishReason::Length,
        Some("tool_use") => FinishReason::ToolCalls,
        Some("refusal") => FinishReason::Refusal,
        Some("pause_turn") => FinishReason::Paused,
        _ => FinishReason::Unknown,
    }
}

impl State {
    fn part(&mut self) -> PartIdx {
        let p = self.next_part;
        self.next_part += 1;
        p
    }
    fn opaque(&self, data: OpaqueData) -> Opaque {
        Opaque {
            origin: origin_or_default(&self.origin),
            data,
        }
    }

    fn step(&mut self, f: Frame, out: &mut Vec<StreamEvent>) -> Result<(), ProviderError> {
        match f {
            Frame::MessageStart { message } => {
                if !self.started {
                    self.started = true;
                    out.push(StreamEvent::StepStart { step: 0 });
                }
                if let (Some(m), Some(o)) = (str_at(&message, "model"), self.origin.as_mut()) {
                    o.model = m.to_string();
                }
                read_usage(
                    &mut self.usage,
                    message.get("usage").unwrap_or(&Value::Null),
                );
            }
            Frame::ContentBlockStart {
                index,
                content_block: b,
            } => {
                let ty = str_at(&b, "type").unwrap_or("").to_string();
                let block = match ty.as_str() {
                    "text" => {
                        let part = self.part();
                        out.push(StreamEvent::TextStart { part });
                        if let Some(t) = str_at(&b, "text").filter(|t| !t.is_empty()) {
                            out.push(StreamEvent::TextDelta {
                                part,
                                delta: t.to_string(),
                            });
                        }
                        Block::Text {
                            part,
                            citations: vec![],
                        }
                    }
                    "thinking" => {
                        let part = self.part();
                        out.push(StreamEvent::ReasoningStart { part });
                        let text = str_at(&b, "thinking").unwrap_or("").to_string();
                        if !text.is_empty() {
                            out.push(StreamEvent::ReasoningDelta {
                                part,
                                delta: text.clone(),
                            });
                        }
                        Block::Thinking {
                            part,
                            text,
                            signature: str_at(&b, "signature")
                                .map(String::from)
                                .filter(|s| !s.is_empty()),
                        }
                    }
                    "redacted_thinking" => {
                        let part = self.part();
                        let data = str_at(&b, "data").unwrap_or("").to_string();
                        out.push(StreamEvent::OpaqueBlock {
                            part,
                            value: Part::Reasoning(ReasoningPart {
                                text: String::new(),
                                redacted: true,
                                opaque: Some(self.opaque(OpaqueData::AnthropicRedacted { data })),
                            }),
                        });
                        Block::Whole
                    }
                    "tool_use" | "server_tool_use" | "mcp_tool_use" => {
                        let part = self.part();
                        let id = str_at(&b, "id").unwrap_or("").to_string();
                        let name = str_at(&b, "name").unwrap_or("").to_string();
                        let server = ty != "tool_use";
                        out.push(StreamEvent::ToolInputStart {
                            part,
                            call_id: ToolCallId::new(id.clone()),
                            name: name.clone(),
                            provider_executed: server,
                        });
                        Block::Tool {
                            part,
                            id,
                            name,
                            json: String::new(),
                            server,
                            raw: b,
                        }
                    }
                    t if t.ends_with("_tool_result") => {
                        let part = self.part();
                        let call_id = ToolCallId::new(str_at(&b, "tool_use_id").unwrap_or(""));
                        let is_error = b
                            .get("content")
                            .and_then(|c| str_at(c, "type"))
                            .is_some_and(|t| t.ends_with("_error"));
                        out.push(StreamEvent::ProviderToolResult {
                            part,
                            result: ToolResult {
                                call_id,
                                content: ToolOutput::Json(
                                    b.get("content").cloned().unwrap_or(Value::Null),
                                ),
                                is_error,
                                cache: None,
                                opaque: Some(self.opaque(OpaqueData::Raw { value: b.clone() })),
                            },
                        });
                        Block::Whole
                    }
                    other => {
                        if other == "fallback" {
                            let from = str_at(&b, "from_model")
                                .or_else(|| str_at(&b, "from"))
                                .unwrap_or("");
                            let to = str_at(&b, "to_model")
                                .or_else(|| str_at(&b, "to"))
                                .unwrap_or("");
                            out.push(StreamEvent::ModelSwitched {
                                from: from.into(),
                                to: to.into(),
                            });
                            if let Some(o) = self.origin.as_mut().filter(|_| !to.is_empty()) {
                                o.model = to.to_string();
                            }
                        }
                        let part = self.part();
                        out.push(StreamEvent::OpaqueBlock {
                            part,
                            value: Part::Unknown(UnknownPart {
                                origin: origin_or_default(&self.origin),
                                raw: b,
                            }),
                        });
                        Block::Whole
                    }
                };
                self.blocks.insert(index, block);
            }
            Frame::ContentBlockDelta { index, delta } => {
                let Some(block) = self.blocks.get_mut(&index) else {
                    return Err(ProviderError::invalid_output(format!(
                        "anthropic: delta for unknown block {index}"
                    )));
                };
                match (str_at(&delta, "type").unwrap_or(""), block) {
                    ("text_delta", Block::Text { part, .. }) => out.push(StreamEvent::TextDelta {
                        part: *part,
                        delta: str_at(&delta, "text").unwrap_or("").into(),
                    }),
                    ("citations_delta", Block::Text { part, citations }) => {
                        let c = citation_from(delta.get("citation").unwrap_or(&Value::Null));
                        citations.push(c.clone());
                        out.push(StreamEvent::Citation {
                            part: *part,
                            citation: c,
                        });
                    }
                    ("thinking_delta", Block::Thinking { part, text, .. }) => {
                        let d = str_at(&delta, "thinking").unwrap_or("");
                        text.push_str(d);
                        if !d.is_empty() {
                            out.push(StreamEvent::ReasoningDelta {
                                part: *part,
                                delta: d.into(),
                            });
                        }
                    }
                    ("signature_delta", Block::Thinking { signature, .. }) => {
                        // Kept byte-exact; the API may send it in more than one piece.
                        let s = str_at(&delta, "signature").unwrap_or("");
                        signature.get_or_insert_with(String::new).push_str(s);
                    }
                    ("input_json_delta", Block::Tool { part, json, .. }) => {
                        let d = str_at(&delta, "partial_json").unwrap_or("");
                        json.push_str(d);
                        if !d.is_empty() {
                            out.push(StreamEvent::ToolInputDelta {
                                part: *part,
                                json_fragment: d.into(),
                            });
                        }
                    }
                    _ => {} // unknown delta types are ignored
                }
            }
            Frame::ContentBlockStop { index } => {
                let origin = origin_or_default(&self.origin);
                match self.blocks.remove(&index) {
                    Some(Block::Text { part, citations }) => out.push(StreamEvent::TextEnd {
                        part,
                        citations,
                        opaque: None,
                    }),
                    Some(Block::Thinking {
                        part,
                        text,
                        signature,
                    }) => out.push(StreamEvent::ReasoningEnd {
                        part,
                        final_text: Some(text),
                        opaque: signature.map(|s| Opaque {
                            origin,
                            data: OpaqueData::AnthropicSignature { signature: s },
                        }),
                    }),
                    Some(Block::Tool {
                        part,
                        id,
                        name,
                        json,
                        server,
                        mut raw,
                    }) => match parse_tool_input(&json) {
                        Ok(input) => {
                            let opaque = server.then(|| {
                                raw["input"] = input.clone();
                                Opaque {
                                    origin,
                                    data: OpaqueData::Raw { value: raw },
                                }
                            });
                            out.push(StreamEvent::ToolInputEnd {
                                part,
                                call: ToolCall {
                                    id: ToolCallId::new(id),
                                    name,
                                    input,
                                    provider_executed: server,
                                    opaque,
                                },
                            })
                        }
                        Err(error) => out.push(StreamEvent::ToolInputError {
                            part,
                            call_id: ToolCallId::new(id),
                            raw: json,
                            error,
                        }),
                    },
                    Some(Block::Whole) | None => {}
                }
            }
            Frame::MessageDelta { delta, usage } => {
                if let Some(s) = str_at(&delta, "stop_reason") {
                    self.stop = Some(s.to_string());
                }
                if let Some(d) = delta.get("stop_details").filter(|d| !d.is_null()) {
                    self.stop_details = Some(d.clone());
                }
                read_usage(&mut self.usage, &usage);
            }
            Frame::MessageStop => {
                let reason = finish_reason(self.stop.as_deref());
                let usage = self.usage.finalize();
                let details = FinishDetails {
                    raw: self.stop.clone(),
                    category: self
                        .stop_details
                        .as_ref()
                        .and_then(|d| str_at(d, "category"))
                        .map(String::from),
                    explanation: self
                        .stop_details
                        .as_ref()
                        .and_then(|d| str_at(d, "explanation"))
                        .map(String::from),
                };
                out.push(StreamEvent::StepFinish {
                    reason,
                    details,
                    usage,
                });
                out.push(StreamEvent::Finish { reason, usage });
                self.done = true;
            }
            Frame::Ping | Frame::Unknown => {}
            Frame::Error { error } => {
                let ty = str_at(&error, "type");
                let msg = str_at(&error, "message")
                    .unwrap_or("stream error")
                    .to_string();
                let f = HttpFailure {
                    status: match ty {
                        Some("overloaded_error") => 529,
                        Some("rate_limit_error") => 429,
                        Some("api_error") => 500,
                        _ => 400,
                    },
                    headers: vec![],
                    body: json!({"type": "error", "error": error}).to_string(),
                };
                let mut e = classify(
                    &f,
                    ProtocolHint {
                        message: Some(msg),
                        ..hint_for_type(ty)
                    },
                );
                e.status = None; // in-stream, not an HTTP status
                return Err(e);
            }
        }
        Ok(())
    }
}
