//! Headless runs, `genie -p` (07 §4).
//!
//! The runner is a protocol client like any other: it opens a session, subscribes,
//! prompts and follows the event stream until the parent's final turn has settled and
//! every background subagent has reported (D47). Asks pause the run; `--on-ask deny`
//! and `--ask-timeout` are client-side policies (02 §7 step 4).
//!
//! Exit codes (§4.3): 0 success · 1 error · 2 interrupted · 3 approval timeout or an
//! `--on-ask deny` denial that ended the run.

use std::collections::{BTreeMap, HashSet};
use std::io::Write;
use std::path::PathBuf;
use std::time::Duration;

use anyhow::Result;
use genie_client::{Action, Connection, Incoming, SessionTracker};
use genie_protocol::api::session::{InterruptParams, PromptParams, RespondParams};
use genie_protocol::ask::{Ask, AskAnswer, Decision};
use genie_protocol::common::Usage;
use genie_protocol::event::{ErrorInfo, LeaseLost, SessionEvent, SessionEventEnvelope, TurnStatus};
use genie_protocol::message::{InputPart, Part};
use genie_protocol::methods::{self as m, Method};
use genie_protocol::session::Delivery;
use genie_protocol::{PROTOCOL_VERSION, RpcError};
use serde::Serialize;
use serde_json::{Value, json};
use tokio::sync::mpsc::UnboundedReceiver;
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;

use crate::args::{OnAsk, OutputFormat};
use crate::errors;
use crate::session::{self, Spec, Target};

pub const EXIT_OK: u8 = 0;
pub const EXIT_ERROR: u8 = 1;
pub const EXIT_INTERRUPTED: u8 = 2;
pub const EXIT_ASK: u8 = 3;

/// How long to wait for the turn to settle after we interrupt it.
const SETTLE_GRACE: Duration = Duration::from_secs(10);

#[derive(Debug, Clone)]
pub struct Options {
    pub prompt: String,
    pub format: OutputFormat,
    pub target: Target,
    pub spec: Spec,
    pub cwd: PathBuf,
    pub max_turns: Option<u32>,
    pub ask_timeout: Option<Duration>,
    pub on_ask: OnAsk,
    /// `CI=true`: print a hint about `--on-ask` / `--ask-timeout` when a run pauses.
    pub ci: bool,
    pub trust: bool,
    pub allow: Vec<String>,
    pub deny: Vec<String>,
    pub settings: Option<PathBuf>,
    pub add_dirs: Vec<PathBuf>,
}

impl Options {
    pub fn new(prompt: impl Into<String>) -> Options {
        Options {
            prompt: prompt.into(),
            format: OutputFormat::Text,
            target: Target::New,
            spec: Spec::code(),
            cwd: PathBuf::from("."),
            max_turns: None,
            ask_timeout: None,
            on_ask: OnAsk::Wait,
            ci: false,
            trust: false,
            allow: vec![],
            deny: vec![],
            settings: None,
            add_dirs: vec![],
        }
    }
}

/// One background subagent in the `json` / `result` output.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct SubagentReport {
    pub session_id: String,
    pub agent: String,
    pub description: String,
    pub status: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub summary: Option<String>,
    pub cost_usd: f64,
}

/// The run's outcome; also the body of the `json` output and the `result` line.
#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct Outcome {
    pub session_id: String,
    pub result: String,
    pub usage: Usage,
    pub cost_usd: f64,
    /// `success` | `error` | `interrupted` | `ask_timeout` | `ask_denied` | `max_turns`.
    pub status: String,
    pub num_turns: u32,
    pub subagents: Vec<SubagentReport>,
    pub is_error: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    pub exit_code: u8,
}

/// Where output goes. Generic so tests can capture it.
pub struct Sinks<'a> {
    pub out: &'a mut (dyn Write + Send),
    pub err: &'a mut (dyn Write + Send),
}

/// Run one headless prompt. `cancel` is Ctrl-C / SIGTERM: interrupt and exit 2.
pub async fn run(
    conn: &Connection,
    rx: &mut UnboundedReceiver<Incoming>,
    opts: &Options,
    sinks: Sinks<'_>,
    cancel: CancellationToken,
) -> Result<Outcome> {
    let Sinks { out, err } = sinks;
    notices(conn, opts, err).await;

    let mut spec = opts.spec.clone();
    if spec.overlay.is_none()
        && (!opts.allow.is_empty()
            || !opts.deny.is_empty()
            || opts.settings.is_some()
            || !opts.add_dirs.is_empty())
    {
        spec.overlay = Some(genie_protocol::api::session::SessionSettingsOverlay {
            allow: opts.allow.clone(),
            deny: opts.deny.clone(),
            settings_path: opts
                .settings
                .as_ref()
                .map(|p| opts.cwd.join(p).to_string_lossy().into_owned()),
            add_dirs: opts
                .add_dirs
                .iter()
                .map(|p| opts.cwd.join(p).to_string_lossy().into_owned())
                .collect(),
        });
    }
    let opened = match session::open(conn, &opts.target, &spec, &opts.cwd).await {
        Ok(o) => o,
        Err(e) => return Ok(fail_early(opts, out, err, "", &format!("{e:#}"))),
    };
    let sid = opened.session_id.clone();
    // A new session streams from its start (session.created …); a resumed one from now.
    let after = if opened.created {
        None
    } else {
        Some(opened.info.head_seq)
    };
    if let Err(e) = session::subscribe(conn, &sid, after).await {
        return Ok(fail_early(opts, out, err, &sid, &format!("{e:#}")));
    }
    let mut run = Run::new(sid.clone(), opts, after.unwrap_or(0));

    if opts.format == OutputFormat::StreamJson {
        // `tools.list` (older engines: method not found → empty list).
        let tools: Vec<String> = conn
            .call::<m::ToolsList>(genie_protocol::api::session::SessionIdParams {
                session_id: sid.clone(),
            })
            .await
            .map(|r| r.tools.into_iter().map(|t| t.name).collect())
            .unwrap_or_default();
        line(
            out,
            &json!({
                "type": "system/init",
                "genie_protocol": PROTOCOL_VERSION,
                "session_id": sid,
                "model": opened.info.model,
                "mode": opened.info.mode,
                "cwd": opts.cwd.to_string_lossy(),
                "tools": tools,
            }),
        );
    }

    let prompt = PromptParams {
        session_id: sid.clone(),
        parts: vec![InputPart::text(opts.prompt.clone())],
        delivery: Delivery::Steer,
        parent_id: None,
        client_msg_id: session::client_msg_id(),
    };
    // The user picked this session (new, --continue, --resume): take the lease if needed.
    let sent =
        session::with_takeover(conn, &sid, || conn.call::<m::SessionPrompt>(prompt.clone())).await;
    match sent {
        Ok(r) => run.prompt_message = Some(r.message_id),
        Err(e) => {
            let msg = errors::describe(&e);
            return Ok(fail_early(opts, out, err, &sid, &msg));
        }
    }

    let mut interrupt_sent: Option<Instant> = None;
    loop {
        if run.finished() {
            break;
        }
        let ask_deadline = run.next_ask_deadline();
        let settle_deadline = interrupt_sent.map(|t| t + SETTLE_GRACE);
        tokio::select! {
            () = cancel.cancelled(), if interrupt_sent.is_none() => {
                run.interrupted_by_user = true;
                interrupt(conn, &sid).await;
                interrupt_sent = Some(Instant::now());
            }
            () = sleep_until(ask_deadline) => {
                let expired = run.take_expired_asks();
                for ask_id in expired {
                    let _ = writeln!(err, "genie: approval timed out for {ask_id}; denying (--ask-timeout)");
                    respond(conn, &ask_id, "Denied: approval timed out (--ask-timeout)").await;
                }
                run.ask_timed_out = true;
                if interrupt_sent.is_none() {
                    interrupt(conn, &sid).await;
                    interrupt_sent = Some(Instant::now());
                }
            }
            () = sleep_until(settle_deadline) => {
                // The engine did not settle after an interrupt; stop waiting.
                run.gave_up = true;
                break;
            }
            msg = rx.recv() => {
                let Some(msg) = msg else {
                    run.lost_connection = true;
                    break;
                };
                match msg {
                    Incoming::Notification(n) => {
                        if n.method == m::LeaseLostNote::NAME {
                            if let Ok(l) = serde_json::from_value::<LeaseLost>(n.params.clone()) {
                                let who = l.new_driver.and_then(|d| d.label).unwrap_or_else(|| "another client".into());
                                let _ = writeln!(err, "genie: {who} took over this session; following it read-only");
                            }
                            continue;
                        }
                        match run.tracker.on_notification(&n) {
                            Action::Event(env) => {
                                if opts.format == OutputFormat::StreamJson {
                                    line(out, &*env);
                                }
                                for act in run.apply(&env) {
                                    match act {
                                        RunAction::Deny(ask_id) => {
                                            respond(conn, &ask_id, "Denied by --on-ask deny (headless run)").await;
                                        }
                                        RunAction::Waiting(ask) => {
                                            waiting_notice(err, &ask, opts);
                                        }
                                        RunAction::Interrupt => {
                                            if interrupt_sent.is_none() {
                                                interrupt(conn, &sid).await;
                                                interrupt_sent = Some(Instant::now());
                                            }
                                        }
                                    }
                                }
                            }
                            Action::Resubscribe { after_seq } => {
                                if session::subscribe(conn, &sid, Some(after_seq)).await.is_err() {
                                    run.lost_connection = true;
                                    break;
                                }
                            }
                            Action::Reload => {
                                match session::get(conn, &sid).await {
                                    Ok(info) => {
                                        run.tracker.reset_to(info.head_seq);
                                        if session::subscribe(conn, &sid, Some(info.head_seq)).await.is_err() {
                                            run.lost_connection = true;
                                            break;
                                        }
                                    }
                                    Err(_) => {
                                        run.lost_connection = true;
                                        break;
                                    }
                                }
                            }
                            Action::Synced | Action::Ignore => {}
                        }
                    }
                    Incoming::Request(r) => {
                        // We do not advertise ask kinds, so nothing should arrive here.
                        let _ = conn.respond(r.id, Err(RpcError::method_not_found(&r.method)));
                    }
                }
            }
        }
    }

    let outcome = run.outcome();
    emit(opts, out, err, &outcome);
    Ok(outcome)
}

async fn sleep_until(t: Option<Instant>) {
    match t {
        Some(t) => tokio::time::sleep_until(t).await,
        None => std::future::pending().await,
    }
}

async fn interrupt(conn: &Connection, sid: &str) {
    let _ = conn
        .call::<m::SessionInterrupt>(InterruptParams {
            session_id: sid.to_string(),
            stop_subagents: true,
        })
        .await;
}

async fn respond(conn: &Connection, ask_id: &str, message: &str) {
    let _ = conn
        .call::<m::PermissionRespond>(RespondParams {
            ask_id: ask_id.to_string(),
            answer: AskAnswer {
                decision: Decision::Reject,
                scope: None,
                message: Some(message.to_string()),
                data: None,
            },
        })
        .await;
}

/// One-line notices for flags the engine cannot honour yet, and the trust notice (D43).
async fn notices(conn: &Connection, opts: &Options, err: &mut (dyn Write + Send)) {
    let path = opts.cwd.to_string_lossy().into_owned();
    if opts.trust {
        let r = conn
            .call::<m::TrustSet>(genie_protocol::api::config::TrustSetParams {
                path,
                decision: genie_protocol::api::config::TrustDecision::Once,
            })
            .await;
        if let Err(e) = r
            && !errors::is_not_implemented(&e)
        {
            let _ = writeln!(err, "genie: --trust failed: {}", errors::describe(&e));
        }
    } else if let Ok(t) = conn
        .call::<m::TrustGet>(genie_protocol::api::config::PathParams { path })
        .await
        && !matches!(
            t.state,
            genie_protocol::api::config::TrustState::Trusted
                | genie_protocol::api::config::TrustState::Once
        )
    {
        let _ = writeln!(
            err,
            "genie: folder not trusted: project settings, MCP servers, hooks and project \
             skills were not loaded (use --trust)"
        );
    }
}

fn waiting_notice(err: &mut (dyn Write + Send), ask: &Ask, opts: &Options) {
    let what = crate::render::ask_summary(ask);
    let _ = writeln!(
        err,
        "genie: waiting for approval: {what}\n       answer from the desktop app or with \
         `genie approvals approve {}` / `genie approvals deny {}`",
        ask.id, ask.id
    );
    if opts.ci {
        let _ = writeln!(
            err,
            "genie: CI detected: pass --on-ask deny or --ask-timeout <duration> so runs never \
             wait for a person"
        );
    }
}

fn line<T: Serialize + ?Sized>(out: &mut (dyn Write + Send), v: &T) {
    if let Ok(s) = serde_json::to_string(v) {
        let _ = writeln!(out, "{s}");
        let _ = out.flush();
    }
}

fn fail_early(
    opts: &Options,
    out: &mut (dyn Write + Send),
    err: &mut (dyn Write + Send),
    sid: &str,
    msg: &str,
) -> Outcome {
    let o = Outcome {
        session_id: sid.to_string(),
        result: String::new(),
        usage: Usage::default(),
        cost_usd: 0.0,
        status: "error".into(),
        num_turns: 0,
        subagents: vec![],
        is_error: true,
        error: Some(msg.to_string()),
        exit_code: EXIT_ERROR,
    };
    if opts.format == OutputFormat::StreamJson && !sid.is_empty() {
        // `system/init` was not written yet for early failures; keep the stream shape.
        line(
            out,
            &json!({"type": "system/init", "genie_protocol": PROTOCOL_VERSION, "session_id": sid, "tools": []}),
        );
    }
    emit(opts, out, err, &o);
    o
}

fn emit(opts: &Options, out: &mut (dyn Write + Send), err: &mut (dyn Write + Send), o: &Outcome) {
    if let Some(e) = &o.error {
        let _ = writeln!(err, "genie: {e}");
    }
    match opts.format {
        OutputFormat::Text => {
            if !o.result.is_empty() {
                let _ = writeln!(out, "{}", o.result.trim_end());
            }
        }
        OutputFormat::Json => line(out, o),
        OutputFormat::StreamJson => {
            let mut v = serde_json::to_value(o).unwrap_or(Value::Null);
            if let Value::Object(map) = &mut v {
                map.insert("type".into(), Value::String("result".into()));
            }
            line(out, &v);
        }
    }
    let _ = out.flush();
}

enum RunAction {
    Deny(String),
    Waiting(Box<Ask>),
    Interrupt,
}

/// Pure state of one run, fed with events. Kept I/O-free for unit tests.
struct Run {
    sid: String,
    tracker: SessionTracker,
    on_ask: OnAsk,
    ask_timeout: Option<Duration>,
    max_turns: Option<u32>,
    prompt_message: Option<String>,
    /// Turns that belong to this run (ours and the follow-ups after subagents report).
    our_turn_seen: bool,
    turn_running: bool,
    /// Admitted messages that have not started a turn yet.
    admitted_waiting: u32,
    num_turns: u32,
    /// Text parts of the last parent turn, by part id, in order.
    text: Vec<(String, String)>,
    usage: Usage,
    cost_usd: f64,
    subagents: BTreeMap<String, SubagentReport>,
    running_subagents: HashSet<String>,
    pending_asks: BTreeMap<String, Instant>,
    last_status: Option<TurnStatus>,
    last_error: Option<ErrorInfo>,
    session_error: Option<String>,
    policy_denied: bool,
    ask_timed_out: bool,
    interrupted_by_user: bool,
    engine_interrupted: bool,
    max_turns_hit: bool,
    lost_connection: bool,
    gave_up: bool,
}

impl Run {
    fn new(sid: String, opts: &Options, after: u64) -> Run {
        Run {
            tracker: SessionTracker::new(&sid, after),
            sid,
            on_ask: opts.on_ask,
            ask_timeout: opts.ask_timeout,
            max_turns: opts.max_turns,
            prompt_message: None,
            our_turn_seen: false,
            turn_running: false,
            admitted_waiting: 0,
            num_turns: 0,
            text: vec![],
            usage: Usage::default(),
            cost_usd: 0.0,
            subagents: BTreeMap::new(),
            running_subagents: HashSet::new(),
            pending_asks: BTreeMap::new(),
            last_status: None,
            last_error: None,
            session_error: None,
            policy_denied: false,
            ask_timed_out: false,
            interrupted_by_user: false,
            engine_interrupted: false,
            max_turns_hit: false,
            lost_connection: false,
            gave_up: false,
        }
    }

    fn finished(&self) -> bool {
        if self.engine_interrupted {
            return true;
        }
        self.our_turn_seen
            && !self.turn_running
            && self.admitted_waiting == 0
            && self.running_subagents.is_empty()
            && self.last_status.is_some()
    }

    fn next_ask_deadline(&self) -> Option<Instant> {
        self.pending_asks.values().min().copied()
    }

    fn take_expired_asks(&mut self) -> Vec<String> {
        let now = Instant::now();
        let expired: Vec<String> = self
            .pending_asks
            .iter()
            .filter(|(_, d)| **d <= now)
            .map(|(k, _)| k.clone())
            .collect();
        for k in &expired {
            self.pending_asks.remove(k);
        }
        expired
    }

    fn apply(&mut self, env: &SessionEventEnvelope) -> Vec<RunAction> {
        let mut acts = vec![];
        if env.session_id != self.sid {
            return acts;
        }
        let from_child = env.subagent_of.is_some();
        match &env.event {
            // Admitted while a turn runs = steer injection; otherwise a turn will start for it.
            SessionEvent::MessageAdmitted(_)
                if self.prompt_message.is_some() && !self.turn_running =>
            {
                self.admitted_waiting += 1;
            }
            SessionEvent::TurnStarted(t) => {
                self.admitted_waiting = self.admitted_waiting.saturating_sub(1);
                if !self.our_turn_seen && self.prompt_message.as_deref() != Some(&t.message_id) {
                    // A turn that was already queued before our prompt; not ours.
                    return acts;
                }
                self.our_turn_seen = true;
                self.turn_running = true;
                self.num_turns += 1;
                self.text.clear();
                if let Some(max) = self.max_turns
                    && self.num_turns > max
                {
                    self.max_turns_hit = true;
                    acts.push(RunAction::Interrupt);
                }
            }
            SessionEvent::PartDelta(d) if !from_child => {
                if let Some(t) = &d.text {
                    match self.text.iter_mut().find(|(id, _)| *id == d.part_id) {
                        Some((_, s)) => s.push_str(t),
                        None => self.text.push((d.part_id.clone(), t.clone())),
                    }
                }
            }
            SessionEvent::PartEnded(p) if !from_child => {
                if let Part::Text { text, .. } = &p.part {
                    match self.text.iter_mut().find(|(id, _)| *id == p.part_id) {
                        Some((_, s)) => s.clone_from(text),
                        None => self.text.push((p.part_id.clone(), text.clone())),
                    }
                }
            }
            SessionEvent::StepEnded(s) => {
                add_usage(&mut self.usage, &s.usage);
                self.cost_usd += s.cost.usd;
            }
            SessionEvent::UsageUpdated(u) => {
                self.usage = u.session;
                self.cost_usd = u.cost.usd;
            }
            SessionEvent::SubagentStarted(s) => {
                self.running_subagents.insert(s.child_session_id.clone());
                self.subagents.insert(
                    s.child_session_id.clone(),
                    SubagentReport {
                        session_id: s.child_session_id.clone(),
                        agent: s.agent.clone(),
                        description: s.description.clone(),
                        status: "running".into(),
                        summary: None,
                        cost_usd: 0.0,
                    },
                );
            }
            SessionEvent::SubagentEnded(s) => {
                self.running_subagents.remove(&s.child_session_id);
                let r = self
                    .subagents
                    .entry(s.child_session_id.clone())
                    .or_insert_with(|| SubagentReport {
                        session_id: s.child_session_id.clone(),
                        agent: String::new(),
                        description: String::new(),
                        status: String::new(),
                        summary: None,
                        cost_usd: 0.0,
                    });
                r.status = turn_status_str(s.status).into();
                r.summary.clone_from(&s.summary);
                r.cost_usd = s.cost.usd;
            }
            SessionEvent::PermissionAsked(a) => {
                let ask = (*a.ask).clone();
                match self.on_ask {
                    OnAsk::Deny => {
                        self.policy_denied = true;
                        acts.push(RunAction::Deny(ask.id.clone()));
                    }
                    OnAsk::Wait => {
                        if let Some(t) = self.ask_timeout {
                            self.pending_asks.insert(ask.id.clone(), Instant::now() + t);
                        }
                        acts.push(RunAction::Waiting(Box::new(ask)));
                    }
                }
            }
            SessionEvent::PermissionResolved(r) => {
                self.pending_asks.remove(&r.ask_id);
            }
            SessionEvent::TurnSettled(t) if self.our_turn_seen => {
                self.turn_running = false;
                self.last_status = Some(t.status);
                self.last_error.clone_from(&t.error);
                // Asks of a settled turn are gone.
                self.pending_asks.clear();
            }
            SessionEvent::TurnInterrupted(_) if self.our_turn_seen => {
                self.engine_interrupted = true;
            }
            SessionEvent::SessionError(e) => {
                self.session_error = Some(format!("{} ({})", e.message, e.code));
            }
            _ => {}
        }
        acts
    }

    fn result_text(&self) -> String {
        self.text
            .iter()
            .map(|(_, t)| t.as_str())
            .collect::<Vec<_>>()
            .join("")
    }

    fn outcome(&self) -> Outcome {
        let (status, exit, error): (&str, u8, Option<String>) = if self.ask_timed_out {
            (
                "ask_timeout",
                EXIT_ASK,
                Some("an approval was not answered within --ask-timeout".into()),
            )
        } else if self.max_turns_hit {
            (
                "max_turns",
                EXIT_ERROR,
                Some("stopped after --max-turns turns".into()),
            )
        } else if self.interrupted_by_user {
            ("interrupted", EXIT_INTERRUPTED, None)
        } else if self.engine_interrupted {
            (
                "interrupted",
                EXIT_INTERRUPTED,
                Some("the engine stopped while the run was in progress".into()),
            )
        } else if self.lost_connection || self.gave_up {
            (
                "error",
                EXIT_ERROR,
                Some(errors::describe(&genie_client::ClientError::Closed)),
            )
        } else {
            match self.last_status {
                Some(TurnStatus::Completed) => ("success", EXIT_OK, None),
                _ if self.policy_denied => (
                    "ask_denied",
                    EXIT_ASK,
                    Some("an ask was denied by --on-ask deny and the run did not complete".into()),
                ),
                Some(TurnStatus::Cancelled) => ("interrupted", EXIT_INTERRUPTED, None),
                _ => (
                    "error",
                    EXIT_ERROR,
                    Some(
                        self.last_error
                            .as_ref()
                            .map(errors::turn_error)
                            .or_else(|| self.session_error.clone())
                            .unwrap_or_else(|| "the turn failed".into()),
                    ),
                ),
            }
        };
        Outcome {
            session_id: self.sid.clone(),
            result: self.result_text(),
            usage: self.usage,
            cost_usd: self.cost_usd,
            status: status.into(),
            num_turns: self.num_turns,
            subagents: self.subagents.values().cloned().collect(),
            is_error: exit != EXIT_OK,
            error,
            exit_code: exit,
        }
    }
}

fn add_usage(a: &mut Usage, b: &Usage) {
    a.input_tokens += b.input_tokens;
    a.output_tokens += b.output_tokens;
    a.cache_read_tokens += b.cache_read_tokens;
    a.cache_write_tokens += b.cache_write_tokens;
    a.reasoning_tokens += b.reasoning_tokens;
}

fn turn_status_str(s: TurnStatus) -> &'static str {
    match s {
        TurnStatus::Completed => "completed",
        TurnStatus::Failed => "failed",
        TurnStatus::Cancelled => "cancelled",
        TurnStatus::Unknown => "unknown",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use genie_protocol::event::{PartDelta, PartEnded, TurnSettled, TurnStarted};

    fn env(seq: Option<u64>, event: SessionEvent) -> SessionEventEnvelope {
        SessionEventEnvelope {
            session_id: "ses_1".into(),
            seq,
            ref_seq: None,
            ts: String::new(),
            turn_id: None,
            step: None,
            subagent_of: None,
            event,
        }
    }

    #[test]
    fn collects_final_text_and_finishes() {
        let opts = Options::new("hi");
        let mut r = Run::new("ses_1".into(), &opts, 0);
        r.prompt_message = Some("msg_1".into());
        r.apply(&env(
            Some(1),
            SessionEvent::TurnStarted(TurnStarted {
                turn_id: "t1".into(),
                message_id: "msg_1".into(),
                resume: false,
            }),
        ));
        r.apply(&env(
            None,
            SessionEvent::PartDelta(PartDelta {
                part_id: "p1".into(),
                text: Some("Hel".into()),
                json: None,
            }),
        ));
        assert_eq!(r.result_text(), "Hel");
        r.apply(&env(
            Some(2),
            SessionEvent::PartEnded(PartEnded {
                part_id: "p1".into(),
                part: Part::Text {
                    id: "p1".into(),
                    text: "Hello".into(),
                    citations: vec![],
                },
            }),
        ));
        assert!(!r.finished());
        r.apply(&env(
            Some(3),
            SessionEvent::TurnSettled(TurnSettled {
                turn_id: "t1".into(),
                status: TurnStatus::Completed,
                error: None,
            }),
        ));
        assert!(r.finished());
        let o = r.outcome();
        assert_eq!(o.result, "Hello");
        assert_eq!(o.exit_code, EXIT_OK);
        assert_eq!(o.num_turns, 1);
    }

    #[test]
    fn waits_for_background_subagents_d47() {
        use genie_protocol::common::Cost;
        use genie_protocol::event::{SubagentEnded, SubagentStarted};
        let opts = Options::new("hi");
        let mut r = Run::new("ses_1".into(), &opts, 0);
        r.prompt_message = Some("msg_1".into());
        r.apply(&env(
            Some(1),
            SessionEvent::TurnStarted(TurnStarted {
                turn_id: "t1".into(),
                message_id: "msg_1".into(),
                resume: false,
            }),
        ));
        r.apply(&env(
            Some(2),
            SessionEvent::SubagentStarted(SubagentStarted {
                child_session_id: "c".into(),
                agent: "a".into(),
                description: "d".into(),
            }),
        ));
        r.apply(&env(
            Some(3),
            SessionEvent::TurnSettled(TurnSettled {
                turn_id: "t1".into(),
                status: TurnStatus::Completed,
                error: None,
            }),
        ));
        assert!(!r.finished(), "a background subagent is still running");
        r.apply(&env(
            Some(4),
            SessionEvent::SubagentEnded(SubagentEnded {
                child_session_id: "c".into(),
                status: TurnStatus::Completed,
                summary: None,
                cost: Cost { usd: 0.0 },
            }),
        ));
        assert!(r.finished());
        // If its report is admitted as a new message, wait for that follow-up turn.
        r.apply(&env(
            Some(5),
            SessionEvent::MessageAdmitted(genie_protocol::event::MessageAdmitted {
                message_id: "msg_2".into(),
                parent_id: None,
                role: genie_protocol::message::Role::User,
                parts: vec![],
                delivery: genie_protocol::session::Delivery::Queue,
                source: genie_protocol::event::MessageSource::Synthetic,
            }),
        ));
        assert!(!r.finished());
        r.apply(&env(
            Some(6),
            SessionEvent::TurnStarted(TurnStarted {
                turn_id: "t2".into(),
                message_id: "msg_2".into(),
                resume: false,
            }),
        ));
        r.apply(&env(
            Some(7),
            SessionEvent::TurnSettled(TurnSettled {
                turn_id: "t2".into(),
                status: TurnStatus::Completed,
                error: None,
            }),
        ));
        assert!(r.finished());
        assert_eq!(r.outcome().num_turns, 2);
    }

    #[test]
    fn foreign_turn_before_ours_is_ignored() {
        let opts = Options::new("hi");
        let mut r = Run::new("ses_1".into(), &opts, 0);
        r.prompt_message = Some("msg_2".into());
        r.apply(&env(
            Some(1),
            SessionEvent::TurnStarted(TurnStarted {
                turn_id: "t0".into(),
                message_id: "msg_0".into(),
                resume: false,
            }),
        ));
        r.apply(&env(
            Some(2),
            SessionEvent::TurnSettled(TurnSettled {
                turn_id: "t0".into(),
                status: TurnStatus::Completed,
                error: None,
            }),
        ));
        assert!(!r.finished());
    }
}
