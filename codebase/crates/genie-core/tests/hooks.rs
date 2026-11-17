//! Hooks (06 §12, D48) against cassettes with small `sh` test scripts: deny, rewrite,
//! timeout (ignored / fail-closed), `turn.end` continue, `prompt.submit` block, untrusted
//! and unapproved project hooks skipped, Chat never runs hooks.

#![cfg(unix)]
#![allow(clippy::unwrap_used, clippy::expect_used)] // test code

mod support;

use std::path::{Path, PathBuf};
use std::sync::Arc;

use genie_core::bus::Subscription;
use genie_core::config::ConfigHub;
use genie_core::runtime::hooks::{
    ConfigHooks, HookScope, HookSource, HooksHub, StaticHooks, parse_hooks,
};
use genie_core::runtime::{AllowAllAuthorizer, MemoryConversation, RuntimeBuilder, RuntimeConfig};
use genie_core::{EngineBuilder, SessionHandle, TaskScope};
use genie_protocol::api::config::{ApproveDecision, DefinitionKind, TrustDecision};
use genie_protocol::common::SessionKind;
use genie_protocol::event::{SessionEvent, ToolOutcome};
use genie_protocol::message::InputPart;
use genie_protocol::session::Delivery;
use genie_providers::cassette::RecordedResponse;
use serde_json::{Value, json};
use support::*;

const MODEL: &str = "anthropic/claude-test";

struct H {
    _root: TaskScope,
    dir: tempfile::TempDir,
    models: Arc<Models>,
    echo: Arc<Echo>,
    s: SessionHandle,
    sub: Subscription,
}

fn script(dir: &Path, name: &str, body: &str) -> String {
    let p = dir.join(name);
    std::fs::write(&p, format!("#!/bin/sh\n{body}\n")).unwrap();
    p.display().to_string()
}

fn harness_with(
    responses: Vec<RecordedResponse>,
    kind: SessionKind,
    source: impl FnOnce(&Path) -> Arc<dyn HookSource>,
) -> H {
    let dir = tempfile::tempdir().unwrap();
    let ws = dir.path().join("ws");
    std::fs::create_dir_all(&ws).unwrap();
    let source = source(dir.path());
    let models = Models::new(vec![("claude-test", responses)]);
    let echo = Echo::new();
    let mut reg = genie_tools::chat::registry();
    reg.register(echo.clone());
    let root = TaskScope::root();
    let runner = RuntimeBuilder::new(
        Arc::new(Settings {
            kind,
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
    .hooks(HooksHub::new(source))
    .build();
    let engine = EngineBuilder::new()
        .turn_runner(Arc::new(runner))
        .build(&root);
    let s = engine.create_session();
    let sub = engine.bus().subscribe(s.id(), 0).unwrap();
    H {
        _root: root,
        dir,
        models,
        echo,
        s,
        sub,
    }
}

/// Hooks given as settings JSON; `{dir}` in `run` args is the temp dir.
fn harness(responses: Vec<RecordedResponse>, hooks: impl FnOnce(&Path) -> Value) -> H {
    harness_with(responses, SessionKind::Code, |d| {
        let (specs, problems) = parse_hooks(Some(&hooks(d)), HookScope::Local);
        assert!(problems.is_empty(), "{problems:?}");
        Arc::new(StaticHooks(specs))
    })
}

fn echo_turn(final_text: &str) -> Vec<RecordedResponse> {
    vec![
        tool_call("toolu_1", "echo", json!({"text": "hi"})),
        text(final_text),
    ]
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

fn ran(ev: &[SessionEvent]) -> Vec<genie_protocol::event::HookRan> {
    ev.iter()
        .filter_map(|e| match e {
            SessionEvent::HookRan(r) => Some(r.clone()),
            _ => None,
        })
        .collect()
}

fn tool_outcome(ev: &[SessionEvent]) -> genie_protocol::event::ToolEnded {
    ev.iter()
        .find_map(|e| match e {
            SessionEvent::ToolEnded(t) => Some(t.clone()),
            _ => None,
        })
        .expect("tool.ended")
}

#[tokio::test]
async fn tool_before_deny_blocks_the_call_and_tells_the_model() {
    let mut h = harness(echo_turn("ok"), |d| {
        json!([{ "id": "no-echo", "event": "tool.before", "match": {"tool": "echo"},
                 "run": ["sh", script(d, "deny.sh",
                    r#"cat > /dev/null; printf '{"decision":"deny","message":"echo is not allowed here"}'"#)] }])
    });
    let ev = h.turn("go").await;
    let t = tool_outcome(&ev);
    assert_eq!(t.outcome, ToolOutcome::Blocked);
    assert_eq!(t.error.as_ref().unwrap().code, "hook_denied");
    assert!(h.echo.seen.lock().unwrap().is_empty(), "the tool never ran");
    let r = ran(&ev);
    assert_eq!(r.len(), 1);
    assert_eq!(r[0].hook_id, "no-echo");
    assert_eq!(r[0].decision.as_deref(), Some("deny"));
    assert!(r[0].error.is_none());
    assert!(h.bodies()[1].contains("echo is not allowed here"));
}

#[tokio::test]
async fn tool_before_rewrites_input_and_gets_the_envelope_on_stdin() {
    let mut h = harness(echo_turn("ok"), |d| {
        let out = d.join("envelope.json").display().to_string();
        json!([{ "event": "tool.before", "match": {"tool": ["echo"]},
                 "run": ["sh", script(d, "rewrite.sh", &format!(
                    r#"cat > '{out}'; printf '{{"updatedInput":{{"text":"rewritten"}},"addContext":"ignored here"}}'"#))] }])
    });
    let ev = h.turn("go").await;
    assert_eq!(tool_outcome(&ev).outcome, ToolOutcome::Ok);
    assert_eq!(
        h.echo.seen.lock().unwrap().clone(),
        vec![json!({"text": "rewritten"})]
    );
    let env: Value =
        serde_json::from_str(&std::fs::read_to_string(h.dir.path().join("envelope.json")).unwrap())
            .unwrap();
    assert_eq!(env["genieHook"], 1);
    assert_eq!(env["event"], "tool.before");
    assert_eq!(env["session"], h.s.id());
    assert_eq!(env["agent"], "main");
    assert_eq!(env["mode"], "default");
    assert_eq!(env["data"]["tool"], "echo");
    assert_eq!(env["data"]["input"]["text"], "hi");
    assert!(h.bodies()[1].contains("echo: rewritten"));
}

#[tokio::test]
async fn timeout_is_a_hook_error_ignored_unless_fail_closed() {
    let mut h = harness(echo_turn("ok"), |d| {
        json!([{ "event": "tool.before", "run": ["sh", script(d, "slow.sh", "sleep 5")],
                 "timeoutMs": 200 }])
    });
    let ev = h.turn("go").await;
    let r = ran(&ev);
    assert_eq!(r.len(), 1);
    assert!(
        r[0].error.as_deref().unwrap().contains("timed out"),
        "{r:?}"
    );
    assert_eq!(tool_outcome(&ev).outcome, ToolOutcome::Ok);
    assert_eq!(h.echo.seen.lock().unwrap().len(), 1);

    let mut h = harness(echo_turn("ok"), |d| {
        json!([{ "event": "tool.before", "run": ["sh", script(d, "slow.sh", "sleep 5")],
                 "timeoutMs": 200, "failClosed": true },
               { "event": "tool.after", "run": ["sh", script(d, "fail.sh", "exit 3")] }])
    });
    let ev = h.turn("go").await;
    assert_eq!(tool_outcome(&ev).outcome, ToolOutcome::Blocked);
    assert!(h.echo.seen.lock().unwrap().is_empty());
    // The tool never ran, so `tool.after` never fired.
    assert_eq!(ran(&ev).len(), 1);
}

#[tokio::test]
async fn non_zero_exit_and_bad_json_are_errors_and_tool_after_adds_context() {
    let mut h = harness(echo_turn("ok"), |d| {
        json!([{ "event": "tool.before", "run": ["sh", script(d, "bad.sh", "echo oops >&2; exit 2")] },
               { "event": "tool.before", "run": ["sh", script(d, "junk.sh", "echo not-json")] },
               { "event": "tool.after", "match": {"tool": "echo"},
                 "run": ["sh", script(d, "ctx.sh", r#"printf '{"addContext":"formatter ran"}'"#)] }])
    });
    let ev = h.turn("go").await;
    let r = ran(&ev);
    assert_eq!(r.len(), 3);
    assert!(
        r[0].error.as_deref().unwrap().contains("code 2: oops"),
        "{r:?}"
    );
    assert!(r[1].error.as_deref().unwrap().contains("invalid JSON"));
    assert!(r[2].error.is_none());
    assert_eq!(tool_outcome(&ev).outcome, ToolOutcome::Ok);
    assert!(h.bodies()[1].contains("formatter ran"));
}

#[tokio::test]
async fn turn_end_continue_runs_another_step() {
    let mut h = harness(vec![text("first"), text("second")], |d| {
        let flag = d.join("once").display().to_string();
        json!([{ "event": "turn.end", "run": ["sh", script(d, "cont.sh", &format!(
            r#"cat > /dev/null; if [ -e '{flag}' ]; then printf '{{}}'; else touch '{flag}'; printf '{{"decision":"continue","message":"run the tests"}}'; fi"#))] }])
    });
    let ev = h.turn("go").await;
    let r = ran(&ev);
    assert_eq!(r.len(), 2);
    assert_eq!(r[0].decision.as_deref(), Some("continue"));
    let b = h.bodies();
    assert_eq!(b.len(), 2);
    assert!(b[1].contains("A project hook asked you to continue: run the tests"));
}

#[tokio::test]
async fn prompt_submit_can_block_or_add_context() {
    let mut h = harness(vec![text("answer")], |d| {
        json!([{ "event": "prompt.submit", "run": ["sh", script(d, "gate.sh",
            r#"if grep -q secret; then printf '{"decision":"block","message":"no secrets please"}'; else printf '{"addContext":"repo uses tabs"}'; fi"#)] }])
    });
    let ev = h.turn("here is a secret").await;
    let err = ev
        .iter()
        .find_map(|e| match e {
            SessionEvent::SessionError(x) => Some(x.clone()),
            _ => None,
        })
        .expect("session.error");
    assert_eq!(err.code, "hook_blocked");
    assert_eq!(err.message, "no secrets please");
    assert!(
        h.bodies().is_empty(),
        "blocked prompt never reached the model"
    );
    h.turn("normal question").await;
    let b = h.bodies();
    assert_eq!(b.len(), 1);
    assert!(b[0].contains("repo uses tabs"));
}

#[tokio::test]
async fn chat_sessions_never_run_hooks() {
    let mut h = harness_with(echo_turn("ok"), SessionKind::Chat, |d| {
        let (specs, _) = parse_hooks(
            Some(&json!([{ "event": "tool.before",
                "run": ["sh", script(d, "deny.sh", r#"printf '{"decision":"deny"}'"#)] }])),
            HookScope::User,
        );
        Arc::new(StaticHooks(specs))
    });
    let ev = h.turn("go").await;
    assert!(ran(&ev).is_empty());
    assert_eq!(h.echo.seen.lock().unwrap().len(), 1);
}

fn hub_in(d: &Path) -> (Arc<ConfigHub>, PathBuf) {
    let home = d.join("home/.genie");
    std::fs::create_dir_all(&home).unwrap();
    let hub = Arc::new(
        ConfigHub::new(
            home.clone(),
            home.join("trust.json"),
            Some(d.join("home").display().to_string()),
        )
        .unwrap(),
    );
    (hub, d.join("ws"))
}

#[tokio::test]
async fn project_hooks_need_trust_and_definition_approval() {
    let mut responses = vec![];
    for _ in 0..3 {
        responses.extend(echo_turn("ok"));
    }
    let hub_cell: Arc<std::sync::OnceLock<Arc<ConfigHub>>> = Arc::default();
    let cell = hub_cell.clone();
    let mut h = harness_with(responses, SessionKind::Code, move |d| {
        let (hub, ws) = hub_in(d);
        std::fs::create_dir_all(ws.join(".genie")).unwrap();
        std::fs::create_dir_all(ws.join(".git")).unwrap();
        let deny = script(
            d,
            "deny.sh",
            r#"cat > /dev/null; printf '{"decision":"deny","message":"repo hook"}'"#,
        );
        std::fs::write(
            ws.join(".genie/settings.json"),
            json!({"hooks": [{"id": "repo-deny", "event": "tool.before", "run": ["sh", deny]}]})
                .to_string(),
        )
        .unwrap();
        let _ = cell.set(hub.clone());
        Arc::new(ConfigHooks { hub })
    });
    let hub = hub_cell.get().unwrap().clone();
    let ws = h.dir.path().join("ws");
    let wsp = ws.display().to_string();

    // Untrusted: the repo's hook is never run.
    let ev = h.turn("go").await;
    assert!(ran(&ev).is_empty());
    assert_eq!(tool_outcome(&ev).outcome, ToolOutcome::Ok);

    // Trusted but not approved: still skipped, and listed as pending.
    hub.trust_set(&wsp, TrustDecision::Trust).unwrap();
    let pending = hub.pending_definitions(&wsp).unwrap();
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].kind, DefinitionKind::Hook);
    assert_eq!(pending[0].name, "repo-deny");
    let ev = h.turn("go").await;
    assert!(ran(&ev).is_empty());
    assert_eq!(tool_outcome(&ev).outcome, ToolOutcome::Ok);

    // Approved with its current hash: it runs.
    assert!(
        hub.approve_definition(
            &wsp,
            DefinitionKind::Hook,
            "repo-deny",
            &pending[0].hash,
            ApproveDecision::Approve
        )
        .unwrap()
    );
    let ev = h.turn("go").await;
    assert_eq!(ran(&ev).len(), 1);
    assert_eq!(tool_outcome(&ev).outcome, ToolOutcome::Blocked);
    assert_eq!(h.echo.seen.lock().unwrap().len(), 2);
}
