//! The browser service (one per daemon) and per-session state: tabs, refs, last snapshot
//! per tab, take-over control, per-chat site grants, per-turn taint and step caps, and
//! downloads. Agent actions of one session run one at a time ([`Op`]).

use std::collections::{HashMap, HashSet, VecDeque};
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex as StdMutex, Weak};
use std::time::{Duration, Instant};

use genie_protocol::api::browser::{
    BrowserStatus, ClearWhat, SiteState, TabActivateParams, TabInfo, TabsResult,
};
use genie_protocol::ask::{BrowserConfirm, Preview, SaveSuggestion};
use genie_protocol::common::{Mode, RuleScope, SessionKind};
use genie_protocol::event::{BrowserAction, SessionEvent};
use genie_tools::{PermissionRequest, ToolCtx, ToolError, ToolOutput};
use serde_json::{Value, json};
use tokio::sync::{OwnedMutexGuard, broadcast};
use tokio_util::task::TaskTracker;

use crate::backend::{BrowserBackend, BrowserError};
use crate::cdp::{CdpClient, CdpError, CdpEvent, str_at, wait_event};
use crate::host::{BrowserHost, ConfirmDecision, ConfirmRequest, Control};
use crate::site::{self, Category, Need, Provenance, SitePolicy, Source};
use crate::snapshot::{COMPUTED_STYLES, DIFF_CHARS, PageMeta, RefInfo, Snapshot, origin_of};

#[derive(Debug, Clone)]
pub struct BrowserConfig {
    /// Thumbnail after each `act` step in the audit record (research §4.5, default on).
    pub thumbnails: bool,
    /// Check in with the user after this many browser actions in one turn (06 §10).
    pub max_steps: u32,
    /// …or after this long.
    pub max_duration: Duration,
    /// Page load wait after navigation.
    pub load_timeout: Duration,
    /// Download quarantine (`<data dir>/browser/downloads`).
    pub downloads_dir: Option<PathBuf>,
}

impl Default for BrowserConfig {
    fn default() -> Self {
        BrowserConfig {
            thumbnails: true,
            max_steps: 50,
            max_duration: Duration::from_secs(15 * 60),
            load_timeout: Duration::from_secs(20),
            downloads_dir: None,
        }
    }
}

/// Every action's status (06 §10).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    Ok,
    Blocked,
    NeedsConfirmation,
    UserInControl,
    /// The result was lost; never retried.
    Unknown,
}

impl Status {
    pub fn as_str(self) -> &'static str {
        match self {
            Status::Ok => "ok",
            Status::Blocked => "blocked",
            Status::NeedsConfirmation => "needs_confirmation",
            Status::UserInControl => "user_in_control",
            Status::Unknown => "unknown",
        }
    }
}

/// What a browser tool returns to the model.
#[derive(Debug, Clone, PartialEq)]
pub struct Outcome {
    pub status: Status,
    pub tab_id: Option<String>,
    pub url: String,
    pub title: String,
    pub note: Option<String>,
    /// Page-derived text, framed as untrusted.
    pub body: Option<String>,
    pub origin: String,
}

impl Outcome {
    pub fn status(status: Status, note: impl Into<String>) -> Outcome {
        Outcome {
            status,
            tab_id: None,
            url: String::new(),
            title: String::new(),
            note: Some(note.into()),
            body: None,
            origin: String::new(),
        }
    }

    /// Plain text for the model. Page text is wrapped in `untrusted_web_content` with its
    /// origin (research §4.4 layer 1).
    pub fn to_text(&self) -> String {
        let mut s = format!("status: {}\n", self.status.as_str());
        if let Some(t) = &self.tab_id {
            s.push_str(&format!("tab: {t}\n"));
        }
        if !self.url.is_empty() {
            s.push_str(&format!("url: {}\n", self.url));
        }
        if !self.title.is_empty() {
            s.push_str(&format!("title: {}\n", self.title.replace('\n', " ")));
        }
        if let Some(n) = &self.note {
            s.push_str(&format!("note: {n}\n"));
        }
        if let Some(b) = &self.body {
            let b = b.replace("</untrusted_web_content", "&lt;/untrusted_web_content");
            s.push_str(&format!(
                "<untrusted_web_content origin=\"{}\">\n{}{}</untrusted_web_content>\n",
                self.origin,
                b,
                if b.ends_with('\n') { "" } else { "\n" }
            ));
        }
        s
    }
}

/// Early end of an action: an outcome for the model, or a tool error.
#[derive(Debug)]
pub enum Stop {
    Out(Box<Outcome>),
    Err(ToolError),
}

impl Stop {
    pub fn boxed(o: Outcome) -> Stop {
        Stop::Out(Box::new(o))
    }
}

impl From<ToolError> for Stop {
    fn from(e: ToolError) -> Self {
        Stop::Err(e)
    }
}

fn lost() -> Stop {
    Stop::boxed(Outcome::status(
        Status::Unknown,
        "the browser didn't answer; the action may or may not have happened. It was not \
         retried. Take a snapshot before continuing.",
    ))
}

/// Map a CDP failure during an action.
pub(crate) fn cdp_stop(e: CdpError) -> Stop {
    if e.result_lost() {
        lost()
    } else {
        Stop::Err(ToolError::failed(e.to_string()))
    }
}

/// A tab of one session.
#[derive(Debug, Clone, PartialEq)]
pub struct TabHandle {
    pub tab_id: String,
    pub target_id: String,
    pub cdp_session: String,
}

#[derive(Default)]
struct Shared {
    tabs: Vec<TabHandle>,
    active: Option<String>,
    control: Option<Control>,
    handed_back: bool,
    next_tab: u32,
    client: Option<CdpClient>,
}

#[derive(Default)]
struct TabState {
    refs: crate::snapshot::RefTable,
    last: Option<Snapshot>,
}

struct Turn {
    id: String,
    steps: u32,
    started: Instant,
    read_origins: Vec<String>,
    source: Option<String>,
}

#[derive(Default)]
pub(crate) struct Ops {
    tabs: HashMap<String, TabState>,
    /// "Allow for this chat" grants by site key.
    grants: HashMap<String, SiteState>,
    turn: Option<Turn>,
    /// `(origin, visible text)` of pages seen (exfiltration guard), newest last.
    seen: VecDeque<(String, String)>,
    /// Link targets seen on pages (page-provided URLs).
    links: HashSet<String>,
    approved_downloads: HashSet<String>,
}

/// One Genie session's browser state.
pub struct BrowserSession {
    pub id: String,
    shared: StdMutex<Shared>,
    ops: Arc<tokio::sync::Mutex<Ops>>,
}

impl BrowserSession {
    fn new(id: &str) -> BrowserSession {
        BrowserSession {
            id: id.to_string(),
            shared: StdMutex::new(Shared::default()),
            ops: Arc::new(tokio::sync::Mutex::new(Ops::default())),
        }
    }

    fn with<R>(&self, f: impl FnOnce(&mut Shared) -> R) -> Option<R> {
        self.shared.lock().ok().map(|mut g| f(&mut g))
    }

    pub fn control(&self) -> Control {
        self.with(|s| s.control.unwrap_or(Control::Agent))
            .unwrap_or(Control::Agent)
    }

    pub(crate) fn set_control(&self, c: Control) {
        self.with(|s| s.control = Some(c));
    }

    pub fn tabs(&self) -> Vec<TabHandle> {
        self.with(|s| s.tabs.clone()).unwrap_or_default()
    }

    pub fn active_tab(&self) -> Option<TabHandle> {
        self.with(|s| {
            let a = s.active.clone()?;
            s.tabs.iter().find(|t| t.tab_id == a).cloned()
        })
        .flatten()
    }

    pub fn tab(&self, tab_id: &str) -> Option<TabHandle> {
        self.with(|s| s.tabs.iter().find(|t| t.tab_id == tab_id).cloned())
            .flatten()
    }

    fn owns_target(&self, target_id: &str) -> bool {
        self.with(|s| s.tabs.iter().any(|t| t.target_id == target_id))
            .unwrap_or(false)
    }
}

/// A download seen in the browser (quarantine folder, research §4.1).
#[derive(Debug, Clone, PartialEq)]
pub struct Download {
    pub seq: u64,
    pub guid: String,
    pub url: String,
    pub filename: String,
    pub frame_id: String,
    pub session_id: Option<String>,
    /// `in_progress | completed | canceled`.
    pub state: String,
    pub received: u64,
    pub total: u64,
}

/// The browser subsystem: one per daemon.
pub struct Browser {
    me: Weak<Browser>,
    pub(crate) backend: Arc<dyn BrowserBackend>,
    pub sites: Arc<SitePolicy>,
    pub(crate) host: Arc<dyn BrowserHost>,
    pub(crate) cfg: BrowserConfig,
    pub(crate) tasks: TaskTracker,
    sessions: StdMutex<HashMap<String, Arc<BrowserSession>>>,
    watched: tokio::sync::Mutex<Option<CdpClient>>,
    downloads: StdMutex<Vec<Download>>,
    download_seq: AtomicU64,
    pub(crate) casts: crate::screencast::Casts,
}

impl Browser {
    pub fn new(
        backend: Arc<dyn BrowserBackend>,
        host: Arc<dyn BrowserHost>,
        sites: Arc<SitePolicy>,
        cfg: BrowserConfig,
        tasks: TaskTracker,
    ) -> Arc<Browser> {
        Arc::new_cyclic(|me| Browser {
            me: me.clone(),
            backend,
            sites,
            host,
            cfg,
            tasks,
            sessions: StdMutex::new(HashMap::new()),
            watched: tokio::sync::Mutex::new(None),
            downloads: StdMutex::new(Vec::new()),
            download_seq: AtomicU64::new(0),
            casts: crate::screencast::Casts::default(),
        })
    }

    pub(crate) fn weak(&self) -> Weak<Browser> {
        self.me.clone()
    }

    /// The session's browser state (created on first use).
    pub fn session(&self, session_id: &str) -> Arc<BrowserSession> {
        let Ok(mut g) = self.sessions.lock() else {
            return Arc::new(BrowserSession::new(session_id));
        };
        g.entry(session_id.to_string())
            .or_insert_with(|| Arc::new(BrowserSession::new(session_id)))
            .clone()
    }

    fn existing(&self, session_id: &str) -> Option<Arc<BrowserSession>> {
        self.sessions.lock().ok()?.get(session_id).cloned()
    }

    /// `browser.status`.
    pub async fn status(&self) -> BrowserStatus {
        self.backend.status().await
    }

    /// A live connection; (re)arms the download watcher on a new one.
    pub async fn client(&self) -> Result<CdpClient, BrowserError> {
        let c = self.backend.connect().await?;
        let mut w = self.watched.lock().await;
        if !w.as_ref().is_some_and(|x| x.same_connection(&c)) {
            let mut p = json!({"behavior": "allowAndName", "eventsEnabled": true});
            if let Some(d) = &self.cfg.downloads_dir {
                p["downloadPath"] = json!(d.to_string_lossy());
            } else {
                p["behavior"] = json!("deny");
            }
            if let Err(e) = c.call("Browser.setDownloadBehavior", p).await {
                tracing::warn!(error = %e, "setDownloadBehavior failed");
            }
            let me = self.me.clone();
            let mut rx = c.subscribe();
            self.tasks.spawn(async move {
                loop {
                    match rx.recv().await {
                        Ok(e) => match me.upgrade() {
                            Some(b) => b.on_download_event(&e),
                            None => break,
                        },
                        Err(broadcast::error::RecvError::Lagged(_)) => {}
                        Err(broadcast::error::RecvError::Closed) => break,
                    }
                }
            });
            *w = Some(c.clone());
        }
        Ok(c)
    }

    fn on_download_event(&self, e: &CdpEvent) {
        match e.method.as_str() {
            "Browser.downloadWillBegin" => {
                let frame = str_at(&e.params, &["frameId"]).unwrap_or("").to_string();
                let session = self.sessions.lock().ok().and_then(|g| {
                    g.values()
                        .find(|s| s.owns_target(&frame))
                        .map(|s| s.id.clone())
                });
                let d = Download {
                    seq: self.download_seq.fetch_add(1, Ordering::SeqCst) + 1,
                    guid: str_at(&e.params, &["guid"]).unwrap_or("").into(),
                    url: str_at(&e.params, &["url"]).unwrap_or("").into(),
                    filename: str_at(&e.params, &["suggestedFilename"])
                        .unwrap_or("")
                        .into(),
                    frame_id: frame,
                    session_id: session,
                    state: "in_progress".into(),
                    received: 0,
                    total: 0,
                };
                if let Ok(mut g) = self.downloads.lock() {
                    g.push(d);
                }
            }
            "Browser.downloadProgress" => {
                let guid = str_at(&e.params, &["guid"]).unwrap_or("");
                if let Ok(mut g) = self.downloads.lock()
                    && let Some(d) = g.iter_mut().find(|d| d.guid == guid)
                {
                    d.received = e.params["receivedBytes"].as_f64().unwrap_or(0.0) as u64;
                    d.total = e.params["totalBytes"].as_f64().unwrap_or(0.0) as u64;
                    d.state = match str_at(&e.params, &["state"]) {
                        Some("completed") => "completed",
                        Some("canceled") => "canceled",
                        _ => "in_progress",
                    }
                    .into();
                }
            }
            _ => {}
        }
    }

    pub fn downloads(&self, session_id: &str) -> Vec<Download> {
        self.downloads
            .lock()
            .map(|g| {
                g.iter()
                    .filter(|d| d.session_id.as_deref() == Some(session_id))
                    .cloned()
                    .collect()
            })
            .unwrap_or_default()
    }

    // ------------------------------------------------------------ daemon controls (05 §9)

    /// `browser.takeover`: the agent pauses; tools return `user_in_control`.
    pub fn takeover(&self, session_id: &str) {
        let s = self.session(session_id);
        s.set_control(Control::User);
        self.host.control_changed(session_id, Control::User);
    }

    /// `browser.handback`: the next tool result carries a fresh snapshot and a note that
    /// the user acted (D56).
    pub fn handback(&self, session_id: &str) {
        let s = self.session(session_id);
        s.with(|g| {
            g.control = Some(Control::Agent);
            g.handed_back = true;
        });
        self.host.control_changed(session_id, Control::Agent);
    }

    /// `browser.stop`: the agent stops driving (the daemon also interrupts the turn);
    /// screencasts of the session end. Tabs stay open for the user.
    pub async fn stop(&self, session_id: &str) {
        self.takeover(session_id);
        crate::screencast::stop_session(self, session_id).await;
    }

    /// Close the session's tabs (session deleted or evicted).
    pub async fn end_session(&self, session_id: &str) {
        crate::screencast::stop_session(self, session_id).await;
        let s = self
            .sessions
            .lock()
            .ok()
            .and_then(|mut g| g.remove(session_id));
        if let Some(s) = s
            && let Some(c) = s.with(|g| g.client.clone()).flatten()
        {
            for t in s.tabs() {
                let _ = c
                    .call("Target.closeTarget", json!({"targetId": t.target_id}))
                    .await;
            }
        }
    }

    /// `browser.tabs`.
    pub async fn tabs(&self, session_id: &str) -> TabsResult {
        let Some(s) = self.existing(session_id) else {
            return TabsResult { tabs: vec![] };
        };
        let c = s.with(|g| g.client.clone()).flatten();
        let active = s.active_tab().map(|t| t.tab_id);
        let mut tabs = Vec::new();
        for t in s.tabs() {
            let (url, title) = match &c {
                Some(c) => target_info(c, &t.target_id).await.unwrap_or_default(),
                None => Default::default(),
            };
            tabs.push(TabInfo {
                active: active.as_deref() == Some(t.tab_id.as_str()),
                tab_id: t.tab_id,
                url,
                title,
            });
        }
        TabsResult { tabs }
    }

    /// `browser.tab.activate`: the user switched the viewed tab.
    pub async fn tab_activate(&self, p: &TabActivateParams) -> Result<(), BrowserError> {
        let s = self.session(&p.session_id);
        let Some(t) = s.tab(&p.tab_id) else {
            return Err(BrowserError::Launch(format!("no tab {}", p.tab_id)));
        };
        s.with(|g| g.active = Some(t.tab_id.clone()));
        if let Some(c) = s.with(|g| g.client.clone()).flatten() {
            c.call("Target.activateTarget", json!({"targetId": t.target_id}))
                .await?;
        }
        Ok(())
    }

    /// `browser.data.clear`.
    pub async fn clear_data(&self, what: ClearWhat) -> Result<(), BrowserError> {
        let c = self.client().await?;
        c.call("Storage.clearCookies", json!({})).await?;
        if what == ClearWhat::All {
            // Cache and site storage need a page session.
            let r = c
                .call("Target.createTarget", json!({"url": "about:blank"}))
                .await?;
            let target = str_at(&r, &["targetId"]).unwrap_or("").to_string();
            let a = c
                .call(
                    "Target.attachToTarget",
                    json!({"targetId": target, "flatten": true}),
                )
                .await?;
            let sid = str_at(&a, &["sessionId"]).map(String::from);
            let _ = c
                .call_in(sid.as_deref(), "Network.clearBrowserCache", json!({}))
                .await;
            let _ = c
                .call("Target.closeTarget", json!({"targetId": target}))
                .await;
        }
        Ok(())
    }
}

pub(crate) async fn target_info(
    c: &CdpClient,
    target_id: &str,
) -> Result<(String, String), CdpError> {
    let r = c
        .call("Target.getTargetInfo", json!({"targetId": target_id}))
        .await?;
    Ok((
        str_at(&r, &["targetInfo", "url"]).unwrap_or("").to_string(),
        str_at(&r, &["targetInfo", "title"])
            .unwrap_or("")
            .to_string(),
    ))
}

fn is_web(url: &str) -> bool {
    url.starts_with("http://") || url.starts_with("https://")
}

fn without_fragment(u: &str) -> &str {
    u.split('#').next().unwrap_or(u)
}

const SNAPSHOT_TEXT_ORIGINS: usize = 16;

// ---------------------------------------------------------------- one agent action

/// One browser tool call, holding the session's action lock.
pub struct Op<'a> {
    pub(crate) b: &'a Browser,
    pub(crate) s: Arc<BrowserSession>,
    ops: OwnedMutexGuard<Ops>,
    pub(crate) c: CdpClient,
    pub(crate) ctx: &'a ToolCtx,
    pub(crate) tool: &'static str,
    decision: String,
    tab_id: Option<String>,
    origin: String,
    note: Option<String>,
    fresh: bool,
    act: bool,
    thumbnail: Option<String>,
    start_seq: u64,
    events: broadcast::Receiver<CdpEvent>,
}

impl<'a> Op<'a> {
    /// Start an action: control check, connection, step caps.
    pub async fn begin(
        b: &'a Browser,
        ctx: &'a ToolCtx,
        tool: &'static str,
    ) -> Result<Op<'a>, Stop> {
        let s = b.session(&ctx.env.session_id);
        if s.control() == Control::User {
            return Err(Stop::boxed(Outcome::status(
                Status::UserInControl,
                "the user is in control of the browser. Wait until they hand back, then continue.",
            )));
        }
        let ops = s.ops.clone().lock_owned().await;
        let c = b
            .client()
            .await
            .map_err(|e| Stop::Err(ToolError::failed(e.to_string())))?;
        let mut ops = ops;
        let changed = s
            .with(|g| {
                let changed = !g.client.as_ref().is_some_and(|x| x.same_connection(&c));
                if changed {
                    g.tabs.clear();
                    g.active = None;
                    g.client = Some(c.clone());
                }
                changed
            })
            .unwrap_or(false);
        if changed {
            ops.tabs.clear();
        }
        let fresh = s
            .with(|g| std::mem::take(&mut g.handed_back))
            .unwrap_or(false);
        let events = c.subscribe();
        let mut op = Op {
            b,
            s,
            ops,
            c,
            ctx,
            tool,
            decision: "none".into(),
            tab_id: None,
            origin: String::new(),
            note: fresh
                .then(|| "the user acted in the browser; state may have changed".to_string()),
            fresh,
            act: false,
            thumbnail: None,
            start_seq: b.download_seq.load(Ordering::SeqCst),
            events,
        };
        op.count_step().await?;
        Ok(op)
    }

    /// 50 actions or 15 minutes per turn before checking in with the user (06 §10).
    async fn count_step(&mut self) -> Result<(), Stop> {
        let turn_id = self.ctx.turn_id.clone();
        let t = self.ops.turn.get_or_insert_with(|| Turn {
            id: turn_id.clone(),
            steps: 0,
            started: Instant::now(),
            read_origins: vec![],
            source: None,
        });
        if t.id != turn_id {
            *t = Turn {
                id: turn_id,
                steps: 0,
                started: Instant::now(),
                read_origins: vec![],
                source: None,
            };
        }
        t.steps += 1;
        let (steps, elapsed) = (t.steps, t.started.elapsed());
        if steps > self.b.cfg.max_steps || elapsed > self.b.cfg.max_duration {
            let summary = format!(
                "Genie has taken {} browser steps over {} min in this turn. Continue browsing?",
                steps - 1,
                elapsed.as_secs() / 60
            );
            self.confirm("continue", summary, false).await?;
            if let Some(t) = self.ops.turn.as_mut() {
                t.steps = 1;
                t.started = Instant::now();
            }
        }
        Ok(())
    }

    // -------------------------------------------------------------- tabs

    /// The tab to act on: the given one, the active one, or a new one.
    pub async fn tab(&mut self, tab_id: Option<&str>) -> Result<TabHandle, Stop> {
        let t = match tab_id {
            Some(id) => self.s.tab(id).ok_or_else(|| {
                Stop::Err(ToolError::invalid_input(vec![format!(
                    "no tab `{id}`; use browser_tabs to list tabs"
                )]))
            })?,
            None => match self.s.active_tab() {
                Some(t) => t,
                None => self.new_tab().await?,
            },
        };
        self.tab_id = Some(t.tab_id.clone());
        Ok(t)
    }

    pub async fn new_tab(&mut self) -> Result<TabHandle, Stop> {
        let r = self
            .c
            .call("Target.createTarget", json!({"url": "about:blank"}))
            .await
            .map_err(cdp_stop)?;
        let target = str_at(&r, &["targetId"]).unwrap_or("").to_string();
        self.adopt(&target).await
    }

    /// Attach to a page target and register it as a tab of this session.
    pub(crate) async fn adopt(&mut self, target: &str) -> Result<TabHandle, Stop> {
        let a = self
            .c
            .call(
                "Target.attachToTarget",
                json!({"targetId": target, "flatten": true}),
            )
            .await
            .map_err(cdp_stop)?;
        let sid = str_at(&a, &["sessionId"]).unwrap_or("").to_string();
        self.c
            .call_in(Some(&sid), "Page.enable", json!({}))
            .await
            .map_err(cdp_stop)?;
        let handle = self
            .s
            .with(|g| {
                g.next_tab += 1;
                let h = TabHandle {
                    tab_id: format!("t{}", g.next_tab),
                    target_id: target.to_string(),
                    cdp_session: sid.clone(),
                };
                g.tabs.push(h.clone());
                g.active = Some(h.tab_id.clone());
                h
            })
            .ok_or_else(|| Stop::Err(ToolError::failed("browser state poisoned")))?;
        self.tab_id = Some(handle.tab_id.clone());
        Ok(handle)
    }

    /// Pop-ups opened by our tabs join the tab strip (not auto-focused, SA-24).
    pub(crate) async fn adopt_popups(&mut self) -> Result<(), Stop> {
        let r = self
            .c
            .call("Target.getTargets", json!({}))
            .await
            .map_err(cdp_stop)?;
        let ours: HashSet<String> = self.s.tabs().into_iter().map(|t| t.target_id).collect();
        let active = self.s.with(|g| g.active.clone()).flatten();
        for t in r["targetInfos"].as_array().into_iter().flatten() {
            let id = t["targetId"].as_str().unwrap_or("");
            let opener = t["openerId"].as_str().unwrap_or("");
            if t["type"] == "page" && !ours.contains(id) && ours.contains(opener) {
                self.adopt(id).await?;
                self.s.with(|g| g.active.clone_from(&active));
            }
        }
        Ok(())
    }

    pub(crate) async fn close_tab(&mut self, t: &TabHandle) -> Result<(), Stop> {
        self.c
            .call("Target.closeTarget", json!({"targetId": t.target_id}))
            .await
            .map_err(cdp_stop)?;
        self.s.with(|g| {
            g.tabs.retain(|x| x.tab_id != t.tab_id);
            if g.active.as_deref() == Some(t.tab_id.as_str()) {
                g.active = g.tabs.last().map(|x| x.tab_id.clone());
            }
        });
        self.ops.tabs.remove(&t.tab_id);
        Ok(())
    }

    pub(crate) fn activate(&mut self, t: &TabHandle) {
        self.s.with(|g| g.active = Some(t.tab_id.clone()));
        self.c.send(
            None,
            "Target.activateTarget",
            json!({"targetId": t.target_id}),
        );
    }

    // -------------------------------------------------------------- page state

    pub async fn page_info(&self, t: &TabHandle) -> Result<(String, String), Stop> {
        target_info(&self.c, &t.target_id).await.map_err(cdp_stop)
    }

    /// AX tree + DOMSnapshot → [`Snapshot`]; refs reset when the document changed.
    pub async fn capture(&mut self, t: &TabHandle) -> Result<Snapshot, Stop> {
        let sid = Some(t.cdp_session.as_str());
        let (url, title) = self.page_info(t).await?;
        let ax = self
            .c
            .call_in(sid, "Accessibility.getFullAXTree", json!({}))
            .await
            .map_err(cdp_stop)?;
        let dom = self
            .c
            .call_in(
                sid,
                "DOMSnapshot.captureSnapshot",
                json!({"computedStyles": COMPUTED_STYLES}),
            )
            .await
            .map_err(cdp_stop)?;
        let viewport = self
            .c
            .call_in(sid, "Page.getLayoutMetrics", json!({}))
            .await
            .ok()
            .and_then(|m| {
                let v = &m["cssLayoutViewport"];
                Some((v["pageY"].as_f64()?, v["clientHeight"].as_f64()?))
            });
        let st = self.ops.tabs.entry(t.tab_id.clone()).or_default();
        if st
            .last
            .as_ref()
            .is_some_and(|l| without_fragment(&l.meta.url) != without_fragment(&url))
        {
            st.refs.clear();
        }
        let snap = Snapshot::build(
            &ax,
            &dom,
            PageMeta {
                url,
                title,
                viewport,
            },
            &mut st.refs,
        );
        self.origin = snap.origin();
        Ok(snap)
    }

    /// Keep `snap` as the tab's latest snapshot; returns the previous one.
    pub fn store(&mut self, t: &TabHandle, snap: Snapshot) -> Option<Snapshot> {
        for l in &snap.links {
            if self.ops.links.len() < 10_000 {
                self.ops.links.insert(l.clone());
            }
        }
        self.mark_read(&snap);
        let st = self.ops.tabs.entry(t.tab_id.clone()).or_default();
        st.last.replace(snap)
    }

    pub fn last(&self, t: &TabHandle) -> Option<&Snapshot> {
        self.ops.tabs.get(&t.tab_id)?.last.as_ref()
    }

    /// Page content entered the turn: taint (SA-29) and remember its text for the
    /// exfiltration guard.
    fn mark_read(&mut self, snap: &Snapshot) {
        self.mark_read_text(&snap.origin(), &snap.text);
    }

    /// As [`Op::mark_read`] for content that isn't a snapshot (screenshots).
    pub fn mark_read_text(&mut self, origin: &str, text: &str) {
        let origin = origin.to_string();
        if origin.is_empty() {
            return;
        }
        if let Some(t) = self.ops.turn.as_mut() {
            if !t.read_origins.contains(&origin) {
                t.read_origins.push(origin.clone());
            }
            t.source = Some(origin.clone());
        }
        self.b
            .host
            .taint(&self.ctx.env.session_id, &self.ctx.turn_id, &origin);
        self.ops.seen.retain(|(o, _)| *o != origin);
        self.ops.seen.push_back((origin, text.to_string()));
        while self.ops.seen.len() > SNAPSHOT_TEXT_ORIGINS {
            self.ops.seen.pop_front();
        }
    }

    /// The element behind a ref of the tab's latest snapshot.
    pub fn ref_info(&self, t: &TabHandle, r: &str) -> Result<RefInfo, Stop> {
        let Some(last) = self.last(t) else {
            return Err(Stop::Err(ToolError::invalid_input(vec![
                "no snapshot of this tab yet; call browser_snapshot first".into(),
            ])));
        };
        last.refs.get(r).cloned().ok_or_else(|| {
            Stop::Err(ToolError::invalid_input(vec![format!(
                "unknown ref `{r}`; the page may have changed, take a new snapshot"
            )]))
        })
    }

    pub fn current_url(&self, t: &TabHandle) -> String {
        self.last(t).map(|s| s.meta.url.clone()).unwrap_or_default()
    }

    // -------------------------------------------------------------- gates

    /// Per-site permission (D21): blocked → `blocked`; not covered → ask *Allow for this
    /// chat · Always allow · Decline* (a permission ask with a `Browser(site:…)`
    /// suggestion; `once` = this chat). `act` in plan mode is blocked; messaging sites
    /// confirm every `act`; a tainted turn asks before `act` on an origin it didn't read.
    pub async fn gate(&mut self, url: &str, need: Need) -> Result<(), Stop> {
        if need == Need::Act {
            self.act = true;
        }
        if need == Need::Act && self.ctx.env.mode == Mode::Plan {
            self.decision = "blocked:mode".into();
            return Err(Stop::boxed(Outcome::status(
                Status::Blocked,
                "plan mode: the browser can navigate and read only",
            )));
        }
        if !is_web(url) {
            return Ok(());
        }
        let eff = self.b.sites.effective(url);
        let host = site::host_of(url).unwrap_or_default();
        let key = site::site_key(&host);
        self.origin = origin_of(url);
        if eff.state == SiteState::Blocked {
            let why = match (eff.source, eff.category) {
                (Source::Policy, _) => "blocked by your organization's policy".to_string(),
                (Source::Category, Some(c)) => format!(
                    "{host} is a {} site, blocked by default; the user can allow it in Settings → Browser",
                    c.label()
                ),
                _ => format!("{host} is blocked in browser site settings"),
            };
            self.decision = match eff.source {
                Source::Policy => "blocked:policy",
                Source::Category => "blocked:category",
                _ => "blocked:user",
            }
            .into();
            return Err(Stop::boxed(Outcome::status(Status::Blocked, why)));
        }
        let granted = self.ops.grants.get(&key).copied();
        let auto = site::covers(eff.state, need) || granted.is_some_and(|g| site::covers(g, need));
        if auto {
            self.decision = match eff.source {
                Source::Policy => "allowed:policy",
                _ if granted.is_some() && !site::covers(eff.state, need) => "allowed:chat",
                _ => "allowed:site",
            }
            .into();
        } else {
            let scope = match self.ctx.env.kind {
                SessionKind::Code => RuleScope::Local,
                _ => RuleScope::User,
            };
            self.ctx
                .authorize(PermissionRequest {
                    resource: Some(key.clone()),
                    preview: Some(Preview::Url {
                        url: url.to_string(),
                    }),
                    save_suggestions: vec![SaveSuggestion {
                        rule: format!("Browser(site:{key})"),
                        scope,
                    }],
                })
                .await
                .map_err(|e| {
                    self.decision = "denied:user".into();
                    Stop::Err(e)
                })?;
            let level = if need == Need::Act {
                SiteState::Act
            } else {
                granted.unwrap_or(SiteState::Read)
            };
            self.ops.grants.insert(key.clone(), level);
            self.decision = "allowed:user".into();
        }
        if need == Need::Act {
            if eff.category == Some(Category::Messaging) {
                self.confirm(
                    "act",
                    format!("{} on {host} (email / messaging site)", self.tool),
                    true,
                )
                .await?;
            }
            let origin = origin_of(url);
            let tainted_elsewhere = self.ops.turn.as_ref().and_then(|t| {
                (t.source.is_some() && !t.read_origins.contains(&origin))
                    .then(|| t.source.clone())
                    .flatten()
            });
            if let Some(src) = tainted_elsewhere
                && auto
                && self.ctx.env.mode != Mode::Bypass
            {
                self.ctx
                    .authorize(PermissionRequest {
                        resource: Some(key),
                        preview: Some(Preview::Text {
                            text: format!("This step follows content from {src}"),
                        }),
                        save_suggestions: vec![],
                    })
                    .await
                    .map_err(Stop::Err)?;
            }
        }
        Ok(())
    }

    /// Navigation guard for a URL the agent asks for: scheme, exfiltration, category
    /// confirmation, then the site gate.
    pub async fn gate_navigation(&mut self, t: &TabHandle, url: &str) -> Result<(), Stop> {
        if url != "about:blank" && !is_web(url) {
            self.decision = "blocked:scheme".into();
            return Err(Stop::boxed(Outcome::status(
                Status::Blocked,
                "only http(s) URLs can be opened",
            )));
        }
        let provenance = if self.ops.links.contains(url) {
            Provenance::Page
        } else if self.b.host.user_supplied_url(&self.ctx.env.session_id, url) {
            Provenance::User
        } else {
            Provenance::Model
        };
        let current = origin_of(&self.current_url(t));
        let seen: Vec<(String, String)> = self.ops.seen.iter().cloned().collect();
        if let Err(why) = site::exfiltration_check(
            url,
            provenance,
            (!current.is_empty()).then_some(current.as_str()),
            &seen,
        ) {
            self.decision = "blocked:exfiltration".into();
            return Err(Stop::boxed(Outcome::status(Status::Blocked, why)));
        }
        self.gate(url, Need::Read).await?;
        let eff = self.b.sites.effective(url);
        if let Some(c) = eff.category
            && c != Category::Messaging
        {
            self.confirm(
                "navigate",
                format!("Open {} ({} site)", url, c.label()),
                false,
            )
            .await?;
        }
        Ok(())
    }

    /// A D21 confirmation card (`browser_confirm` ask; applies in bypass too).
    pub async fn confirm(&mut self, action: &str, summary: String, shot: bool) -> Result<(), Stop> {
        let screenshot_blob = if shot {
            self.screenshot_ref(60).await
        } else {
            None
        };
        let sid = self.ctx.env.session_id.clone();
        self.b
            .host
            .control_changed(&sid, Control::WaitingForApproval);
        let d = self
            .b
            .host
            .confirm(ConfirmRequest {
                session_id: sid.clone(),
                turn_id: self.ctx.turn_id.clone(),
                call_id: self.ctx.call_id.clone(),
                confirm: BrowserConfirm {
                    origin: self.origin.clone(),
                    action: action.to_string(),
                    summary: summary.clone(),
                    screenshot_blob,
                },
            })
            .await;
        let back = if d == ConfirmDecision::TakeOver {
            Control::User
        } else {
            Control::Agent
        };
        self.s.set_control(back);
        self.b.host.control_changed(&sid, back);
        match d {
            ConfirmDecision::Approve => {
                self.decision = "confirmed:user".into();
                Ok(())
            }
            ConfirmDecision::Deny => {
                self.decision = "denied:user".into();
                Err(Stop::boxed(Outcome::status(
                    Status::Blocked,
                    format!("the user declined: {summary}"),
                )))
            }
            ConfirmDecision::TakeOver => {
                self.decision = "takeover:user".into();
                Err(Stop::boxed(Outcome::status(
                    Status::UserInControl,
                    "the user took over instead of approving. Wait until they hand back.",
                )))
            }
            ConfirmDecision::Unavailable => {
                self.decision = "needs_confirmation".into();
                Err(Stop::boxed(Outcome::status(
                    Status::NeedsConfirmation,
                    format!(
                        "this needs the user's confirmation, and nobody can answer now: {summary}"
                    ),
                )))
            }
        }
    }

    async fn screenshot_ref(&self, quality: u8) -> Option<String> {
        let t = self.tab_id.as_deref().and_then(|id| self.s.tab(id))?;
        let r = self
            .c
            .call_in(
                Some(&t.cdp_session),
                "Page.captureScreenshot",
                json!({"format": "jpeg", "quality": quality}),
            )
            .await
            .ok()?;
        let bytes = decode_b64(r["data"].as_str()?)?;
        self.b
            .host
            .store_image(&self.ctx.env.session_id, &bytes, "image/jpeg")
    }

    // -------------------------------------------------------------- element helpers

    /// Centre of an element in viewport CSS pixels, after scrolling it into view.
    pub async fn center(&self, t: &TabHandle, backend: i64) -> Result<(f64, f64), Stop> {
        let sid = Some(t.cdp_session.as_str());
        let _ = self
            .c
            .call_in(
                sid,
                "DOM.scrollIntoViewIfNeeded",
                json!({"backendNodeId": backend}),
            )
            .await;
        let q = self
            .c
            .call_in(
                sid,
                "DOM.getContentQuads",
                json!({"backendNodeId": backend}),
            )
            .await
            .map_err(cdp_stop)?;
        let quad: Vec<f64> = q["quads"][0]
            .as_array()
            .map(|a| a.iter().filter_map(Value::as_f64).collect())
            .unwrap_or_default();
        if quad.len() != 8 {
            return Err(Stop::Err(ToolError::failed(
                "the element is not visible on the page; scroll or take a new snapshot",
            )));
        }
        let x = (quad[0] + quad[2] + quad[4] + quad[6]) / 4.0;
        let y = (quad[1] + quad[3] + quad[5] + quad[7]) / 4.0;
        Ok((x, y))
    }

    pub async fn mouse(&self, t: &TabHandle, p: Value) -> Result<(), Stop> {
        self.c
            .call_in(Some(&t.cdp_session), "Input.dispatchMouseEvent", p)
            .await
            .map(|_| ())
            .map_err(cdp_stop)
    }

    pub async fn call_fn(
        &self,
        t: &TabHandle,
        backend: i64,
        function: &str,
        args: Vec<Value>,
    ) -> Result<Value, Stop> {
        let sid = Some(t.cdp_session.as_str());
        let r = self
            .c
            .call_in(sid, "DOM.resolveNode", json!({"backendNodeId": backend}))
            .await
            .map_err(cdp_stop)?;
        let obj = str_at(&r, &["object", "objectId"])
            .unwrap_or("")
            .to_string();
        let args: Vec<Value> = args.into_iter().map(|v| json!({"value": v})).collect();
        let r = self
            .c
            .call_in(
                sid,
                "Runtime.callFunctionOn",
                json!({
                    "objectId": obj, "functionDeclaration": function,
                    "arguments": args, "returnByValue": true, "awaitPromise": true,
                }),
            )
            .await
            .map_err(cdp_stop)?;
        Ok(r["result"]["value"].clone())
    }

    // -------------------------------------------------------------- after an action

    /// Wait briefly for the page to settle: if a navigation started, until it loads.
    pub async fn settle(&mut self, t: &TabHandle) {
        let sid = t.cdp_session.clone();
        let started = wait_event(&mut self.events, Duration::from_millis(150), |e| {
            e.session_id.as_deref() == Some(sid.as_str())
                && (e.method == "Page.frameStartedLoading" || e.method == "Page.frameNavigated")
        })
        .await;
        if started.is_some() {
            self.wait_load(t).await;
        }
    }

    pub async fn wait_load(&mut self, t: &TabHandle) -> bool {
        let sid = t.cdp_session.clone();
        wait_event(&mut self.events, self.b.cfg.load_timeout, |e| {
            e.session_id.as_deref() == Some(sid.as_str())
                && (e.method == "Page.loadEventFired" || e.method == "Page.frameStoppedLoading")
        })
        .await
        .is_some()
    }

    /// Downloads started during this action need confirmation (D21): cancel, ask, and
    /// fetch again into quarantine when approved.
    async fn check_downloads(&mut self, t: &TabHandle) -> Result<(), Stop> {
        let me = self.s.id.clone();
        let new: Vec<Download> = self
            .b
            .downloads
            .lock()
            .map(|mut g| {
                g.iter_mut()
                    .filter(|d| {
                        d.seq > self.start_seq
                            && d.session_id.as_deref().is_none_or(|s| s == me)
                            && d.state != "canceled"
                    })
                    .map(|d| {
                        d.session_id = Some(me.clone());
                        d.clone()
                    })
                    .collect()
            })
            .unwrap_or_default();
        for d in new {
            if self.ops.approved_downloads.contains(&d.url) {
                continue;
            }
            let _ = self
                .c
                .call("Browser.cancelDownload", json!({"guid": d.guid}))
                .await;
            if let Ok(mut g) = self.b.downloads.lock()
                && let Some(x) = g.iter_mut().find(|x| x.guid == d.guid)
            {
                x.state = "canceled".into();
            }
            self.confirm(
                "download",
                format!(
                    "Download {} from {}",
                    if d.filename.is_empty() {
                        &d.url
                    } else {
                        &d.filename
                    },
                    origin_of(&d.url)
                ),
                false,
            )
            .await?;
            self.ops.approved_downloads.insert(d.url.clone());
            let _ = self
                .c
                .call_in(Some(&t.cdp_session), "Page.navigate", json!({"url": d.url}))
                .await;
        }
        Ok(())
    }

    /// The common tail of an action: settle, downloads, the landing page's read gate,
    /// snapshot + compact diff, audit thumbnail.
    pub async fn finish(&mut self, t: &TabHandle) -> Result<Outcome, Stop> {
        self.settle(t).await;
        self.check_downloads(t).await?;
        let (url, _) = self.page_info(t).await?;
        let decision = self.decision.clone();
        if is_web(&url)
            && let Err(stop) = self.gate(&url, Need::Read).await
        {
            // Landed somewhere not allowed (redirect, link to another site): leave it.
            let _ = self
                .c
                .call_in(
                    Some(&t.cdp_session),
                    "Page.navigate",
                    json!({"url": "about:blank"}),
                )
                .await;
            return Err(stop);
        }
        if decision != "none" {
            self.decision = decision;
        }
        let snap = self.capture(t).await?;
        let prev = self.store(t, snap);
        let fresh = self.fresh;
        let cur = self.last(t).cloned().unwrap_or_default();
        let body =
            crate::snapshot::diff(if fresh { None } else { prev.as_ref() }, &cur, DIFF_CHARS);
        if self.act && self.b.cfg.thumbnails {
            self.thumbnail = self.screenshot_ref(30).await;
        }
        Ok(self.outcome(t, &cur, Some(body)))
    }

    pub fn outcome(&self, t: &TabHandle, snap: &Snapshot, body: Option<String>) -> Outcome {
        Outcome {
            status: Status::Ok,
            tab_id: Some(t.tab_id.clone()),
            url: snap.meta.url.clone(),
            title: snap.meta.title.clone(),
            note: self.note.clone(),
            body,
            origin: snap.origin(),
        }
    }

    pub fn is_fresh(&self) -> bool {
        self.fresh
    }

    pub fn set_note(&mut self, n: impl Into<String>) {
        let n = n.into();
        self.note = Some(match self.note.take() {
            Some(o) => format!("{o}; {n}"),
            None => n,
        });
    }

    /// Record the `browser.action` audit event and turn the result into tool output.
    pub fn conclude(self, r: Result<Outcome, Stop>) -> Result<ToolOutput, ToolError> {
        let (out, err) = match r {
            Ok(o) => (Some(o), None),
            Err(Stop::Out(o)) => (Some(*o), None),
            Err(Stop::Err(e)) => (None, Some(e)),
        };
        let status = match (&out, &err) {
            (Some(o), _) => o.status.as_str().to_string(),
            (_, Some(ToolError::Declined { .. })) => "declined".into(),
            (_, Some(ToolError::Blocked { .. })) => "blocked".into(),
            (_, Some(ToolError::Cancelled)) => "cancelled".into(),
            _ => "error".into(),
        };
        let origin = out
            .as_ref()
            .map(|o| o.origin.clone())
            .filter(|o| !o.is_empty())
            .unwrap_or_else(|| self.origin.clone());
        let tab_id = out
            .as_ref()
            .and_then(|o| o.tab_id.clone())
            .or_else(|| self.tab_id.clone())
            .unwrap_or_default();
        audit(
            self.ctx,
            BrowserAction {
                tab_id,
                tool: self.tool.to_string(),
                origin,
                decision: self.decision.clone(),
                status,
                thumbnail_ref: self.thumbnail.clone(),
            },
        );
        match (out, err) {
            (Some(mut o), _) => {
                if o.tab_id.is_none() {
                    o.tab_id.clone_from(&self.tab_id);
                }
                if o.note.is_none() {
                    o.note.clone_from(&self.note);
                }
                Ok(ToolOutput::Text(o.to_text()))
            }
            (None, Some(e)) => Err(e),
            (None, None) => Err(ToolError::failed("no result")),
        }
    }
}

/// `browser.action` (durable, research §4.5).
pub(crate) fn audit(ctx: &ToolCtx, a: BrowserAction) {
    ctx.events.publish(SessionEvent::BrowserAction(a));
}

/// An action that ended before it began (user in control, no browser).
pub(crate) fn conclude_early(
    ctx: &ToolCtx,
    tool: &str,
    stop: Stop,
) -> Result<ToolOutput, ToolError> {
    let (status, out) = match stop {
        Stop::Out(o) => (
            o.status.as_str().to_string(),
            Ok(ToolOutput::Text(o.to_text())),
        ),
        Stop::Err(e) => ("error".to_string(), Err(e)),
    };
    audit(
        ctx,
        BrowserAction {
            tab_id: String::new(),
            tool: tool.to_string(),
            origin: String::new(),
            decision: "none".into(),
            status,
            thumbnail_ref: None,
        },
    );
    out
}

pub(crate) fn decode_b64(s: &str) -> Option<Vec<u8>> {
    use base64::Engine;
    base64::engine::general_purpose::STANDARD.decode(s).ok()
}
