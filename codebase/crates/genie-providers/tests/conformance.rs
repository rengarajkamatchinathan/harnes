//! Conformance suite, layers 1–2 (03 §11): cassette decoding per adapter and golden
//! normalisation tests (canonical history → exact wire JSON per protocol and route).
//!
//! Cassettes in `tests/cassettes` are written from the vendors' documented stream shapes;
//! the live-smoke layer re-records them. Goldens in `tests/golden` are regenerated with
//! `GENIE_UPDATE_GOLDEN=1 cargo test -p genie-providers --test conformance` and reviewed.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::result_large_err)] // test helpers

use std::path::PathBuf;
use std::sync::Arc;

use genie_providers::auth::NoAuth;
use genie_providers::compat::{EffortMapping, EffortWire, ModelSpec};
use genie_providers::event::StreamEvent;
use genie_providers::model::*;
use genie_providers::protocol::{StreamContext, decode_all, for_kind};
use genie_providers::route::{Preset, PresetProvider, ProviderConfig, Route};
use genie_providers::{ErrorKind, Provider, compile};
use serde_json::{Value, json};

fn dir(sub: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join(sub)
}

fn cassette(name: &str) -> String {
    std::fs::read_to_string(dir("cassettes").join(name)).unwrap()
}

fn origin(p: ProtocolKind, model: &str) -> Origin {
    Origin {
        provider: ProviderId::new("p"),
        model: model.into(),
        protocol: p,
        account: None,
    }
}

fn decode(
    p: ProtocolKind,
    model: &str,
    name: &str,
) -> (Vec<StreamEvent>, Option<genie_providers::ProviderError>) {
    let proto = for_kind(p).unwrap();
    let ctx = StreamContext {
        origin: origin(p, model),
        compat: Default::default(),
    };
    decode_all(proto.as_ref(), &ctx, cassette(name).as_bytes())
}

fn route_for(
    preset: Preset,
    p: ProtocolKind,
    model: &str,
) -> Result<Route, genie_providers::ProviderError> {
    let cfg = ProviderConfig {
        id: ProviderId::new("p"),
        preset,
        base_url: match preset {
            Preset::AzureFoundry => Some("https://res.services.ai.azure.com".into()),
            Preset::OpenAiCompatible => Some("http://vllm.local:8000".into()),
            _ => None,
        },
        project: Some("proj".into()),
        location: Some("global".into()),
        auth: Arc::new(NoAuth),
        account: None,
        compat_overlay: None,
        headers: vec![],
    };
    PresetProvider(cfg).route(&ModelSpec::new("p", model, p))
}

fn text_of(events: &[StreamEvent]) -> String {
    events
        .iter()
        .filter_map(|e| {
            if let StreamEvent::TextDelta { delta, .. } = e {
                Some(delta.as_str())
            } else {
                None
            }
        })
        .collect()
}

fn finish(events: &[StreamEvent]) -> (FinishReason, Usage) {
    match events.last() {
        Some(StreamEvent::Finish { reason, usage }) => (*reason, *usage),
        other => panic!("last event is not Finish: {other:?}"),
    }
}

// ---------- layer 1: decoding ----------

#[test]
fn anthropic_thinking_text_tool() {
    let (ev, err) = decode(
        ProtocolKind::AnthropicMessages,
        "claude-opus-5-5",
        "anthropic_thinking_tool.sse",
    );
    assert!(err.is_none(), "{err:?}");
    assert_eq!(ev[0], StreamEvent::StepStart { step: 0 });
    let reasoning_end = ev.iter().find_map(|e| match e {
        StreamEvent::ReasoningEnd {
            final_text, opaque, ..
        } => Some((final_text.clone(), opaque.clone())),
        _ => None,
    });
    let (text, opaque) = reasoning_end.unwrap();
    assert_eq!(text.as_deref(), Some("Need to read the file."));
    let o = opaque.unwrap();
    assert_eq!(
        o.data,
        OpaqueData::AnthropicSignature {
            signature: "EqQBCkgIARABGAIiQL+/sig==".into()
        }
    );
    assert_eq!(o.origin.model, "claude-opus-5-5");
    assert_eq!(text_of(&ev), "Let me look at it — ok.");
    let call = ev
        .iter()
        .find_map(|e| {
            if let StreamEvent::ToolInputEnd { call, .. } = e {
                Some(call.clone())
            } else {
                None
            }
        })
        .unwrap();
    assert_eq!(call.id.0, "toolu_01A");
    assert_eq!(call.input, json!({"path": "src/main.rs"}));
    let (reason, usage) = finish(&ev);
    assert_eq!(reason, FinishReason::ToolCalls);
    assert_eq!(usage.non_cached_input, 12);
    assert_eq!(usage.cache_read, 3000);
    assert_eq!(usage.cache_write_5m, 200);
    assert_eq!(usage.cache_write_1h, 100);
    assert_eq!(usage.input_total, 3312);
    assert_eq!(usage.output_total, 87, "message_delta usage is cumulative");
}

#[test]
fn anthropic_redacted_search_citations_fallback() {
    let (ev, err) = decode(
        ProtocolKind::AnthropicMessages,
        "claude-sonnet-5-5",
        "anthropic_search_redacted.sse",
    );
    assert!(err.is_none(), "{err:?}");
    assert!(ev.iter().any(|e| matches!(e,
        StreamEvent::OpaqueBlock { value: Part::Reasoning(ReasoningPart { redacted: true, opaque: Some(Opaque { data: OpaqueData::AnthropicRedacted { data }, .. }), .. }), .. }
        if data.starts_with("EmwKAhgB"))));
    let server_call = ev
        .iter()
        .find_map(|e| {
            if let StreamEvent::ToolInputEnd { call, .. } = e {
                Some(call.clone())
            } else {
                None
            }
        })
        .unwrap();
    assert!(server_call.provider_executed);
    assert_eq!(server_call.input, json!({"query": "rust 2026 edition"}));
    assert!(
        matches!(&server_call.opaque, Some(Opaque { data: OpaqueData::Raw { value }, .. }) if value["type"] == "server_tool_use")
    );
    let result = ev
        .iter()
        .find_map(|e| {
            if let StreamEvent::ProviderToolResult { result, .. } = e {
                Some(result.clone())
            } else {
                None
            }
        })
        .unwrap();
    assert_eq!(result.call_id.0, "srvtoolu_01");
    assert!(!result.is_error);
    let cites = ev
        .iter()
        .find_map(|e| {
            if let StreamEvent::TextEnd { citations, .. } = e {
                Some(citations.clone())
            } else {
                None
            }
        })
        .unwrap();
    assert_eq!(cites.len(), 1);
    assert_eq!(cites[0].kind, CitationKind::WebUrl);
    assert_eq!(
        cites[0].source.url.as_deref(),
        Some("https://example.org/rust")
    );
    assert!(ev.contains(&StreamEvent::ModelSwitched {
        from: "claude-sonnet-5-5".into(),
        to: "claude-haiku-5-5".into()
    }));
    assert_eq!(finish(&ev).0, FinishReason::Stop);
}

#[test]
fn anthropic_truncated_stream_is_incomplete() {
    let full = cassette("anthropic_thinking_tool.sse");
    let cut = &full[..full.find("event: message_delta").unwrap()];
    let proto = for_kind(ProtocolKind::AnthropicMessages).unwrap();
    let ctx = StreamContext {
        origin: origin(ProtocolKind::AnthropicMessages, "m"),
        compat: Default::default(),
    };
    let (_, err) = decode_all(proto.as_ref(), &ctx, cut.as_bytes());
    assert_eq!(err.unwrap().kind, ErrorKind::IncompleteStream);
}

#[test]
fn anthropic_in_stream_error_is_classified() {
    let body = "event: message_start\ndata: {\"type\":\"message_start\",\"message\":{\"model\":\"m\",\"usage\":{}}}\n\n\
                event: error\ndata: {\"type\":\"error\",\"error\":{\"type\":\"overloaded_error\",\"message\":\"Overloaded\"}}\n\n";
    let proto = for_kind(ProtocolKind::AnthropicMessages).unwrap();
    let ctx = StreamContext {
        origin: origin(ProtocolKind::AnthropicMessages, "m"),
        compat: Default::default(),
    };
    let (_, err) = decode_all(proto.as_ref(), &ctx, body.as_bytes());
    let err = err.unwrap();
    assert_eq!(err.kind, ErrorKind::Overloaded);
    assert!(err.retry.retryable);
}

#[test]
fn responses_reasoning_text_citation_tool() {
    let (ev, err) = decode(
        ProtocolKind::OpenaiResponses,
        "gpt-5.1",
        "responses_reasoning_tool.sse",
    );
    assert!(err.is_none(), "{err:?}");
    let (text, opaque) = ev
        .iter()
        .find_map(|e| match e {
            StreamEvent::ReasoningEnd {
                final_text, opaque, ..
            } => Some((final_text.clone(), opaque.clone())),
            _ => None,
        })
        .unwrap();
    assert_eq!(text.as_deref(), Some("Check the weather tool."));
    let o = opaque.unwrap();
    assert_eq!(
        o.data,
        OpaqueData::ResponsesReasoning {
            item_id: "rs_1".into(),
            encrypted_content: Some("gAAAAABo-enc-1".into()),
            summary: vec!["Check the weather tool.".into()]
        }
    );
    assert_eq!(
        o.origin.model, "gpt-5.1-2026-01-01",
        "served model is recorded"
    );
    assert_eq!(text_of(&ev), "Per the forecast");
    let cites = ev
        .iter()
        .find_map(|e| {
            if let StreamEvent::TextEnd { citations, .. } = e {
                Some(citations.clone())
            } else {
                None
            }
        })
        .unwrap();
    assert_eq!(cites[0].span, Some((0, 16)));
    let call = ev
        .iter()
        .find_map(|e| {
            if let StreamEvent::ToolInputEnd { call, .. } = e {
                Some(call.clone())
            } else {
                None
            }
        })
        .unwrap();
    assert_eq!(call.id.0, "call_abc");
    assert_eq!(call.input, json!({"city": "Oslo"}));
    let (reason, usage) = finish(&ev);
    assert_eq!(reason, FinishReason::ToolCalls);
    assert_eq!(
        (
            usage.non_cached_input,
            usage.cache_read,
            usage.output_total,
            usage.reasoning
        ),
        (200, 800, 120, Some(64))
    );
}

#[test]
fn chat_ollama_reasoning_index_less_tools_derived_finish() {
    let (ev, err) = decode(
        ProtocolKind::OpenaiChat,
        "qwen3:8b",
        "chat_ollama_tools.sse",
    );
    assert!(err.is_none(), "{err:?}");
    let r = ev
        .iter()
        .find_map(|e| {
            if let StreamEvent::ReasoningEnd { final_text, .. } = e {
                final_text.clone()
            } else {
                None
            }
        })
        .unwrap();
    assert_eq!(r, "User wants two files.");
    assert_eq!(text_of(&ev), "Reading both.");
    let calls: Vec<ToolCall> = ev
        .iter()
        .filter_map(|e| {
            if let StreamEvent::ToolInputEnd { call, .. } = e {
                Some(call.clone())
            } else {
                None
            }
        })
        .collect();
    assert_eq!(calls.len(), 2);
    assert_eq!(calls[0].input, json!({"path": "a.txt"}));
    assert_eq!(calls[1].id.0, "call_b");
    let (reason, usage) = finish(&ev);
    assert_eq!(
        reason,
        FinishReason::ToolCalls,
        "derived, not copied from `stop`"
    );
    assert_eq!(
        (usage.non_cached_input, usage.cache_read, usage.output_total),
        (200, 100, 42)
    );
}

#[test]
fn chat_without_done_or_finish_is_incomplete() {
    let body = "data: {\"choices\":[{\"index\":0,\"delta\":{\"content\":\"hi\"}}]}\n\n";
    let proto = for_kind(ProtocolKind::OpenaiChat).unwrap();
    let ctx = StreamContext {
        origin: origin(ProtocolKind::OpenaiChat, "m"),
        compat: Default::default(),
    };
    let (_, err) = decode_all(proto.as_ref(), &ctx, body.as_bytes());
    assert_eq!(err.unwrap().kind, ErrorKind::IncompleteStream);
    // …but `[DONE]` with no finish reason is a clean stop.
    let body = format!("{body}data: [DONE]\n\n");
    let (ev, err) = decode_all(proto.as_ref(), &ctx, body.as_bytes());
    assert!(err.is_none());
    assert_eq!(finish(&ev).0, FinishReason::Stop);
}

#[test]
fn gemini_thoughts_signatures_grounding_call() {
    let (ev, err) = decode(
        ProtocolKind::Gemini,
        "gemini-3-pro",
        "gemini_thinking_call.sse",
    );
    assert!(err.is_none(), "{err:?}");
    let (text, opaque) = ev
        .iter()
        .find_map(|e| match e {
            StreamEvent::ReasoningEnd {
                final_text, opaque, ..
            } => Some((final_text.clone(), opaque.clone())),
            _ => None,
        })
        .unwrap();
    assert_eq!(text.as_deref(), Some("Plan: search, then answer."));
    assert_eq!(
        opaque.unwrap().data,
        OpaqueData::GeminiThoughtSignature {
            signature: "CiQB0e2Kb-thought-sig".into()
        }
    );
    assert_eq!(text_of(&ev), "The answer is 42.");
    let cites = ev
        .iter()
        .find_map(|e| {
            if let StreamEvent::TextEnd { citations, .. } = e {
                Some(citations.clone())
            } else {
                None
            }
        })
        .unwrap();
    assert_eq!(
        cites[0].source.url.as_deref(),
        Some("https://example.org/answer")
    );
    assert_eq!(cites[0].span, Some((0, 17)));
    let call = ev
        .iter()
        .find_map(|e| {
            if let StreamEvent::ToolInputEnd { call, .. } = e {
                Some(call.clone())
            } else {
                None
            }
        })
        .unwrap();
    assert_eq!(call.name, "save_note");
    assert_eq!(
        call.opaque.unwrap().data,
        OpaqueData::GeminiThoughtSignature {
            signature: "CiQB0e2Kb-call-sig".into()
        }
    );
    let (reason, usage) = finish(&ev);
    assert_eq!(reason, FinishReason::ToolCalls);
    assert_eq!(
        (
            usage.non_cached_input,
            usage.cache_read,
            usage.output_total,
            usage.reasoning
        ),
        (300, 600, 100, Some(70))
    );
}

#[test]
fn gemini_http_errors() {
    let proto = for_kind(ProtocolKind::Gemini).unwrap();
    let f = |status, body: &str| genie_providers::HttpFailure {
        status,
        headers: vec![],
        body: body.into(),
    };
    let e = proto.classify(&f(429, r#"{"error":{"code":429,"status":"RESOURCE_EXHAUSTED","message":"Resource has been exhausted"}}"#));
    assert_eq!(e.kind, ErrorKind::RateLimited);
    let e = proto.classify(&f(429, r#"{"error":{"status":"RESOURCE_EXHAUSTED","message":"You exceeded your current quota, please check your plan and billing details."}}"#));
    assert_eq!(e.kind, ErrorKind::QuotaExceeded);
    let e = proto.classify(&f(400, r#"[{"error":{"status":"INVALID_ARGUMENT","message":"The input token count (2000000) exceeds the maximum number of tokens allowed (1048576)."}}]"#));
    assert_eq!(e.kind, ErrorKind::ContextOverflow);
}

#[test]
fn anthropic_and_openai_http_errors() {
    let a = for_kind(ProtocolKind::AnthropicMessages).unwrap();
    let f = |status, body: &str| genie_providers::HttpFailure {
        status,
        headers: vec![],
        body: body.into(),
    };
    assert_eq!(
        a.classify(&f(
            529,
            r#"{"type":"error","error":{"type":"overloaded_error","message":"Overloaded"}}"#
        ))
        .kind,
        ErrorKind::Overloaded
    );
    assert_eq!(
        a.classify(&f(400, r#"{"type":"error","error":{"type":"invalid_request_error","message":"prompt is too long: 210000 tokens > 200000 maximum"}}"#)).kind,
        ErrorKind::ContextOverflow
    );
    assert_eq!(a.classify(&f(401, r#"{"type":"error","error":{"type":"authentication_error","message":"invalid x-api-key"}}"#)).kind, ErrorKind::Authentication);
    let o = for_kind(ProtocolKind::OpenaiResponses).unwrap();
    assert_eq!(
        o.classify(&f(
            429,
            r#"{"error":{"code":"insufficient_quota","message":"quota"}}"#
        ))
        .kind,
        ErrorKind::QuotaExceeded
    );
    assert_eq!(
        o.classify(&f(
            400,
            r#"{"error":{"code":"context_length_exceeded","message":"too long"}}"#
        ))
        .kind,
        ErrorKind::ContextOverflow
    );
}

// ---------- layer 2: golden normalisation ----------

fn golden(name: &str, actual: &Value) {
    let path = dir("golden").join(format!("{name}.json"));
    let pretty = serde_json::to_string_pretty(actual).unwrap() + "\n";
    if std::env::var_os("GENIE_UPDATE_GOLDEN").is_some()
        || !path.exists() && std::env::var_os("CI").is_none()
    {
        std::fs::write(&path, &pretty).unwrap();
        return;
    }
    let expected = std::fs::read_to_string(&path).unwrap();
    assert_eq!(
        pretty, expected,
        "golden {name} differs; rerun with GENIE_UPDATE_GOLDEN=1 and review the diff"
    );
}

/// A canonical history exercising text, media, reasoning replay, tool calls and results.
fn history(origin: Origin, opaque: Option<OpaqueData>) -> Vec<Message> {
    let mut a = Message::new(
        "a1",
        Role::Assistant,
        vec![
            Part::Reasoning(ReasoningPart {
                text: "I should read it.".into(),
                redacted: false,
                opaque: opaque.map(|data| Opaque {
                    origin: origin.clone(),
                    data,
                }),
            }),
            Part::text("Reading."),
            Part::ToolCall(ToolCall {
                id: ToolCallId::new("call:1"),
                name: "read_file".into(),
                input: json!({"path": "a.txt"}),
                provider_executed: false,
                opaque: None,
            }),
        ],
    );
    a.origin = Some(origin);
    vec![
        Message::new(
            "u1",
            Role::User,
            vec![
                Part::text("Summarise a.txt"),
                Part::Media(MediaPart {
                    mime: "image/png".into(),
                    source: MediaSource::Base64 {
                        data: "iVBORw0KGgo=".into(),
                    },
                    filename: None,
                    extracted_text: None,
                    cache: None,
                }),
            ],
        ),
        a,
        Message::new(
            "t1",
            Role::Tool,
            vec![Part::ToolResult(ToolResult {
                call_id: ToolCallId::new("call:1"),
                content: ToolOutput::Text("hello world".into()),
                is_error: false,
                cache: None,
                opaque: None,
            })],
        ),
    ]
}

fn spec(p: ProtocolKind, model: &str) -> ModelSpec {
    let mut m = ModelSpec::new("p", model, p);
    m.input.image = true;
    m.max_output = Some(4096);
    m.prompt_cache.ttl_1h = true;
    m.effort = vec![EffortMapping {
        level: EffortLevel::High,
        wire: match p {
            ProtocolKind::Gemini => EffortWire::ThinkingLevel("high".into()),
            _ => EffortWire::Effort("high".into()),
        },
    }];
    m
}

fn request(p: ProtocolKind, model: &str, msgs: Vec<Message>) -> LlmRequest {
    let mut r = LlmRequest::new(spec(p, model), msgs);
    r.system = vec![
        TextPart {
            text: "You are Genie.".into(),
            ..Default::default()
        },
        TextPart {
            text: "Project: demo".into(),
            ..Default::default()
        },
    ];
    r.tools = vec![ToolDef {
        name: "read_file".into(),
        description: "Read a file".into(),
        input_schema: json!({"type": "object", "$schema": "x", "additionalProperties": false, "properties": {"path": {"type": "string"}}, "required": ["path"]}),
        cache: None,
    }];
    r.effort = Some(EffortLevel::High);
    r.prompt_cache_key = Some("ses_root".into());
    r.baseline_long_ttl = true;
    r
}

fn build(route: &Route, req: LlmRequest) -> (Value, Vec<WireRecord>) {
    let compiled = compile(req, &route.info()).unwrap();
    let built = route.protocol.build(&compiled, &route.compat).unwrap();
    (serde_json::from_slice(&built.body).unwrap(), built.records)
}

#[test]
fn golden_anthropic_api() {
    let route = route_for(
        Preset::Anthropic,
        ProtocolKind::AnthropicMessages,
        "claude-opus-5-5",
    )
    .unwrap();
    let o = Origin {
        provider: ProviderId::new("p"),
        model: "claude-opus-5-5".into(),
        protocol: ProtocolKind::AnthropicMessages,
        account: None,
    };
    let (body, _) = build(
        &route,
        request(
            ProtocolKind::AnthropicMessages,
            "claude-opus-5-5",
            history(
                o,
                Some(OpaqueData::AnthropicSignature {
                    signature: "sig-1".into(),
                }),
            ),
        ),
    );
    golden("anthropic_api", &body);
    // Signature replayed byte-exact; tool id rewritten to the Anthropic charset.
    assert_eq!(
        body["messages"][1]["content"][0],
        json!({"type": "thinking", "thinking": "I should read it.", "signature": "sig-1"})
    );
    assert_eq!(body["messages"][1]["content"][2]["id"], "call_1");
    assert_eq!(body["messages"][2]["content"][0]["tool_use_id"], "call_1");
    assert_eq!(body["output_config"], json!({"effort": "high"}));
    assert_eq!(
        body["system"][1]["cache_control"],
        json!({"type": "ephemeral", "ttl": "1h"})
    );
}

#[test]
fn golden_anthropic_vertex() {
    let route = route_for(
        Preset::Vertex,
        ProtocolKind::AnthropicMessages,
        "claude-opus-5-5",
    )
    .unwrap();
    let (body, _) = build(
        &route,
        request(
            ProtocolKind::AnthropicMessages,
            "claude-opus-5-5",
            vec![Message::user_text("u1", "hi")],
        ),
    );
    golden("anthropic_vertex", &body);
    assert!(body.get("model").is_none());
    assert_eq!(body["anthropic_version"], "vertex-2023-10-16");
}

#[test]
fn golden_responses_openai_and_azure() {
    let o = Origin {
        provider: ProviderId::new("p"),
        model: "gpt-5.1".into(),
        protocol: ProtocolKind::OpenaiResponses,
        account: None,
    };
    let data = OpaqueData::ResponsesReasoning {
        item_id: "rs_1".into(),
        encrypted_content: Some("enc".into()),
        summary: vec!["I should read it.".into()],
    };
    let route = route_for(Preset::OpenAi, ProtocolKind::OpenaiResponses, "gpt-5.1").unwrap();
    let (body, _) = build(
        &route,
        request(
            ProtocolKind::OpenaiResponses,
            "gpt-5.1",
            history(o.clone(), Some(data.clone())),
        ),
    );
    golden("responses_openai", &body);
    assert_eq!(body["store"], false);
    assert_eq!(body["prompt_cache_key"], "ses_root");
    assert_eq!(body["reasoning"], json!({"effort": "high"}));
    let input = body["input"].as_array().unwrap();
    assert!(
        input
            .iter()
            .any(|i| i["type"] == "reasoning" && i["encrypted_content"] == "enc")
    );
    assert!(
        input
            .iter()
            .any(|i| i["type"] == "function_call_output" && i["call_id"] == "call:1")
    );

    let route = route_for(
        Preset::AzureFoundry,
        ProtocolKind::OpenaiResponses,
        "gpt-5.1",
    )
    .unwrap();
    let (azure, _) = build(
        &route,
        request(
            ProtocolKind::OpenaiResponses,
            "gpt-5.1",
            history(o, Some(data)),
        ),
    );
    assert_eq!(
        azure, body,
        "Azure v1 uses the same body; only endpoint and auth differ"
    );
}

#[test]
fn golden_chat_variants() {
    // Reasoning from another protocol is out of scope → sent as readable text.
    let o = Origin {
        provider: ProviderId::new("p"),
        model: "claude".into(),
        protocol: ProtocolKind::AnthropicMessages,
        account: None,
    };
    let route = route_for(Preset::Ollama, ProtocolKind::OpenaiChat, "qwen3:8b").unwrap();
    let mut req = request(
        ProtocolKind::OpenaiChat,
        "qwen3:8b",
        history(
            o.clone(),
            Some(OpaqueData::AnthropicSignature {
                signature: "s".into(),
            }),
        ),
    );
    req.max_output_tokens = Some(1000);
    let compiled = compile(req.clone(), &route.info()).unwrap();
    assert!(
        compiled
            .diagnostics
            .iter()
            .any(|d| d.0.contains("dropped 1 opaque"))
    );
    let (body, _) = build(&route, req.clone());
    golden("chat_ollama", &body);
    assert_eq!(body["stream_options"], json!({"include_usage": true}));
    assert_eq!(body["max_tokens"], 1000);
    assert_eq!(
        body["messages"][3]["content"],
        "I should read it.\n\nReading."
    );

    let route = route_for(
        Preset::AzureFoundry,
        ProtocolKind::OpenaiChat,
        "deepseek-r2",
    )
    .unwrap();
    let (body, _) = build(&route, req.clone());
    assert_eq!(body["max_completion_tokens"], 1000);

    let mut route = route_for(Preset::OpenAiCompatible, ProtocolKind::OpenaiChat, "llama").unwrap();
    route.compat.baseline_as_user = true;
    let (body, _) = build(&route, req);
    golden("chat_compatible_baseline_as_user", &body);
    assert_eq!(
        body["messages"][1]["content"],
        "<context>\nProject: demo\n</context>"
    );
}

#[test]
fn golden_gemini_api() {
    let o = Origin {
        provider: ProviderId::new("p"),
        model: "gemini-3-pro".into(),
        protocol: ProtocolKind::Gemini,
        account: None,
    };
    let route = route_for(Preset::GeminiApi, ProtocolKind::Gemini, "gemini-3-pro").unwrap();
    let (body, _) = build(
        &route,
        request(
            ProtocolKind::Gemini,
            "gemini-3-pro",
            history(
                o,
                Some(OpaqueData::GeminiThoughtSignature {
                    signature: "gsig".into(),
                }),
            ),
        ),
    );
    golden("gemini_api", &body);
    assert_eq!(
        body["contents"][1]["parts"][0],
        json!({"text": "I should read it.", "thought": true, "thoughtSignature": "gsig"})
    );
    assert_eq!(
        body["contents"][2]["parts"][0]["functionResponse"]["name"],
        "read_file"
    );
    let params = &body["tools"][0]["functionDeclarations"][0]["parameters"];
    assert!(
        params.get("$schema").is_none() && params.get("additionalProperties").is_none(),
        "schema sanitised"
    );
    assert_eq!(
        body["generationConfig"]["thinkingConfig"],
        json!({"thinkingLevel": "high", "includeThoughts": true})
    );

    let route = route_for(Preset::Vertex, ProtocolKind::Gemini, "gemini-3-pro").unwrap();
    assert_eq!(
        route.endpoint.url("gemini-3-pro"),
        "https://aiplatform.googleapis.com/v1/projects/proj/locations/global/publishers/google/models/gemini-3-pro:streamGenerateContent?alt=sse"
    );
}

#[test]
fn stored_history_is_replayed_byte_for_byte() {
    let route = route_for(
        Preset::Anthropic,
        ProtocolKind::AnthropicMessages,
        "claude-opus-5-5",
    )
    .unwrap();
    let o = Origin {
        provider: ProviderId::new("p"),
        model: "claude-opus-5-5".into(),
        protocol: ProtocolKind::AnthropicMessages,
        account: None,
    };
    let msgs = history(
        o,
        Some(OpaqueData::AnthropicSignature {
            signature: "sig-1".into(),
        }),
    );
    let req = request(
        ProtocolKind::AnthropicMessages,
        "claude-opus-5-5",
        msgs.clone(),
    );
    let compiled = compile(req.clone(), &route.info()).unwrap();
    let first = route.protocol.build(&compiled, &route.compat).unwrap();
    assert_eq!(first.records.len(), 3);

    // Turn 2: stored records are reused; tamper with a stored record to prove it is the
    // record (not a re-normalisation) that goes on the wire.
    let mut records = first.records.clone();
    records[1].items = records[1].items.replace("\"Reading.\"", "\"Reading.\" ");
    let mut msgs2 = msgs;
    msgs2.push(Message::user_text("u2", "thanks"));
    let mut req2 = request(ProtocolKind::AnthropicMessages, "claude-opus-5-5", msgs2);
    req2.wire_records = records.clone();
    let compiled2 = compile(req2, &route.info()).unwrap();
    let second = route.protocol.build(&compiled2, &route.compat).unwrap();
    let text = String::from_utf8(second.body).unwrap();
    assert!(
        text.contains("\"Reading.\" "),
        "stored bytes replayed verbatim"
    );
    assert_eq!(
        second.records.len(),
        1,
        "only the new message is normalised"
    );
    assert_eq!(second.records[0].message_id.0, "u2");
    // The tail breakpoint moved to the new message and was never persisted.
    assert!(!records.iter().any(|r| r.items.contains("cache_control")));
    let v: Value = serde_json::from_str(&text).unwrap();
    assert_eq!(
        v["messages"][3]["content"][0]["cache_control"],
        json!({"type": "ephemeral"})
    );
    assert!(
        v["messages"][1]["content"][1]
            .get("cache_control")
            .is_none()
    );
}

#[test]
fn account_change_drops_anthropic_signatures() {
    let mut route = route_for(
        Preset::Anthropic,
        ProtocolKind::AnthropicMessages,
        "claude-opus-5-5",
    )
    .unwrap();
    let a = AccountTag::derive(&ProviderId::new("p"), "cred-a", "", "api.anthropic.com");
    let b = AccountTag::derive(&ProviderId::new("p"), "cred-b", "", "api.anthropic.com");
    route.account = Some(b);
    let o = Origin {
        provider: ProviderId::new("p"),
        model: "claude-sonnet-5-5".into(),
        protocol: ProtocolKind::AnthropicMessages,
        account: Some(a),
    };
    let (body, _) = build(
        &route,
        request(
            ProtocolKind::AnthropicMessages,
            "claude-opus-5-5",
            history(
                o,
                Some(OpaqueData::AnthropicSignature {
                    signature: "sig".into(),
                }),
            ),
        ),
    );
    // Out of scope → readable text, no thinking block.
    assert_eq!(
        body["messages"][1]["content"][0],
        json!({"type": "text", "text": "I should read it."})
    );
}

#[test]
fn catalog_patches_strip_sampling_and_patch_body() {
    use genie_providers::compat::ModelPatch;
    let mut m = spec(ProtocolKind::AnthropicMessages, "claude-opus-5-5");
    m.api_version = Some("2026-10-21".into());
    m.sampling_supported = vec![SamplingParam::Temperature];
    m.patches = vec![ModelPatch {
        compat: json!({"sampling_params_allowed": false}),
        body: Some(json!({"metadata": {"user_id": "genie"}})),
        headers: vec![("anthropic-beta".into(), "x".into())],
    }];
    let provider = PresetProvider(ProviderConfig {
        id: ProviderId::new("p"),
        preset: Preset::Anthropic,
        base_url: None,
        project: None,
        location: None,
        auth: Arc::new(NoAuth),
        account: None,
        compat_overlay: None,
        headers: vec![],
    });
    let route = provider.route(&m).unwrap();
    assert_eq!(
        route.compat.anthropic_version.as_deref(),
        Some("2026-10-21")
    );
    assert!(
        route
            .headers
            .contains(&("anthropic-beta".into(), "x".into()))
    );
    let mut req = LlmRequest::new(m, vec![Message::user_text("u", "hi")]);
    req.sampling.temperature = Some(0.2);
    let (body, _) = build(&route, req);
    assert!(
        body.get("temperature").is_none(),
        "2026-10-21 rejects sampling params"
    );
    assert_eq!(body["metadata"]["user_id"], "genie");
}

#[test]
fn fixtures_contain_no_secrets() {
    for d in ["cassettes", "golden"] {
        for e in std::fs::read_dir(dir(d)).unwrap() {
            let p = e.unwrap().path();
            let text = std::fs::read_to_string(&p).unwrap();
            assert!(
                !genie_providers::cassette::contains_secret(&text),
                "secret-like string in {}",
                p.display()
            );
        }
    }
}
