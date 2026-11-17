//! OpenAI Responses (03 §5): OpenAI and Azure `/openai/v1`.
//!
//! Stateless: `store: false` with encrypted reasoning items replayed in order; no
//! `previous_response_id` in v1.

use std::collections::HashMap;

use serde_json::{Value, json};

use super::util::{WireBody, assemble, data_url, parse_tool_input, str_at, u64_at};
use super::{BuiltBody, Framing, Protocol, RawFrame, StreamContext};
use crate::compat::{Compat, EffortWire};
use crate::compile::CompiledRequest;
use crate::error::{ErrorKind, HttpFailure, ProtocolHint, ProviderError, classify};
use crate::event::{PartIdx, StreamEvent};
use crate::model::{
    Citation, CitationKind, CitationSource, FinishDetails, FinishReason, MediaSource, Message,
    Opaque, OpaqueData, Origin, Part, ProtocolKind, ProviderId, ReplayScope, Role, ToolCall,
    ToolCallId, ToolChoice, ToolOutput, Usage,
};

pub struct OpenAiResponses;

fn content_item(p: &Part, assistant: bool) -> Result<Option<Value>, ProviderError> {
    Ok(match p {
        Part::Text(t) if assistant => Some(json!({"type": "output_text", "text": t.text})),
        Part::Text(t) => Some(json!({"type": "input_text", "text": t.text})),
        Part::Media(m) if !assistant => {
            let url = match &m.source {
                MediaSource::Base64 { data } => data_url(&m.mime, data),
                MediaSource::Url { url } => url.clone(),
                MediaSource::ProviderFile { id } => {
                    let ty = if m.is_image() {
                        "input_image"
                    } else {
                        "input_file"
                    };
                    return Ok(Some(json!({"type": ty, "file_id": id})));
                }
                MediaSource::Blob { id } => {
                    return Err(ProviderError::invalid_request(format!(
                        "unresolved blob {id}"
                    )));
                }
            };
            if m.is_image() {
                Some(json!({"type": "input_image", "image_url": url}))
            } else {
                let name = m.filename.clone().unwrap_or_else(|| "document.pdf".into());
                Some(json!({"type": "input_file", "filename": name, "file_data": url}))
            }
        }
        _ => None,
    })
}

fn normalise(
    m: &Message,
    req: &CompiledRequest,
    items: &mut Vec<Value>,
) -> Result<(), ProviderError> {
    let assistant = m.role == Role::Assistant;
    let role = match m.role {
        Role::Assistant => "assistant",
        Role::System => "developer",
        _ => "user",
    };
    let mut content: Vec<Value> = vec![];
    let flush = |content: &mut Vec<Value>, items: &mut Vec<Value>| {
        if !content.is_empty() {
            items
                .push(json!({"type": "message", "role": role, "content": std::mem::take(content)}));
        }
    };
    for p in &m.parts {
        match p {
            Part::Text(_) | Part::Media(_) => {
                if let Some(c) = content_item(p, assistant)? {
                    content.push(c);
                }
            }
            Part::Reasoning(r) => {
                if let Some(OpaqueData::ResponsesReasoning {
                    item_id,
                    encrypted_content,
                    summary,
                }) = r.opaque.as_ref().map(|o| &o.data)
                {
                    flush(&mut content, items);
                    let mut it = json!({
                        "type": "reasoning",
                        "id": item_id,
                        "summary": summary.iter().map(|s| json!({"type": "summary_text", "text": s})).collect::<Vec<_>>(),
                    });
                    if let Some(enc) = encrypted_content {
                        it["encrypted_content"] = json!(enc);
                    }
                    items.push(it);
                }
            }
            Part::ToolCall(c) => {
                flush(&mut content, items);
                if c.provider_executed {
                    if let Some(Opaque {
                        data: OpaqueData::Raw { value },
                        ..
                    }) = &c.opaque
                    {
                        items.push(value.clone());
                    }
                    continue;
                }
                items.push(json!({
                    "type": "function_call",
                    "call_id": req.wire_tool_id(&c.id),
                    "name": c.name,
                    "arguments": c.input.to_string(),
                }));
            }
            Part::ToolResult(r) => {
                flush(&mut content, items);
                if r.opaque.is_some() {
                    continue; // hosted-tool results live inside their call item
                }
                let output = match &r.content {
                    ToolOutput::Rich(parts) => {
                        let mut arr = vec![];
                        for p in parts {
                            if let Some(c) = content_item(p, false)? {
                                arr.push(c);
                            }
                        }
                        Value::Array(arr)
                    }
                    other => json!(other.to_text()),
                };
                items.push(json!({"type": "function_call_output", "call_id": req.wire_tool_id(&r.call_id), "output": output}));
            }
            Part::Compaction(c) => {
                flush(&mut content, items);
                match (&c.opaque, &c.summary) {
                    (Some(Opaque { data: OpaqueData::Raw { value }, .. }), _) => items.push(value.clone()),
                    (_, Some(s)) => content.push(json!({"type": if assistant {"output_text"} else {"input_text"}, "text": s})),
                    _ => {}
                }
            }
            Part::EffortMarker(e) => {
                flush(&mut content, items);
                items.push(json!({"type": "message", "role": "developer", "content": [{"type": "input_text", "text": format!("Reasoning effort is now {:?}.", e.effort)}]}));
            }
            Part::Unknown(u) => {
                flush(&mut content, items);
                items.push(u.raw.clone());
            }
        }
    }
    flush(&mut content, items);
    Ok(())
}

impl Protocol for OpenAiResponses {
    type Body = WireBody;
    type Frame = Value;
    type State = State;
    const KIND: ProtocolKind = ProtocolKind::OpenaiResponses;
    const FRAMING: Framing = Framing::Sse;
    const REPLAY: ReplayScope = ReplayScope::ModelFamily;

    fn build_body(
        &self,
        req: &CompiledRequest,
        _compat: &Compat,
    ) -> Result<BuiltBody<WireBody>, ProviderError> {
        let r = &req.request;
        let mut body = WireBody {
            list_key: "input",
            ..Default::default()
        };
        // System blocks → developer messages at the head of `input` (baseline = block 2, SA-41).
        for s in r.system.iter().filter(|s| !s.text.is_empty()) {
            body.list.push(super::util::WireItem::Json(json!({
                "type": "message", "role": "developer", "content": [{"type": "input_text", "text": s.text}],
            })));
        }
        let records = assemble(req, &mut body.list, |m, items| normalise(m, req, items))?;
        body.head.insert("model".into(), json!(r.model.wire_id));
        let t = &mut body.tail;
        let mut tools: Vec<Value> = r
            .tools
            .iter()
            .map(|d| json!({"type": "function", "name": d.name, "description": d.description, "parameters": d.input_schema, "strict": false}))
            .collect();
        if let Some(ws) = &r.server_tools.web_search {
            let mut w = json!({"type": "web_search"});
            if !ws.allowed_domains.is_empty() {
                w["filters"] = json!({"allowed_domains": ws.allowed_domains});
            }
            tools.push(w);
        }
        if r.server_tools.code_execution {
            tools.push(json!({"type": "code_interpreter", "container": {"type": "auto"}}));
        }
        if !tools.is_empty() {
            t.insert("tools".into(), Value::Array(tools));
            t.insert(
                "tool_choice".into(),
                match &r.tool_choice {
                    ToolChoice::Auto => json!("auto"),
                    ToolChoice::Required => json!("required"),
                    ToolChoice::None => json!("none"),
                    ToolChoice::Tool { name } => json!({"type": "function", "name": name}),
                },
            );
        }
        if let Some(e) = r.effort
            && let Some(EffortWire::Effort(level)) = r.model.effort_wire(e)
        {
            let mut reasoning = json!({"effort": level});
            if r.reasoning_summary {
                reasoning["summary"] = json!("auto");
            }
            t.insert("reasoning".into(), reasoning);
        }
        if let Some(n) = r.max_output_tokens {
            t.insert("max_output_tokens".into(), json!(n));
        }
        if let Some(v) = r.sampling.temperature {
            t.insert("temperature".into(), json!(v));
        }
        if let Some(v) = r.sampling.top_p {
            t.insert("top_p".into(), json!(v));
        }
        if let Some(k) = &r.prompt_cache_key {
            t.insert("prompt_cache_key".into(), json!(k));
        }
        t.insert("store".into(), json!(false));
        t.insert("include".into(), json!(["reasoning.encrypted_content"]));
        t.insert("stream".into(), json!(true));
        Ok(BuiltBody { body, records })
    }

    fn decode(&self, frame: &RawFrame) -> Result<Option<Value>, ProviderError> {
        serde_json::from_str::<Value>(&frame.data)
            .map(Some)
            .map_err(|e| ProviderError::invalid_output(format!("responses: bad JSON event: {e}")))
    }

    fn begin(&self, st: &mut State, ctx: &StreamContext) {
        st.origin = Some(ctx.origin.clone());
    }

    fn step(
        &self,
        st: &mut State,
        f: Value,
        out: &mut Vec<StreamEvent>,
    ) -> Result<(), ProviderError> {
        st.step(f, out)
    }

    fn on_halt(&self, st: State, _out: &mut Vec<StreamEvent>) -> Result<(), ProviderError> {
        if st.done {
            Ok(())
        } else {
            Err(ProviderError::incomplete(
                "responses: stream ended before response.completed",
            ))
        }
    }

    fn classify(&self, f: &HttpFailure) -> ProviderError {
        classify(f, openai_hint(f.json().as_ref()))
    }
}

/// OpenAI error codes shared by Responses and Chat.
pub fn openai_hint(body: Option<&Value>) -> ProtocolHint {
    let err = body.and_then(|b| b.get("error"));
    let code = err
        .and_then(|e| str_at(e, "code").or_else(|| str_at(e, "type")))
        .map(String::from);
    let kind = match code.as_deref() {
        Some("insufficient_quota") => Some(ErrorKind::QuotaExceeded),
        Some("context_length_exceeded" | "string_above_max_length") => {
            Some(ErrorKind::ContextOverflow)
        }
        Some("rate_limit_exceeded") => Some(ErrorKind::RateLimited),
        Some("invalid_api_key") => Some(ErrorKind::Authentication),
        Some("content_filter" | "content_policy_violation") => {
            Some(ErrorKind::ContentPolicy { category: None })
        }
        Some("server_error") => Some(ErrorKind::ProviderInternal),
        Some("server_is_overloaded" | "overloaded") => Some(ErrorKind::Overloaded),
        _ => None,
    };
    ProtocolHint {
        kind,
        code,
        message: None,
    }
}

// ---------- stream ----------

#[derive(Debug)]
enum Item {
    Message {
        text_parts: HashMap<u64, (PartIdx, Vec<Citation>)>,
    },
    Reasoning {
        part: PartIdx,
        id: String,
        summary: Vec<String>,
        text: String,
    },
    Function {
        part: PartIdx,
        call_id: String,
        name: String,
        args: String,
    },
    Hosted {
        part: PartIdx,
    },
}

#[derive(Debug, Default)]
pub struct State {
    origin: Option<Origin>,
    items: HashMap<String, Item>,
    next_part: PartIdx,
    started: bool,
    done: bool,
    saw_tool_call: bool,
    refusal: bool,
}

fn origin_or_default(o: &Option<Origin>) -> Origin {
    o.clone().unwrap_or(Origin {
        provider: ProviderId::new("openai"),
        model: String::new(),
        protocol: ProtocolKind::OpenaiResponses,
        account: None,
    })
}

pub fn read_usage(u: &Value) -> Usage {
    let input = u64_at(u, &["input_tokens"]);
    let cached = u64_at(u, &["input_tokens_details", "cached_tokens"]);
    let reasoning = u
        .get("output_tokens_details")
        .map(|_| u64_at(u, &["output_tokens_details", "reasoning_tokens"]));
    Usage {
        non_cached_input: input.saturating_sub(cached),
        cache_read: cached,
        output_total: u64_at(u, &["output_tokens"]),
        reasoning,
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

    fn item_key(v: &Value) -> String {
        str_at(v, "item_id")
            .or_else(|| v.get("item").and_then(|i| str_at(i, "id")))
            .unwrap_or("")
            .to_string()
    }

    fn step(&mut self, v: Value, out: &mut Vec<StreamEvent>) -> Result<(), ProviderError> {
        let ty = str_at(&v, "type").unwrap_or("").to_string();
        match ty.as_str() {
            "response.created" | "response.in_progress" | "response.queued" => {
                if !self.started {
                    self.started = true;
                    out.push(StreamEvent::StepStart { step: 0 });
                }
                if let (Some(m), Some(o)) = (
                    v.get("response").and_then(|r| str_at(r, "model")),
                    self.origin.as_mut(),
                ) {
                    o.model = m.to_string();
                }
            }
            "response.output_item.added" => {
                let item = v.get("item").cloned().unwrap_or(Value::Null);
                let id = str_at(&item, "id").unwrap_or("").to_string();
                let it = match str_at(&item, "type").unwrap_or("") {
                    "message" => Item::Message {
                        text_parts: HashMap::new(),
                    },
                    "reasoning" => {
                        let part = self.part();
                        out.push(StreamEvent::ReasoningStart { part });
                        Item::Reasoning {
                            part,
                            id: id.clone(),
                            summary: vec![],
                            text: String::new(),
                        }
                    }
                    "function_call" => {
                        let part = self.part();
                        let call_id = str_at(&item, "call_id").unwrap_or(&id).to_string();
                        let name = str_at(&item, "name").unwrap_or("").to_string();
                        out.push(StreamEvent::ToolInputStart {
                            part,
                            call_id: ToolCallId::new(call_id.clone()),
                            name: name.clone(),
                            provider_executed: false,
                        });
                        self.saw_tool_call = true;
                        Item::Function {
                            part,
                            call_id,
                            name,
                            args: String::new(),
                        }
                    }
                    t if t.ends_with("_call") => {
                        let part = self.part();
                        out.push(StreamEvent::ToolInputStart {
                            part,
                            call_id: ToolCallId::new(id.clone()),
                            name: t.trim_end_matches("_call").to_string(),
                            provider_executed: true,
                        });
                        Item::Hosted { part }
                    }
                    _ => return Ok(()),
                };
                self.items.insert(id, it);
            }
            "response.content_part.added" => {
                let key = Self::item_key(&v);
                let idx = v.get("content_index").and_then(Value::as_u64).unwrap_or(0);
                let ptype = v
                    .get("part")
                    .and_then(|p| str_at(p, "type"))
                    .unwrap_or("output_text")
                    .to_string();
                if !matches!(self.items.get(&key), Some(Item::Message { .. })) {
                    return Ok(());
                }
                if ptype == "refusal" {
                    self.refusal = true;
                }
                let part = self.part();
                if let Some(Item::Message { text_parts }) = self.items.get_mut(&key) {
                    out.push(StreamEvent::TextStart { part });
                    text_parts.insert(idx, (part, vec![]));
                }
            }
            "response.output_text.delta" | "response.refusal.delta" => {
                if ty == "response.refusal.delta" {
                    self.refusal = true;
                }
                let key = Self::item_key(&v);
                let idx = v.get("content_index").and_then(Value::as_u64).unwrap_or(0);
                if let Some(Item::Message { text_parts }) = self.items.get_mut(&key)
                    && let Some((part, _)) = text_parts.get(&idx)
                {
                    out.push(StreamEvent::TextDelta {
                        part: *part,
                        delta: str_at(&v, "delta").unwrap_or("").into(),
                    });
                }
            }
            "response.output_text.annotation.added" => {
                let key = Self::item_key(&v);
                let idx = v.get("content_index").and_then(Value::as_u64).unwrap_or(0);
                let a = v.get("annotation").cloned().unwrap_or(Value::Null);
                if let Some(Item::Message { text_parts }) = self.items.get_mut(&key)
                    && let Some((part, cites)) = text_parts.get_mut(&idx)
                {
                    let c = Citation {
                        kind: if str_at(&a, "type") == Some("url_citation") {
                            CitationKind::WebUrl
                        } else {
                            CitationKind::Annotation
                        },
                        cited_text: None,
                        source: CitationSource {
                            url: str_at(&a, "url").map(String::from),
                            title: str_at(&a, "title").map(String::from),
                            document_index: None,
                        },
                        span: match (
                            a.get("start_index").and_then(Value::as_u64),
                            a.get("end_index").and_then(Value::as_u64),
                        ) {
                            (Some(s), Some(e)) => Some((s as usize, e as usize)),
                            _ => None,
                        },
                        opaque: None,
                    };
                    cites.push(c.clone());
                    out.push(StreamEvent::Citation {
                        part: *part,
                        citation: c,
                    });
                }
            }
            "response.content_part.done" => {
                let key = Self::item_key(&v);
                let idx = v.get("content_index").and_then(Value::as_u64).unwrap_or(0);
                if let Some(Item::Message { text_parts }) = self.items.get_mut(&key)
                    && let Some((part, citations)) = text_parts.remove(&idx)
                {
                    out.push(StreamEvent::TextEnd {
                        part,
                        citations,
                        opaque: None,
                    });
                }
            }
            "response.reasoning_summary_part.added" => {
                let key = Self::item_key(&v);
                if let Some(Item::Reasoning { part, summary, .. }) = self.items.get_mut(&key) {
                    if !summary.is_empty() {
                        out.push(StreamEvent::ReasoningDelta {
                            part: *part,
                            delta: "\n\n".into(),
                        });
                    }
                    summary.push(String::new());
                }
            }
            "response.reasoning_summary_text.delta" | "response.reasoning_text.delta" => {
                let key = Self::item_key(&v);
                let d = str_at(&v, "delta").unwrap_or("").to_string();
                if let Some(Item::Reasoning {
                    part,
                    summary,
                    text,
                    ..
                }) = self.items.get_mut(&key)
                {
                    if ty == "response.reasoning_summary_text.delta" {
                        if summary.is_empty() {
                            summary.push(String::new());
                        }
                        if let Some(last) = summary.last_mut() {
                            last.push_str(&d);
                        }
                    } else {
                        text.push_str(&d);
                    }
                    out.push(StreamEvent::ReasoningDelta {
                        part: *part,
                        delta: d,
                    });
                }
            }
            "response.function_call_arguments.delta" => {
                let key = Self::item_key(&v);
                if let Some(Item::Function { part, args, .. }) = self.items.get_mut(&key) {
                    let d = str_at(&v, "delta").unwrap_or("");
                    args.push_str(d);
                    out.push(StreamEvent::ToolInputDelta {
                        part: *part,
                        json_fragment: d.into(),
                    });
                }
            }
            "response.output_item.done" => {
                let item = v.get("item").cloned().unwrap_or(Value::Null);
                let id = str_at(&item, "id").unwrap_or("").to_string();
                let origin = origin_or_default(&self.origin);
                match self.items.remove(&id) {
                    Some(Item::Message { text_parts }) => {
                        let mut rest: Vec<_> = text_parts.into_iter().collect();
                        rest.sort_by_key(|(k, _)| *k);
                        for (_, (part, citations)) in rest {
                            out.push(StreamEvent::TextEnd {
                                part,
                                citations,
                                opaque: None,
                            });
                        }
                    }
                    Some(Item::Reasoning {
                        part,
                        id,
                        summary,
                        text,
                    }) => {
                        let summary = if summary.is_empty() {
                            item.get("summary")
                                .and_then(Value::as_array)
                                .map(|a| {
                                    a.iter()
                                        .filter_map(|s| str_at(s, "text").map(String::from))
                                        .collect()
                                })
                                .unwrap_or_default()
                        } else {
                            summary
                        };
                        let readable = if text.is_empty() {
                            summary.join("\n\n")
                        } else {
                            text
                        };
                        out.push(StreamEvent::ReasoningEnd {
                            part,
                            final_text: Some(readable),
                            opaque: Some(Opaque {
                                origin,
                                data: OpaqueData::ResponsesReasoning {
                                    item_id: id,
                                    encrypted_content: str_at(&item, "encrypted_content")
                                        .map(String::from),
                                    summary,
                                },
                            }),
                        });
                    }
                    Some(Item::Function {
                        part,
                        call_id,
                        name,
                        args,
                    }) => {
                        let args = str_at(&item, "arguments").map(String::from).unwrap_or(args);
                        match parse_tool_input(&args) {
                            Ok(input) => out.push(StreamEvent::ToolInputEnd {
                                part,
                                call: ToolCall {
                                    id: ToolCallId::new(call_id),
                                    name,
                                    input,
                                    provider_executed: false,
                                    opaque: None,
                                },
                            }),
                            Err(error) => out.push(StreamEvent::ToolInputError {
                                part,
                                call_id: ToolCallId::new(call_id),
                                raw: args,
                                error,
                            }),
                        }
                    }
                    Some(Item::Hosted { part }) => {
                        let name = str_at(&item, "type")
                            .unwrap_or("")
                            .trim_end_matches("_call")
                            .to_string();
                        let input = item.get("action").cloned().unwrap_or(Value::Null);
                        out.push(StreamEvent::ToolInputEnd {
                            part,
                            call: ToolCall {
                                id: ToolCallId::new(id),
                                name,
                                input,
                                provider_executed: true,
                                opaque: Some(Opaque {
                                    origin,
                                    data: OpaqueData::Raw { value: item },
                                }),
                            },
                        });
                    }
                    None => {}
                }
            }
            "response.completed" | "response.incomplete" => {
                let resp = v.get("response").cloned().unwrap_or(Value::Null);
                let usage = resp.get("usage").map(read_usage).unwrap_or_default();
                let incomplete = resp
                    .get("incomplete_details")
                    .and_then(|d| str_at(d, "reason"))
                    .map(String::from);
                let reason = match incomplete.as_deref() {
                    Some("max_output_tokens") => FinishReason::Length,
                    Some("content_filter") => FinishReason::ContentFilter,
                    Some(_) => FinishReason::Unknown,
                    None if self.refusal => FinishReason::Refusal,
                    None if self.saw_tool_call => FinishReason::ToolCalls,
                    None => FinishReason::Stop,
                };
                let details = FinishDetails {
                    raw: incomplete.or_else(|| str_at(&resp, "status").map(String::from)),
                    ..Default::default()
                };
                out.push(StreamEvent::StepFinish {
                    reason,
                    details,
                    usage,
                });
                out.push(StreamEvent::Finish { reason, usage });
                self.done = true;
            }
            "response.failed" | "error" => {
                let err = if ty == "error" {
                    v.get("error").cloned().unwrap_or_else(|| v.clone())
                } else {
                    v.get("response")
                        .and_then(|r| r.get("error"))
                        .cloned()
                        .unwrap_or(Value::Null)
                };
                let code = str_at(&err, "code").map(String::from);
                let status = match code.as_deref() {
                    Some("rate_limit_exceeded") => 429,
                    Some("server_error" | "internal_error") => 500,
                    Some("server_is_overloaded") => 503,
                    _ => 400,
                };
                let f = HttpFailure {
                    status,
                    headers: vec![],
                    body: json!({"error": err}).to_string(),
                };
                let mut e = self_classify(&f);
                e.status = None;
                return Err(e);
            }
            _ => {} // unknown events are ignored (tolerant decoding)
        }
        Ok(())
    }
}

fn self_classify(f: &HttpFailure) -> ProviderError {
    classify(f, openai_hint(f.json().as_ref()))
}
