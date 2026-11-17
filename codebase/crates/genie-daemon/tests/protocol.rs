//! Protocol contract tests (02 §11): subscribe → replay → synced → live, overflow →
//! resync, take_over, asks with and without a client, idempotent prompts. All run over
//! the in-memory transport against the real core with a fake turn runner.

#![allow(clippy::unwrap_used, clippy::expect_used)] // test code

use std::sync::Arc;
use std::time::Duration;

use futures::future::BoxFuture;
use genie_client::connection::{ClientError, ClientInfo, Connection, Incoming};
use genie_client::{Action, Endpoint, SessionTracker};
use genie_core::{EngineBuilder, TaskScope, TurnCtx, TurnError, TurnRunner};
use genie_daemon::{DaemonConfig, DaemonState, serve_stream};
use genie_protocol::api::session::{
    CreateParams, PendingParams, PromptParams, RespondParams, SessionIdParams, SubscribeParams,
    UpdateParams,
};
use genie_protocol::ask::{Ask, AskAnswer, AskKind, Decision, Preview};
use genie_protocol::common::SessionKind;
use genie_protocol::event::{SessionEvent, SessionEventEnvelope};
use genie_protocol::handshake::{ClientCapabilities, ClientKind};
use genie_protocol::message::InputPart;
use genie_protocol::methods::{self as m, Method};
use genie_protocol::session::{Delivery, LiveStatus};
use genie_protocol::{Channel, ErrorKind, Id, Message, Notification, Request};
use serde_json::{Value, json};
use tokio::sync::mpsc::UnboundedReceiver;

const TOKEN: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
const WAIT: Duration = Duration::from_secs(5);

/// `ask` → asks for a shell permission; `block` → waits for interrupt; else completes.
struct Runner;

impl TurnRunner for Runner {
    fn run(&self, ctx: TurnCtx) -> BoxFuture<'static, Result<(), TurnError>> {
        Box::pin(async move {
            match ctx.input.text.as_str() {
                "ask" => {
                    let mut a = Ask::new("", "", AskKind::Permission, "");
                    a.tool = Some("shell".into());
                    a.preview = Some(Preview::Command {
                        command: "rm -rf build".into(),
                        cwd: None,
                    });
                    ctx.ask_with(a).await?;
                    Ok(())
                }
                "block" => {
                    ctx.token().cancelled().await;
                    Err(TurnError::Cancelled)
                }
                _ => Ok(()),
            }
        })
    }
}

struct Mem {
    state: Arc<DaemonState>,
    _dir: tempfile::TempDir,
}

fn mem_daemon(tweak: impl FnOnce(&mut DaemonConfig)) -> Mem {
    let dir = tempfile::tempdir().unwrap();
    let mut cfg = DaemonConfig::new(Endpoint::in_dir(dir.path(), Channel::Dev).unwrap());
    cfg.engine_version = "1.0.0".into();
    cfg.write_logs = false;
    tweak(&mut cfg);
    let root = TaskScope::root();
    let engine = EngineBuilder::new()
        .turn_runner(Arc::new(Runner))
        .build(&root);
    Mem {
        state: Arc::new(DaemonState::new(cfg, engine, TOKEN.into(), root)),
        _dir: dir,
    }
}

struct Client {
    conn: Connection,
    rx: UnboundedReceiver<Incoming>,
}

async fn connect(m: &Mem, instance: &str, kind: ClientKind) -> Client {
    let (client, server) = genie_testkit::memory_pair();
    let st = m.state.clone();
    tokio::spawn(async move { serve_stream(st, Box::new(server), TaskScope::root()).await });
    let info = ClientInfo {
        kind,
        version: "1.0.0".into(),
        instance_id: instance.into(),
        capabilities: ClientCapabilities {
            asks: vec![AskKind::Permission],
            notifications: true,
            ..Default::default()
        },
    };
    let conn = Connection::handshake(Box::new(client), &info, TOKEN.into())
        .await
        .unwrap();
    let rx = conn.take_incoming().unwrap();
    Client { conn, rx }
}

impl Client {
    async fn next(&mut self) -> Incoming {
        tokio::time::timeout(WAIT, self.rx.recv())
            .await
            .expect("timed out waiting for a message")
            .expect("connection closed")
    }

    /// Next notification with `method`, skipping others.
    async fn note(&mut self, method: &str) -> Notification {
        loop {
            if let Incoming::Notification(n) = self.next().await
                && n.method == method
            {
                return n;
            }
        }
    }

    /// Next session event matching `pred`.
    async fn event(&mut self, pred: impl Fn(&SessionEvent) -> bool) -> SessionEventEnvelope {
        loop {
            let n = self.note(m::Event::NAME).await;
            let env: SessionEventEnvelope = serde_json::from_value(n.params).unwrap();
            if pred(&env.event) {
                return env;
            }
        }
    }

    async fn create(&self) -> String {
        self.conn
            .call::<m::SessionCreate>(CreateParams {
                kind: SessionKind::Chat,
                project_id: None,
                workspace_path: None,
                model: "test/model".into(),
                effort: None,
                agent: None,
                mode: None,
                incognito: false,
                worktree: None,
            })
            .await
            .unwrap()
            .session_id
    }

    async fn prompt(&self, session: &str, text: &str, msg_id: &str) -> Result<(), ClientError> {
        self.conn
            .call::<m::SessionPrompt>(PromptParams {
                session_id: session.into(),
                parts: vec![InputPart::text(text)],
                delivery: Delivery::Queue,
                parent_id: None,
                client_msg_id: msg_id.into(),
            })
            .await
            .map(|_| ())
    }

    async fn subscribe(&self, session: &str, after: Option<u64>) -> u64 {
        self.conn
            .call::<m::SessionSubscribe>(SubscribeParams {
                session_id: session.into(),
                after_seq: after,
            })
            .await
            .unwrap()
            .head_seq
    }
}

fn kind(e: ClientError) -> ErrorKind {
    match e {
        ClientError::Rpc(e) => e.kind(),
        other => panic!("expected rpc error, got {other:?}"),
    }
}

fn title(t: &str) -> UpdateParams {
    UpdateParams {
        session_id: String::new(),
        title: Some(t.into()),
        ..Default::default()
    }
}

#[tokio::test]
async fn subscribe_replays_then_synced_then_live() {
    let mem = mem_daemon(|_| {});
    let mut a = connect(&mem, "desk_1", ClientKind::Desktop).await;
    let s = a.create().await;
    for t in ["one", "two", "three"] {
        let mut p = title(t);
        p.session_id.clone_from(&s);
        a.conn.call::<m::SessionUpdate>(p).await.unwrap();
    }
    // seq 1 = session.created, 2..=4 = session.updated
    let head = a.subscribe(&s, Some(2)).await;
    assert_eq!(head, 4);
    let mut tracker = SessionTracker::new(&s, 2);
    let mut replayed = vec![];
    loop {
        match a.next().await {
            Incoming::Notification(n) => match tracker.on_notification(&n) {
                Action::Event(e) => replayed.push(e.seq.unwrap()),
                Action::Synced => break,
                other => panic!("unexpected {other:?}"),
            },
            Incoming::Request(r) => panic!("unexpected request {r:?}"),
        }
    }
    assert_eq!(replayed, [3, 4]);

    // Live: a prompt produces inbox.changed, message.admitted, turn.started, turn.settled.
    a.prompt(&s, "hello", "c1").await.unwrap();
    let mut types = vec![];
    loop {
        let n = a.note(m::Event::NAME).await;
        if let Action::Event(e) = tracker.on_notification(&n)
            && e.seq.is_some()
        {
            types.push(e.event.type_name());
            if matches!(e.event, SessionEvent::TurnSettled(_)) {
                break;
            }
        }
    }
    assert!(
        types.contains(&"lease.changed"),
        "first prompt acquires: {types:?}"
    );
    let pos = |t: &str| types.iter().position(|x| *x == t).unwrap();
    assert!(pos("message.admitted") < pos("turn.started"));
    assert!(pos("turn.started") < pos("turn.settled"));
    // Gap-free seqs.
    assert_eq!(tracker.last_seq(), 4 + types.len() as u64);
}

#[tokio::test]
async fn unknown_session_and_bad_params() {
    let mem = mem_daemon(|_| {});
    let a = connect(&mem, "cli_1", ClientKind::Cli).await;
    let e = a
        .conn
        .call::<m::SessionSubscribe>(SubscribeParams {
            session_id: "ses_nope".into(),
            after_seq: None,
        })
        .await
        .unwrap_err();
    assert_eq!(kind(e), ErrorKind::NotFound);
    let e = a
        .conn
        .request::<_, Value>(m::SessionPrompt::NAME, json!({"session_id": 1}))
        .await
        .unwrap_err();
    assert_eq!(kind(e), ErrorKind::InvalidParams);
    // In the table but not wired yet: typed method_not_found with a reason.
    let e = a
        .conn
        .request::<_, Value>(m::ProviderList::NAME, Value::Null)
        .await
        .unwrap_err();
    let ClientError::Rpc(e) = e else { panic!() };
    assert_eq!(e.kind(), ErrorKind::MethodNotFound);
    assert_eq!(e.data.unwrap()["reason"], "not_implemented");
}

#[tokio::test]
async fn prompt_is_idempotent_by_client_msg_id() {
    let mem = mem_daemon(|_| {});
    let a = connect(&mem, "cli_1", ClientKind::Cli).await;
    let s = a.create().await;
    let p = PromptParams {
        session_id: s.clone(),
        parts: vec![InputPart::text("block")],
        delivery: Delivery::Queue,
        parent_id: None,
        client_msg_id: "same".into(),
    };
    let r1 = a.conn.call::<m::SessionPrompt>(p.clone()).await.unwrap();
    let r2 = a.conn.call::<m::SessionPrompt>(p).await.unwrap();
    assert_eq!(r1, r2);
    let r = a
        .conn
        .call::<m::SessionInterrupt>(genie_protocol::api::session::InterruptParams {
            session_id: s,
            stop_subagents: false,
        })
        .await
        .unwrap();
    assert!(r.interrupted);
}

#[tokio::test]
async fn take_over_moves_the_lease() {
    let mem = mem_daemon(|_| {});
    let mut a = connect(&mem, "desk_1", ClientKind::Desktop).await;
    let b = connect(&mem, "cli_1", ClientKind::Cli).await;
    let s = a.create().await;
    a.prompt(&s, "hi", "a1").await.unwrap();

    let e = b.prompt(&s, "hi", "b1").await.unwrap_err();
    let ClientError::Rpc(e) = e else { panic!() };
    assert_eq!(e.kind(), ErrorKind::NotDriver);
    assert_eq!(e.data.unwrap()["driver"]["client_instance_id"], "desk_1");

    // Metadata edits are not lease-gated; model changes are.
    let mut p = title("renamed");
    p.session_id.clone_from(&s);
    b.conn.call::<m::SessionUpdate>(p).await.unwrap();
    let e = b
        .conn
        .call::<m::SessionUpdate>(UpdateParams {
            session_id: s.clone(),
            model: Some("x/y".into()),
            ..Default::default()
        })
        .await
        .unwrap_err();
    assert_eq!(kind(e), ErrorKind::NotDriver);

    b.conn
        .call::<m::SessionTakeOver>(SessionIdParams {
            session_id: s.clone(),
        })
        .await
        .unwrap();
    let lost = a.note(m::LeaseLostNote::NAME).await;
    assert_eq!(lost.params["new_driver"]["client_instance_id"], "cli_1");
    b.prompt(&s, "mine now", "b2").await.unwrap();
    assert_eq!(
        kind(a.prompt(&s, "x", "a2").await.unwrap_err()),
        ErrorKind::NotDriver
    );

    // The durable trail shows both drivers.
    let c = connect(&mem, "obs", ClientKind::Headless).await;
    let mut c = c;
    c.subscribe(&s, None).await;
    let first = c
        .event(|e| matches!(e, SessionEvent::LeaseChanged(_)))
        .await;
    let second = c
        .event(|e| matches!(e, SessionEvent::LeaseChanged(_)))
        .await;
    let drv = |e: &SessionEventEnvelope| match &e.event {
        SessionEvent::LeaseChanged(l) => l.driver.clone().unwrap().client_instance_id,
        _ => unreachable!(),
    };
    assert_eq!(
        (drv(&first), drv(&second)),
        ("desk_1".into(), "cli_1".into())
    );

    b.conn
        .call::<m::SessionRelease>(SessionIdParams {
            session_id: s.clone(),
        })
        .await
        .unwrap();
    a.prompt(&s, "back", "a3").await.unwrap();
}

#[tokio::test]
async fn reconnect_keeps_the_lease_within_grace() {
    let mem = mem_daemon(|_| {});
    let a = connect(&mem, "desk_1", ClientKind::Desktop).await;
    let b = connect(&mem, "cli_1", ClientKind::Cli).await;
    let s = a.create().await;
    a.prompt(&s, "hi", "a1").await.unwrap();
    a.conn.close().await;
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert_eq!(
        kind(b.prompt(&s, "hi", "b1").await.unwrap_err()),
        ErrorKind::NotDriver
    );
    let a2 = connect(&mem, "desk_1", ClientKind::Desktop).await;
    a2.prompt(&s, "again", "a2").await.unwrap();
}

#[tokio::test]
async fn lease_lapses_after_grace() {
    let mem = mem_daemon(|c| c.lease_grace = Duration::from_millis(50));
    let a = connect(&mem, "desk_1", ClientKind::Desktop).await;
    let b = connect(&mem, "cli_1", ClientKind::Cli).await;
    let s = a.create().await;
    a.prompt(&s, "hi", "a1").await.unwrap();
    a.conn.close().await;
    tokio::time::sleep(Duration::from_millis(120)).await;
    b.prompt(&s, "hi", "b1").await.unwrap();
}

#[tokio::test]
async fn ask_goes_to_the_driver_as_a_request() {
    let mem = mem_daemon(|_| {});
    let mut a = connect(&mem, "desk_1", ClientKind::Desktop).await;
    let s = a.create().await;
    a.subscribe(&s, None).await;
    a.prompt(&s, "ask", "a1").await.unwrap();
    let req = loop {
        if let Incoming::Request(r) = a.next().await {
            break r;
        }
    };
    assert_eq!(req.method, m::PermissionAsk::NAME);
    assert_eq!(req.params["ask"]["tool"], "shell");
    assert_eq!(req.params["ask"]["preview"]["command"], "rm -rf build");
    a.conn
        .respond(
            req.id,
            Ok(serde_json::to_value(AskAnswer::decision(Decision::Once)).unwrap()),
        )
        .unwrap();
    let resolved = a
        .event(|e| matches!(e, SessionEvent::PermissionResolved(_)))
        .await;
    let SessionEvent::PermissionResolved(r) = resolved.event else {
        unreachable!()
    };
    assert_eq!(r.decision, Decision::Once);
    assert_eq!(
        serde_json::to_value(&r.by).unwrap()["client_instance_id"],
        "desk_1"
    );
    a.event(|e| matches!(e, SessionEvent::TurnSettled(_))).await;
}

#[tokio::test]
async fn ask_with_no_client_pauses_and_any_client_answers() {
    let mem = mem_daemon(|_| {});
    let mut feed = connect(&mem, "desk_tray", ClientKind::Desktop).await;
    feed.conn
        .call::<m::WorkspaceSubscribe>(genie_protocol::Empty {})
        .await
        .unwrap();
    feed.note(m::WorkspaceSnapshot::NAME).await;

    let headless = connect(&mem, "run_1", ClientKind::Headless).await;
    let s = headless.create().await;
    headless.prompt(&s, "ask", "h1").await.unwrap();

    // Nobody is subscribed: the desktop gets an OS-notification request.
    let n = feed.note(m::UserNotification::NAME).await;
    assert_eq!(n.params["title"], "Genie needs your approval");
    assert_eq!(n.params["session_id"], s.as_str());
    let ask_id = n.params["ask_id"].as_str().unwrap().to_string();

    // The ask is durable and listed; the session shows as paused.
    let other = connect(&mem, "cli_2", ClientKind::Cli).await;
    let pending = other
        .conn
        .call::<m::PermissionPending>(PendingParams {
            session_id: Some(s.clone()),
        })
        .await
        .unwrap();
    assert_eq!(pending.asks.len(), 1);
    assert_eq!(pending.asks[0].id, ask_id);
    let info = other
        .conn
        .call::<m::SessionGet>(genie_protocol::api::session::GetParams {
            session_id: s.clone(),
            leaf_id: None,
        })
        .await
        .unwrap();
    assert_eq!(info.session.summary.status, LiveStatus::Paused);

    // A non-driver may answer (02 §5); answering twice conflicts.
    let resp = RespondParams {
        ask_id: ask_id.clone(),
        answer: AskAnswer {
            decision: Decision::Reject,
            scope: None,
            message: Some("not now".into()),
            data: None,
        },
    };
    other
        .conn
        .call::<m::PermissionRespond>(resp.clone())
        .await
        .unwrap();
    let e = other
        .conn
        .call::<m::PermissionRespond>(resp)
        .await
        .unwrap_err();
    assert_eq!(kind(e), ErrorKind::NotFound);
    tokio::time::sleep(Duration::from_millis(50)).await;
    let pending = other
        .conn
        .call::<m::PermissionPending>(PendingParams::default())
        .await
        .unwrap();
    assert!(pending.asks.is_empty());
}

/// A client that stops reading: its subscription overflows and gets
/// `session.resync_required` with a `from_seq` it can resume from. Nothing is lost.
#[tokio::test]
async fn overflow_sends_resync_not_loss() {
    let mem = mem_daemon(|c| c.sub_queue_max_msgs = 16);
    let writer = connect(&mem, "desk_1", ClientKind::Desktop).await;
    let s = writer.create().await;

    // Raw slow reader.
    let (client, server) = genie_testkit::memory_pair();
    let st = mem.state.clone();
    tokio::spawn(async move { serve_stream(st, Box::new(server), TaskScope::root()).await });
    let (mut r, mut w) = genie_client::framing::split(client);
    let init = json!({
        "protocol": genie_protocol::PROTOCOL_VERSION, "client_kind": "cli",
        "client_version": "1.0.0", "client_instance_id": "slow", "token": TOKEN,
    });
    w.send(&Message::Request(Request::new(
        Id::Num(0),
        m::INITIALIZE,
        init,
    )))
    .await
    .unwrap();
    r.next().await.unwrap().unwrap();
    w.send(&Message::Request(Request::new(
        Id::Num(1),
        m::SessionSubscribe::NAME,
        json!({"session_id": s}),
    )))
    .await
    .unwrap();

    // ~3 MB of events: more than the 1 MiB pipe plus 16 queued lines.
    let big = "x".repeat(1000);
    for i in 0..3000 {
        let p = UpdateParams {
            session_id: s.clone(),
            title: Some(format!("{i}{big}")),
            ..Default::default()
        };
        writer.conn.call::<m::SessionUpdate>(p).await.unwrap();
    }

    let mut seqs = vec![];
    let from_seq = loop {
        let msg = tokio::time::timeout(WAIT, r.next())
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        let Message::Notification(n) = msg else {
            continue;
        };
        if n.method == m::Event::NAME {
            seqs.push(n.params["seq"].as_u64().unwrap());
        } else if n.method == m::SessionResyncRequired::NAME {
            break n.params["from_seq"].as_u64().unwrap();
        }
    };
    assert!(from_seq < 3001, "overflow happened before the end");
    assert_eq!(
        seqs,
        (1..=from_seq).collect::<Vec<_>>(),
        "no gaps up to from_seq"
    );

    // Resume from from_seq (again after each resync): the rest arrives, gap-free.
    let mut next = from_seq + 1;
    let mut req_id = 2;
    let mut resyncs = 1;
    while next <= 3001 {
        w.send(&Message::Request(Request::new(
            Id::Num(req_id),
            m::SessionSubscribe::NAME,
            json!({"session_id": s, "after_seq": next - 1}),
        )))
        .await
        .unwrap();
        req_id += 1;
        while next <= 3001 {
            let msg = tokio::time::timeout(WAIT, r.next())
                .await
                .unwrap()
                .unwrap()
                .unwrap();
            let Message::Notification(n) = msg else {
                continue;
            };
            if n.method == m::Event::NAME {
                assert_eq!(n.params["seq"].as_u64().unwrap(), next);
                next += 1;
            } else if n.method == m::SessionResyncRequired::NAME {
                assert_eq!(n.params["from_seq"].as_u64().unwrap(), next - 1);
                resyncs += 1;
                break;
            }
        }
    }
    assert!(resyncs >= 1);
}
