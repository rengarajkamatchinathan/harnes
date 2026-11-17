//! The turn loop (06 §1–§4) end to end against cassettes: no live network. The real
//! engine, session actor, bus, `Executor` and Anthropic adapter run; only the transport
//! is a [`CassetteTransport`].

#![allow(clippy::unwrap_used, clippy::expect_used)] // test code

use std::borrow::Cow;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use genie_catalog::Pricing;
use genie_core::bus::SubItem;
use genie_core::bus::Subscription;
use genie_core::runtime::conversation::{ConversationStore, MsgKind};
use genie_core::runtime::{
    MemoryConversation, ModelResolver, ResolveError, ResolvedModel, RuntimeBuilder, RuntimeConfig,
    SessionSettings, SettingsSource,
};
use genie_core::{EngineBuilder, SessionHandle, TaskScope};
use genie_protocol::ask::{AskAnswer, Decision, Preview};
use genie_protocol::event::{SessionEvent, StepFinish, ToolOutcome, TurnStatus};
use genie_protocol::message::{Part as WirePart, PartKind};
use genie_protocol::session::Delivery;
use genie_providers::auth::NoAuth;
use genie_providers::cassette::{CassetteTransport, RecordedResponse};
use genie_providers::compat::ModelSpec;
use genie_providers::model::{ProtocolKind, ProviderId, Role};
use genie_providers::route::{Preset, PresetProvider, ProviderConfig, Route};
use genie_providers::{Executor, Provider, RetryPolicy};
use genie_tools::{
    PermissionRequest, Tool, ToolCtx, ToolEnv, ToolError, ToolKind, ToolOutput, schema_of,
};
use serde_json::{Value, json};
use tokio::sync::Notify;

const WAIT: Duration = Duration::from_secs(10);

// ------------------------------------------------------------------ SSE builders

fn ev(name: &str, data: Value) -> String {
    format!("event: {name}\ndata: {data}\n\n")
}

fn msg_start(input: u64) -> String {
    ev(
        "message_start",
        json!({"type":"message_start","message":{"id":"msg_x","type":"message","role":"assistant",
            "model":"claude-test","content":[],"stop_reason":null,
            "usage":{"input_tokens":input,"cache_read_input_tokens":0,"cache_creation_input_tokens":0,"output_tokens":1}}}),
    )
}

fn text_block(idx: u32, chunks: &[&str]) -> String {
    let mut s = ev(
        "content_block_start",
        json!({"type":"content_block_start","index":idx,"content_block":{"type":"text","text":""}}),
    );
    for c in chunks {
        s += &ev(
            "content_block_delta",
            json!({"type":"content_block_delta","index":idx,"delta":{"type":"text_delta","text":c}}),
        );
    }
    s + &ev(
        "content_block_stop",
        json!({"type":"content_block_stop","index":idx}),
    )
}

fn tool_block(idx: u32, id: &str, name: &str, input: Value) -> String {
    let s = input.to_string();
    let (a, b) = s.split_at(s.len() / 2);
    ev(
        "content_block_start",
        json!({"type":"content_block_start","index":idx,"content_block":{"type":"tool_use","id":id,"name":name,"input":{}}}),
    ) + &ev(
        "content_block_delta",
        json!({"type":"content_block_delta","index":idx,"delta":{"type":"input_json_delta","partial_json":a}}),
    ) + &ev(
        "content_block_delta",
        json!({"type":"content_block_delta","index":idx,"delta":{"type":"input_json_delta","partial_json":b}}),
    ) + &ev(
        "content_block_stop",
        json!({"type":"content_block_stop","index":idx}),
    )
}

fn msg_end(stop: &str, output: u64) -> String {
    ev(
        "message_delta",
        json!({"type":"message_delta","delta":{"stop_reason":stop,"stop_sequence":null},"usage":{"output_tokens":output}}),
    ) + &ev("message_stop", json!({"type":"message_stop"}))
}

fn reply(blocks: &[String], stop: &str) -> RecordedResponse {
    reply_in(blocks, stop, 100)
}

/// A reply that reports `input` prompt tokens (the measured context size).
fn reply_in(blocks: &[String], stop: &str, input: u64) -> RecordedResponse {
    let body = msg_start(input) + &blocks.concat() + &msg_end(stop, 20);
    RecordedResponse::sse(200, &body, 37)
}

fn text(t: &[&str]) -> RecordedResponse {
    reply(&[text_block(0, t)], "end_turn")
}

fn summary_reply() -> RecordedResponse {
    text(&[
        "## Goal\nAnswer questions\n## User preferences & constraints\nNone\n## Decisions\nNone\n",
        "## Current state\nSUMMARY-MARKER\n## Open threads\nNone\n## Key references\nNone",
    ])
}

// ------------------------------------------------------------------ harness

struct Resolver {
    route: Route,
    exec: Arc<Executor>,
}

impl ModelResolver for Resolver {
    fn resolve(&self, _model: &str) -> Result<ResolvedModel, ResolveError> {
        Ok(ResolvedModel {
            route: self.route.clone(),
            client: self.exec.clone(),
            display_name: "Claude Test".into(),
            provider_display_name: "Anthropic".into(),
            pricing: Some(Pricing {
                input: 3.0,
                output: 15.0,
                ..Pricing::default()
            }),
            native_compaction: false,
        })
    }
}

struct Settings;

impl SettingsSource for Settings {
    fn settings(&self, _id: &str) -> Option<SessionSettings> {
        Some(SessionSettings {
            model: "anthropic/claude-test".into(),
            ..SessionSettings::default()
        })
    }
}

struct H {
    _root: TaskScope,
    t: Arc<CassetteTransport>,
    conv: Arc<MemoryConversation>,
    s: SessionHandle,
    sub: Subscription,
}

fn resolver(
    responses: Vec<RecordedResponse>,
    window: Option<u64>,
) -> (Arc<Resolver>, Arc<CassetteTransport>) {
    let t = Arc::new(CassetteTransport::new(responses));
    let exec = Arc::new(Executor::new(
        t.clone(),
        RetryPolicy {
            base: Duration::from_millis(5),
            max_gap: Duration::from_millis(20),
            jitter: false,
            ..RetryPolicy::default()
        },
    ));
    let mut spec = ModelSpec::new("anthropic", "claude-test", ProtocolKind::AnthropicMessages);
    spec.context_window = window.or(Some(200_000));
    spec.max_output = Some(4_096);
    let route = PresetProvider(ProviderConfig {
        id: ProviderId::new("anthropic"),
        preset: Preset::Anthropic,
        base_url: None,
        project: None,
        location: None,
        auth: Arc::new(NoAuth),
        account: None,
        compat_overlay: None,
        headers: vec![],
    })
    .route(&spec)
    .unwrap();
    (Arc::new(Resolver { route, exec }), t)
}

fn harness(
    responses: Vec<RecordedResponse>,
    tweak: impl FnOnce(&mut RuntimeConfig),
    tools: Vec<Arc<dyn Tool>>,
    window: Option<u64>,
) -> H {
    harness_with(
        responses,
        tweak,
        tools,
        window,
        Arc::new(Settings),
        |b, _, _| b,
    )
}

fn harness_with(
    responses: Vec<RecordedResponse>,
    tweak: impl FnOnce(&mut RuntimeConfig),
    tools: Vec<Arc<dyn Tool>>,
    window: Option<u64>,
    settings: Arc<dyn SettingsSource>,
    extra: impl FnOnce(RuntimeBuilder, &TaskScope, Arc<MemoryConversation>) -> RuntimeBuilder,
) -> H {
    let (resolver, t) = resolver(responses, window);
    let mut cfg = RuntimeConfig {
        titles: false,
        ..RuntimeConfig::default()
    };
    tweak(&mut cfg);
    let conv = Arc::new(MemoryConversation::new());
    let mut reg = genie_tools::chat::registry();
    for tool in tools {
        reg.register(tool);
    }
    let root = TaskScope::root();
    let b = RuntimeBuilder::new(settings, resolver)
        .config(cfg)
        .conversation(conv.clone())
        .chat_tools(reg.clone())
        .code_tools(reg);
    let runner = extra(b, &root, conv.clone()).build();
    let engine = EngineBuilder::new()
        .turn_runner(Arc::new(runner))
        .build(&root);
    let s = engine.create_session();
    let sub = engine.bus().subscribe(s.id(), 0).unwrap();
    H {
        _root: root,
        t,
        conv,
        s,
        sub,
    }
}

impl H {
    /// Wire events (durable and ephemeral) up to and including `turn.settled`.
    async fn settle(&mut self) -> Vec<SessionEvent> {
        let mut out = vec![];
        loop {
            let item = tokio::time::timeout(WAIT, self.sub.next())
                .await
                .expect("turn did not settle")
                .expect("subscription ended");
            if let SubItem::Event(e) = item {
                let w = e.to_wire().event;
                let done = matches!(w, SessionEvent::TurnSettled(_));
                out.push(w);
                if done {
                    return out;
                }
            }
        }
    }

    async fn until(&mut self, pred: impl Fn(&SessionEvent) -> bool) -> Vec<SessionEvent> {
        let mut out = vec![];
        loop {
            let item = tokio::time::timeout(WAIT, self.sub.next())
                .await
                .expect("event did not arrive")
                .expect("subscription ended");
            if let SubItem::Event(e) = item {
                let w = e.to_wire().event;
                let hit = pred(&w);
                out.push(w);
                if hit {
                    return out;
                }
            }
        }
    }

    fn bodies(&self) -> Vec<Value> {
        self.t
            .recorded()
            .iter()
            .map(|r| serde_json::from_str(&r.body).unwrap())
            .collect()
    }

    fn path(&self) -> Vec<(Role, MsgKind)> {
        self.conv
            .path(self.s.id())
            .unwrap()
            .iter()
            .map(|m| (m.message.role, m.kind))
            .collect()
    }
}

async fn wait_settled(sub: &mut Subscription) {
    let fut = async {
        loop {
            if let Some(SubItem::Event(e)) = sub.next().await
                && matches!(e.to_wire().event, SessionEvent::TurnSettled(_))
            {
                return;
            }
        }
    };
    tokio::time::timeout(WAIT, fut).await.unwrap();
}

fn types(ev: &[SessionEvent]) -> Vec<&'static str> {
    ev.iter()
        .map(SessionEvent::type_name)
        .filter(|t| *t != "status" && *t != "inbox.changed")
        .collect()
}

fn settled(ev: &[SessionEvent]) -> &genie_protocol::event::TurnSettled {
    match ev.last() {
        Some(SessionEvent::TurnSettled(s)) => s,
        other => panic!("not settled: {other:?}"),
    }
}

// ------------------------------------------------------------------ test tools

macro_rules! simple_tool {
    ($ty:ident, $name:literal, $kind:expr) => {
        impl $ty {
            fn schema() -> &'static Value {
                static S: std::sync::OnceLock<Value> = std::sync::OnceLock::new();
                S.get_or_init(|| schema_of::<TextIn>())
            }
        }
        #[async_trait]
        impl Tool for $ty {
            fn name(&self) -> &str {
                $name
            }
            fn description(&self, _env: &ToolEnv) -> Cow<'_, str> {
                Cow::Borrowed("test tool")
            }
            fn input_schema(&self) -> &Value {
                Self::schema()
            }
            fn kind(&self) -> ToolKind {
                $kind
            }
            async fn execute(&self, input: Value, ctx: ToolCtx) -> Result<ToolOutput, ToolError> {
                let i: TextIn = genie_tools::parse_input(input, Self::schema())?;
                self.run(i.text, ctx).await
            }
        }
    };
}

#[derive(serde::Deserialize, schemars::JsonSchema)]
struct TextIn {
    text: String,
}

struct Echo;
impl Echo {
    async fn run(&self, t: String, _ctx: ToolCtx) -> Result<ToolOutput, ToolError> {
        Ok(ToolOutput::Text(format!("echo: {t}")))
    }
}
simple_tool!(Echo, "echo", ToolKind::ReadOnly);

struct Note;
impl Note {
    async fn run(&self, t: String, ctx: ToolCtx) -> Result<ToolOutput, ToolError> {
        ctx.authorize(PermissionRequest {
            resource: Some("notes".into()),
            preview: Some(Preview::Text { text: t.clone() }),
            save_suggestions: vec![],
        })
        .await?;
        Ok(ToolOutput::Text(format!("saved {t}")))
    }
}
simple_tool!(Note, "note", ToolKind::Mutating);

struct Gate(Arc<Notify>);
impl Gate {
    async fn run(&self, _t: String, _ctx: ToolCtx) -> Result<ToolOutput, ToolError> {
        self.0.notified().await;
        Ok(ToolOutput::Text("released".into()))
    }
}
simple_tool!(Gate, "gate", ToolKind::ReadOnly);

struct Big;
impl Big {
    async fn run(&self, _t: String, _ctx: ToolCtx) -> Result<ToolOutput, ToolError> {
        Ok(ToolOutput::Text(
            (1..=3000).map(|i| format!("row {i}\n")).collect(),
        ))
    }
}
simple_tool!(Big, "big", ToolKind::ReadOnly);

// ------------------------------------------------------------------ tests

#[tokio::test]
async fn full_turn_streams_text_and_accounts_usage() {
    let mut h = harness(vec![text(&["Hel", "lo ", "world"])], |_| {}, vec![], None);
    h.s.prompt("hi there", Delivery::Steer).unwrap();
    let ev = h.settle().await;
    assert_eq!(
        types(&ev),
        [
            "message.admitted",
            "turn.started",
            "step.started",
            "part.started",
            "part.delta",
            "part.delta",
            "part.delta",
            "part.ended",
            "step.ended",
            "usage.updated",
            "turn.settled"
        ]
    );
    let deltas: String = ev
        .iter()
        .filter_map(|e| match e {
            SessionEvent::PartDelta(d) => d.text.clone(),
            _ => None,
        })
        .collect();
    assert_eq!(deltas, "Hello world");
    let ended = ev.iter().find_map(|e| match e {
        SessionEvent::PartEnded(p) => Some(p),
        _ => None,
    });
    assert!(matches!(&ended.unwrap().part, WirePart::Text { text, .. } if text == "Hello world"));
    let step_end = ev
        .iter()
        .find_map(|e| match e {
            SessionEvent::StepEnded(s) => Some(s),
            _ => None,
        })
        .unwrap();
    assert_eq!(step_end.finish, StepFinish::Stop);
    assert_eq!(step_end.usage.input_tokens, 100);
    assert_eq!(step_end.usage.output_tokens, 20);
    assert!((step_end.cost.usd - (100.0 * 3.0 + 20.0 * 15.0) / 1e6).abs() < 1e-12);
    assert_eq!(settled(&ev).status, TurnStatus::Completed);

    // Request layout (06 §2.1): base prompt and baseline as two system blocks.
    let body = &h.bodies()[0];
    let system = body["system"].as_array().unwrap();
    assert_eq!(system.len(), 2);
    assert!(system[0]["text"].as_str().unwrap().starts_with(
        "You are Genie, an AI assistant. In this session you are running on Claude Test via Anthropic."
    ));
    assert!(system[1]["text"].as_str().unwrap().contains("<env>"));
    assert_eq!(body["messages"][0]["content"][0]["text"], "hi there");
    assert_eq!(
        h.path(),
        [
            (Role::User, MsgKind::Normal),
            (Role::Assistant, MsgKind::Normal)
        ]
    );
    assert_eq!(h.conv.usage_lines(h.s.id()).len(), 1);
}

#[tokio::test]
async fn tool_call_result_and_second_step() {
    let mut h = harness(
        vec![
            reply(
                &[
                    text_block(0, &["Let me check."]),
                    tool_block(1, "toolu_1", "echo", json!({"text": "ping"})),
                ],
                "tool_use",
            ),
            text(&["Got ping."]),
        ],
        |_| {},
        vec![Arc::new(Echo)],
        None,
    );
    h.s.prompt("use echo", Delivery::Steer).unwrap();
    let ev = h.settle().await;
    let t = types(&ev);
    let pos = |name: &str| t.iter().position(|x| *x == name).unwrap();
    assert!(pos("tool.started") < pos("tool.ended"));
    assert_eq!(t.iter().filter(|x| **x == "step.started").count(), 2);
    let ended = ev
        .iter()
        .find_map(|e| match e {
            SessionEvent::ToolEnded(t) => Some(t),
            _ => None,
        })
        .unwrap();
    assert_eq!(ended.call_id, "toolu_1");
    assert_eq!(ended.outcome, ToolOutcome::Ok);
    assert!(
        ev.iter()
            .any(|e| matches!(e, SessionEvent::PartDelta(d) if d.json.is_some()))
    );
    assert_eq!(settled(&ev).status, TurnStatus::Completed);

    let bodies = h.bodies();
    assert_eq!(bodies.len(), 2);
    let msgs = bodies[1]["messages"].as_array().unwrap();
    assert_eq!(msgs.len(), 3);
    assert_eq!(msgs[1]["content"][1]["type"], "tool_use");
    assert_eq!(msgs[2]["content"][0]["type"], "tool_result");
    assert_eq!(msgs[2]["content"][0]["content"], "echo: ping");
    // Tools are declared with schemas; the step-1 tool list is reused byte-for-byte.
    assert_eq!(bodies[0]["tools"], bodies[1]["tools"]);
    assert_eq!(
        h.path(),
        [
            (Role::User, MsgKind::Normal),
            (Role::Assistant, MsgKind::Normal),
            (Role::Tool, MsgKind::Normal),
            (Role::Assistant, MsgKind::Normal)
        ]
    );
}

#[tokio::test]
async fn mutating_tool_asks_and_a_declined_call_reaches_the_model_as_feedback() {
    let mut h = harness(
        vec![
            reply(
                &[tool_block(0, "toolu_a", "note", json!({"text": "x"}))],
                "tool_use",
            ),
            reply(
                &[tool_block(0, "toolu_b", "note", json!({"text": "y"}))],
                "tool_use",
            ),
            text(&["ok"]),
        ],
        |_| {},
        vec![Arc::new(Note)],
        None,
    );
    let _attached = h.s.attach();
    h.s.prompt("save notes", Delivery::Steer).unwrap();
    for (decision, msg) in [
        (Decision::Once, None),
        (Decision::Reject, Some("not that one")),
    ] {
        let ev = h
            .until(|e| matches!(e, SessionEvent::PermissionAsked(_)))
            .await;
        let SessionEvent::PermissionAsked(a) = ev.last().unwrap() else {
            unreachable!()
        };
        assert_eq!(a.ask.tool.as_deref(), Some("note"));
        assert_eq!(a.ask.resource.as_deref(), Some("notes"));
        let mut ans = AskAnswer::decision(decision);
        ans.message = msg.map(String::from);
        h.s.answer_ask(&a.ask.id, serde_json::to_value(ans).unwrap())
            .unwrap();
    }
    let ev = h.settle().await;
    let outcomes: Vec<ToolOutcome> = ev
        .iter()
        .filter_map(|e| match e {
            SessionEvent::ToolEnded(t) => Some(t.outcome),
            _ => None,
        })
        .collect();
    assert_eq!(outcomes, [ToolOutcome::Declined]);
    assert_eq!(settled(&ev).status, TurnStatus::Completed);
    let bodies = h.bodies();
    let first = &bodies[1]["messages"].as_array().unwrap()[2]["content"][0];
    assert_eq!(first["content"], "saved x");
    let second = &bodies[2]["messages"].as_array().unwrap()[4]["content"][0];
    assert_eq!(
        second["content"],
        "The user declined this tool call and said: not that one"
    );
    assert_eq!(second["is_error"], true);
}

#[tokio::test]
async fn steer_is_injected_at_the_next_step_boundary() {
    let gate = Arc::new(Notify::new());
    let mut h = harness(
        vec![
            reply(
                &[tool_block(0, "toolu_g", "gate", json!({"text": "wait"}))],
                "tool_use",
            ),
            text(&["Both done."]),
        ],
        |_| {},
        vec![Arc::new(Gate(gate.clone()))],
        None,
    );
    h.s.prompt("start", Delivery::Steer).unwrap();
    h.until(|e| matches!(e, SessionEvent::ToolStarted(_))).await;
    // A steer while the tool runs waits for the step boundary (01 §5.2).
    h.s.prompt("also mention cats", Delivery::Steer).unwrap();
    assert_eq!(h.s.pending_inbox().len(), 1);
    gate.notify_one();
    let ev = h.settle().await;
    let admitted: Vec<usize> = ev
        .iter()
        .enumerate()
        .filter(|(_, e)| matches!(e, SessionEvent::MessageAdmitted(_)))
        .map(|(i, _)| i)
        .collect();
    let tool_end = ev
        .iter()
        .position(|e| matches!(e, SessionEvent::ToolEnded(_)))
        .unwrap();
    assert_eq!(admitted.len(), 1, "the steer item is admitted mid-turn");
    assert!(admitted[0] > tool_end);
    assert_eq!(settled(&ev).status, TurnStatus::Completed);
    let msgs = h.bodies()[1]["messages"].as_array().unwrap().clone();
    assert_eq!(msgs.len(), 4);
    assert_eq!(msgs[2]["content"][0]["type"], "tool_result");
    assert_eq!(msgs[3]["role"], "user");
    assert_eq!(msgs[3]["content"][0]["text"], "also mention cats");
    assert!(h.s.pending_inbox().is_empty());
}

#[tokio::test]
async fn retry_is_surfaced_as_retry_scheduled() {
    let mut h = harness(
        vec![
            RecordedResponse::json(
                529,
                r#"{"type":"error","error":{"type":"overloaded_error","message":"Overloaded"}}"#,
                vec![],
            ),
            text(&["after retry"]),
        ],
        |_| {},
        vec![],
        None,
    );
    h.s.prompt("hi", Delivery::Steer).unwrap();
    let ev = h.settle().await;
    let retry = ev
        .iter()
        .find_map(|e| match e {
            SessionEvent::RetryScheduled(r) => Some(r),
            _ => None,
        })
        .unwrap();
    assert_eq!(retry.attempt, 1);
    assert_eq!(retry.error_class, "overloaded");
    assert!(ev.iter().any(|e| matches!(e, SessionEvent::Status(s) if s.activity == Some(genie_protocol::event::Activity::Retrying))));
    assert_eq!(settled(&ev).status, TurnStatus::Completed);
    assert_eq!(h.t.recorded().len(), 2);
}

#[tokio::test]
async fn non_retryable_provider_error_fails_the_turn_with_a_fix_link() {
    let mut h = harness(
        vec![RecordedResponse::json(
            401,
            r#"{"type":"error","error":{"type":"authentication_error","message":"bad key"}}"#,
            vec![],
        )],
        |_| {},
        vec![],
        None,
    );
    h.s.prompt("hi", Delivery::Steer).unwrap();
    let ev = h.settle().await;
    let s = settled(&ev);
    assert_eq!(s.status, TurnStatus::Failed);
    let e = s.error.as_ref().unwrap();
    assert_eq!(e.code, "GENIE-PROV");
    assert!(e.message.contains("authentication"), "{}", e.message);
    assert_eq!(e.link.as_deref(), Some("genie://settings/providers"));
}

#[tokio::test]
async fn long_output_is_spilled_and_pageable() {
    let mut h = harness(
        vec![
            reply(
                &[tool_block(0, "toolu_big", "big", json!({"text": "x"}))],
                "tool_use",
            ),
            text(&["done"]),
        ],
        |_| {},
        vec![Arc::new(Big)],
        None,
    );
    h.s.prompt("go", Delivery::Steer).unwrap();
    let ev = h.settle().await;
    let ended = ev
        .iter()
        .find_map(|e| match e {
            SessionEvent::ToolEnded(t) => Some(t.clone()),
            _ => None,
        })
        .unwrap();
    let r = ended.spill_ref.expect("spilled");
    let body = &h.bodies()[1];
    let result = body["messages"][2]["content"][0]["content"]
        .as_str()
        .unwrap();
    assert!(result.starts_with("row 1\nrow 2\n"));
    assert!(result.ends_with(&format!("[showing 1–2000 of 3000; full output: {r}]")));
    let names: Vec<&str> = body["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["name"].as_str().unwrap())
        .collect();
    assert!(names.contains(&"tool_output_read"));
}

#[tokio::test]
async fn manual_compaction_replaces_history_with_the_summary() {
    let long = "first answer ".repeat(300);
    let mut h = harness(
        vec![
            text(&[&long]),
            text(&["second answer"]),
            summary_reply(),
            text(&["third answer"]),
        ],
        |c| c.compaction_tail_tokens = 1,
        vec![],
        None,
    );
    h.s.prompt("first question", Delivery::Queue).unwrap();
    h.settle().await;
    h.s.prompt("second question", Delivery::Queue).unwrap();
    h.settle().await;
    h.s.compact(Some("the answers".into())).unwrap();
    let ev = h.settle().await;
    assert!(
        !ev.iter()
            .any(|e| matches!(e, SessionEvent::MessageAdmitted(_))),
        "compaction admits no message"
    );
    let c = ev
        .iter()
        .find_map(|e| match e {
            SessionEvent::SessionCompacted(c) => Some(c.clone()),
            _ => None,
        })
        .unwrap();
    assert_eq!(c.trigger, "manual");
    assert!(!c.native);
    assert!(c.tokens_after < c.tokens_before);
    assert!(
        ev.iter()
            .any(|e| matches!(e, SessionEvent::PartStarted(p) if p.kind == PartKind::Compaction))
    );
    // The summary request carries the focus and the template; no tools may be called.
    let bodies = h.bodies();
    let sreq = &bodies[2];
    let last = sreq["messages"].as_array().unwrap().last().unwrap().clone();
    let prompt = last["content"][0]["text"].as_str().unwrap();
    assert!(prompt.contains("## Key references"));
    assert!(prompt.contains("Pay particular attention to: the answers"));
    assert_eq!(sreq["system"], bodies[0]["system"], "same cached prefix");

    h.s.prompt("third question", Delivery::Queue).unwrap();
    h.settle().await;
    let next = serde_json::to_string(&h.bodies()[3]["messages"]).unwrap();
    assert!(next.contains("SUMMARY-MARKER"));
    assert!(!next.contains("first question"));
    assert!(next.contains("third question"));
    let usage: Vec<&str> = h
        .conv
        .usage_lines(h.s.id())
        .iter()
        .map(|l| l.kind)
        .collect();
    assert!(usage.contains(&"compaction"));
    let path = h.path();
    assert!(path.contains(&(Role::User, MsgKind::Compaction)));
}

#[tokio::test]
async fn auto_compaction_at_the_threshold_keeps_the_newest_message() {
    let big = "lorem ipsum ".repeat(400); // ~1,400 estimated tokens
    let mut h = harness(
        vec![
            reply_in(&[text_block(0, &["short"])], "end_turn", 2_000),
            summary_reply(),
            text(&["after compaction"]),
        ],
        |c| {
            c.compaction_buffer = Some(8_500);
            c.compaction_tail_tokens = 10;
        },
        vec![],
        Some(10_000),
    );
    h.s.prompt(big.clone(), Delivery::Queue).unwrap();
    let ev = h.settle().await;
    assert!(
        !ev.iter()
            .any(|e| matches!(e, SessionEvent::SessionCompacted(_))),
        "nothing to compact yet"
    );
    h.s.prompt("newest question", Delivery::Queue).unwrap();
    let ev = h.settle().await;
    let c = ev
        .iter()
        .find_map(|e| match e {
            SessionEvent::SessionCompacted(c) => Some(c.clone()),
            _ => None,
        })
        .unwrap();
    assert_eq!(c.trigger, "auto");
    assert_eq!(settled(&ev).status, TurnStatus::Completed);
    let bodies = h.bodies();
    let step = serde_json::to_string(&bodies[2]["messages"]).unwrap();
    assert!(step.contains("SUMMARY-MARKER"));
    assert!(
        step.contains("newest question"),
        "the newest message is kept verbatim"
    );
    assert!(!step.contains("lorem ipsum"));
}

#[tokio::test]
async fn context_overflow_compacts_and_retries_the_step() {
    let mut h = harness(
        vec![
            text(&["first"]),
            RecordedResponse::json(
                400,
                r#"{"type":"error","error":{"type":"invalid_request_error","message":"prompt is too long: 213456 tokens > 200000 maximum"}}"#,
                vec![],
            ),
            summary_reply(),
            text(&["fits now"]),
        ],
        |_| {},
        vec![],
        None,
    );
    h.s.prompt("one", Delivery::Queue).unwrap();
    h.settle().await;
    h.s.prompt("two", Delivery::Queue).unwrap();
    let ev = h.settle().await;
    let c = ev
        .iter()
        .find_map(|e| match e {
            SessionEvent::SessionCompacted(c) => Some(c.clone()),
            _ => None,
        })
        .unwrap();
    assert_eq!(c.trigger, "overflow");
    assert_eq!(settled(&ev).status, TurnStatus::Completed);
    let finishes: Vec<StepFinish> = ev
        .iter()
        .filter_map(|e| match e {
            SessionEvent::StepEnded(s) => Some(s.finish),
            _ => None,
        })
        .collect();
    assert_eq!(finishes, [StepFinish::Error, StepFinish::Stop]);
    assert_eq!(h.t.recorded().len(), 4);
}

#[tokio::test]
async fn length_finish_continues_in_smaller_pieces_at_most_twice() {
    let cut = |t: &str| reply(&[text_block(0, &[t])], "max_tokens");
    let mut h = harness(vec![cut("a"), cut("b"), cut("c")], |_| {}, vec![], None);
    h.s.prompt("write a lot", Delivery::Steer).unwrap();
    let ev = h.settle().await;
    assert_eq!(settled(&ev).status, TurnStatus::Completed);
    assert_eq!(h.t.recorded().len(), 3);
    let last = serde_json::to_string(&h.bodies()[2]["messages"]).unwrap();
    assert!(last.contains("Your last reply was cut off."));
}

#[tokio::test]
async fn max_steps_runs_a_final_step_without_tools() {
    let call = |id: &str| {
        reply(
            &[tool_block(0, id, "echo", json!({"text": "x"}))],
            "tool_use",
        )
    };
    let mut h = harness(
        vec![call("toolu_1"), call("toolu_2"), text(&["summary"])],
        |c| c.max_steps_chat = 3,
        vec![Arc::new(Echo)],
        None,
    );
    h.s.prompt("loop", Delivery::Steer).unwrap();
    let ev = h.settle().await;
    assert_eq!(settled(&ev).status, TurnStatus::Completed);
    let bodies = h.bodies();
    assert_eq!(bodies.len(), 3);
    assert_eq!(bodies[2]["tool_choice"], json!({"type": "none"}));
    let msgs = serde_json::to_string(&bodies[2]["messages"]).unwrap();
    assert!(msgs.contains("You have reached the step limit"));
}

#[tokio::test]
async fn interrupt_keeps_partial_text_and_cancels_the_turn() {
    let gate = Arc::new(Notify::new());
    let mut h = harness(
        vec![reply(
            &[tool_block(0, "toolu_g", "gate", json!({"text": "wait"}))],
            "tool_use",
        )],
        |_| {},
        vec![Arc::new(Gate(gate))],
        None,
    );
    h.s.prompt("start", Delivery::Steer).unwrap();
    h.until(|e| matches!(e, SessionEvent::ToolStarted(_))).await;
    assert!(h.s.interrupt());
    let ev = h.settle().await;
    assert_eq!(settled(&ev).status, TurnStatus::Cancelled);
    let ended = ev
        .iter()
        .find_map(|e| match e {
            SessionEvent::ToolEnded(t) => Some(t.outcome),
            _ => None,
        })
        .unwrap();
    assert_eq!(ended, ToolOutcome::Cancelled);
    // The tool call and its (cancelled) result are both in history.
    assert_eq!(
        h.path(),
        [
            (Role::User, MsgKind::Normal),
            (Role::Assistant, MsgKind::Normal),
            (Role::Tool, MsgKind::Normal)
        ]
    );
}

#[tokio::test]
async fn regenerate_branches_from_the_user_message() {
    let mut h = harness(
        vec![text(&["first try"]), text(&["second try"])],
        |_| {},
        vec![],
        None,
    );
    h.s.prompt("question", Delivery::Steer).unwrap();
    h.settle().await;
    let path = h.conv.path(h.s.id()).unwrap();
    let assistant = path[1].id().to_string();
    h.s.regenerate(assistant.clone()).unwrap();
    let ev = h.settle().await;
    assert_eq!(settled(&ev).status, TurnStatus::Completed);
    let path = h.conv.path(h.s.id()).unwrap();
    assert_eq!(path.len(), 2);
    assert_ne!(path[1].id(), assistant, "a sibling reply");
    assert_eq!(path[1].parent_id.as_deref(), Some(path[0].id()));
    let msgs = h.bodies()[1]["messages"].as_array().unwrap().clone();
    assert_eq!(msgs.len(), 1, "the old reply is not sent");
    assert_eq!(h.conv.all(h.s.id()).len(), 3);
}

#[tokio::test]
async fn title_after_the_first_reply() {
    let mut h = harness(
        vec![text(&["Paris."]), text(&["\"Capital of France.\"\n"])],
        |c| c.titles = true,
        vec![],
        None,
    );
    h.s.prompt("capital of france?", Delivery::Steer).unwrap();
    let ev = h.settle().await;
    let title = ev.iter().find_map(|e| match e {
        SessionEvent::SessionUpdated(u) => u.title.clone(),
        _ => None,
    });
    assert_eq!(title.as_deref(), Some("Capital of France"));
    let usage: Vec<&str> = h
        .conv
        .usage_lines(h.s.id())
        .iter()
        .map(|l| l.kind)
        .collect();
    assert_eq!(usage, ["chat", "title"]);
}

#[tokio::test]
async fn baseline_changes_arrive_as_system_updates() {
    use genie_core::runtime::InstructionProvider;
    use genie_core::runtime::context::InstructionSource;
    use std::sync::Mutex;

    struct Src(Mutex<String>);
    impl InstructionProvider for Src {
        fn sources(&self, _s: &str, _st: &SessionSettings) -> Vec<InstructionSource> {
            vec![InstructionSource::user_instructions(
                &self.0.lock().unwrap(),
                None,
            )]
        }
    }
    let src = Arc::new(Src(Mutex::new("Be brief.".into())));
    let (resolver, t) = resolver(vec![text(&["a"]), text(&["b"])], None);
    let runner = RuntimeBuilder::new(Arc::new(Settings), resolver)
        .config(RuntimeConfig {
            titles: false,
            ..RuntimeConfig::default()
        })
        .instructions(src.clone())
        .build();
    let root = TaskScope::root();
    let engine = EngineBuilder::new()
        .turn_runner(Arc::new(runner))
        .build(&root);
    let s = engine.create_session();
    let mut sub = engine.bus().subscribe(s.id(), 0).unwrap();
    s.prompt("one", Delivery::Queue).unwrap();
    wait_settled(&mut sub).await;
    *src.0.lock().unwrap() = "Be detailed.".into();
    s.prompt("two", Delivery::Queue).unwrap();
    wait_settled(&mut sub).await;
    let bodies: Vec<Value> = t
        .recorded()
        .iter()
        .map(|r| serde_json::from_str(&r.body).unwrap())
        .collect();
    // The frozen baseline (system block 2) is byte-identical across turns.
    assert_eq!(bodies[0]["system"], bodies[1]["system"]);
    assert!(
        bodies[1]["system"][1]["text"]
            .as_str()
            .unwrap()
            .contains("Be brief.")
    );
    // The change rides on the next user turn (Anthropic lowering, SA-45).
    let user = &bodies[1]["messages"][2]["content"];
    assert_eq!(user[0]["text"], "two");
    let update = user[1]["text"].as_str().unwrap();
    assert!(update.starts_with("<system-update>Updated custom:"));
    assert!(update.contains("Be detailed."));
}

#[tokio::test]
async fn store_backed_turn_persists_messages_memory_and_usage() {
    use genie_core::journal::StoreJournal;
    use genie_core::runtime::StoreConversation;

    let dir = tempfile::tempdir().unwrap();
    let store = Arc::new(
        genie_store::Store::open_with_keystore(
            &genie_store::StoreOptions::new(dir.path(), "genie.db"),
            &genie_store::MemoryKeyStore::new(),
        )
        .unwrap(),
    );
    let (resolver, _t) = resolver(
        vec![
            reply(
                &[tool_block(
                    0,
                    "toolu_m",
                    "memory_save",
                    json!({"text": "Prefers tea"}),
                )],
                "tool_use",
            ),
            text(&["Noted."]),
        ],
        None,
    );
    let runner = RuntimeBuilder::new(Arc::new(Settings), resolver)
        .config(RuntimeConfig {
            titles: false,
            ..RuntimeConfig::default()
        })
        .store(store.clone())
        .build();
    let root = TaskScope::root();
    let engine = EngineBuilder::new()
        .journal(Arc::new(StoreJournal::new(store.clone())))
        .turn_runner(Arc::new(runner))
        .build(&root);
    let s = engine.create_session();
    let _attached = s.attach();
    let mut sub = engine.bus().subscribe(s.id(), 0).unwrap();
    s.prompt("remember I like tea", Delivery::Steer).unwrap();
    // Memory tools are always allowed (06 §4.1): no ask.
    let mut changed = false;
    loop {
        let Some(SubItem::Event(e)) = tokio::time::timeout(WAIT, sub.next()).await.unwrap() else {
            continue;
        };
        match e.to_wire().event {
            SessionEvent::MemoryChanged(m) => {
                assert_eq!((m.op.as_str(), m.scope.as_str()), ("add", "global"));
                changed = true;
            }
            SessionEvent::TurnSettled(t) => {
                assert_eq!(t.status, TurnStatus::Completed);
                break;
            }
            _ => {}
        }
    }
    assert!(changed);
    // A fresh view over the same DB (daemon restart) sees the tree.
    let conv = StoreConversation::new(store.clone());
    let path = conv.path(s.id()).unwrap();
    let roles: Vec<Role> = path.iter().map(|m| m.message.role).collect();
    assert_eq!(
        roles,
        [Role::User, Role::Assistant, Role::Tool, Role::Assistant]
    );
    assert!(conv.meta(s.id()).unwrap().baseline.is_some());
    let found = store
        .read(|c| genie_store::memory::search(c, &["global"], "tea", 5))
        .unwrap();
    assert_eq!(found.len(), 1);
    let day = genie_core::runtime::context::utc_day(genie_core::bus::now_ms());
    let usage = store
        .read(|c| genie_store::kv::usage_between(c, &day, &day))
        .unwrap();
    assert_eq!(usage.iter().map(|u| u.calls).sum::<i64>(), 2);
    let hits = store
        .read(|c| genie_store::search::chat_search(c, "tea", &genie_store::search::Scope::All, 10))
        .unwrap();
    assert!(!hits.is_empty(), "parts are indexed for chat search");
}

// ------------------------------------------------------------------ permissions + MCP wiring

mod wiring {
    use super::*;
    use genie_core::config::ConfigHub;
    use genie_core::runtime::FixedSettings;
    use genie_core::runtime::active::ActiveTurns;
    use genie_core::runtime::mcp::{McpHub, McpPorts};
    use genie_core::runtime::permissions::PermissionAuthorizer;
    use genie_mcp::transport::{ConnectEnv, Connector, TransportPair, stdio};
    use genie_protocol::ask::{AutoResolver, ResolvedBy};
    use genie_protocol::common::{Mode, RuleScope, SessionKind};
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

    fn hub(dir: &std::path::Path) -> Arc<ConfigHub> {
        Arc::new(ConfigHub::new(dir.join(".genie"), dir.join(".genie/trust.json"), None).unwrap())
    }

    fn policy(values: Value) -> genie_config::EffectivePolicy {
        let v: genie_config::policy::PolicyValues = serde_json::from_value(values).unwrap();
        genie_config::policy::merge(&[(genie_config::PolicySourceKind::File, &v)])
    }

    fn code_settings(ws: &std::path::Path) -> Arc<dyn SettingsSource> {
        Arc::new(FixedSettings(SessionSettings {
            kind: SessionKind::Code,
            model: "anthropic/claude-test".into(),
            mode: Mode::Default,
            workspace: Some(ws.to_path_buf()),
            ..SessionSettings::default()
        }))
    }

    fn ended(ev: &[SessionEvent]) -> Vec<&genie_protocol::event::ToolEnded> {
        ev.iter()
            .filter_map(|e| match e {
                SessionEvent::ToolEnded(t) => Some(t),
                _ => None,
            })
            .collect()
    }

    #[tokio::test]
    async fn policy_deny_blocks_the_call_as_policy_blocked() {
        let d = tempfile::tempdir().unwrap();
        let h = hub(d.path());
        h.set_policy(policy(
            json!({"org.name": "Acme", "permissions.deny": ["note"]}),
        ));
        let settings = code_settings(d.path());
        let mut t = harness_with(
            vec![
                reply(
                    &[tool_block(0, "toolu_a", "note", json!({"text": "x"}))],
                    "tool_use",
                ),
                text(&["ok"]),
            ],
            |_| {},
            vec![Arc::new(Note)],
            None,
            settings.clone(),
            |b, _, _| {
                b.authorizer(Arc::new(PermissionAuthorizer::new(
                    h.clone(),
                    settings.clone(),
                )))
            },
        );
        t.s.prompt("save", Delivery::Steer).unwrap();
        let ev = t.settle().await;
        let e = ended(&ev);
        assert_eq!(e[0].outcome, ToolOutcome::Blocked);
        let err = e[0].error.as_ref().unwrap();
        assert_eq!(err.code, "policy_blocked");
        assert_eq!(err.message, "Blocked by Acme policy (note)");
        let bodies = t.bodies();
        let res = &bodies[1]["messages"].as_array().unwrap()[2]["content"][0];
        assert_eq!(res["content"], "Blocked by Acme policy (note).");
    }

    #[tokio::test]
    async fn always_saves_the_rule_and_releases_other_pending_asks() {
        let d = tempfile::tempdir().unwrap();
        let ws = d.path().join("proj");
        std::fs::create_dir_all(&ws).unwrap();
        let h = hub(d.path());
        h.trust_set(
            &ws.display().to_string(),
            genie_protocol::api::config::TrustDecision::Trust,
        )
        .unwrap();
        let settings = code_settings(&ws);
        let mut t = harness_with(
            vec![
                reply(
                    &[
                        tool_block(0, "toolu_a", "note", json!({"text": "x"})),
                        tool_block(1, "toolu_b", "note", json!({"text": "y"})),
                    ],
                    "tool_use",
                ),
                text(&["ok"]),
            ],
            |_| {},
            vec![Arc::new(Note)],
            None,
            settings.clone(),
            |b, _, _| {
                b.authorizer(Arc::new(PermissionAuthorizer::new(
                    h.clone(),
                    settings.clone(),
                )))
            },
        );
        let _attached = t.s.attach();
        t.s.prompt("save", Delivery::Steer).unwrap();
        let mut asks = vec![];
        while asks.len() < 2 {
            let ev = t
                .until(|e| matches!(e, SessionEvent::PermissionAsked(_)))
                .await;
            let SessionEvent::PermissionAsked(a) = ev.last().unwrap() else {
                unreachable!()
            };
            asks.push(a.ask.clone());
        }
        assert_eq!(asks[0].save_suggestions[0].rule, "note");
        let mut ans = AskAnswer::decision(Decision::Always);
        ans.scope = Some(RuleScope::Local);
        t.s.answer_ask(&asks[0].id, serde_json::to_value(ans).unwrap())
            .unwrap();
        let ev = t.settle().await;
        let resolved: Vec<_> = ev
            .iter()
            .filter_map(|e| match e {
                SessionEvent::PermissionResolved(r) => Some(r.clone()),
                _ => None,
            })
            .collect();
        assert_eq!(resolved[0].saved_rule.as_deref(), Some("note"));
        assert_eq!(resolved[1].ask_id, asks[1].id);
        assert_eq!(resolved[1].by, ResolvedBy::Auto(AutoResolver::Rule));
        assert!(ended(&ev).iter().all(|e| e.outcome == ToolOutcome::Ok));
        let local = std::fs::read_to_string(ws.join(".genie/settings.local.json")).unwrap();
        assert!(local.contains("\"note\""), "{local}");
    }

    /// A minimal MCP server over an in-memory pipe: one `echo` tool.
    struct FakeMcp;

    #[async_trait]
    impl Connector for FakeMcp {
        async fn connect(
            &self,
            _cfg: &genie_mcp::ServerConfig,
            env: &ConnectEnv,
        ) -> Result<TransportPair, genie_mcp::McpError> {
            let (client, server) = tokio::io::duplex(1 << 16);
            let (sr, mut sw) = tokio::io::split(server);
            env.tasks.spawn(async move {
                let mut lines = BufReader::new(sr).lines();
                while let Ok(Some(l)) = lines.next_line().await {
                    let v: Value = serde_json::from_str(&l).unwrap();
                    let Some(id) = v.get("id").cloned() else { continue };
                    let result = match v["method"].as_str().unwrap_or("") {
                        "initialize" => json!({"protocolVersion": "2025-06-18",
                            "capabilities": {"tools": {}}, "serverInfo": {"name": "fake", "version": "1"}}),
                        "tools/list" => json!({"tools": [{"name": "echo", "description": "Echo text",
                            "inputSchema": {"type": "object", "properties": {"text": {"type": "string"}}}}]}),
                        "tools/call" => json!({"content": [{"type": "text",
                            "text": format!("echo: {}", v["params"]["arguments"]["text"].as_str().unwrap_or(""))}]}),
                        _ => json!({}),
                    };
                    let msg = json!({"jsonrpc": "2.0", "id": id, "result": result});
                    if sw.write_all(format!("{msg}\n").as_bytes()).await.is_err() {
                        break;
                    }
                }
            });
            let (cr, cw) = tokio::io::split(client);
            Ok(stdio::from_streams(
                cr,
                cw,
                &env.tasks,
                env.logs.clone(),
                env.parent.child_token(),
                None,
            ))
        }
    }

    #[tokio::test]
    async fn mcp_tools_run_through_the_gate_and_taint_the_turn() {
        let d = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(d.path().join(".genie")).unwrap();
        std::fs::write(
            d.path().join(".genie/settings.json"),
            r#"{"mcpServers": {"fake": {"command": "fake"}}, "permissions": {"allow": ["mcp__fake__echo"]}}"#,
        )
        .unwrap();
        let h = hub(d.path());
        let settings: Arc<dyn SettingsSource> = Arc::new(Settings);
        let turns = Arc::new(ActiveTurns::new());
        let mcp_slot: Arc<std::sync::Mutex<Option<Arc<McpHub>>>> = Arc::default();
        let slot = mcp_slot.clone();
        let mut t = harness_with(
            vec![
                reply(
                    &[tool_block(
                        0,
                        "toolu_a",
                        "mcp__fake__echo",
                        json!({"text": "hi"}),
                    )],
                    "tool_use",
                ),
                reply(
                    &[tool_block(
                        0,
                        "toolu_b",
                        "mcp__fake__echo",
                        json!({"text": "again"}),
                    )],
                    "tool_use",
                ),
                text(&["done"]),
            ],
            |_| {},
            vec![],
            None,
            settings.clone(),
            |b, root, _| {
                let mcp = McpHub::new(
                    h.clone(),
                    root.child("mcp"),
                    McpPorts {
                        connector: Arc::new(FakeMcp),
                        auth: Arc::new(genie_mcp::auth::NoAuth),
                        secrets: Arc::new(genie_mcp::config::LiteralSecrets),
                    },
                    settings.clone(),
                    None,
                    turns.clone(),
                    None,
                );
                *slot.lock().unwrap() = Some(mcp.clone());
                b.turns(turns.clone())
                    .tool_provider(mcp)
                    .authorizer(Arc::new(PermissionAuthorizer::new(
                        h.clone(),
                        settings.clone(),
                    )))
            },
        );
        let mcp = mcp_slot.lock().unwrap().clone().unwrap();
        let _attached = t.s.attach();
        t.s.prompt("echo please", Delivery::Steer).unwrap();
        // The first call is allowed by the user rule; the second follows untrusted MCP
        // output, so the allow is downgraded to an ask (SA-29).
        let ev = t
            .until(|e| matches!(e, SessionEvent::PermissionAsked(_)))
            .await;
        assert_eq!(ended(&ev)[0].outcome, ToolOutcome::Ok);
        let SessionEvent::PermissionAsked(a) = ev.last().unwrap() else {
            unreachable!()
        };
        assert_eq!(a.ask.tool.as_deref(), Some("mcp__fake__echo"));
        t.s.answer_ask(
            &a.ask.id,
            serde_json::to_value(AskAnswer::decision(Decision::Once)).unwrap(),
        )
        .unwrap();
        let ev = t.settle().await;
        assert_eq!(ended(&ev)[0].outcome, ToolOutcome::Ok);
        let bodies = t.bodies();
        let tools: Vec<&str> = bodies[0]["tools"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|x| x["name"].as_str())
            .collect();
        assert!(tools.contains(&"mcp__fake__echo"), "{tools:?}");
        let res = &bodies[1]["messages"].as_array().unwrap()[2]["content"][0];
        assert_eq!(res["content"], "echo: hi");
        let list = mcp.list(None);
        assert_eq!(list[0].name, "fake");
        assert_eq!(list[0].status, genie_mcp::McpStatus::Connected);
        assert_eq!(list[0].tools, 1);
    }

    #[tokio::test]
    async fn skills_index_skill_tool_and_user_commands() {
        let d = tempfile::tempdir().unwrap();
        let home = d.path().join(".genie");
        std::fs::create_dir_all(home.join("skills/greet")).unwrap();
        std::fs::write(
            home.join("skills/greet/SKILL.md"),
            "---\nname: greet\ndescription: Greet people warmly.\n---\nAlways greet with GREET-MARKER.\n",
        )
        .unwrap();
        std::fs::create_dir_all(home.join("commands")).unwrap();
        std::fs::write(home.join("commands/hello.md"), "Say hello to $ARGUMENTS").unwrap();
        let h = hub(d.path());
        let settings: Arc<dyn SettingsSource> = Arc::new(Settings);
        let skills = genie_core::runtime::skills::SkillsHub::new(home, h.clone(), settings.clone());
        let mut t = harness_with(
            vec![
                reply(
                    &[tool_block(0, "toolu_a", "skill", json!({"name": "greet"}))],
                    "tool_use",
                ),
                text(&["done"]),
            ],
            |_| {},
            vec![],
            None,
            settings.clone(),
            |b, _, _| {
                b.skills(skills.clone())
                    .authorizer(Arc::new(PermissionAuthorizer::new(
                        h.clone(),
                        settings.clone(),
                    )))
            },
        );
        t.s.prompt("/hello Bob", Delivery::Steer).unwrap();
        let ev = t.settle().await;
        assert_eq!(ended(&ev)[0].outcome, ToolOutcome::Ok);
        let bodies = t.bodies();
        let system = bodies[0]["system"].to_string();
        assert!(system.contains("<available_skills>"), "{system}");
        assert!(system.contains("greet"), "{system}");
        let msgs = bodies[0]["messages"].to_string();
        assert!(msgs.contains("Say hello to Bob"), "{msgs}");
        let res = bodies[1]["messages"].as_array().unwrap()[2]["content"][0]["content"].to_string();
        assert!(res.contains("GREET-MARKER"), "{res}");
        assert_eq!(skills.loaded(t.s.id()).len(), 1);
        // A `Skill(greet)` deny rule blocks it.
        std::fs::write(
            d.path().join(".genie/settings.json"),
            r#"{"permissions": {"deny": ["Skill(greet)"]}}"#,
        )
        .unwrap();
        h.refresh();
        let ev = genie_core::runtime::permissions::action_for(
            &genie_core::runtime::AuthzRequest {
                session_id: "x".into(),
                turn_id: "t".into(),
                tool: "skill".into(),
                kind: ToolKind::ReadOnly,
                resource: Some("greet".into()),
                preview: None,
                save_suggestions: vec![],
                mode: Mode::Default,
                taint: None,
            },
            None,
        );
        assert!(matches!(ev, genie_permissions::Action::Other { ref tool, .. } if tool == "Skill"));
        let authz = PermissionAuthorizer::new(h.clone(), settings.clone());
        use genie_core::runtime::ToolAuthorizer;
        let dec = authz.authorize(&genie_core::runtime::AuthzRequest {
            session_id: "x".into(),
            turn_id: "t".into(),
            tool: "skill".into(),
            kind: ToolKind::ReadOnly,
            resource: Some("greet".into()),
            preview: None,
            save_suggestions: vec![],
            mode: Mode::Default,
            taint: None,
        });
        assert!(
            matches!(dec, genie_core::runtime::AuthzDecision::Deny { .. }),
            "{dec:?}"
        );
    }

    async fn settle_answering(t: &mut H) -> (Vec<SessionEvent>, usize) {
        let mut all = vec![];
        let mut asks = 0;
        loop {
            let ev = t
                .until(|e| {
                    matches!(
                        e,
                        SessionEvent::PermissionAsked(_) | SessionEvent::TurnSettled(_)
                    )
                })
                .await;
            let last = ev.last().cloned().unwrap();
            all.extend(ev);
            match last {
                SessionEvent::PermissionAsked(a) => {
                    asks += 1;
                    t.s.answer_ask(
                        &a.ask.id,
                        serde_json::to_value(AskAnswer::decision(Decision::Once)).unwrap(),
                    )
                    .unwrap();
                }
                _ => return (all, asks),
            }
        }
    }

    #[tokio::test]
    async fn shell_runs_through_the_real_authorizer() {
        let d = tempfile::tempdir().unwrap();
        let ws = d.path().join("proj");
        std::fs::create_dir_all(&ws).unwrap();
        let h = hub(d.path());
        h.trust_set(
            &ws.display().to_string(),
            genie_protocol::api::config::TrustDecision::Trust,
        )
        .unwrap();
        std::fs::create_dir_all(ws.join(".genie")).unwrap();
        std::fs::write(
            ws.join(".genie/settings.json"),
            r#"{"permissions": {"deny": ["Bash(rm:*)"]}}"#,
        )
        .unwrap();
        h.refresh();
        let settings = code_settings(&ws);
        let mut t = harness_with(
            vec![
                reply(
                    &[tool_block(
                        0,
                        "toolu_a",
                        "shell",
                        json!({"command": "echo hi-from-shell"}),
                    )],
                    "tool_use",
                ),
                reply(
                    &[tool_block(
                        0,
                        "toolu_b",
                        "shell",
                        json!({"command": "rm -rf build"}),
                    )],
                    "tool_use",
                ),
                text(&["done"]),
            ],
            |_| {},
            vec![],
            None,
            settings.clone(),
            |b, root, _| {
                let sh =
                    genie_tools::shell::ShellRuntime::new(genie_tools::shell::ShellConfig::new(
                        Arc::new(genie_tools::shell::FixedDir(ws.clone())),
                        root.tracker(),
                        root.token().clone(),
                    ));
                b.shell(Arc::new(sh))
                    .authorizer(Arc::new(PermissionAuthorizer::new(
                        h.clone(),
                        settings.clone(),
                    )))
            },
        );
        let _attached = t.s.attach();
        t.s.prompt("run it", Delivery::Steer).unwrap();
        let (ev, asks) = settle_answering(&mut t).await;
        // `echo` is read-only (no ask); `rm` is denied by the project rule.
        assert_eq!(asks, 0);
        let e = ended(&ev);
        assert_eq!(e[0].outcome, ToolOutcome::Ok);
        assert_eq!(e[1].outcome, ToolOutcome::Blocked);
        let bodies = t.bodies();
        let res = bodies[1]["messages"].as_array().unwrap()[2]["content"][0]["content"].to_string();
        assert!(res.contains("hi-from-shell"), "{res}");
    }

    async fn page_server() -> std::net::SocketAddr {
        let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = l.local_addr().unwrap();
        tokio::spawn(async move {
            while let Ok((mut s, _)) = l.accept().await {
                tokio::spawn(async move {
                    let mut buf = [0u8; 4096];
                    let _ = tokio::io::AsyncReadExt::read(&mut s, &mut buf).await;
                    let body = "<html><body><h1>Hello web</h1><p>PAGE-MARKER</p></body></html>";
                    let resp = format!(
                        "HTTP/1.1 200 OK\r\ncontent-type: text/html\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                        body.len()
                    );
                    let _ = s.write_all(resp.as_bytes()).await;
                });
            }
        });
        addr
    }

    fn web_harness(d: &std::path::Path, url: &str) -> H {
        let h = hub(d);
        let settings: Arc<dyn SettingsSource> = Arc::new(Settings);
        let fetch = genie_tools::web::FetchConfig {
            ssrf: genie_tools::web::SsrfPolicy {
                allow_private: false,
                allow: vec!["127.0.0.1".parse().unwrap()],
            },
            ..genie_tools::web::FetchConfig::default()
        };
        harness_with(
            vec![
                reply(
                    &[tool_block(0, "toolu_a", "web_fetch", json!({"url": url}))],
                    "tool_use",
                ),
                text(&["done"]),
            ],
            |_| {},
            vec![],
            None,
            settings.clone(),
            move |b, _, conv| {
                b.web(genie_tools::web::WebConfig {
                    fetch,
                    ..Default::default()
                })
                .authorizer(Arc::new(
                    PermissionAuthorizer::new(h.clone(), settings.clone()).with_conversation(conv),
                ))
            },
        )
    }

    #[tokio::test]
    async fn web_fetch_asks_for_a_new_domain_and_skips_the_ask_for_a_pasted_url() {
        let addr = page_server().await;
        let url = format!("http://{addr}/");
        // Not pasted: the fetch asks first.
        let d = tempfile::tempdir().unwrap();
        let mut t = web_harness(d.path(), &url);
        let _a = t.s.attach();
        t.s.prompt("read the page", Delivery::Steer).unwrap();
        let (ev, asks) = settle_answering(&mut t).await;
        assert_eq!(asks, 1);
        assert_eq!(ended(&ev)[0].outcome, ToolOutcome::Ok);
        let res =
            t.bodies()[1]["messages"].as_array().unwrap()[2]["content"][0]["content"].to_string();
        assert!(res.contains("PAGE-MARKER"), "{res}");
        assert!(res.contains("untrusted_web_content"), "{res}");
        // Pasted by the user: allowed without an ask (D35).
        let d = tempfile::tempdir().unwrap();
        let mut t = web_harness(d.path(), &url);
        let _a = t.s.attach();
        t.s.prompt(format!("summarise {url}"), Delivery::Steer)
            .unwrap();
        let (ev, asks) = settle_answering(&mut t).await;
        assert_eq!(asks, 0);
        assert_eq!(ended(&ev)[0].outcome, ToolOutcome::Ok);
    }
}
