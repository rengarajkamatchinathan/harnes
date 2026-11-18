//! The terminal driver (07 §1): crossterm input, the ratatui inline viewport, the daemon
//! connection and effect execution. All decisions live in [`super::model`]; this file
//! only moves bytes, so it is exercised by hand rather than by unit tests.

use std::io::{IsTerminal, Stdout, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use anyhow::{Context, Result, bail};
use crossterm::event::{
    self, DisableBracketedPaste, EnableBracketedPaste, Event, KeyEventKind,
    KeyboardEnhancementFlags, PopKeyboardEnhancementFlags, PushKeyboardEnhancementFlags,
};
use crossterm::{execute, terminal};
use genie_client::{Action, Connection, Incoming, SessionTracker};
use genie_protocol::api::session::{
    InterruptParams, PromptParams, RespondParams, SessionIdParams, UpdateParams,
};
use genie_protocol::common::SessionKind;
use genie_protocol::event::{CancelRequest, LeaseLost};
use genie_protocol::handshake::ClientKind;
use genie_protocol::methods::{self as m, Method};
use genie_protocol::{Channel, RpcError};
use ratatui::backend::CrosstermBackend;
use ratatui::text::Line;
use ratatui::widgets::{Paragraph, Widget};
use ratatui::{Terminal, TerminalOptions, Viewport};
use serde_json::json;
use tokio::sync::mpsc;
use tokio::time::Instant;

use super::model::{App, ConnState, Effect, Msg};
use super::theme::Theme;
use super::view;
use crate::connect::{self, Link};
use crate::errors;
use crate::render;
use crate::session::{self, Spec, Target};

/// Deltas are batched this long before a redraw (t05).
const BATCH: Duration = Duration::from_millis(10);
const MAX_FILES: usize = 5000;

pub struct Options {
    pub channel: Channel,
    pub standalone: bool,
    pub target: Target,
    pub spec: Spec,
    pub cwd: PathBuf,
    pub prompt: Option<String>,
    pub bypass_allowed: bool,
    /// `genie chat open/new`: print the active branch first.
    pub print_history: bool,
    pub genie_home: Option<PathBuf>,
}

type Term = Terminal<CrosstermBackend<Stdout>>;

struct TermGuard {
    stop: Arc<AtomicBool>,
}

impl Drop for TermGuard {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        let mut out = std::io::stdout();
        let _ = execute!(out, PopKeyboardEnhancementFlags, DisableBracketedPaste);
        let _ = terminal::disable_raw_mode();
        let _ = writeln!(out);
    }
}

pub async fn run(opts: Options) -> Result<u8> {
    if !std::io::stdout().is_terminal() || !std::io::stdin().is_terminal() {
        bail!("the interactive TUI needs a terminal; use `genie -p` for headless runs");
    }
    let mut link = connect::connect(opts.channel, ClientKind::Cli, opts.standalone, false).await?;

    let target = match &opts.target {
        Target::Resume(id) if id.is_empty() => pick_session(&link.conn, opts.spec.kind).await?,
        t => t.clone(),
    };
    let opened = session::open(&link.conn, &target, &opts.spec, &opts.cwd).await?;
    let sid = opened.session_id.clone();

    // History: print the active branch path when the store provides it; otherwise
    // replay the event log from the start.
    let after = if opened.created {
        None
    } else {
        let got = link
            .conn
            .call::<m::SessionGet>(genie_protocol::api::session::GetParams {
                session_id: sid.clone(),
                leaf_id: None,
            })
            .await;
        match got {
            Ok(g) if !g.path.is_empty() => {
                for l in render::transcript(&sid, &g.path) {
                    println!("{l}");
                }
                Some(g.session.head_seq)
            }
            _ => None,
        }
    };
    if opts.print_history && opened.created {
        println!("New chat {sid}");
    }
    session::subscribe(&link.conn, &sid, after).await?;
    let mut tracker = Follow::new(&sid, after.unwrap_or(0));

    let mut app = App::new(
        Theme::detect(|k| std::env::var(k).ok()),
        link.info.instance_id.clone(),
    );
    app.bypass_allowed = opts.bypass_allowed;
    app.model.clone_from(&opened.info.model);
    app.effort.clone_from(&opened.info.effort);
    app.mode = opened.info.mode;
    app.update(Msg::Session {
        id: sid.clone(),
        kind: opened.info.summary.kind,
    });
    app.update(Msg::Files(list_files(&opts.cwd)));
    refresh_commands(&link.conn, &mut app, &sid).await;

    // Terminal setup.
    terminal::enable_raw_mode().context("raw mode")?;
    let stop = Arc::new(AtomicBool::new(false));
    let _guard = TermGuard { stop: stop.clone() };
    let mut out = std::io::stdout();
    let _ = execute!(
        out,
        EnableBracketedPaste,
        PushKeyboardEnhancementFlags(KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES)
    );
    let (w, _) = terminal::size().unwrap_or((80, 24));
    app.update(Msg::Width(w));
    let mut height = view::footer(&app).height();
    let mut term = new_term(height)?;

    // Input thread: crossterm's read blocks, so it lives on a plain thread.
    let (in_tx, mut in_rx) = mpsc::unbounded_channel::<Event>();
    {
        let stop = stop.clone();
        std::thread::spawn(move || {
            while !stop.load(Ordering::SeqCst) {
                if let Ok(true) = event::poll(Duration::from_millis(100))
                    && let Ok(ev) = event::read()
                    && in_tx.send(ev).is_err()
                {
                    break;
                }
            }
        });
    }

    if let Some(p) = opts.prompt.clone().filter(|p| !p.trim().is_empty()) {
        let fx = vec![Effect::Prompt {
            parts: vec![genie_protocol::message::InputPart::text(p)],
            delivery: genie_protocol::session::Delivery::Steer,
        }];
        exec(&mut link, &mut app, &mut tracker, fx, &opts).await;
    }
    redraw(&mut term, &mut app, &mut height)?;

    let mut redraw_at: Option<Instant> = None;
    loop {
        if app.exit {
            break;
        }
        tokio::select! {
            ev = in_rx.recv() => {
                let Some(ev) = ev else { break };
                let msg = match ev {
                    Event::Key(k) if k.kind != KeyEventKind::Release => Some(Msg::Key(k)),
                    Event::Paste(s) => Some(Msg::Paste(s)),
                    Event::Resize(w, _) => Some(Msg::Width(w)),
                    _ => None,
                };
                if let Some(msg) = msg {
                    let fx = app.update(msg);
                    exec(&mut link, &mut app, &mut tracker, fx, &opts).await;
                    // Keys redraw at once; only stream deltas are batched.
                    redraw(&mut term, &mut app, &mut height)?;
                }
            }
            inc = link.rx.recv() => {
                match inc {
                    Some(i) => {
                        on_incoming(&mut link, &mut app, &mut tracker, i).await;
                        redraw_at.get_or_insert_with(|| Instant::now() + BATCH);
                    }
                    None => {
                        app.update(Msg::Connection(ConnState::Reconnecting));
                        redraw(&mut term, &mut app, &mut height)?;
                        if opts.standalone {
                            app.update(Msg::Error("the standalone engine exited".into()));
                            break;
                        }
                        match reconnect(opts.channel, &mut tracker).await {
                            Ok(l) => {
                                link = l;
                                app.instance_id.clone_from(&link.info.instance_id);
                                app.update(Msg::Connection(ConnState::Connected));
                            }
                            Err(e) => {
                                app.update(Msg::Error(format!("could not reconnect: {e:#}")));
                                break;
                            }
                        }
                        redraw(&mut term, &mut app, &mut height)?;
                    }
                }
            }
            () = sleep_until(redraw_at) => {
                redraw_at = None;
                redraw(&mut term, &mut app, &mut height)?;
            }
        }
    }
    redraw(&mut term, &mut app, &mut height)?;
    let _ = term.clear();
    drop(term);
    link.shutdown().await;
    Ok(0)
}

async fn sleep_until(t: Option<Instant>) {
    match t {
        Some(t) => tokio::time::sleep_until(t).await,
        None => std::future::pending().await,
    }
}

fn new_term(height: u16) -> Result<Term> {
    Ok(Terminal::with_options(
        CrosstermBackend::new(std::io::stdout()),
        TerminalOptions {
            viewport: Viewport::Inline(height.max(1)),
        },
    )?)
}

/// Commit scrollback above the viewport, resize the viewport to the footer, draw it.
fn redraw(term: &mut Term, app: &mut App, height: &mut u16) -> Result<()> {
    let lines: Vec<Line<'static>> = app.take_scrollback();
    if !lines.is_empty() {
        let n = u16::try_from(lines.len()).unwrap_or(u16::MAX);
        term.insert_before(n, |buf| {
            Paragraph::new(lines).render(buf.area, buf);
        })?;
    }
    let footer = view::footer(app);
    let (_, rows) = terminal::size().unwrap_or((80, 24));
    let want = footer.height().min(rows.saturating_sub(1)).max(1);
    if want != *height {
        // The inline viewport has a fixed height: clear it and start a new one where it
        // began, so the footer grows and shrinks without touching scrollback.
        let top = term.get_frame().area().y;
        term.clear()?;
        term.set_cursor_position((0, top))?;
        *term = new_term(want)?;
        *height = want;
    }
    term.draw(|f| view::draw(f, &footer))?;
    Ok(())
}

async fn reconnect(channel: Channel, tracker: &mut Follow) -> Result<Link> {
    let mut delay = Duration::from_millis(200);
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        match connect::shared(channel, ClientKind::Cli, true).await {
            Ok(link) => {
                let sid = session_of(tracker);
                session::subscribe(&link.conn, &sid, Some(tracker.t.last_seq())).await?;
                return Ok(link);
            }
            Err(e) if Instant::now() >= deadline => return Err(e),
            Err(_) => {
                tokio::time::sleep(delay).await;
                delay = (delay * 2).min(Duration::from_secs(2));
            }
        }
    }
}

/// The followed session: its id and stream cursor.
struct Follow {
    sid: String,
    t: SessionTracker,
}

impl Follow {
    fn new(sid: &str, after: u64) -> Follow {
        Follow {
            sid: sid.to_string(),
            t: SessionTracker::new(sid, after),
        }
    }
}

fn session_of(f: &Follow) -> String {
    f.sid.clone()
}

async fn on_incoming(link: &mut Link, app: &mut App, tracker: &mut Follow, i: Incoming) {
    match i {
        Incoming::Notification(n) => {
            let name = n.method.as_str();
            if name == m::LeaseLostNote::NAME {
                if let Ok(l) = serde_json::from_value::<LeaseLost>(n.params) {
                    app.update(Msg::LeaseLost {
                        by: l.new_driver.and_then(|d| d.label),
                    });
                }
                return;
            }
            if name == m::CancelRequestNote::NAME {
                if let Ok(c) = serde_json::from_value::<CancelRequest>(n.params) {
                    app.update(Msg::AskWithdrawn(c.id));
                }
                return;
            }
            if name == m::CommandsChangedNote::NAME {
                let sid = session_of(tracker);
                refresh_commands(&link.conn, app, &sid).await;
                return;
            }
            if name == m::DaemonRestarting::NAME {
                app.update(Msg::Notice("Engine is restarting…".into()));
                return;
            }
            match tracker.t.on_notification(&n) {
                Action::Event(env) => {
                    app.update(Msg::Event(env));
                }
                Action::Resubscribe { after_seq } => {
                    let sid = session_of(tracker);
                    let _ = session::subscribe(&link.conn, &sid, Some(after_seq)).await;
                }
                Action::Reload => {
                    let sid = session_of(tracker);
                    if let Ok(info) = session::get(&link.conn, &sid).await {
                        tracker.t.reset_to(info.head_seq);
                        let _ = session::subscribe(&link.conn, &sid, Some(info.head_seq)).await;
                    }
                }
                Action::Synced | Action::Ignore => {}
            }
        }
        Incoming::Request(r) => {
            if r.method == m::PermissionAsk::NAME
                && let Ok(p) = serde_json::from_value::<genie_protocol::api::session::AskParams>(
                    r.params.clone(),
                )
            {
                app.update(Msg::AskRequest {
                    id: r.id,
                    ask: Box::new(p.ask),
                });
            } else {
                let _ = link
                    .conn
                    .respond(r.id, Err(RpcError::method_not_found(&r.method)));
            }
        }
    }
}

async fn exec(
    link: &mut Link,
    app: &mut App,
    tracker: &mut Follow,
    effects: Vec<Effect>,
    opts: &Options,
) {
    for fx in effects {
        let sid = app.session_id.clone().unwrap_or_default();
        let conn = &link.conn;
        match fx {
            Effect::Prompt { parts, delivery } => {
                let r = conn
                    .call::<m::SessionPrompt>(PromptParams {
                        session_id: sid,
                        parts,
                        delivery,
                        parent_id: None,
                        client_msg_id: session::client_msg_id(),
                    })
                    .await;
                if let Err(e) = r {
                    app.update(Msg::Error(errors::describe(&e)));
                }
            }
            Effect::Interrupt => {
                let _ = conn
                    .call::<m::SessionInterrupt>(InterruptParams {
                        session_id: sid,
                        stop_subagents: false,
                    })
                    .await;
            }
            Effect::Update {
                model,
                effort,
                mode,
            } => {
                let r = conn
                    .call::<m::SessionUpdate>(UpdateParams {
                        session_id: sid,
                        model,
                        effort,
                        mode,
                        ..Default::default()
                    })
                    .await;
                if let Err(e) = r {
                    app.update(Msg::Error(errors::describe(&e)));
                }
            }
            Effect::Answer {
                ask_id,
                request,
                answer,
            } => {
                let r = match request {
                    Some(id) => conn
                        .respond(id, Ok(serde_json::to_value(&answer).unwrap_or_default()))
                        .map_err(|e| errors::describe(&e)),
                    None => conn
                        .call::<m::PermissionRespond>(RespondParams { ask_id, answer })
                        .await
                        .map(|_| ())
                        .map_err(|e| errors::describe(&e)),
                };
                if let Err(e) = r {
                    app.update(Msg::Error(e));
                }
            }
            Effect::Call {
                label,
                method,
                params,
            } => {
                let result = conn
                    .request_value(method, params)
                    .await
                    .map_err(|e| errors::describe(&e));
                app.update(Msg::CallResult { label, result });
            }
            Effect::TakeOver => {
                if let Err(e) = conn
                    .call::<m::SessionTakeOver>(SessionIdParams { session_id: sid })
                    .await
                {
                    app.update(Msg::Error(errors::describe(&e)));
                }
            }
            Effect::NewSession => {
                let mut spec = opts.spec.clone();
                spec.kind = app.kind;
                spec.mode = Some(app.mode);
                spec.model = Some(app.model.clone()).filter(|m| !m.is_empty());
                switch(link, app, tracker, &Target::New, &spec, &opts.cwd).await;
            }
            Effect::SwitchSession(id) => {
                let spec = Spec {
                    kind: app.kind,
                    ..Spec::code()
                };
                switch(link, app, tracker, &Target::Resume(id), &spec, &opts.cwd).await;
            }
            Effect::ListAgents => {
                // The engine's catalog (built-in, user, trusted project, 06 §11).
                let cwd = Some(opts.cwd.display().to_string());
                if let Ok(r) = conn
                    .call::<m::AgentsList>(genie_protocol::api::config::CwdParams { cwd })
                    .await
                {
                    app.update(Msg::CallResult {
                        label: "agents".into(),
                        result: Ok(serde_json::to_value(r).unwrap_or_default()),
                    });
                    continue;
                }
                // Older engines: list the definition files.
                let mut names = vec![];
                for dir in agent_dirs(opts.genie_home.as_deref(), &opts.cwd) {
                    if let Ok(rd) = std::fs::read_dir(&dir) {
                        for e in rd.flatten() {
                            let p = e.path();
                            if p.extension().is_some_and(|x| x == "md") {
                                names.push(json!({
                                    "name": p.file_stem().map(|s| s.to_string_lossy().into_owned()),
                                    "path": p.to_string_lossy(),
                                }));
                            }
                        }
                    }
                }
                app.update(Msg::CallResult {
                    label: "agents".into(),
                    result: Ok(json!({ "agents": names })),
                });
            }
            Effect::Exit => {}
        }
    }
}

async fn switch(
    link: &mut Link,
    app: &mut App,
    tracker: &mut Follow,
    target: &Target,
    spec: &Spec,
    cwd: &Path,
) {
    let old = app.session_id.clone().unwrap_or_default();
    let opened = match session::open(&link.conn, target, spec, cwd).await {
        Ok(o) => o,
        Err(e) => {
            app.update(Msg::Error(format!("{e:#}")));
            return;
        }
    };
    let _ = link
        .conn
        .call::<m::SessionUnsubscribe>(SessionIdParams { session_id: old })
        .await;
    let sid = opened.session_id.clone();
    *tracker = Follow::new(&sid, 0);
    app.update(Msg::Session {
        id: sid.clone(),
        kind: opened.info.summary.kind,
    });
    app.model.clone_from(&opened.info.model);
    app.mode = opened.info.mode;
    refresh_commands(&link.conn, app, &sid).await;
    app.push_line(Line::from(format!("── session {sid} ──")));
    if let Err(e) = session::subscribe(&link.conn, &sid, None).await {
        app.update(Msg::Error(format!("{e:#}")));
    }
}

/// `--resume` without an id: a numbered picker on stdin, before raw mode.
async fn pick_session(conn: &Connection, kind: SessionKind) -> Result<Target> {
    let list = conn
        .call::<m::SessionList>(genie_protocol::api::session::ListParams {
            kind: Some(kind),
            limit: Some(15),
            ..Default::default()
        })
        .await
        .map_err(|e| anyhow::anyhow!(errors::describe(&e)))?;
    if list.items.is_empty() {
        eprintln!("No sessions to resume; starting a new one.");
        return Ok(Target::New);
    }
    for (i, s) in list.items.iter().enumerate() {
        eprintln!(
            "{:>3}. {}  {}  {}",
            i + 1,
            s.session_id,
            s.updated_at,
            s.title.as_deref().unwrap_or("(untitled)")
        );
    }
    eprint!("Resume which? [1] ");
    let mut line = String::new();
    std::io::stdin().read_line(&mut line)?;
    let n: usize = line.trim().parse().unwrap_or(1);
    let s = list
        .items
        .get(n.saturating_sub(1))
        .context("no such entry")?;
    Ok(Target::Resume(s.session_id.clone()))
}

/// Workspace files for `@` completion, honouring ignore rules.
pub fn list_files(root: &Path) -> Vec<String> {
    let mut out = vec![];
    for e in ignore::WalkBuilder::new(root)
        .hidden(true)
        .build()
        .flatten()
    {
        if e.file_type().is_some_and(|t| t.is_file())
            && let Ok(rel) = e.path().strip_prefix(root)
        {
            out.push(rel.to_string_lossy().replace('\\', "/"));
            if out.len() >= MAX_FILES {
                break;
            }
        }
    }
    out.sort();
    out
}

pub fn genie_home() -> Option<PathBuf> {
    std::env::var_os("GENIE_HOME")
        .map(PathBuf::from)
        .or_else(|| std::env::home_dir().map(|h| h.join(".genie")))
}

/// The `/` menu from the engine (`command.list`, 06 §9.1). An older engine without it
/// leaves the list empty, and the menu falls back to the local key table.
async fn refresh_commands(conn: &Connection, app: &mut App, sid: &str) {
    let params = genie_protocol::api::mcp::CommandListParams {
        session_id: Some(sid.to_string()),
        ..Default::default()
    };
    if let Ok(r) = conn.call::<m::CommandList>(params).await {
        app.update(Msg::Commands(r.commands));
    }
}

fn agent_dirs(home: Option<&Path>, cwd: &Path) -> Vec<PathBuf> {
    let mut v = vec![];
    if let Some(h) = home {
        v.push(h.join("agents"));
    }
    v.push(cwd.join(".genie/agents"));
    v
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    #[test]
    fn file_listing_honours_gitignore() {
        let d = tempfile::tempdir().unwrap();
        std::fs::create_dir(d.path().join(".git")).unwrap();
        std::fs::write(d.path().join(".gitignore"), "target/\n").unwrap();
        std::fs::create_dir_all(d.path().join("target")).unwrap();
        std::fs::write(d.path().join("target/x.o"), "").unwrap();
        std::fs::create_dir_all(d.path().join("src")).unwrap();
        std::fs::write(d.path().join("src/main.rs"), "").unwrap();
        assert_eq!(list_files(d.path()), ["src/main.rs"]);
    }
}
