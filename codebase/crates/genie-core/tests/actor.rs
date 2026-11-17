//! Session actor behaviour (01 §5) against a scripted runner.

#![allow(clippy::unwrap_used, clippy::expect_used)] // test code

use std::sync::{Arc, Mutex};
use std::time::Duration;

use futures::future::BoxFuture;
use genie_core::bus::{CoreEvent, SubItem};
use genie_core::journal::{Journal, MemoryJournal, TurnRecord};
use genie_core::session::{SessionHandle, TurnCtx, TurnRunner};
use genie_core::{EngineBuilder, TaskScope, TurnError};
use genie_protocol::handshake::AskKind;
use genie_protocol::session::{Delivery, LiveStatus, TurnFinish};
use serde_json::json;

#[derive(Default, Clone)]
struct Log(Arc<Mutex<Vec<String>>>);

impl Log {
    fn push(&self, s: impl Into<String>) {
        self.0.lock().unwrap().push(s.into());
    }
    fn get(&self) -> Vec<String> {
        self.0.lock().unwrap().clone()
    }
}

/// Behaviour by prompt text: `steps:N`, `block`, `ask`, `panic`, `fail`, anything else completes.
struct Scripted(Log);

impl TurnRunner for Scripted {
    fn run(&self, ctx: TurnCtx) -> BoxFuture<'static, Result<(), TurnError>> {
        let log = self.0.clone();
        Box::pin(async move {
            let text = ctx.input.text.clone();
            log.push(if ctx.resume {
                format!("resume:{}", ctx.turn_id)
            } else {
                format!("start:{text}")
            });
            if let Some(n) = text.strip_prefix("steps:") {
                let n: u32 = n.parse().unwrap();
                for _ in 0..n {
                    tokio::time::sleep(Duration::from_millis(60)).await;
                    for item in ctx.step_boundary() {
                        log.push(format!("steer:{}", item.text));
                    }
                }
            } else if text == "block" {
                ctx.token().cancelled().await;
                return Err(TurnError::Cancelled);
            } else if text == "ask" {
                let answer = ctx.ask(AskKind::Permission).await?;
                log.push(format!("answer:{answer}"));
            } else if text == "panic" {
                panic!("boom");
            } else if text == "fail" {
                return Err(TurnError::MaxSteps);
            }
            log.push(format!("end:{text}"));
            Ok(())
        })
    }
}

async fn wait_for(h: &SessionHandle, want: LiveStatus) {
    let mut rx = h.watch_status();
    tokio::time::timeout(Duration::from_secs(5), rx.wait_for(|s| *s == want))
        .await
        .unwrap_or_else(|_| panic!("timed out waiting for {want:?}, now {:?}", h.status()))
        .unwrap();
}

fn engine(log: &Log, journal: Arc<dyn Journal>) -> (TaskScope, genie_core::Engine) {
    let root = TaskScope::root();
    let e = EngineBuilder::new()
        .turn_runner(Arc::new(Scripted(log.clone())))
        .journal(journal)
        .build(&root);
    (root, e)
}

#[tokio::test]
async fn steer_is_injected_at_step_boundary_and_queue_waits() {
    let log = Log::default();
    let (_root, e) = engine(&log, Arc::new(MemoryJournal::new()));
    let s = e.create_session();
    s.prompt("steps:3", Delivery::Steer).unwrap();
    wait_for(&s, LiveStatus::Running).await;
    s.prompt("later", Delivery::Queue).unwrap();
    s.prompt("nudge", Delivery::Steer).unwrap();
    // Wait until the queued turn has completed.
    tokio::time::timeout(Duration::from_secs(5), async {
        while !log.get().contains(&"end:later".to_string()) {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    assert_eq!(
        log.get(),
        [
            "start:steps:3",
            "steer:nudge",
            "end:steps:3",
            "start:later",
            "end:later"
        ]
    );
    wait_for(&s, LiveStatus::Idle).await;
}

#[tokio::test]
async fn queued_items_can_be_removed() {
    let log = Log::default();
    let (_root, e) = engine(&log, Arc::new(MemoryJournal::new()));
    let s = e.create_session();
    s.prompt("block", Delivery::Steer).unwrap();
    wait_for(&s, LiveStatus::Running).await;
    let id = s.prompt("never", Delivery::Queue).unwrap();
    s.remove_queued(&id).unwrap();
    assert!(s.remove_queued(&id).is_err());
    assert!(s.interrupt());
    wait_for(&s, LiveStatus::Idle).await;
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert!(!log.get().iter().any(|l| l.contains("never")));
}

#[tokio::test]
async fn interrupt_returns_immediately_and_cancels_asks() {
    let log = Log::default();
    let journal = Arc::new(MemoryJournal::new());
    let (_root, e) = engine(&log, journal.clone());
    let s = e.create_session();
    let _attached = s.attach();
    let mut sub = e.bus().subscribe(s.id(), 0).unwrap();
    s.prompt("ask", Delivery::Steer).unwrap();
    wait_for(&s, LiveStatus::AwaitingAsk).await;
    assert_eq!(e.busy().pending_asks, 1);

    assert!(s.interrupt());
    assert!(matches!(
        s.status(),
        LiveStatus::Interrupting | LiveStatus::Idle
    ));
    wait_for(&s, LiveStatus::Idle).await;
    assert!(!s.interrupt(), "nothing left to interrupt");
    assert_eq!(e.busy(), Default::default());

    let mut saw_cancelled_ask = false;
    let mut finish = None;
    while let Ok(Some(item)) = tokio::time::timeout(Duration::from_millis(200), sub.next()).await {
        if let SubItem::Event(env) = item {
            match env.event {
                CoreEvent::AskResolved { outcome, .. } => {
                    saw_cancelled_ask = outcome == genie_core::bus::AskOutcome::Cancelled
                }
                CoreEvent::TurnSettled { finish: f, .. } => finish = Some(f),
                _ => {}
            }
        }
    }
    assert!(saw_cancelled_ask);
    assert_eq!(finish, Some(TurnFinish::Cancelled));
    assert!(matches!(
        journal.last_turn(s.id()).unwrap(),
        Some(TurnRecord::Settled {
            finish: TurnFinish::Cancelled,
            ..
        })
    ));
}

#[tokio::test]
async fn ask_without_client_pauses_and_any_client_can_answer() {
    let log = Log::default();
    let (_root, e) = engine(&log, Arc::new(MemoryJournal::new()));
    let s = e.create_session();
    let mut sub = e.bus().subscribe(s.id(), 0).unwrap();
    s.prompt("ask", Delivery::Steer).unwrap();
    wait_for(&s, LiveStatus::Paused).await;
    let guard = s.attach();
    assert_eq!(s.status(), LiveStatus::AwaitingAsk);

    let ask_id = loop {
        if let Some(SubItem::Event(env)) = sub.next().await
            && let CoreEvent::AskCreated { ask_id, .. } = env.event
        {
            break ask_id;
        }
    };
    drop(guard);
    s.answer_ask(&ask_id, json!({"decision": "allow"})).unwrap();
    assert!(
        s.answer_ask(&ask_id, json!(null)).is_err(),
        "second answer is rejected"
    );
    wait_for(&s, LiveStatus::Idle).await;
    assert_eq!(
        log.get(),
        ["start:ask", r#"answer:{"decision":"allow"}"#, "end:ask"]
    );
}

#[tokio::test]
async fn panic_fails_only_that_turn() {
    let log = Log::default();
    let journal = Arc::new(MemoryJournal::new());
    let (_root, e) = engine(&log, journal.clone());
    let a = e.create_session();
    let b = e.create_session();
    b.prompt("steps:2", Delivery::Steer).unwrap();
    a.prompt("panic", Delivery::Steer).unwrap();
    a.prompt("after", Delivery::Queue).unwrap();
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let l = log.get();
            if l.contains(&"end:after".into()) && l.contains(&"end:steps:2".into()) {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    wait_for(&a, LiveStatus::Idle).await;
}

#[tokio::test]
async fn failed_turn_reports_code() {
    let log = Log::default();
    let (_root, e) = engine(&log, Arc::new(MemoryJournal::new()));
    let s = e.create_session();
    let mut sub = e.bus().subscribe(s.id(), 0).unwrap();
    s.prompt("fail", Delivery::Steer).unwrap();
    loop {
        if let Some(SubItem::Event(env)) = sub.next().await
            && let CoreEvent::TurnSettled {
                finish, error_code, ..
            } = env.event
        {
            assert_eq!(finish, TurnFinish::Failed);
            assert_eq!(error_code.as_deref(), Some("GENIE-TURN-MAXSTEPS"));
            break;
        }
    }
}

#[tokio::test]
async fn shutdown_interrupts_and_resume_is_manual() {
    let log = Log::default();
    let journal: Arc<MemoryJournal> = Arc::new(MemoryJournal::new());
    let (root, e) = engine(&log, journal.clone());
    let s = e.create_session();
    let id = s.id().to_string();
    s.prompt("block", Delivery::Steer).unwrap();
    wait_for(&s, LiveStatus::Running).await;
    e.shutdown();
    root.cancel();
    assert!(root.close_and_wait(Duration::from_secs(5)).await);
    let Some(TurnRecord::Settled {
        turn_id,
        finish: TurnFinish::Interrupted,
    }) = journal.last_turn(&id).unwrap()
    else {
        panic!("turn should be interrupted");
    };

    // "Restart": a new engine on the same journal.
    let (_root2, e2) = engine(&log, journal.clone());
    assert_eq!(
        e2.recover_on_start().unwrap(),
        0,
        "already settled by clean shutdown"
    );
    let s2 = e2.session(&id).unwrap();
    assert_eq!(s2.status(), LiveStatus::Interrupted);
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(
        log.get()
            .iter()
            .filter(|l| l.starts_with("resume:"))
            .count(),
        0,
        "no auto-resume"
    );
    s2.resume().unwrap();
    tokio::time::timeout(Duration::from_secs(5), async {
        while !log.get().contains(&format!("resume:{turn_id}")) {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    assert!(s2.resume().is_err());
}

#[tokio::test]
async fn crash_claims_become_interrupted_on_start() {
    let log = Log::default();
    let journal = Arc::new(MemoryJournal::new());
    journal.claim_turn("ses_crashed", "trn_1").unwrap();
    let (_root, e) = engine(&log, journal.clone());
    assert_eq!(e.recover_on_start().unwrap(), 1);
    let s = e.session("ses_crashed").unwrap();
    assert_eq!(s.status(), LiveStatus::Interrupted);
    // A new prompt runs; the old turn stays interrupted.
    s.prompt("hello", Delivery::Steer).unwrap();
    wait_for(&s, LiveStatus::Idle).await;
    assert!(s.resume().is_err());
    let events = journal.events_after("ses_crashed", 0).unwrap();
    assert!(matches!(events[0].event, CoreEvent::TurnInterrupted { .. }));
}

#[tokio::test]
async fn idle_actors_are_evicted() {
    let log = Log::default();
    let root = TaskScope::root();
    let e = EngineBuilder::new()
        .turn_runner(Arc::new(Scripted(log.clone())))
        .actor_idle_ttl(Duration::from_millis(50), Duration::from_millis(20))
        .build(&root);
    let s = e.create_session();
    let guard = s.attach();
    tokio::time::sleep(Duration::from_millis(150)).await;
    assert_eq!(e.evict_idle(), 0, "attached sessions stay");
    drop(guard);
    tokio::time::sleep(Duration::from_millis(150)).await;
    assert_eq!(e.evict_idle(), 0, "background job already evicted it");
    assert_eq!(e.sessions_running(), 0);
}

#[tokio::test]
async fn background_work_keeps_daemon_alive() {
    let log = Log::default();
    let (_root, e) = engine(&log, Arc::new(MemoryJournal::new()));
    assert!(!e.busy().keeps_alive());
    let job = e.background_job();
    let term = e.terminal();
    let b = e.busy();
    assert!(b.keeps_alive() && !b.has_turns());
    drop((job, term));
    assert!(!e.busy().keeps_alive());
}
