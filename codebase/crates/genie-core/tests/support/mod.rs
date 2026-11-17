//! Shared cassette helpers for the subagent and hook tests (no live network).

#![allow(dead_code, clippy::unwrap_used, clippy::expect_used)]

use std::borrow::Cow;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use genie_catalog::Pricing;
use genie_core::bus::{SubItem, Subscription};
use genie_core::runtime::{
    ModelResolver, ResolveError, ResolvedModel, SessionSettings, SettingsSource,
};
use genie_protocol::common::SessionKind;
use genie_protocol::event::SessionEvent;
use genie_providers::auth::NoAuth;
use genie_providers::cassette::{CassetteTransport, RecordedResponse};
use genie_providers::compat::ModelSpec;
use genie_providers::model::{ProtocolKind, ProviderId};
use genie_providers::route::{Preset, PresetProvider, ProviderConfig, Route};
use genie_providers::{Executor, Provider, RetryPolicy};
use genie_tools::{PermissionRequest, Tool, ToolCtx, ToolEnv, ToolError, ToolKind, ToolOutput};
use serde_json::{Value, json};
use tokio::sync::Notify;

pub const WAIT: Duration = Duration::from_secs(10);

pub fn ev(name: &str, data: Value) -> String {
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

pub fn text_block(idx: u32, chunks: &[&str]) -> String {
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

pub fn tool_block(idx: u32, id: &str, name: &str, input: Value) -> String {
    let s = input.to_string();
    ev(
        "content_block_start",
        json!({"type":"content_block_start","index":idx,"content_block":{"type":"tool_use","id":id,"name":name,"input":{}}}),
    ) + &ev(
        "content_block_delta",
        json!({"type":"content_block_delta","index":idx,"delta":{"type":"input_json_delta","partial_json":s}}),
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

pub fn reply(blocks: &[String], stop: &str) -> RecordedResponse {
    let body = msg_start(100) + &blocks.concat() + &msg_end(stop, 20);
    RecordedResponse::sse(200, &body, 37)
}

pub fn text(t: &str) -> RecordedResponse {
    reply(&[text_block(0, &[t])], "end_turn")
}

pub fn tool_call(id: &str, name: &str, input: Value) -> RecordedResponse {
    reply(&[tool_block(0, id, name, input)], "tool_use")
}

/// One cassette per model id (`anthropic/<wire>`), so concurrent sessions on different
/// models replay deterministically.
pub struct Models {
    routes: HashMap<String, (Route, Arc<Executor>, Arc<CassetteTransport>)>,
}

impl Models {
    pub fn new(models: Vec<(&str, Vec<RecordedResponse>)>) -> Arc<Models> {
        let mut routes = HashMap::new();
        for (wire, responses) in models {
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
            let mut spec = ModelSpec::new("anthropic", wire, ProtocolKind::AnthropicMessages);
            spec.context_window = Some(200_000);
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
            routes.insert(format!("anthropic/{wire}"), (route, exec, t));
        }
        Arc::new(Models { routes })
    }

    /// Request bodies sent for `model`.
    pub fn bodies(&self, model: &str) -> Vec<Value> {
        self.routes[model]
            .2
            .recorded()
            .iter()
            .map(|r| serde_json::from_str(&r.body).unwrap())
            .collect()
    }
}

impl ModelResolver for Models {
    fn resolve(&self, model: &str) -> Result<ResolvedModel, ResolveError> {
        let Some((route, exec, _)) = self.routes.get(model) else {
            return Err(ResolveError {
                class: "model_not_found",
                message: model.to_string(),
                fix_link: None,
            });
        };
        Ok(ResolvedModel {
            route: route.clone(),
            client: exec.clone(),
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

/// Every session is a Code (or Chat) session in `workspace` on `anthropic/claude-test`.
pub struct Settings {
    pub kind: SessionKind,
    pub workspace: PathBuf,
}

impl SettingsSource for Settings {
    fn settings(&self, _id: &str) -> Option<SessionSettings> {
        Some(SessionSettings {
            kind: self.kind,
            model: "anthropic/claude-test".into(),
            workspace: Some(self.workspace.clone()),
            title: Some("test".into()),
            ..SessionSettings::default()
        })
    }
}

/// Wire events from `sub` until `pred` matches (inclusive).
pub async fn until(
    sub: &mut Subscription,
    pred: impl Fn(&SessionEvent) -> bool,
) -> Vec<SessionEvent> {
    let mut out = vec![];
    loop {
        let item = tokio::time::timeout(WAIT, sub.next())
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

pub fn settled(e: &SessionEvent) -> bool {
    matches!(e, SessionEvent::TurnSettled(_))
}

/// A mutating tool that records its input and asks first (`ctx.authorize`).
pub struct Echo {
    pub seen: Arc<Mutex<Vec<Value>>>,
    schema: Value,
}

impl Echo {
    pub fn new() -> Arc<Echo> {
        Arc::new(Echo {
            seen: Arc::default(),
            schema: json!({"type":"object","properties":{"text":{"type":"string"}}}),
        })
    }
}

#[async_trait]
impl Tool for Echo {
    fn name(&self) -> &str {
        "echo"
    }
    fn description(&self, _env: &ToolEnv) -> Cow<'_, str> {
        Cow::Borrowed("Echo text back.")
    }
    fn input_schema(&self) -> &Value {
        &self.schema
    }
    fn kind(&self) -> ToolKind {
        ToolKind::Mutating
    }
    async fn execute(&self, input: Value, ctx: ToolCtx) -> Result<ToolOutput, ToolError> {
        ctx.authorize(PermissionRequest {
            resource: input["text"].as_str().map(String::from),
            ..PermissionRequest::default()
        })
        .await?;
        self.seen.lock().unwrap().push(input.clone());
        Ok(ToolOutput::text(format!(
            "echo: {}",
            input["text"].as_str().unwrap_or("")
        )))
    }
}

/// A read-only tool that blocks until released (or cancelled).
pub struct Gate {
    pub open: Arc<Notify>,
    schema: Value,
}

impl Gate {
    pub fn new() -> Arc<Gate> {
        Arc::new(Gate {
            open: Arc::new(Notify::new()),
            schema: json!({"type":"object","properties":{}}),
        })
    }
}

#[async_trait]
impl Tool for Gate {
    fn name(&self) -> &str {
        "gate"
    }
    fn description(&self, _env: &ToolEnv) -> Cow<'_, str> {
        Cow::Borrowed("Wait for the gate.")
    }
    fn input_schema(&self) -> &Value {
        &self.schema
    }
    fn kind(&self) -> ToolKind {
        ToolKind::ReadOnly
    }
    async fn execute(&self, _input: Value, _ctx: ToolCtx) -> Result<ToolOutput, ToolError> {
        self.open.notified().await;
        Ok(ToolOutput::text("gate opened"))
    }
}
