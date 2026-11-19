//! Auto-verify (ticket 47 phase 3): a Code turn that changed files while a dev server runs
//! is asked once to check the change in the app before it ends.
#![allow(clippy::unwrap_used, clippy::expect_used)] // test code

mod support;

use std::borrow::Cow;
use std::sync::Arc;

use async_trait::async_trait;
use genie_core::bus::Subscription;
use genie_core::runtime::{AllowAllAuthorizer, MemoryConversation, RuntimeBuilder, RuntimeConfig};
use genie_core::{EngineBuilder, SessionHandle, TaskScope};
use genie_protocol::common::SessionKind;
use genie_protocol::event::SessionEvent;
use genie_protocol::message::InputPart;
use genie_protocol::session::Delivery;
use genie_providers::cassette::RecordedResponse;
use genie_tools::preview::{DevServers, LaunchFile, ServerConfig};
use genie_tools::shell::FixedDir;
use genie_tools::{Tool, ToolCtx, ToolEnv, ToolError, ToolKind, ToolOutput};
use serde_json::{Value, json};
use support::*;
use tokio_util::sync::CancellationToken;

const MODEL: &str = "anthropic/claude-test";
const ASK: &str = "auto-verify is on for this project";

/// Stands in for a file tool (`edit`) or a page check (`browser_console`).
struct Named(&'static str, Value);

#[async_trait]
impl Tool for Named {
    fn name(&self) -> &str {
        self.0
    }
    fn description(&self, _env: &ToolEnv) -> Cow<'_, str> {
        Cow::Borrowed("test tool")
    }
    fn input_schema(&self) -> &Value {
        &self.1
    }
    fn kind(&self) -> ToolKind {
        ToolKind::ReadOnly
    }
    async fn execute(&self, _input: Value, _ctx: ToolCtx) -> Result<ToolOutput, ToolError> {
        Ok(ToolOutput::text("ok"))
    }
}

struct H {
    _root: TaskScope,
    _dir: tempfile::TempDir,
    _port: std::net::TcpListener,
    models: Arc<Models>,
    s: SessionHandle,
    sub: Subscription,
}

/// A Code session whose folder has a running (attached) dev server `web`.
async fn harness(responses: Vec<RecordedResponse>, launch: impl FnOnce(&mut LaunchFile)) -> H {
    let dir = tempfile::tempdir().unwrap();
    let ws = dir.path().join("ws");
    std::fs::create_dir_all(&ws).unwrap();
    let port = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!("http://localhost:{}/", port.local_addr().unwrap().port());
    let mut f = LaunchFile::default();
    f.upsert(ServerConfig {
        name: "web".into(),
        url: Some(url),
        ..Default::default()
    });
    launch(&mut f);
    f.save(&ws).unwrap();

    let dev = Arc::new(DevServers::new(
        Arc::new(FixedDir(ws.clone())),
        tokio_util::task::TaskTracker::new(),
        CancellationToken::new(),
    ));
    let models = Models::new(vec![("claude-test", responses)]);
    let schema = json!({"type": "object", "properties": {}});
    let mut reg = genie_tools::chat::registry();
    reg.register(Arc::new(Named("edit", schema.clone())));
    reg.register(Arc::new(Named("browser_console", schema)));
    let root = TaskScope::root();
    let runner = RuntimeBuilder::new(
        Arc::new(Settings {
            kind: SessionKind::Code,
            workspace: ws,
        }),
        models.clone(),
    )
    .config(RuntimeConfig {
        titles: false,
        ..RuntimeConfig::default()
    })
    .conversation(Arc::new(MemoryConversation::new()))
    .authorizer(Arc::new(AllowAllAuthorizer))
    .chat_tools(reg.clone())
    .code_tools(reg)
    .preview(&dev)
    .build();
    let engine = EngineBuilder::new()
        .turn_runner(Arc::new(runner))
        .build(&root);
    let s = engine.create_session();
    dev.start(s.id(), "web", &CancellationToken::new())
        .await
        .unwrap();
    let sub = engine.bus().subscribe(s.id(), 0).unwrap();
    H {
        _root: root,
        _dir: dir,
        _port: port,
        models,
        s,
        sub,
    }
}

impl H {
    async fn turn(&mut self, prompt: &str) -> Vec<SessionEvent> {
        self.s
            .prompt_parts(vec![InputPart::text(prompt)], Delivery::Queue)
            .unwrap();
        until(&mut self.sub, settled).await
    }

    fn bodies(&self) -> Vec<String> {
        self.models
            .bodies(MODEL)
            .iter()
            .map(Value::to_string)
            .collect()
    }
}

#[tokio::test]
async fn an_edit_with_a_running_server_is_checked_once() {
    let mut h = harness(
        vec![
            tool_call("toolu_1", "edit", json!({})),
            text("done"),
            text("checked: the header renders"),
        ],
        |_| {},
    )
    .await;
    h.turn("make the header blue").await;
    let b = h.bodies();
    assert_eq!(b.len(), 3, "one extra step to verify");
    assert!(!b[1].contains(ASK));
    assert!(b[2].contains(ASK), "{}", b[2]);
    assert!(b[2].contains("the dev server `web` (http://localhost:"));
}

#[tokio::test]
async fn a_turn_that_already_looked_at_the_app_is_not_asked() {
    let mut h = harness(
        vec![
            tool_call("toolu_1", "edit", json!({})),
            tool_call("toolu_2", "browser_console", json!({})),
            text("done, no console errors"),
        ],
        |_| {},
    )
    .await;
    h.turn("fix the error").await;
    assert_eq!(h.bodies().len(), 3);
    assert!(!h.bodies().iter().any(|b| b.contains(ASK)));
}

#[tokio::test]
async fn auto_verify_off_in_launch_json_is_respected() {
    let mut h = harness(
        vec![tool_call("toolu_1", "edit", json!({})), text("done")],
        |f| f.auto_verify = Some(false),
    )
    .await;
    h.turn("change it").await;
    assert_eq!(h.bodies().len(), 2);
}

#[tokio::test]
async fn no_edit_no_ask() {
    let mut h = harness(vec![text("it uses vite")], |_| {}).await;
    h.turn("what bundler is this?").await;
    assert_eq!(h.bodies().len(), 1);
}
