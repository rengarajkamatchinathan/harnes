//! Code-mode methods (02 §6.10) over the in-memory transport against a real temp git
//! repository: `session.create` with a worktree, `git.status/diff/commit`,
//! `files.tree/read`, `worktree.finish`, `checkpoint.*` without checkpoints.

#![allow(clippy::unwrap_used, clippy::expect_used)] // test code

use std::path::Path;
use std::sync::Arc;

use futures::future::BoxFuture;
use genie_client::Endpoint;
use genie_client::connection::{ClientError, ClientInfo, Connection};
use genie_core::runtime::MemoryConversation;
use genie_core::runtime::checkpoint::Checkpoints;
use genie_core::{EngineBuilder, TaskScope, TurnCtx, TurnError, TurnRunner};
use genie_daemon::{CodeServices, DaemonConfig, DaemonState, Registry, Services, serve_stream};
use genie_protocol::api::code::{
    CheckpointParams, GitCommitParams, RestoreWhat, SessionPathParams, WorktreeAction,
    WorktreeFinishParams,
};
use genie_protocol::api::session::{CreateParams, SessionIdParams, WorktreeChoice};
use genie_protocol::common::SessionKind;
use genie_protocol::handshake::{ClientCapabilities, ClientKind};
use genie_protocol::methods as m;
use genie_protocol::{Channel, ErrorKind};
use genie_tools::code::git::Git;

const TOKEN: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

struct Runner;

impl TurnRunner for Runner {
    fn run(&self, _ctx: TurnCtx) -> BoxFuture<'static, Result<(), TurnError>> {
        Box::pin(async { Ok(()) })
    }
}

async fn repo(dir: &Path) {
    let g = Git::new(dir);
    g.ok(&["init", "-q", "-b", "main"]).await.unwrap();
    g.ok(&["config", "user.email", "t@example.com"]).await.unwrap();
    g.ok(&["config", "user.name", "T"]).await.unwrap();
    g.ok(&["config", "commit.gpgsign", "false"]).await.unwrap();
    std::fs::write(dir.join("a.txt"), "a\n").unwrap();
    std::fs::create_dir(dir.join("src")).unwrap();
    std::fs::write(dir.join("src/lib.rs"), "fn x() {}\n").unwrap();
    g.ok(&["add", "."]).await.unwrap();
    g.ok(&["commit", "-q", "-m", "init"]).await.unwrap();
}

async fn setup(dir: &Path) -> Connection {
    let mut cfg = DaemonConfig::new(Endpoint::in_dir(dir, Channel::Dev).unwrap());
    cfg.engine_version = "1.0.0".into();
    cfg.write_logs = false;
    let root = TaskScope::root();
    let registry = Arc::new(Registry::default());
    let engine = EngineBuilder::new()
        .turn_runner(Arc::new(Runner))
        .build(&root);
    let mut code = CodeServices::new(registry.clone());
    code.conversation = Some(Arc::new(MemoryConversation::new()));
    code.checkpoints = Some(Checkpoints::new(dir.join("snapshots"), registry.clone()));
    let services = Services {
        code: Some(Arc::new(code)),
        ..Services::default()
    };
    let state = Arc::new(DaemonState::with_services(
        cfg,
        engine,
        TOKEN.into(),
        root,
        registry,
        None,
        Some(Arc::new(services)),
    ));
    let (client, server) = genie_testkit::memory_pair();
    tokio::spawn(async move { serve_stream(state, Box::new(server), TaskScope::root()).await });
    let info = ClientInfo {
        kind: ClientKind::Desktop,
        version: "1.0.0".into(),
        instance_id: "desk".into(),
        capabilities: ClientCapabilities::default(),
    };
    Connection::handshake(Box::new(client), &info, TOKEN.into())
        .await
        .unwrap()
}

fn code_session(ws: &Path, worktree: Option<WorktreeChoice>, name: Option<&str>) -> CreateParams {
    CreateParams {
        kind: SessionKind::Code,
        model: "anthropic/claude-test".into(),
        workspace_path: Some(ws.display().to_string()),
        worktree,
        worktree_name: name.map(String::from),
        ..CreateParams::default()
    }
}

fn kind(e: &ClientError) -> i64 {
    match e {
        ClientError::Rpc(r) => r.code,
        other => panic!("{other:?}"),
    }
}

fn sp(sid: &str, path: Option<&str>) -> SessionPathParams {
    SessionPathParams {
        session_id: sid.into(),
        path: path.map(String::from),
    }
}

#[tokio::test]
async fn worktree_session_git_files_and_finish() {
    let d = tempfile::tempdir().unwrap();
    let base = std::fs::canonicalize(d.path()).unwrap();
    let ws = base.join("proj");
    std::fs::create_dir(&ws).unwrap();
    repo(&ws).await;
    let conn = setup(&base.join("daemon")).await;

    // A plain session works in the main checkout; `auto` then gets a worktree.
    let main = conn
        .call::<m::SessionCreate>(code_session(&ws, None, None))
        .await
        .unwrap()
        .session_id;
    let auto = conn
        .call::<m::SessionCreate>(code_session(&ws, Some(WorktreeChoice::Auto), None))
        .await
        .unwrap()
        .session_id;
    let named = conn
        .call::<m::SessionCreate>(code_session(&ws, None, Some("Feature X")))
        .await
        .unwrap()
        .session_id;
    let wt = base.join("proj.genie-worktrees/feature-x");
    assert!(wt.join("a.txt").exists());
    assert!(base.join("proj.genie-worktrees/session/a.txt").exists());

    // Files panel.
    let tree = conn
        .call::<m::FilesTree>(sp(&named, None))
        .await
        .unwrap();
    let names: Vec<(String, bool)> = tree.nodes.iter().map(|n| (n.path.clone(), n.dir)).collect();
    assert_eq!(names, [("src".to_string(), true), ("a.txt".to_string(), false)]);
    let f = conn
        .call::<m::FilesRead>(sp(&named, Some("src/lib.rs")))
        .await
        .unwrap();
    assert_eq!(f.text.as_deref(), Some("fn x() {}\n"));
    let e = conn
        .call::<m::FilesRead>(sp(&named, Some("../../proj/a.txt")))
        .await
        .unwrap_err();
    assert_eq!(kind(&e), ErrorKind::PermissionDenied.code());

    // Changes panel in the worktree.
    std::fs::write(wt.join("a.txt"), "changed\n").unwrap();
    std::fs::write(wt.join("new.txt"), "n\n").unwrap();
    let st = conn.call::<m::GitStatus>(sp(&named, None)).await.unwrap();
    assert_eq!(st.branch.as_deref(), Some("genie/feature-x"));
    let mut files: Vec<(String, String)> = st
        .files
        .iter()
        .map(|f| (f.path.clone(), f.change.clone()))
        .collect();
    files.sort();
    assert_eq!(
        files,
        [
            ("a.txt".to_string(), "modified".to_string()),
            ("new.txt".to_string(), "added".to_string())
        ]
    );
    let diff = conn.call::<m::GitDiff>(sp(&named, Some("a.txt"))).await.unwrap();
    assert!(diff.text.contains("-a\n+changed"), "{}", diff.text);
    let c = conn
        .call::<m::GitCommit>(GitCommitParams {
            session_id: named.clone(),
            message: "work".into(),
            paths: None,
            then: Some(genie_protocol::api::code::CommitThen::Pr),
        })
        .await
        .unwrap();
    assert_eq!(c.commit.len(), 40);
    assert!(c.note.unwrap().contains("gh pr create"));
    let e = conn
        .call::<m::GitCommit>(GitCommitParams {
            session_id: named.clone(),
            message: "again".into(),
            paths: None,
            then: None,
        })
        .await
        .unwrap_err();
    assert_eq!(kind(&e), ErrorKind::InvalidState.code());

    // Finish: merge into main, the session moves back to the main checkout.
    let done = conn
        .call::<m::WorktreeFinish>(WorktreeFinishParams {
            session_id: named.clone(),
            action: WorktreeAction::Merge,
        })
        .await
        .unwrap();
    assert!(done.removed && done.branch_deleted && done.merge_commit.is_some());
    assert!(!wt.exists());
    assert_eq!(std::fs::read_to_string(ws.join("new.txt")).unwrap(), "n\n");
    let st = conn.call::<m::GitStatus>(sp(&named, None)).await.unwrap();
    assert_eq!(st.branch.as_deref(), Some("main"));
    // No worktree any more / never had one.
    for sid in [&named, &main] {
        let e = conn
            .call::<m::WorktreeFinish>(WorktreeFinishParams {
                session_id: sid.clone(),
                action: WorktreeAction::Keep,
            })
            .await
            .unwrap_err();
        assert_eq!(kind(&e), ErrorKind::InvalidState.code());
    }
    // Keep, then discard the auto worktree.
    let kept = conn
        .call::<m::WorktreeFinish>(WorktreeFinishParams {
            session_id: auto.clone(),
            action: WorktreeAction::Keep,
        })
        .await
        .unwrap();
    assert!(!kept.removed);
    let gone = conn
        .call::<m::WorktreeFinish>(WorktreeFinishParams {
            session_id: auto.clone(),
            action: WorktreeAction::Discard,
        })
        .await
        .unwrap();
    assert!(gone.removed && gone.branch_deleted);

    // Checkpoints: none yet; unknown message → not_found.
    let list = conn
        .call::<m::CheckpointList>(SessionIdParams {
            session_id: main.clone(),
        })
        .await
        .unwrap();
    assert!(list.checkpoints.is_empty());
    let e = conn
        .call::<m::CheckpointRewind>(CheckpointParams {
            session_id: main.clone(),
            message_id: "msg_nope".into(),
            restore: RestoreWhat::Files,
        })
        .await
        .unwrap_err();
    assert_eq!(kind(&e), ErrorKind::NotFound.code());
    let e = conn
        .call::<m::CheckpointUndo>(SessionIdParams { session_id: main })
        .await
        .unwrap_err();
    assert_eq!(kind(&e), ErrorKind::InvalidState.code());
}

#[tokio::test]
async fn new_worktree_needs_git() {
    let d = tempfile::tempdir().unwrap();
    let base = std::fs::canonicalize(d.path()).unwrap();
    let plain = base.join("plain");
    std::fs::create_dir(&plain).unwrap();
    let conn = setup(&base.join("daemon")).await;
    let e = conn
        .call::<m::SessionCreate>(code_session(&plain, Some(WorktreeChoice::New), None))
        .await
        .unwrap_err();
    assert_eq!(kind(&e), ErrorKind::InvalidState.code());
    // `auto` falls back to the folder itself.
    let sid = conn
        .call::<m::SessionCreate>(code_session(&plain, Some(WorktreeChoice::Auto), None))
        .await
        .unwrap()
        .session_id;
    let e = conn.call::<m::GitStatus>(sp(&sid, None)).await.unwrap_err();
    assert_eq!(kind(&e), ErrorKind::InvalidState.code());
}
