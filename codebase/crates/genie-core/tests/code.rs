//! Code mode end to end against cassettes (06 §4.2, D49): the real engine, turn loop,
//! Anthropic adapter and code tools edit files in a real temp git repository; every
//! mutating step leaves a shadow-git checkpoint; rewind/undo restore files and the
//! conversation.

#![allow(clippy::unwrap_used, clippy::expect_used)] // test code

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use genie_core::bus::{SubItem, Subscription};
use genie_core::runtime::checkpoint::Checkpoints;
use genie_core::runtime::conversation::ConversationStore;
use genie_core::runtime::{
    AllowAllAuthorizer, MemoryConversation, ModelResolver, ResolveError, ResolvedModel,
    RuntimeBuilder, RuntimeConfig, SessionSettings, SettingsSource,
};
use genie_core::{EngineBuilder, SessionHandle, TaskScope};
use genie_protocol::api::code::RestoreWhat;
use genie_protocol::common::SessionKind;
use genie_protocol::event::{SessionEvent, ToolOutcome};
use genie_protocol::session::Delivery;
use genie_providers::auth::NoAuth;
use genie_providers::cassette::{CassetteTransport, RecordedResponse};
use genie_providers::compat::ModelSpec;
use genie_providers::model::{ProtocolKind, ProviderId, Role};
use genie_providers::route::{Preset, PresetProvider, ProviderConfig, Route};
use genie_providers::{Executor, Provider, RetryPolicy};
use genie_tools::code::git::Git;
use serde_json::{Value, json};

const WAIT: Duration = Duration::from_secs(20);

// ------------------------------------------------------------------ SSE builders

fn ev(name: &str, data: Value) -> String {
    format!("event: {name}\ndata: {data}\n\n")
}

fn msg_start() -> String {
    ev(
        "message_start",
        json!({"type":"message_start","message":{"id":"msg_x","type":"message","role":"assistant",
            "model":"claude-test","content":[],"stop_reason":null,
            "usage":{"input_tokens":100,"cache_read_input_tokens":0,"cache_creation_input_tokens":0,"output_tokens":1}}}),
    )
}

fn text_block(idx: u32, t: &str) -> String {
    ev(
        "content_block_start",
        json!({"type":"content_block_start","index":idx,"content_block":{"type":"text","text":""}}),
    ) + &ev(
        "content_block_delta",
        json!({"type":"content_block_delta","index":idx,"delta":{"type":"text_delta","text":t}}),
    ) + &ev(
        "content_block_stop",
        json!({"type":"content_block_stop","index":idx}),
    )
}

fn tool_block(idx: u32, id: &str, name: &str, input: Value) -> String {
    ev(
        "content_block_start",
        json!({"type":"content_block_start","index":idx,"content_block":{"type":"tool_use","id":id,"name":name,"input":{}}}),
    ) + &ev(
        "content_block_delta",
        json!({"type":"content_block_delta","index":idx,"delta":{"type":"input_json_delta","partial_json":input.to_string()}}),
    ) + &ev(
        "content_block_stop",
        json!({"type":"content_block_stop","index":idx}),
    )
}

fn reply(blocks: &[String], stop: &str) -> RecordedResponse {
    let end = ev(
        "message_delta",
        json!({"type":"message_delta","delta":{"stop_reason":stop,"stop_sequence":null},"usage":{"output_tokens":20}}),
    ) + &ev("message_stop", json!({"type":"message_stop"}));
    RecordedResponse::sse(200, &(msg_start() + &blocks.concat() + &end), 37)
}

fn tools(calls: &[(&str, &str, Value)]) -> RecordedResponse {
    let blocks: Vec<String> = calls
        .iter()
        .enumerate()
        .map(|(i, (id, name, input))| tool_block(i as u32, id, name, input.clone()))
        .collect();
    reply(&blocks, "tool_use")
}

fn done() -> RecordedResponse {
    reply(&[text_block(0, "done")], "end_turn")
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
            pricing: None,
            native_compaction: false,
        })
    }
}

struct CodeSettings(PathBuf);

impl SettingsSource for CodeSettings {
    fn settings(&self, _id: &str) -> Option<SessionSettings> {
        Some(SessionSettings {
            kind: SessionKind::Code,
            model: "anthropic/claude-test".into(),
            workspace: Some(self.0.clone()),
            ..SessionSettings::default()
        })
    }
}

struct H {
    _root: TaskScope,
    _dir: tempfile::TempDir,
    ws: PathBuf,
    conv: Arc<MemoryConversation>,
    cps: Arc<Checkpoints>,
    s: SessionHandle,
    sub: Subscription,
}

async fn git_repo(dir: &Path) {
    let g = Git::new(dir);
    g.ok(&["init", "-q", "-b", "main"]).await.unwrap();
    g.ok(&["config", "user.email", "t@example.com"]).await.unwrap();
    g.ok(&["config", "user.name", "T"]).await.unwrap();
    std::fs::write(dir.join("a.txt"), "one\ntwo\nthree\n").unwrap();
    g.ok(&["add", "."]).await.unwrap();
    g.ok(&["commit", "-q", "-m", "init"]).await.unwrap();
}

async fn harness(responses: Vec<RecordedResponse>) -> H {
    let dir = tempfile::tempdir().unwrap();
    let base = std::fs::canonicalize(dir.path()).unwrap();
    let ws = base.join("repo");
    std::fs::create_dir(&ws).unwrap();
    git_repo(&ws).await;

    let t = Arc::new(CassetteTransport::new(responses));
    let exec = Arc::new(Executor::new(
        t,
        RetryPolicy {
            base: Duration::from_millis(5),
            max_gap: Duration::from_millis(20),
            jitter: false,
            ..RetryPolicy::default()
        },
    ));
    let mut spec = ModelSpec::new("anthropic", "claude-test", ProtocolKind::AnthropicMessages);
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
    let settings: Arc<dyn SettingsSource> = Arc::new(CodeSettings(ws.clone()));
    let conv = Arc::new(MemoryConversation::new());
    let cps = Checkpoints::new(base.join("data/snapshots"), settings.clone());
    let root = TaskScope::root();
    let shell = genie_tools::shell::ShellRuntime::new(genie_tools::shell::ShellConfig::new(
        Arc::new(genie_tools::shell::FixedDir(ws.clone())),
        root.tracker(),
        root.token().clone(),
    ));
    let runner = RuntimeBuilder::new(settings.clone(), Arc::new(Resolver { route, exec }))
        .config(RuntimeConfig {
            titles: false,
            ..RuntimeConfig::default()
        })
        .conversation(conv.clone())
        .authorizer(Arc::new(AllowAllAuthorizer))
        .shell(Arc::new(shell))
        .code(genie_core::runtime::code::code_config(
            settings,
            Arc::new(genie_core::locks::PathLocks::new()),
        ))
        .checkpoints(cps.clone())
        .build();
    let engine = EngineBuilder::new()
        .turn_runner(Arc::new(runner))
        .build(&root);
    let s = engine.create_session();
    let sub = engine.bus().subscribe(s.id(), 0).unwrap();
    H {
        _root: root,
        _dir: dir,
        ws,
        conv,
        cps,
        s,
        sub,
    }
}

impl H {
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

    fn read(&self, rel: &str) -> Option<String> {
        std::fs::read_to_string(self.ws.join(rel)).ok()
    }

    fn user_messages(&self) -> Vec<String> {
        self.conv
            .path(self.s.id())
            .unwrap()
            .iter()
            .filter(|m| m.message.role == Role::User)
            .map(|m| m.id().to_string())
            .collect()
    }
}

fn checkpoints(ev: &[SessionEvent]) -> Vec<&genie_protocol::event::CheckpointCreated> {
    ev.iter()
        .filter_map(|e| match e {
            SessionEvent::CheckpointCreated(c) => Some(c),
            _ => None,
        })
        .collect()
}

fn outcomes(ev: &[SessionEvent]) -> Vec<ToolOutcome> {
    ev.iter()
        .filter_map(|e| match e {
            SessionEvent::ToolEnded(t) => Some(t.outcome),
            _ => None,
        })
        .collect()
}

#[tokio::test]
async fn edits_and_shell_leave_checkpoints_that_rewind_and_undo() {
    let mut h = harness(vec![
        // Turn 1: read (no checkpoint), then two edits of one file + a new file in one
        // step (serialised per path), then done.
        tools(&[("t1", "read", json!({"path": "a.txt"}))]),
        tools(&[
            ("t2", "edit", json!({"path": "a.txt", "old_string": "one", "new_string": "ONE"})),
            ("t3", "edit", json!({"path": "a.txt", "old_string": "two", "new_string": "TWO"})),
            ("t4", "write", json!({"path": "b.txt", "content": "new file\n"})),
        ]),
        done(),
        // Turn 2: a shell command changes files too.
        tools(&[("t5", "shell", json!({"command": "echo hi > c.txt"}))]),
        done(),
    ])
    .await;

    h.s.prompt("fix it", Delivery::Steer).unwrap();
    let ev = h.settle().await;
    assert_eq!(outcomes(&ev), [ToolOutcome::Ok; 4]);
    let cps = checkpoints(&ev);
    assert_eq!(cps.len(), 1, "only the mutating step is checkpointed");
    assert_ne!(cps[0].before_tree, cps[0].after_tree);
    assert_eq!(h.read("a.txt").unwrap(), "ONE\nTWO\nthree\n");
    assert_eq!(h.read("b.txt").unwrap(), "new file\n");
    // The user's repo has no new commits or staged changes from the shadow repo.
    let st = Git::new(&h.ws).ok(&["status", "--porcelain"]).await.unwrap();
    assert_eq!(st, " M a.txt\n?? b.txt");

    h.s.prompt("more", Delivery::Steer).unwrap();
    let ev = h.settle().await;
    assert_eq!(outcomes(&ev), [ToolOutcome::Ok]);
    assert_eq!(checkpoints(&ev).len(), 1, "shell changes are captured");
    assert_eq!(h.read("c.txt").unwrap(), "hi\n");

    let sid = h.s.id().to_string();
    let conv: &dyn ConversationStore = h.conv.as_ref();
    let list = h.cps.list(&sid, conv).await.unwrap();
    assert_eq!(list.len(), 2);
    assert_eq!(list[0].files_changed, 2);
    assert_eq!(list[1].files_changed, 1);
    assert_eq!(list[0].message_id, cps[0].step_ref);

    let users = h.user_messages();
    assert_eq!(users.len(), 2);

    // Rewind files to before the second prompt: only c.txt goes.
    let pv = h
        .cps
        .preview(&sid, conv, &users[1], RestoreWhat::Files)
        .await
        .unwrap();
    assert_eq!(pv.files.len(), 1);
    assert_eq!((pv.files[0].path.as_str(), pv.files[0].change.as_str()), ("c.txt", "deleted"));
    assert!(pv.external_changes.is_empty());
    let path_len = conv.path(&sid).unwrap().len();
    h.cps
        .rewind(&sid, conv, &users[1], RestoreWhat::Files)
        .await
        .unwrap();
    assert!(h.read("c.txt").is_none());
    assert_eq!(h.read("a.txt").unwrap(), "ONE\nTWO\nthree\n");
    assert_eq!(conv.path(&sid).unwrap().len(), path_len, "conversation untouched");
    // Undo puts it back.
    h.cps.undo(&sid, conv).await.unwrap();
    assert_eq!(h.read("c.txt").unwrap(), "hi\n");
    assert!(h.cps.undo(&sid, conv).await.is_err(), "nothing staged any more");

    // External change detection: the user edits a.txt by hand.
    std::fs::write(h.ws.join("a.txt"), "hand edit\n").unwrap();
    let pv = h
        .cps
        .preview(&sid, conv, &users[0], RestoreWhat::Both)
        .await
        .unwrap();
    assert_eq!(pv.external_changes, ["a.txt"]);
    let mut changed: Vec<(String, String)> = pv
        .files
        .iter()
        .map(|f| (f.path.clone(), f.change.clone()))
        .collect();
    changed.sort();
    assert_eq!(
        changed,
        [
            ("a.txt".to_string(), "modified".to_string()),
            ("b.txt".to_string(), "deleted".to_string()),
            ("c.txt".to_string(), "deleted".to_string()),
        ]
    );

    // Rewind both to the first prompt: files back to HEAD state, empty conversation.
    let out = h
        .cps
        .rewind(&sid, conv, &users[0], RestoreWhat::Both)
        .await
        .unwrap();
    assert_eq!(out.leaf, Some(None));
    assert_eq!(h.read("a.txt").unwrap(), "one\ntwo\nthree\n");
    assert!(h.read("b.txt").is_none() && h.read("c.txt").is_none());
    assert!(conv.path(&sid).unwrap().is_empty());
    // Undo restores the files as they were (including the hand edit) and the leaf.
    let out = h.cps.undo(&sid, conv).await.unwrap();
    assert!(matches!(out.leaf, Some(Some(_))));
    assert_eq!(h.read("a.txt").unwrap(), "hand edit\n");
    assert_eq!(h.read("b.txt").unwrap(), "new file\n");
    assert_eq!(conv.path(&sid).unwrap().len(), path_len);

    // A rewind is committed by the next prompt (undo is then refused).
    h.cps
        .rewind(&sid, conv, &users[1], RestoreWhat::Conversation)
        .await
        .unwrap();
    assert!(h.cps.has_staged(&sid));

    // Deleting the session drops its refs.
    h.cps.drop_session(&sid).await;
    assert!(h.cps.list(&sid, conv).await.unwrap().is_empty());
}

#[tokio::test]
async fn failed_edit_and_read_only_steps_leave_no_checkpoint() {
    let mut h = harness(vec![
        tools(&[(
            "t1",
            "edit",
            json!({"path": "a.txt", "old_string": "one", "new_string": "1"}),
        )]),
        tools(&[("t2", "grep", json!({"pattern": "two"}))]),
        done(),
    ])
    .await;
    h.s.prompt("go", Delivery::Steer).unwrap();
    let ev = h.settle().await;
    // edit without a read fails (read-before-edit); grep is read-only.
    assert_eq!(outcomes(&ev), [ToolOutcome::Error, ToolOutcome::Ok]);
    assert!(checkpoints(&ev).is_empty());
    assert_eq!(h.read("a.txt").unwrap(), "one\ntwo\nthree\n");
    assert!(h.cps.list(h.s.id(), h.conv.as_ref()).await.unwrap().is_empty());
}
