//! Headless end to end (07 §4): a real daemon in-process over the in-memory transport,
//! a fake turn runner, and `genie_cli::headless::run` as the client. Checks stream-json
//! framing, output formats, exit codes and ask handling.

#![allow(clippy::unwrap_used, clippy::expect_used)] // test code

use std::sync::Arc;
use std::time::Duration;

use futures::future::BoxFuture;
use genie_cli::args::{OnAsk, OutputFormat};
use genie_cli::connect::{Link, client_info};
use genie_cli::headless::{self, Options, Outcome, Sinks};
use genie_client::Endpoint;
use genie_client::connection::{ClientInfo, Connection};
use genie_core::{EngineBuilder, TaskScope, TurnCtx, TurnError, TurnRunner};
use genie_daemon::{DaemonConfig, DaemonState, serve_stream};
use genie_protocol::api::session::{PendingParams, RespondParams};
use genie_protocol::ask::{Ask, AskAnswer, AskKind, Decision, Preview};
use genie_protocol::common::{Cost, Usage};
use genie_protocol::event::{
    PartEnded, PartStarted, SessionEvent, StepEnded, StepFinish, SubagentEnded, SubagentStarted,
    TurnStatus,
};
use genie_protocol::handshake::ClientKind;
use genie_protocol::message::{Part, PartKind};
use genie_protocol::methods as m;
use genie_protocol::{Channel, PROTOCOL_VERSION};
use serde_json::Value;
use tokio_util::sync::CancellationToken;

const TOKEN: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

fn text_part(ctx: &TurnCtx, id: &str, chunks: &[&str]) -> Result<(), TurnError> {
    ctx.publish(SessionEvent::PartStarted(PartStarted {
        message_id: "msg_a".into(),
        part_id: id.into(),
        kind: PartKind::Text,
    }))?;
    for c in chunks {
        ctx.emit_part_delta(id, *c);
    }
    ctx.publish(SessionEvent::PartEnded(PartEnded {
        part_id: id.into(),
        part: Part::Text {
            id: id.into(),
            text: chunks.concat(),
            citations: vec![],
        },
    }))?;
    Ok(())
}

fn step_end(ctx: &TurnCtx) -> Result<(), TurnError> {
    ctx.publish(SessionEvent::StepEnded(StepEnded {
        finish: StepFinish::Stop,
        usage: Usage {
            input_tokens: 10,
            output_tokens: 5,
            ..Default::default()
        },
        cost: Cost { usd: 0.25 },
    }))?;
    Ok(())
}

/// Behaviour by prompt text.
struct Runner;

impl TurnRunner for Runner {
    fn run(&self, ctx: TurnCtx) -> BoxFuture<'static, Result<(), TurnError>> {
        Box::pin(async move {
            match ctx.input.text.as_str() {
                "noprovider" => Err(TurnError::Provider {
                    class: "no_provider_configured".into(),
                    fix_link: None,
                }),
                "block" => {
                    ctx.token().cancelled().await;
                    Err(TurnError::Cancelled)
                }
                p if p.starts_with("ask") => {
                    let mut a = Ask::new("", "", AskKind::Permission, "");
                    a.tool = Some("shell".into());
                    a.preview = Some(Preview::Command {
                        command: "rm -rf build".into(),
                        cwd: None,
                    });
                    let ans = ctx.ask_with(a).await?;
                    let ans: AskAnswer = serde_json::from_value(ans).unwrap();
                    // `ask-strict`: a denial ends the run; `ask`: the model carries on.
                    if ans.decision == Decision::Reject && p == "ask-strict" {
                        return Err(TurnError::Provider {
                            class: "declined".into(),
                            fix_link: None,
                        });
                    }
                    text_part(&ctx, "prt_ask", &["after ask: ", ans_word(ans.decision)])?;
                    step_end(&ctx)
                }
                "subagent" => {
                    ctx.publish(SessionEvent::SubagentStarted(SubagentStarted {
                        child_session_id: "ses_child".into(),
                        agent: "explorer".into(),
                        description: "look around".into(),
                    }))?;
                    tokio::time::sleep(Duration::from_millis(50)).await;
                    ctx.publish(SessionEvent::SubagentEnded(SubagentEnded {
                        child_session_id: "ses_child".into(),
                        status: TurnStatus::Completed,
                        summary: Some("found it".into()),
                        cost: Cost { usd: 0.5 },
                    }))?;
                    text_part(&ctx, "prt_s", &["parent done"])?;
                    step_end(&ctx)
                }
                _ => {
                    text_part(&ctx, "prt_1", &["Hello", " from", " genie"])?;
                    step_end(&ctx)
                }
            }
        })
    }
}

fn ans_word(d: Decision) -> &'static str {
    match d {
        Decision::Once | Decision::Always => "allowed",
        _ => "denied",
    }
}

struct Mem {
    state: Arc<DaemonState>,
    _dir: tempfile::TempDir,
}

fn mem_daemon() -> Mem {
    let dir = tempfile::tempdir().unwrap();
    let mut cfg = DaemonConfig::new(Endpoint::in_dir(dir.path(), Channel::Dev).unwrap());
    cfg.engine_version = "1.0.0".into();
    cfg.write_logs = false;
    let root = TaskScope::root();
    let engine = EngineBuilder::new()
        .turn_runner(Arc::new(Runner))
        .build(&root);
    Mem {
        state: Arc::new(DaemonState::new(cfg, engine, TOKEN.into(), root)),
        _dir: dir,
    }
}

async fn connect(m: &Mem, info: ClientInfo) -> Link {
    let (client, server) = genie_testkit::memory_pair();
    let st = m.state.clone();
    tokio::spawn(async move { serve_stream(st, Box::new(server), TaskScope::root()).await });
    let conn = Connection::handshake(Box::new(client), &info, TOKEN.into())
        .await
        .unwrap();
    Link::from_connection(conn, info).unwrap()
}

struct Ran {
    outcome: Outcome,
    out: String,
    err: String,
}

async fn run_with(mem: &Mem, opts: Options, cancel: CancellationToken) -> Ran {
    let mut link = connect(mem, client_info(ClientKind::Headless)).await;
    let mut out = Vec::new();
    let mut err = Vec::new();
    let outcome = tokio::time::timeout(
        Duration::from_secs(10),
        headless::run(
            &link.conn,
            &mut link.rx,
            &opts,
            Sinks {
                out: &mut out,
                err: &mut err,
            },
            cancel,
        ),
    )
    .await
    .expect("headless run timed out")
    .unwrap();
    Ran {
        outcome,
        out: String::from_utf8(out).unwrap(),
        err: String::from_utf8(err).unwrap(),
    }
}

fn opts(prompt: &str, format: OutputFormat) -> Options {
    let mut o = Options::new(prompt);
    o.format = format;
    o.spec.model = Some("test/model".into());
    o
}

#[tokio::test]
async fn text_output_is_the_final_answer() {
    let mem = mem_daemon();
    let r = run_with(
        &mem,
        opts("hi", OutputFormat::Text),
        CancellationToken::new(),
    )
    .await;
    assert_eq!(r.outcome.exit_code, 0, "stderr: {}", r.err);
    assert_eq!(r.out, "Hello from genie\n");
    assert_eq!(r.outcome.num_turns, 1);
}

#[tokio::test]
async fn stream_json_has_init_events_and_result() {
    let mem = mem_daemon();
    let r = run_with(
        &mem,
        opts("hi", OutputFormat::StreamJson),
        CancellationToken::new(),
    )
    .await;
    let lines: Vec<Value> = r
        .out
        .lines()
        .map(|l| serde_json::from_str(l).expect("every line is JSON"))
        .collect();
    let first = &lines[0];
    assert_eq!(first["type"], "system/init");
    assert_eq!(first["genie_protocol"], PROTOCOL_VERSION);
    assert_eq!(first["model"], "test/model");
    assert!(first["tools"].is_array());
    let sid = first["session_id"].as_str().unwrap().to_string();

    let last = lines.last().unwrap();
    assert_eq!(last["type"], "result");
    assert_eq!(last["status"], "success");
    assert_eq!(last["result"], "Hello from genie");
    assert_eq!(last["session_id"], sid.as_str());
    assert_eq!(last["exit_code"], 0);

    // Between them: D22 envelopes, durable seqs gap-free from 1.
    let events = &lines[1..lines.len() - 1];
    let seqs: Vec<u64> = events.iter().filter_map(|e| e["seq"].as_u64()).collect();
    assert_eq!(seqs, (1..=seqs.len() as u64).collect::<Vec<_>>());
    let types: Vec<&str> = events
        .iter()
        .map(|e| e["event"]["type"].as_str().unwrap())
        .collect();
    assert_eq!(types[0], "session.created");
    for t in [
        "message.admitted",
        "turn.started",
        "part.delta",
        "part.ended",
        "turn.settled",
    ] {
        assert!(types.contains(&t), "missing {t}: {types:?}");
    }
    assert!(events.iter().all(|e| e["session_id"] == sid.as_str()));
}

#[tokio::test]
async fn json_output_shape() {
    let mem = mem_daemon();
    let r = run_with(
        &mem,
        opts("subagent", OutputFormat::Json),
        CancellationToken::new(),
    )
    .await;
    let v: Value = serde_json::from_str(r.out.trim()).unwrap();
    for k in [
        "session_id",
        "result",
        "usage",
        "cost_usd",
        "status",
        "num_turns",
        "subagents",
    ] {
        assert!(v.get(k).is_some(), "missing {k} in {v}");
    }
    assert_eq!(v["result"], "parent done");
    assert_eq!(v["usage"]["input_tokens"], 10);
    assert_eq!(v["cost_usd"], 0.25);
    assert_eq!(v["subagents"][0]["agent"], "explorer");
    assert_eq!(v["subagents"][0]["status"], "completed");
    assert_eq!(v["subagents"][0]["summary"], "found it");
}

#[tokio::test]
async fn no_provider_is_exit_1_with_a_readable_error() {
    let mem = mem_daemon();
    let r = run_with(
        &mem,
        opts("noprovider", OutputFormat::Text),
        CancellationToken::new(),
    )
    .await;
    assert_eq!(r.outcome.exit_code, 1);
    assert_eq!(r.outcome.status, "error");
    assert!(r.err.contains("genie login"), "stderr: {}", r.err);
    assert!(r.out.is_empty());
}

#[tokio::test]
async fn interrupt_is_exit_2() {
    let mem = mem_daemon();
    let cancel = CancellationToken::new();
    let c = cancel.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(200)).await;
        c.cancel();
    });
    let r = run_with(&mem, opts("block", OutputFormat::Json), cancel).await;
    assert_eq!(r.outcome.exit_code, 2, "{:?}", r.outcome);
    assert_eq!(r.outcome.status, "interrupted");
}

#[tokio::test]
async fn on_ask_deny_that_ends_the_run_is_exit_3() {
    let mem = mem_daemon();
    let mut o = opts("ask-strict", OutputFormat::Text);
    o.on_ask = OnAsk::Deny;
    let r = run_with(&mem, o, CancellationToken::new()).await;
    assert_eq!(r.outcome.exit_code, 3, "{:?}", r.outcome);
    assert_eq!(r.outcome.status, "ask_denied");
}

#[tokio::test]
async fn on_ask_deny_the_model_recovers_from_is_success() {
    let mem = mem_daemon();
    let mut o = opts("ask", OutputFormat::Text);
    o.on_ask = OnAsk::Deny;
    let r = run_with(&mem, o, CancellationToken::new()).await;
    assert_eq!(r.outcome.exit_code, 0, "{:?}", r.outcome);
    assert_eq!(r.out, "after ask: denied\n");
}

#[tokio::test]
async fn ask_timeout_is_exit_3_and_ci_hint() {
    let mem = mem_daemon();
    let mut o = opts("ask", OutputFormat::Text);
    o.ask_timeout = Some(Duration::from_millis(200));
    o.ci = true;
    let r = run_with(&mem, o, CancellationToken::new()).await;
    assert_eq!(r.outcome.exit_code, 3, "{:?}", r.outcome);
    assert_eq!(r.outcome.status, "ask_timeout");
    assert!(
        r.err.contains("waiting for approval: shell: rm -rf build"),
        "{}",
        r.err
    );
    assert!(r.err.contains("genie approvals approve ask_"), "{}", r.err);
    assert!(r.err.contains("--on-ask deny"), "CI hint: {}", r.err);
}

#[tokio::test]
async fn ask_answered_from_another_client() {
    let mem = mem_daemon();
    // A second client (think `genie approvals approve`) answers the pending ask.
    let other = connect(&mem, client_info(ClientKind::Cli)).await;
    let approver = tokio::spawn(async move {
        loop {
            let pending = other
                .conn
                .call::<m::PermissionPending>(PendingParams::default())
                .await
                .unwrap();
            if let Some(a) = pending.asks.first() {
                other
                    .conn
                    .call::<m::PermissionRespond>(RespondParams {
                        ask_id: a.id.clone(),
                        answer: AskAnswer::decision(Decision::Once),
                    })
                    .await
                    .unwrap();
                return;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    });
    let r = run_with(
        &mem,
        opts("ask", OutputFormat::Text),
        CancellationToken::new(),
    )
    .await;
    approver.await.unwrap();
    assert_eq!(r.outcome.exit_code, 0, "{:?} {}", r.outcome, r.err);
    assert_eq!(r.out, "after ask: allowed\n");
}

#[tokio::test]
async fn resume_streams_only_new_events() {
    let mem = mem_daemon();
    let first = run_with(
        &mem,
        opts("hi", OutputFormat::Json),
        CancellationToken::new(),
    )
    .await;
    let sid = first.outcome.session_id.clone();
    let mut o = opts("again", OutputFormat::StreamJson);
    o.target = genie_cli::session::Target::Resume(sid.clone());
    let r = run_with(&mem, o, CancellationToken::new()).await;
    assert_eq!(r.outcome.exit_code, 0, "{}", r.err);
    assert_eq!(r.outcome.session_id, sid);
    assert!(!r.out.contains("session.created"));
    // --continue picks the most recent Code session.
    let mut o = opts("third", OutputFormat::Json);
    o.target = genie_cli::session::Target::Continue;
    let r = run_with(&mem, o, CancellationToken::new()).await;
    assert_eq!(r.outcome.session_id, sid);
}

#[tokio::test]
async fn unknown_resume_id_is_exit_1() {
    let mem = mem_daemon();
    let mut o = opts("hi", OutputFormat::Text);
    o.target = genie_cli::session::Target::Resume("ses_nope".into());
    let r = run_with(&mem, o, CancellationToken::new()).await;
    assert_eq!(r.outcome.exit_code, 1);
    assert!(r.err.contains("ses_nope"), "{}", r.err);
}

// ---------------------------------------------------------------- subcommands (§2.2, §3)

fn sink() -> Vec<u8> {
    Vec::new()
}

#[tokio::test]
async fn approvals_list_and_approve_unblock_a_headless_run() {
    let mem = mem_daemon();
    let headless_run = {
        let m2 = Mem {
            state: mem.state.clone(),
            _dir: tempfile::tempdir().unwrap(),
        };
        tokio::spawn(async move {
            run_with(
                &m2,
                opts("ask", OutputFormat::Text),
                CancellationToken::new(),
            )
            .await
        })
    };
    let cli = connect(&mem, client_info(ClientKind::Cli)).await;
    // Wait until the ask is pending, as `genie approvals` shows it.
    let ask_id = loop {
        let (mut out, mut err) = (sink(), sink());
        let code = genie_cli::cmd::approvals(
            &cli.conn,
            Some(genie_cli::args::ApprovalsAction::List { json: false }),
            &mut out,
            &mut err,
        )
        .await;
        assert_eq!(code, 0);
        let text = String::from_utf8(out).unwrap();
        if let Some(line) = text.lines().find(|l| l.starts_with("ask_")) {
            assert!(line.contains("shell: rm -rf build"), "{line}");
            break line.split_whitespace().next().unwrap().to_string();
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    };
    let (mut out, mut err) = (sink(), sink());
    let code = genie_cli::cmd::approvals(
        &cli.conn,
        Some(genie_cli::args::ApprovalsAction::Approve {
            id: ask_id.clone(),
            always: false,
        }),
        &mut out,
        &mut err,
    )
    .await;
    assert_eq!(code, 0, "{}", String::from_utf8_lossy(&err));
    let r = headless_run.await.unwrap();
    assert_eq!(r.outcome.exit_code, 0);
    assert_eq!(r.out, "after ask: allowed\n");
    // Answering again is a clean error.
    let (mut out, mut err) = (sink(), sink());
    let code = genie_cli::cmd::approvals(
        &cli.conn,
        Some(genie_cli::args::ApprovalsAction::Deny {
            id: ask_id,
            message: None,
        }),
        &mut out,
        &mut err,
    )
    .await;
    assert_eq!(code, 1);
}

#[tokio::test]
async fn chat_list_sessions_and_unimplemented_methods() {
    let mem = mem_daemon();
    let cli = connect(&mem, client_info(ClientKind::Cli)).await;
    let (mut out, mut err) = (sink(), sink());
    assert_eq!(
        genie_cli::cmd::chat_list(&cli.conn, false, &mut out, &mut err).await,
        0
    );
    assert!(String::from_utf8(out).unwrap().contains("genie chat new"));
    // A chat created elsewhere (the desktop) shows up.
    cli.conn
        .call::<m::SessionCreate>(genie_protocol::api::session::CreateParams {
            kind: genie_protocol::common::SessionKind::Chat,
            project_id: None,
            workspace_path: None,
            model: "m".into(),
            effort: None,
            agent: None,
            mode: None,
            incognito: false,
            worktree: None,
        })
        .await
        .unwrap();
    let (mut out, mut err) = (sink(), sink());
    genie_cli::cmd::chat_list(&cli.conn, false, &mut out, &mut err).await;
    let text = String::from_utf8(out).unwrap();
    assert!(text.contains("(untitled)"), "{text}");
    // `genie sessions` defaults to Code sessions: the chat is not listed.
    let (mut out, mut err) = (sink(), sink());
    genie_cli::cmd::sessions(
        &cli.conn,
        Some(genie_protocol::common::SessionKind::Code),
        false,
        &mut out,
        &mut err,
    )
    .await;
    assert_eq!(String::from_utf8(out).unwrap(), "No sessions.\n");
    // Methods the daemon does not serve yet: one calm line, exit 1.
    let (mut out, mut err) = (sink(), sink());
    let code = genie_cli::cmd::mcp(
        &cli.conn,
        None,
        std::path::Path::new("."),
        &mut out,
        &mut err,
    )
    .await;
    assert_eq!(code, 1);
    assert_eq!(
        String::from_utf8(err).unwrap(),
        "genie: mcp list: not available in this engine build yet\n"
    );
}
