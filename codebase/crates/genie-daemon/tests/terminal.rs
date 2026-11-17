//! Terminal tabs (02 §6.7) over the in-memory transport with a real shell on a PTY:
//! open in the session folder, attach + live `terminal.output`, input, resize, reattach
//! from an offset, `terminal.exited` on the workspace feed, close kills, idle guard.
#![cfg(unix)]
#![allow(clippy::unwrap_used, clippy::expect_used)] // test code

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as B64;
use futures::future::BoxFuture;
use genie_client::Endpoint;
use genie_client::connection::{ClientError, ClientInfo, Connection, Incoming};
use genie_core::{EngineBuilder, TaskScope, TurnCtx, TurnError, TurnRunner};
use genie_daemon::{DaemonConfig, DaemonState, serve_stream};
use genie_protocol::api::browser::{
    TerminalAttachParams, TerminalIdParams, TerminalInputParams, TerminalOpenParams,
    TerminalResizeParams,
};
use genie_protocol::api::session::CreateParams;
use genie_protocol::common::SessionKind;
use genie_protocol::event::{SessionEvent, SessionEventEnvelope};
use genie_protocol::handshake::{ClientCapabilities, ClientKind};
use genie_protocol::methods::{self as m, Method};
use genie_protocol::workspace::TerminalExited;
use genie_protocol::{Channel, ErrorKind};
use tokio::sync::mpsc::UnboundedReceiver;

const TOKEN: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
const WAIT: Duration = Duration::from_secs(20);

struct Runner;

impl TurnRunner for Runner {
    fn run(&self, _ctx: TurnCtx) -> BoxFuture<'static, Result<(), TurnError>> {
        Box::pin(async { Ok(()) })
    }
}

async fn setup(dir: &Path) -> (Arc<DaemonState>, Connection, UnboundedReceiver<Incoming>) {
    let mut cfg = DaemonConfig::new(Endpoint::in_dir(dir, Channel::Dev).unwrap());
    cfg.engine_version = "1.0.0".into();
    cfg.write_logs = false;
    let root = TaskScope::root();
    let engine = EngineBuilder::new()
        .turn_runner(Arc::new(Runner))
        .build(&root);
    let state = Arc::new(DaemonState::new(cfg, engine, TOKEN.into(), root));
    let (client, server) = genie_testkit::memory_pair();
    let st = state.clone();
    tokio::spawn(async move { serve_stream(st, Box::new(server), TaskScope::root()).await });
    let info = ClientInfo {
        kind: ClientKind::Desktop,
        version: "1.0.0".into(),
        instance_id: "desk".into(),
        capabilities: ClientCapabilities::default(),
    };
    let conn = Connection::handshake(Box::new(client), &info, TOKEN.into())
        .await
        .unwrap();
    let rx = conn.take_incoming().unwrap();
    (state, conn, rx)
}

fn kind(e: &ClientError) -> i64 {
    match e {
        ClientError::Rpc(r) => r.code,
        other => panic!("{other:?}"),
    }
}

/// Collect `terminal.output` for `tid` until the text matches; checks offsets are
/// contiguous from `from`. Returns (text, next offset).
async fn read_until(
    rx: &mut UnboundedReceiver<Incoming>,
    tid: &str,
    from: u64,
    pred: impl Fn(&str) -> bool,
) -> (String, u64) {
    let mut text = String::new();
    let mut next = from;
    let deadline = tokio::time::Instant::now() + WAIT;
    while !pred(&text) {
        let i = tokio::time::timeout_at(deadline, rx.recv())
            .await
            .unwrap_or_else(|_| panic!("timed out; got {text:?}"))
            .expect("closed");
        let Incoming::Notification(n) = i else {
            continue;
        };
        if n.method != m::Event::NAME {
            continue;
        }
        let env: SessionEventEnvelope = serde_json::from_value(n.params).unwrap();
        if let SessionEvent::TerminalOutput(o) = env.event
            && o.terminal_id == tid
        {
            assert!(env.seq.is_none(), "ephemeral");
            assert_eq!(o.start, next, "contiguous offsets");
            let data = B64.decode(o.data_b64).unwrap();
            assert_eq!(o.end, o.start + data.len() as u64);
            next = o.end;
            text.push_str(&String::from_utf8_lossy(&data));
        }
    }
    (text, next)
}

async fn exited(rx: &mut UnboundedReceiver<Incoming>, tid: &str) -> Option<i32> {
    let deadline = tokio::time::Instant::now() + WAIT;
    loop {
        let i = tokio::time::timeout_at(deadline, rx.recv())
            .await
            .expect("timed out waiting for terminal.exited")
            .expect("closed");
        if let Incoming::Notification(n) = i
            && n.method == m::TerminalExited::NAME
        {
            let e: TerminalExited = serde_json::from_value(n.params).unwrap();
            if e.terminal_id == tid {
                return e.code;
            }
        }
    }
}

fn input(tid: &str, s: &str) -> TerminalInputParams {
    TerminalInputParams {
        terminal_id: tid.into(),
        data_b64: B64.encode(s),
    }
}

async fn wait_terminals(state: &DaemonState, n: u32) {
    let deadline = tokio::time::Instant::now() + WAIT;
    while state.engine().busy().terminals != n {
        assert!(
            tokio::time::Instant::now() < deadline,
            "terminal guard count"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

#[tokio::test]
async fn terminal_open_attach_input_resize_reattach_exit() {
    let d = tempfile::tempdir().unwrap();
    let ws = std::fs::canonicalize(d.path()).unwrap().join("proj");
    std::fs::create_dir_all(ws.join("sub")).unwrap();
    let (state, conn, mut rx) = setup(d.path()).await;
    conn.call::<m::WorkspaceSubscribe>(Default::default())
        .await
        .unwrap();
    let created = conn
        .call::<m::SessionCreate>(CreateParams {
            kind: SessionKind::Code,
            model: "anthropic/claude-test".into(),
            workspace_path: Some(ws.display().to_string()),
            ..CreateParams::default()
        })
        .await
        .unwrap();
    let sid = created.session_id;

    let tid = conn
        .call::<m::TerminalOpen>(TerminalOpenParams {
            session_id: sid.clone(),
            cwd: Some("sub".into()),
            shell: None,
            cols: 80,
            rows: 24,
        })
        .await
        .unwrap()
        .terminal_id;
    // An open terminal keeps the daemon from idling out.
    assert_eq!(state.engine().busy().terminals, 1);
    assert!(state.engine().busy().keeps_alive());

    let att = conn
        .call::<m::TerminalAttach>(TerminalAttachParams {
            terminal_id: tid.clone(),
            offset: 0,
        })
        .await
        .unwrap();
    assert!(!att.truncated);
    conn.call::<m::TerminalInput>(input(&tid, "pwd; echo hi-''term\n"))
        .await
        .unwrap();
    let (text, next) = read_until(&mut rx, &tid, att.end_offset, |t| t.contains("hi-term")).await;
    let before = String::from_utf8_lossy(&B64.decode(&att.replay_b64).unwrap()).to_string();
    assert!(
        format!("{before}{text}").contains(&ws.join("sub").display().to_string()),
        "opened in the session folder: {text:?}"
    );

    conn.call::<m::TerminalResize>(TerminalResizeParams {
        terminal_id: tid.clone(),
        cols: 100,
        rows: 30,
    })
    .await
    .unwrap();
    conn.call::<m::TerminalInput>(input(&tid, "stty size\n"))
        .await
        .unwrap();
    let (_, mark) = read_until(&mut rx, &tid, next, |t| t.contains("30 100")).await;

    // Reattach from an offset: only newer bytes, then live from `end_offset`.
    conn.call::<m::TerminalInput>(input(&tid, "echo after-''mark\n"))
        .await
        .unwrap();
    let (_, now) = read_until(&mut rx, &tid, mark, |t| t.contains("after-mark")).await;
    let again = conn
        .call::<m::TerminalAttach>(TerminalAttachParams {
            terminal_id: tid.clone(),
            offset: mark,
        })
        .await
        .unwrap();
    assert!(!again.truncated && again.checkpoint_b64.is_none());
    let replay = String::from_utf8_lossy(&B64.decode(&again.replay_b64).unwrap()).to_string();
    assert!(
        replay.contains("after-mark") && !replay.contains("30 100"),
        "{replay:?}"
    );
    assert!(again.end_offset >= now);

    conn.call::<m::TerminalInput>(input(&tid, "exit 3\n"))
        .await
        .unwrap();
    assert_eq!(exited(&mut rx, &tid).await, Some(3));
    wait_terminals(&state, 0).await;
    let e = conn
        .call::<m::TerminalInput>(input(&tid, "x"))
        .await
        .unwrap_err();
    assert_eq!(kind(&e), ErrorKind::NotFound.code());
}

#[tokio::test]
async fn close_kills_and_chat_sessions_have_no_terminal() {
    let d = tempfile::tempdir().unwrap();
    let ws = std::fs::canonicalize(d.path()).unwrap();
    let (state, conn, mut rx) = setup(d.path()).await;
    conn.call::<m::WorkspaceSubscribe>(Default::default())
        .await
        .unwrap();
    let sid = conn
        .call::<m::SessionCreate>(CreateParams {
            kind: SessionKind::Code,
            model: "anthropic/claude-test".into(),
            workspace_path: Some(ws.display().to_string()),
            ..CreateParams::default()
        })
        .await
        .unwrap()
        .session_id;
    let open = TerminalOpenParams {
        session_id: sid.clone(),
        cwd: None,
        shell: None,
        cols: 80,
        rows: 24,
    };
    let tid = conn
        .call::<m::TerminalOpen>(open.clone())
        .await
        .unwrap()
        .terminal_id;
    let att = conn
        .call::<m::TerminalAttach>(TerminalAttachParams {
            terminal_id: tid.clone(),
            offset: 0,
        })
        .await
        .unwrap();
    conn.call::<m::TerminalInput>(input(&tid, "echo $$; echo st''arted; sleep 300\n"))
        .await
        .unwrap();
    let (text, _) = read_until(&mut rx, &tid, att.end_offset, |t| t.contains("started")).await;
    let shell_pid: i32 = text
        .lines()
        .rev()
        .find_map(|l| l.trim().parse().ok())
        .expect("shell pid");
    conn.call::<m::TerminalClose>(TerminalIdParams {
        terminal_id: tid.clone(),
    })
    .await
    .unwrap();
    assert_eq!(exited(&mut rx, &tid).await, None, "killed");
    wait_terminals(&state, 0).await;
    // SAFETY: signal 0 only checks existence; the shell has been reaped.
    assert_ne!(unsafe { libc::kill(shell_pid, 0) }, 0, "shell is gone");

    // Session delete closes the session's terminals.
    let t2 = conn
        .call::<m::TerminalOpen>(open)
        .await
        .unwrap()
        .terminal_id;
    conn.call::<m::SessionDelete>(genie_protocol::api::session::SessionIdParams {
        session_id: sid,
    })
    .await
    .unwrap();
    assert_eq!(exited(&mut rx, &t2).await, None);
    wait_terminals(&state, 0).await;

    let chat = conn
        .call::<m::SessionCreate>(CreateParams {
            kind: SessionKind::Chat,
            model: "anthropic/claude-test".into(),
            ..CreateParams::default()
        })
        .await
        .unwrap()
        .session_id;
    let e = conn
        .call::<m::TerminalOpen>(TerminalOpenParams {
            session_id: chat,
            cwd: None,
            shell: None,
            cols: 80,
            rows: 24,
        })
        .await
        .unwrap_err();
    assert_eq!(kind(&e), ErrorKind::InvalidState.code());
}
