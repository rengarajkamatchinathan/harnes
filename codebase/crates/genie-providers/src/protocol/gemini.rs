//! Gemini `streamGenerateContent?alt=sse` (03 §5): Gemini API and Vertex Gemini.
//!
//! `thoughtSignature`s on parts and function calls are kept and replayed same-model only.
//! Each SSE event is a whole `GenerateContentResponse` chunk; the one carrying
//! `finishReason` is terminal.

use std::collections::HashMap;

use serde_json::{Value, json};

use super::util::{WireBody, assemble, str_at, u64_at};
use super::{BuiltBody, Framing, Protocol, RawFrame, StreamContext};
use crate::compat::{Compat, EffortWire, ToolIdRule};
use crate::compile::CompiledRequest;
use crate::error::{ErrorKind, HttpFailure, ProtocolHint, ProviderError, classify};
use crate::event::{PartIdx, StreamEvent};
use crate::model::{
    Citation, CitationKind, CitationSource, FinishDetails, FinishReason, MediaSource, Message,
    Opaque, OpaqueData, Origin, Part, ProtocolKind, ProviderId, ReplayScope, Role, ToolCall,
    ToolCallId, ToolChoice, ToolOutput, ToolResult, Usage,
};

pub struct Gemini;

fn sig(o: &Option<Opaque>) -> Option<&str> {
    match o.as_ref().map(|o| &o.data) {
        Some(OpaqueData::GeminiThoughtSignature { signature }) => Some(signature),
        _ => None,
    }
}

fn normalise(
    m: &Message,
    req: &CompiledRequest,
    compat: &Compat,
    names: &mut HashMap<String, String>,
    items: &mut Vec<Value>,
) -> Result<(), ProviderError> {
    let role = if m.role == Role::Assistant {
        "model"
    } else {
        "user"
    };
    let mut parts = vec![];
    for p in &m.parts {
        match p {
            Part::Text(t) => {
                let mut v = json!({"text": t.text});
                if let Some(s) = sig(&t.opaque) {
                    v["thoughtSignature"] = json!(s);
                }
                parts.push(v);
            }
            Part::Media(media) => parts.push(match &media.source {
                MediaSource::Base64 { data } => {
                    json!({"inlineData": {"mimeType": media.mime, "data": data}})
                }
                MediaSource::Url { url } | MediaSource::ProviderFile { id: url } => {
                    json!({"fileData": {"mimeType": media.mime, "fileUri": url}})
                }
                MediaSource::Blob { id } => {
                    return Err(ProviderError::invalid_request(format!(
                        "unresolved blob {id}"
                    )));
                }
            }),
            Part::Reasoning(r) => {
                // Only signed thoughts are replayed (same model, enforced by compile).
                if let Some(s) = sig(&r.opaque) {
                    parts.push(json!({"text": r.text, "thought": true, "thoughtSignature": s}));
                }
            }
            Part::ToolCall(c) => {
                if c.provider_executed {
                    if let Some(Opaque {
                        data: OpaqueData::Raw { value },
                        ..
                    }) = &c.opaque
                    {
                        parts.push(value.clone());
                    }
                    continue;
                }
                let wire = req.wire_tool_id(&c.id);
                names.insert(wire.clone(), c.name.clone());
                let mut fc = json!({"name": c.name, "args": c.input});
                if compat.tool_id_rule != ToolIdRule::Omit {
                    fc["id"] = json!(wire);
                }
                let mut v = json!({"functionCall": fc});
                if let Some(s) = sig(&c.opaque) {
                    v["thoughtSignature"] = json!(s);
                }
                parts.push(v);
            }
            Part::ToolResult(r) => {
                if let Some(Opaque {
                    data: OpaqueData::Raw { value },
                    ..
                }) = &r.opaque
                {
                    parts.push(value.clone());
                    continue;
                }
                let wire = req.wire_tool_id(&r.call_id);
                let name = names.get(&wire).cloned().unwrap_or_else(|| wire.clone());
                let response = match &r.content {
                    ToolOutput::Json(v) if v.is_object() => v.clone(),
                    other if r.is_error => json!({"error": other.to_text()}),
                    other => json!({"output": other.to_text()}),
                };
                let mut fr = json!({"name": name, "response": response});
                if compat.tool_id_rule != ToolIdRule::Omit {
                    fr["id"] = json!(wire);
                }
                parts.push(json!({"functionResponse": fr}));
            }
            Part::Compaction(c) => {
                if let Some(s) = &c.summary {
                    parts.push(json!({"text": s}));
                }
            }
            Part::EffortMarker(_) => {}
            Part::Unknown(u) => parts.push(u.raw.clone()),
        }
    }
    if !parts.is_empty() {
        items.push(json!({"role": role, "parts": parts}));
    }
    Ok(())
}

impl Protocol for Gemini {
    type Body = WireBody;
    type Frame = Value;
    type State = State;
    const KIND: ProtocolKind = ProtocolKind::Gemini;
    const FRAMING: Framing = Framing::Sse;
    const REPLAY: ReplayScope = ReplayScope::SameModel;

    fn build_body(
        &self,
        req: &CompiledRequest,
        compat: &Compat,
    ) -> Result<BuiltBody<WireBody>, ProviderError> {
        let r = &req.request;
        let mut body = WireBody {
            list_key: "contents",
            ..Default::default()
        };
        // Tool names for functionResponse come from calls anywhere earlier in the history,
        // including stored records.
        let mut names: HashMap<String, String> = HashMap::new();
        for wm in &req.wire_history {
            if let crate::compile::WireMessage::Stored(rec) = wm
                && let Ok(Value::Array(items)) = serde_json::from_str::<Value>(&rec.items)
            {
                for fc in items
                    .iter()
                    .flat_map(|i| i["parts"].as_array().cloned().unwrap_or_default())
                {
                    if let (Some(id), Some(n)) = (
                        fc["functionCall"]["id"].as_str(),
                        fc["functionCall"]["name"].as_str(),
                    ) {
                        names.insert(id.into(), n.into());
                    }
                }
            }
        }
        let records = assemble(req, &mut body.list, |m, items| {
            normalise(m, req, compat, &mut names, items)
        })?;
        let h = &mut body.head;
        let system: Vec<Value> = r
            .system
            .iter()
            .filter(|s| !s.text.is_empty())
            .map(|s| json!({"text": s.text}))
            .collect();
        if !system.is_empty() {
            h.insert("systemInstruction".into(), json!({"parts": system}));
        }
        let t = &mut body.tail;
        let mut tools = vec![];
        if !r.tools.is_empty() {
            tools.push(json!({"functionDeclarations": r.tools.iter().map(|d| json!({"name": d.name, "description": d.description, "parameters": d.input_schema})).collect::<Vec<_>>()}));
        }
        if r.server_tools.web_search.is_some() {
            tools.push(json!({"googleSearch": {}}));
        }
        if r.server_tools.code_execution {
            tools.push(json!({"codeExecution": {}}));
        }
        if !tools.is_empty() {
            t.insert("tools".into(), Value::Array(tools));
        }
        if !r.tools.is_empty() {
            let cfg = match &r.tool_choice {
                ToolChoice::Auto => json!({"mode": "AUTO"}),
                ToolChoice::Required => json!({"mode": "ANY"}),
                ToolChoice::None => json!({"mode": "NONE"}),
                ToolChoice::Tool { name } => json!({"mode": "ANY", "allowedFunctionNames": [name]}),
            };
            t.insert("toolConfig".into(), json!({"functionCallingConfig": cfg}));
        }
        let mut gen_cfg = serde_json::Map::new();
        if let Some(n) = r.max_output_tokens {
            gen_cfg.insert("maxOutputTokens".into(), json!(n));
        }
        if let Some(v) = r.sampling.temperature {
            gen_cfg.insert("temperature".into(), json!(v));
        }
        if let Some(v) = r.sampling.top_p {
            gen_cfg.insert("topP".into(), json!(v));
        }
        if let Some(v) = r.sampling.top_k {
            gen_cfg.insert("topK".into(), json!(v));
        }
        if !r.sampling.stop.is_empty() {
            gen_cfg.insert("stopSequences".into(), json!(r.sampling.stop));
        }
        if let Some(e) = r.effort {
            let tc = match r.model.effort_wire(e) {
                Some(EffortWire::ThinkingLevel(l)) | Some(EffortWire::Effort(l)) => {
                    Some(json!({"thinkingLevel": l, "includeThoughts": true}))
                }
                Some(EffortWire::Budget(b)) => {
                    Some(json!({"thinkingBudget": b, "includeThoughts": true}))
                }
                Some(EffortWire::Off) => Some(json!({"thinkingBudget": 0})),
                None => None,
            };
            if let Some(tc) = tc {
                gen_cfg.insert("thinkingConfig".into(), tc);
            }
        }
        if !gen_cfg.is_empty() {
            t.insert("generationConfig".into(), Value::Object(gen_cfg));
        }
        Ok(BuiltBody { body, records })
    }

    fn decode(&self, frame: &RawFrame) -> Result<Option<Value>, ProviderError> {
        serde_json::from_str::<Value>(&frame.data)
            .map(Some)
            .map_err(|e| ProviderError::invalid_output(format!("gemini: bad JSON chunk: {e}")))
    }

    fn begin(&self, st: &mut State, ctx: &StreamContext) {
        st.origin = Some(ctx.origin.clone());
    }

    fn step(
        &self,
        st: &mut State,
        v: Value,
        out: &mut Vec<StreamEvent>,
    ) -> Result<(), ProviderError> {
        st.chunk(v, out)
    }

    fn on_halt(&self, mut st: State, out: &mut Vec<StreamEvent>) -> Result<(), ProviderError> {
        if st.done {
            return Ok(());
        }
        if st.blocked.is_some() {
            st.finish(None, out);
            return Ok(());
        }
        Err(ProviderError::incomplete(
            "gemini: stream ended without finishReason",
        ))
    }

    fn classify(&self, f: &HttpFailure) -> ProviderError {
        let json = f.json();
        let (status, message) = json
            .as_ref()
            .map(|v| {
                let e = match v {
                    Value::Array(a) => a.first().and_then(|x| x.get("error")).cloned(),
                    v => v.get("error").cloned(),
                }
                .unwrap_or(Value::Null);
                (
                    str_at(&e, "status").map(String::from),
                    str_at(&e, "message").map(String::from),
                )
            })
            .unwrap_or_default();
        let lower = message.as_deref().unwrap_or("").to_ascii_lowercase();
        let kind = match status.as_deref() {
            Some("RESOURCE_EXHAUSTED")
                if lower.contains("billing") || lower.contains("check your plan") =>
            {
                Some(ErrorKind::QuotaExceeded)
            }
            Some("RESOURCE_EXHAUSTED") => Some(ErrorKind::RateLimited),
            Some("UNAUTHENTICATED" | "PERMISSION_DENIED") => Some(ErrorKind::Authentication),
            Some("UNAVAILABLE") => Some(ErrorKind::Overloaded),
            Some("INTERNAL" | "DEADLINE_EXCEEDED") => Some(ErrorKind::ProviderInternal),
            _ => None,
        };
        classify(
            f,
            ProtocolHint {
                kind,
                code: status,
                message,
            },
        )
    }
}

// ---------- stream ----------

#[derive(Debug, Default)]
pub struct State {
    origin: Option<Origin>,
    next_part: PartIdx,
    started: bool,
    done: bool,
    /// Open text part: (idx, signature).
    text: Option<(PartIdx, Option<String>)>,
    /// Open reasoning part: (idx, text, signature).
    reasoning: Option<(PartIdx, String, Option<String>)>,
    calls: u32,
    usage: Usage,
    grounding: Option<Value>,
    blocked: Option<String>,
}

fn grounding_citations(g: &Value) -> Vec<Citation> {
    let chunks = g
        .get("groundingChunks")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let mut out = vec![];
    for s in g
        .get("groundingSupports")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        let seg = s.get("segment").cloned().unwrap_or(Value::Null);
        let span = match (
            seg.get("startIndex").and_then(Value::as_u64),
            seg.get("endIndex").and_then(Value::as_u64),
        ) {
            (s, Some(e)) => Some((s.unwrap_or(0) as usize, e as usize)),
            _ => None,
        };
        for i in s
            .get("groundingChunkIndices")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            let Some(web) = i
                .as_u64()
                .and_then(|i| chunks.get(i as usize))
                .and_then(|c| c.get("web"))
            else {
                continue;
            };
            out.push(Citation {
                kind: CitationKind::WebUrl,
                cited_text: str_at(&seg, "text").map(String::from),
                source: CitationSource {
                    url: str_at(web, "uri").map(String::from),
                    title: str_at(web, "title").map(String::from),
                    document_index: None,
                },
                span,
                opaque: None,
            });
        }
    }
    if out.is_empty() {
        for c in &chunks {
            if let Some(web) = c.get("web") {
                out.push(Citation {
                    kind: CitationKind::WebUrl,
                    cited_text: None,
                    source: CitationSource {
                        url: str_at(web, "uri").map(String::from),
                        title: str_at(web, "title").map(String::from),
                        document_index: None,
                    },
                    span: None,
                    opaque: None,
                });
            }
        }
    }
    out
}

pub fn read_usage(u: &Value) -> Usage {
    let prompt = u64_at(u, &["promptTokenCount"]);
    let cached = u64_at(u, &["cachedContentTokenCount"]);
    let thoughts = u64_at(u, &["thoughtsTokenCount"]);
    Usage {
        non_cached_input: prompt.saturating_sub(cached),
        cache_read: cached,
        // candidatesTokenCount excludes thoughts.
        output_total: u64_at(u, &["candidatesTokenCount"]) + thoughts,
        reasoning: u.get("thoughtsTokenCount").map(|_| thoughts),
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
            provider: ProviderId::new("google"),
            model: String::new(),
            protocol: ProtocolKind::Gemini,
            account: None,
        })
    }
    fn sig_opaque(&self, s: Option<String>) -> Option<Opaque> {
        s.map(|signature| Opaque {
            origin: self.origin(),
            data: OpaqueData::GeminiThoughtSignature { signature },
        })
    }

    fn close_reasoning(&mut self, out: &mut Vec<StreamEvent>) {
        if let Some((part, text, s)) = self.reasoning.take() {
            let opaque = self.sig_opaque(s);
            out.push(StreamEvent::ReasoningEnd {
                part,
                final_text: Some(text),
                opaque,
            });
        }
    }
    fn close_text(&mut self, out: &mut Vec<StreamEvent>) {
        if let Some((part, s)) = self.text.take() {
            let citations = self
                .grounding
                .as_ref()
                .map(grounding_citations)
                .unwrap_or_default();
            let opaque = self.sig_opaque(s);
            out.push(StreamEvent::TextEnd {
                part,
                citations,
                opaque,
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
            let mut e = Gemini.classify(&f);
            e.status = None;
            return Err(e);
        }
        if !self.started {
            self.started = true;
            out.push(StreamEvent::StepStart { step: 0 });
        }
        if let (Some(m), Some(o)) = (str_at(&v, "modelVersion"), self.origin.as_mut()) {
            o.model = m.to_string();
        }
        if let Some(u) = v.get("usageMetadata") {
            self.usage = read_usage(u);
        }
        if let Some(b) = v
            .get("promptFeedback")
            .and_then(|p| str_at(p, "blockReason"))
        {
            self.blocked = Some(b.to_string());
        }
        let Some(cand) = v
            .get("candidates")
            .and_then(Value::as_array)
            .and_then(|c| c.first())
        else {
            return Ok(());
        };
        if let Some(g) = cand.get("groundingMetadata") {
            self.grounding = Some(g.clone());
        }
        for p in cand
            .get("content")
            .and_then(|c| c.get("parts"))
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            let signature = str_at(p, "thoughtSignature").map(String::from);
            if let Some(fc) = p.get("functionCall") {
                self.close_reasoning(out);
                self.close_text(out);
                let part = self.part();
                self.calls += 1;
                let id = str_at(fc, "id")
                    .map(String::from)
                    .unwrap_or_else(|| format!("call_{}", self.calls));
                let name = str_at(fc, "name").unwrap_or("").to_string();
                let input = fc.get("args").cloned().unwrap_or_else(|| json!({}));
                out.push(StreamEvent::ToolInputStart {
                    part,
                    call_id: ToolCallId::new(id.clone()),
                    name: name.clone(),
                    provider_executed: false,
                });
                out.push(StreamEvent::ToolInputDelta {
                    part,
                    json_fragment: input.to_string(),
                });
                out.push(StreamEvent::ToolInputEnd {
                    part,
                    call: ToolCall {
                        id: ToolCallId::new(id),
                        name,
                        input,
                        provider_executed: false,
                        opaque: self.sig_opaque(signature),
                    },
                });
            } else if p.get("executableCode").is_some() || p.get("codeExecutionResult").is_some() {
                self.close_reasoning(out);
                self.close_text(out);
                let part = self.part();
                let origin = self.origin();
                if let Some(code) = p.get("executableCode") {
                    self.calls += 1;
                    let id = ToolCallId::new(format!("code_{}", self.calls));
                    out.push(StreamEvent::ToolInputStart {
                        part,
                        call_id: id.clone(),
                        name: "code_execution".into(),
                        provider_executed: true,
                    });
                    out.push(StreamEvent::ToolInputEnd {
                        part,
                        call: ToolCall {
                            id,
                            name: "code_execution".into(),
                            input: code.clone(),
                            provider_executed: true,
                            opaque: Some(Opaque {
                                origin,
                                data: OpaqueData::Raw { value: p.clone() },
                            }),
                        },
                    });
                } else if let Some(res) = p.get("codeExecutionResult") {
                    out.push(StreamEvent::ProviderToolResult {
                        part,
                        result: ToolResult {
                            call_id: ToolCallId::new(format!("code_{}", self.calls)),
                            content: ToolOutput::Json(res.clone()),
                            is_error: str_at(res, "outcome").is_some_and(|o| o != "OUTCOME_OK"),
                            cache: None,
                            opaque: Some(Opaque {
                                origin,
                                data: OpaqueData::Raw { value: p.clone() },
                            }),
                        },
                    });
                }
            } else if p.get("thought").and_then(Value::as_bool) == Some(true) {
                self.close_text(out);
                let text = str_at(p, "text").unwrap_or("").to_string();
                if self.reasoning.is_none() {
                    let part = self.part();
                    out.push(StreamEvent::ReasoningStart { part });
                    self.reasoning = Some((part, String::new(), None));
                }
                if let Some((part, acc, s)) = self.reasoning.as_mut() {
                    if !text.is_empty() {
                        acc.push_str(&text);
                        out.push(StreamEvent::ReasoningDelta {
                            part: *part,
                            delta: text,
                        });
                    }
                    if signature.is_some() {
                        *s = signature;
                    }
                }
            } else if let Some(text) = str_at(p, "text") {
                self.close_reasoning(out);
                if self.text.is_none() && (!text.is_empty() || signature.is_some()) {
                    let part = self.part();
                    out.push(StreamEvent::TextStart { part });
                    self.text = Some((part, None));
                }
                if let Some((part, s)) = self.text.as_mut() {
                    if !text.is_empty() {
                        out.push(StreamEvent::TextDelta {
                            part: *part,
                            delta: text.to_string(),
                        });
                    }
                    if signature.is_some() {
                        *s = signature;
                    }
                }
            }
            // other part kinds are ignored
        }
        if let Some(fr) = str_at(cand, "finishReason") {
            self.finish(Some(fr.to_string()), out);
        }
        Ok(())
    }

    fn finish(&mut self, fr: Option<String>, out: &mut Vec<StreamEvent>) {
        self.close_reasoning(out);
        self.close_text(out);
        let reason = match fr.as_deref() {
            Some("STOP") if self.calls > 0 => FinishReason::ToolCalls,
            Some("STOP") => FinishReason::Stop,
            Some("MAX_TOKENS") => FinishReason::Length,
            Some(
                "SAFETY" | "RECITATION" | "BLOCKLIST" | "PROHIBITED_CONTENT" | "SPII"
                | "IMAGE_SAFETY",
            ) => FinishReason::ContentFilter,
            Some("MALFORMED_FUNCTION_CALL" | "UNEXPECTED_TOOL_CALL") => FinishReason::Error,
            None if self.blocked.is_some() => FinishReason::ContentFilter,
            _ => FinishReason::Unknown,
        };
        let usage = self.usage;
        let details = FinishDetails {
            raw: fr.or_else(|| self.blocked.clone()),
            category: self.blocked.clone(),
            explanation: None,
        };
        out.push(StreamEvent::StepFinish {
            reason,
            details,
            usage,
        });
        out.push(StreamEvent::Finish { reason, usage });
        self.done = true;
    }
}
