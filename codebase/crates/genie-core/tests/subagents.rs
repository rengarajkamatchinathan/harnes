//! Subagents (06 §11, D47) end to end against cassettes: the real engine, session actors,
//! bus, executor and Anthropic adapter; the parent and the child talk to different
//! cassettes (one per model id).

#![allow(clippy::unwrap_used, clippy::expect_used)] // test code

mod support;

use std::sync::Arc;
use std::time::Duration;

use genie_core::bus::Subscription;
use genie_core::runtime::agents::AgentCatalog;
use genie_core::runtime::subagents::{SubagentConfig, SubagentHub};
use genie_core::runtime::{
    DefaultAuthorizer, MemoryConversation, RuntimeBuilder, RuntimeConfig,
};
use genie_core::{Engine, EngineBuilder, SessionHandle, TaskScope};
use genie_protocol::ask::{AskAnswer, Decision};
use genie_protocol::common::SessionKind;
use genie_protocol::event::{Activity, MessageSource, SessionEvent, TurnStatus};
use genie_protocol::message::InputPart;
use serde_json::json;
use support::*;

const PARENT: &str = "anthropic/claude-test";
const CHILD: &str = "anthropic/claude-child";

struct H {
    _root: TaskScope,
    _dir: tempfile::TempDir,
    engine: Engine,
    models: Arc<Models>,
    hub: Arc<SubagentHub>,
    s: SessionHandle,
    sub: Subscription,
    gate: Arc<Gate>,
}

fn harness(parent: Vec<genie_providers::cassette::RecordedResponse>, child: Vec<genie_providers::cassette::RecordedResponse>) -> H {
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path().join("home/.genie");
    std::fs::create_dir_all(home.join("agents")).unwrap();
    std::fs::write(
        home.join("agents/worker.md"),
        "---\nname: worker\ndescription: Does delegated work.\nmodel: anthropic/claude-child\n---\nYou are the TEST WORKER agent.\n",
    )
    .unwrap();
    let ws = dir.path().join("ws");
    std::fs::create_dir_all(&ws).unwrap();
    let models = Models::new(vec![("claude-test", parent), ("claude-child", child)]);
    let gate = Gate::new();
    let mut reg = genie_tools::chat::registry();
    reg.register(Echo::new());
    reg.register(gate.clone());
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
    .authorizer(Arc::new(DefaultAuthorizer))
    .chat_tools(reg.clone())
    .code_tools(reg)
    .agents(Arc::new(AgentCatalog::new(Some(home), None)))
    .subagents(SubagentConfig::default(), root.child("subagents"))
    .build();
    let hub = runner.runtime().subagents.clone().unwrap();
    let engine = EngineBuilder::new()
        .turn_runner(Arc::new(runner))
        .build(&root);
    hub.bind(engine.clone(), None);
    let s = engine.create_session();
    let sub = engine.bus().subscribe(s.id(), 0).unwrap();
    H {
        _root: root,
        _dir: dir,
        engine,
        models,
        hub,
        s,
        sub,
        gate,
    }
}

fn task_call() -> genie_providers::cassette::RecordedResponse {
    tool_call(
        "toolu_task",
        "task",
        json!({"agent": "worker", "description": "Do the work", "prompt": "Please do the work."}),
    )
}

fn waiting(e: &SessionEvent) -> bool {
    matches!(e, SessionEvent::Status(s) if s.activity == Some(Activity::WaitingForChildren))
}

fn child_of(events: &[SessionEvent]) -> String {
    events
        .iter()
        .find_map(|e| match e {
            SessionEvent::SubagentStarted(s) => Some(s.child_session_id.clone()),
            _ => None,
        })
        .expect("subagent.started")
}

fn body_text(v: &serde_json::Value) -> String {
    v.to_string()
}

#[tokio::test]
async fn subagent_runs_end_to_end_and_its_ask_surfaces_in_the_parent() {
    let mut h = harness(
        vec![
            task_call(),
            text("Waiting for the worker."),
            text("All done: the worker finished."),
        ],
        vec![
            tool_call("toolu_echo", "echo", json!({"text": "from the child"})),
            text("child done"),
        ],
    );
    h.s.prompt_parts(vec![InputPart::text("Delegate the work.")], genie_protocol::session::Delivery::Queue)
        .unwrap();

    // The child's permission ask is raised in the parent, labelled with the subagent.
    let ev = until(&mut h.sub, |e| matches!(e, SessionEvent::PermissionAsked(_))).await;
    let child = child_of(&ev);
    let ask = match ev.last().unwrap() {
        SessionEvent::PermissionAsked(a) => a.ask.clone(),
        _ => unreachable!(),
    };
    let label = ask.subagent.clone().expect("ask labelled with the subagent");
    assert_eq!(label.session_id, child);
    assert_eq!(label.agent, "worker");
    assert_eq!(ask.session_id, h.s.id());
    assert_eq!(ask.tool.as_deref(), Some("echo"));
    // Listed by the parent and answered there.
    assert_eq!(h.s.pending_ask_list().len(), 1);
    h.s.answer_ask(
        &ask.id,
        serde_json::to_value(AskAnswer::decision(Decision::Once)).unwrap(),
    )
    .unwrap();

    let ev2 = until(&mut h.sub, settled).await;
    let all: Vec<SessionEvent> = ev.into_iter().chain(ev2).collect();
    assert!(all.iter().any(waiting), "parent waited for its child");
    let ended = all
        .iter()
        .find_map(|e| match e {
            SessionEvent::SubagentEnded(x) => Some(x.clone()),
            _ => None,
        })
        .expect("subagent.ended");
    assert_eq!(ended.child_session_id, child);
    assert_eq!(ended.status, TurnStatus::Completed);
    assert_eq!(ended.summary.as_deref(), Some("child done"));
    assert!(ended.cost.usd > 0.0, "child cost reported");
    match all.last().unwrap() {
        SessionEvent::TurnSettled(s) => assert_eq!(s.status, TurnStatus::Completed),
        other => panic!("{other:?}"),
    }
    // The report was steered into the same parent turn.
    assert_eq!(
        all.iter()
            .filter(|e| matches!(e, SessionEvent::TurnStarted(_)))
            .count(),
        1
    );

    let parent = h.models.bodies(PARENT);
    assert_eq!(parent.len(), 3);
    let tools: Vec<String> = parent[0]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["name"].as_str().unwrap().to_string())
        .collect();
    assert!(tools.contains(&"task".to_string()) && tools.contains(&"task_wait".to_string()));
    let last = body_text(&parent[2]);
    assert!(last.contains("<subagent name=\\\"worker\\\""), "{last}");
    assert!(last.contains("child done"));

    let child_bodies = h.models.bodies(CHILD);
    assert_eq!(child_bodies.len(), 2);
    let sys = body_text(&child_bodies[0]["system"]);
    assert!(sys.contains("TEST WORKER"), "agent body replaces the base prompt");
    let child_tools: Vec<String> = child_bodies[0]["tools"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| t["name"].as_str().unwrap().to_string())
        .collect();
    assert!(!child_tools.contains(&"task".to_string()), "depth 1: no task for children");
    assert!(body_text(&child_bodies[0]["messages"]).contains("Please do the work."));
    assert!(!h.hub.has_running(h.s.id()));

    // The child session records its parent.
    let mut csub = h.engine.bus().subscribe(&child, 0).unwrap();
    let cev = until(&mut csub, |e| matches!(e, SessionEvent::SessionCreated(_))).await;
    match cev.last().unwrap() {
        SessionEvent::SessionCreated(c) => {
            assert_eq!(c.parent_session_id.as_deref(), Some(h.s.id()));
            assert_eq!(c.agent.as_deref(), Some("worker"));
            assert_eq!(c.model, CHILD);
        }
        _ => unreachable!(),
    }
}

#[tokio::test]
async fn interrupt_with_stop_subagents_stops_the_children_and_drops_their_reports() {
    let mut h = harness(
        vec![task_call(), text("Waiting.")],
        vec![tool_call("toolu_gate", "gate", json!({}))],
    );
    h.s.prompt_parts(vec![InputPart::text("Delegate.")], genie_protocol::session::Delivery::Queue)
        .unwrap();
    until(&mut h.sub, waiting).await;
    // Let the child reach its blocking tool call.
    for _ in 0..200 {
        if h.models.bodies(CHILD).len() == 1 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert_eq!(h.hub.stop_children(h.s.id()), 1);
    assert!(h.s.interrupt());

    let mut got_settled = None;
    let mut got_ended = None;
    while got_settled.is_none() || got_ended.is_none() {
        let ev = until(&mut h.sub, |e| {
            matches!(e, SessionEvent::TurnSettled(_) | SessionEvent::SubagentEnded(_))
        })
        .await;
        match ev.last().unwrap() {
            SessionEvent::TurnSettled(s) => got_settled = Some(s.status),
            SessionEvent::SubagentEnded(s) => got_ended = Some(s.status),
            _ => {}
        }
    }
    assert_eq!(got_settled, Some(TurnStatus::Cancelled));
    assert_eq!(got_ended, Some(TurnStatus::Cancelled));
    assert!(!h.hub.has_running(h.s.id()));
    // No report: the parent stays idle.
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(h.s.pending_inbox().is_empty());
    assert_eq!(h.models.bodies(PARENT).len(), 2);
}

#[tokio::test]
async fn background_subagent_report_arrives_as_a_new_turn() {
    let mut h = harness(
        vec![task_call(), text("Waiting."), text("Got the late result.")],
        vec![tool_call("toolu_gate", "gate", json!({})), text("late result")],
    );
    h.s.prompt_parts(vec![InputPart::text("Delegate.")], genie_protocol::session::Delivery::Queue)
        .unwrap();
    until(&mut h.sub, waiting).await;
    // Interrupt without stop_subagents: the child keeps running in the background.
    assert!(h.s.interrupt());
    let ev = until(&mut h.sub, settled).await;
    match ev.last().unwrap() {
        SessionEvent::TurnSettled(s) => assert_eq!(s.status, TurnStatus::Cancelled),
        _ => unreachable!(),
    }
    assert!(h.hub.has_running(h.s.id()));
    for _ in 0..200 {
        if h.models.bodies(CHILD).len() == 1 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    h.gate.open.notify_one();

    // The report wakes the idle parent as a synthetic message and a new turn.
    let ev = until(&mut h.sub, settled).await;
    let admitted = ev
        .iter()
        .find_map(|e| match e {
            SessionEvent::MessageAdmitted(m) if m.source == MessageSource::Synthetic => {
                Some(m.clone())
            }
            _ => None,
        })
        .expect("synthetic report admitted");
    let text_of = genie_core::session::parts_text(&admitted.parts);
    assert!(text_of.contains("<subagent name=\"worker\""), "{text_of}");
    assert!(text_of.contains("status=\"done\""));
    assert!(text_of.contains("late result"));
    assert!(ev.iter().any(|e| matches!(e, SessionEvent::TurnStarted(_))));
    assert!(ev.iter().any(|e| matches!(e, SessionEvent::SubagentEnded(x) if x.status == TurnStatus::Completed)));
    match ev.last().unwrap() {
        SessionEvent::TurnSettled(s) => assert_eq!(s.status, TurnStatus::Completed),
        _ => unreachable!(),
    }
    let parent = h.models.bodies(PARENT);
    assert_eq!(parent.len(), 3);
    assert!(body_text(&parent[2]).contains("late result"));
}

#[tokio::test]
async fn task_rejects_unknown_agents_and_follow_ups_reuse_the_child() {
    let mut h = harness(
        vec![
            tool_call(
                "toolu_bad",
                "task",
                json!({"agent": "nobody", "description": "x", "prompt": "y"}),
            ),
            text("That agent does not exist."),
        ],
        vec![],
    );
    h.s.prompt_parts(vec![InputPart::text("Go.")], genie_protocol::session::Delivery::Queue)
        .unwrap();
    let ev = until(&mut h.sub, settled).await;
    assert!(!ev.iter().any(|e| matches!(e, SessionEvent::SubagentStarted(_))));
    let second = body_text(&h.models.bodies(PARENT)[1]);
    assert!(second.contains("unknown agent `nobody`"), "{second}");
    assert!(second.contains("worker"), "lists available agents");
}
