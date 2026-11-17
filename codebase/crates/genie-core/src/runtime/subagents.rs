//! Subagents (06 §11, D47, t19): the `task` / `task_wait` tools and the child sessions
//! they run.
//!
//! - **`task {agent, description, prompt, model?, session_id?}`** creates a child session
//!   (`parent_session_id`, the agent's settings narrowed from the parent's) and returns
//!   `{status: "running", session_id}` at once. A supervisor task on the hub's scope runs
//!   the child's turn on its own session actor, then steers the child's final text into
//!   the parent as a synthetic `<subagent …>` message (D20 inbox) and publishes
//!   `subagent.started` / `subagent.ended {status, summary, cost}` on the parent.
//!   `session_id` sends a follow-up prompt to an existing, idle child.
//! - **Waiting:** a parent turn that ends (`finish = stop`) while its children run waits for
//!   their reports (`status` activity `waiting_for_children`) and continues with them. If the
//!   parent turn is no longer running (interrupted without `stop_subagents`, ended at max
//!   steps), the report arrives as a new turn ("background" completion). `task_wait`
//!   blocks explicitly.
//! - **Asks:** the child's asks are raised in the parent session with `Ask.subagent`
//!   (`SessionHandle::set_ask_relay`), so they surface wherever the parent is shown.
//! - **Limits:** depth 1 by default (only the main agent gets `task`); `subagents.maxDepth`
//!   setting, capped by policy and at 3. At most 4 children run per parent; more queue.
//!   The child shares the parent's provider cache key behaviour (its own session id is the
//!   key; see README gaps).
//! - **Cancellation:** `session.interrupt {stop_subagents: true}` → [`SubagentHub::stop_children`];
//!   deleting the parent closes its children.

use std::borrow::Cow;
use std::collections::HashMap;
use std::sync::{Arc, Mutex, MutexGuard, OnceLock};

use async_trait::async_trait;
use genie_protocol::common::{Cost, SessionKind};
use genie_protocol::event::{
    Activity, SessionCreated, SessionEvent, StatusEvent, SubagentEnded, SubagentStarted, TurnStatus,
};
use genie_protocol::message::InputPart;
use genie_protocol::session::{Delivery, LiveStatus};
use genie_providers::model::{Part, Role};
use genie_tools::{
    Availability, Tool, ToolCtx, ToolEnv, ToolError, ToolKind, ToolOutput, parse_input, schema_of,
};
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::{Value, json};
use tokio::sync::{Semaphore, watch};
use tokio_util::sync::CancellationToken;

use crate::bus::SubItem;
use crate::config::{ConfigHub, project_root};
use crate::engine::Engine;
use crate::error::TurnError;
use crate::runtime::agents::{AgentCatalog, narrow_mode};
use crate::runtime::conversation::ConversationStore;
use crate::runtime::hooks::{HookCtx, HookEvent, HookTarget, HooksHub};
use crate::runtime::ports::{ModelResolver, SessionSettings, SettingsSource};
use crate::runtime::prompts::{self, fragments};
use crate::session::TurnCtx;
use crate::task::TaskScope;

/// Hard cap on `subagents.maxDepth` (D47).
pub const MAX_DEPTH_CAP: u32 = 3;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SubagentConfig {
    /// Concurrent children per parent; more queue (t19: 4).
    pub max_concurrent: usize,
    /// Default nesting depth when settings don't say (D47: 1).
    pub max_depth: u32,
}

impl Default for SubagentConfig {
    fn default() -> Self {
        SubagentConfig {
            max_concurrent: 4,
            max_depth: 1,
        }
    }
}

/// Registers child sessions where the rest of the daemon looks sessions up (the session
/// registry: `session.list/get`, permissions, shell cwd, MCP …).
pub trait ChildRegistrar: Send + Sync {
    fn register(&self, child_id: &str, parent_id: &str, settings: &SessionSettings);
    fn unregister(&self, _child_id: &str) {}
}

struct Child {
    parent: String,
    agent: String,
    depth: u32,
    settings: SessionSettings,
    running: bool,
    stop: CancellationToken,
    last: Option<TurnStatus>,
}

struct ParentState {
    sem: Arc<Semaphore>,
    version: watch::Sender<u64>,
}

/// One finished (or still running) child, as `task_wait` reports it.
#[derive(Debug, Clone, PartialEq)]
pub struct ChildStatus {
    pub session_id: String,
    pub agent: String,
    pub running: bool,
    pub status: Option<TurnStatus>,
}

/// See the module docs.
pub struct SubagentHub {
    cfg: SubagentConfig,
    scope: TaskScope,
    engine: OnceLock<Engine>,
    registrar: OnceLock<Arc<dyn ChildRegistrar>>,
    base: Arc<dyn SettingsSource>,
    models: Arc<dyn ModelResolver>,
    conversation: Arc<dyn ConversationStore>,
    agents: Arc<AgentCatalog>,
    config: Option<Arc<ConfigHub>>,
    hooks: Option<Arc<HooksHub>>,
    children: Mutex<HashMap<String, Child>>,
    parents: Mutex<HashMap<String, ParentState>>,
    /// Durable outbox for reports (the store's `inbox` table, source `subagent:<child>`).
    outbox: OnceLock<Arc<genie_store::Store>>,
}

/// Inbox `source` prefix of a persisted subagent report.
pub const REPORT_SOURCE: &str = "subagent:";

/// What the hub needs from the runtime (built by `RuntimeBuilder::build`).
pub struct HubParts {
    pub base: Arc<dyn SettingsSource>,
    pub models: Arc<dyn ModelResolver>,
    pub conversation: Arc<dyn ConversationStore>,
    pub agents: Arc<AgentCatalog>,
    pub config: Option<Arc<ConfigHub>>,
    pub hooks: Option<Arc<HooksHub>>,
}

fn status_word(s: TurnStatus) -> &'static str {
    match s {
        TurnStatus::Completed => "done",
        TurnStatus::Cancelled => "cancelled",
        TurnStatus::Failed | TurnStatus::Unknown => "failed",
    }
}

impl SubagentHub {
    pub fn new(cfg: SubagentConfig, scope: TaskScope, parts: HubParts) -> Arc<SubagentHub> {
        Arc::new(SubagentHub {
            cfg,
            scope,
            engine: OnceLock::new(),
            registrar: OnceLock::new(),
            base: parts.base,
            models: parts.models,
            conversation: parts.conversation,
            agents: parts.agents,
            config: parts.config,
            hooks: parts.hooks,
            children: Mutex::new(HashMap::new()),
            parents: Mutex::new(HashMap::new()),
            outbox: OnceLock::new(),
        })
    }

    /// Persist reports in the store before they are delivered, so a daemon crash between
    /// a child's end and its parent's next turn doesn't lose them ([`Self::recover_reports`]).
    pub fn set_store(&self, store: Arc<genie_store::Store>) {
        let _ = self.outbox.set(store);
    }

    /// The parent of a child session this hub started.
    pub fn parent_of(&self, child: &str) -> Option<String> {
        self.children().get(child).map(|c| c.parent.clone())
    }

    /// Write the report as a pending inbox row of the parent (one transaction).
    fn save_report(&self, parent: &str, child: &str, report: &str) -> Option<String> {
        let store = self.outbox.get()?;
        let res = store.write(|tx| {
            genie_store::sessions::ensure(tx, parent)?;
            genie_store::events::inbox_add(
                tx,
                parent,
                Delivery::Steer,
                &json!([{ "type": "text", "text": report }]),
                Some(&format!("{REPORT_SOURCE}{child}")),
                None,
            )
        });
        match res {
            Ok((item, _)) => Some(item.id),
            Err(e) => {
                tracing::warn!(error = %e, child, "subagent report not persisted");
                None
            }
        }
    }

    fn mark_delivered(&self, id: Option<String>) {
        if let (Some(store), Some(id)) = (self.outbox.get(), id)
            && let Err(e) = store.write(|tx| {
                genie_store::events::inbox_set_state(tx, &id, genie_store::events::InboxState::Admitted)
            })
        {
            tracing::warn!(error = %e, "subagent report not marked delivered");
        }
    }

    /// On daemon start (after crash recovery): deliver reports that were persisted but
    /// never reached their parent. Each starts a new parent turn. Returns how many.
    pub fn recover_reports(&self) -> usize {
        let (Some(store), Some(engine)) = (self.outbox.get(), self.engine.get()) else {
            return 0;
        };
        let items = match store.read(|c| genie_store::events::inbox_pending_from(c, REPORT_SOURCE)) {
            Ok(v) => v,
            Err(e) => {
                tracing::warn!(error = %e, "reading pending subagent reports failed");
                return 0;
            }
        };
        let mut n = 0;
        for it in items {
            let text = it.parts[0]["text"].as_str().unwrap_or_default().to_string();
            let Ok(ph) = engine.session(&it.session_id) else {
                continue;
            };
            match ph.push_synthetic("subagent", text, Delivery::Queue) {
                Ok(_) => {
                    self.mark_delivered(Some(it.id));
                    n += 1;
                }
                Err(e) => tracing::warn!(error = %e, "recovered subagent report not delivered"),
            }
        }
        n
    }

    /// Give the hub the engine (built after the runtime) and the session registry.
    pub fn bind(&self, engine: Engine, registrar: Option<Arc<dyn ChildRegistrar>>) {
        let _ = self.engine.set(engine);
        if let Some(r) = registrar {
            let _ = self.registrar.set(r);
        }
    }

    fn children(&self) -> MutexGuard<'_, HashMap<String, Child>> {
        self.children.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn parent_state(&self, parent: &str) -> (Arc<Semaphore>, watch::Sender<u64>) {
        let mut g = self.parents.lock().unwrap_or_else(|e| e.into_inner());
        let p = g.entry(parent.to_string()).or_insert_with(|| ParentState {
            sem: Arc::new(Semaphore::new(self.cfg.max_concurrent.max(1))),
            version: watch::channel(0).0,
        });
        (p.sem.clone(), p.version.clone())
    }

    fn bump(&self, parent: &str) {
        let (_, v) = self.parent_state(parent);
        v.send_modify(|n| *n += 1);
    }

    pub fn is_child(&self, session_id: &str) -> bool {
        self.children().contains_key(session_id)
    }

    /// A child's settings (as created; the agent is already applied).
    pub fn child_settings(&self, session_id: &str) -> Option<SessionSettings> {
        self.children().get(session_id).map(|c| c.settings.clone())
    }

    /// The session's settings: a child's own, else the base source's.
    pub fn settings(&self, session_id: &str) -> Option<SessionSettings> {
        self.child_settings(session_id)
            .or_else(|| self.base.settings(session_id))
    }

    /// 0 for a main session.
    pub fn depth(&self, session_id: &str) -> u32 {
        self.children().get(session_id).map_or(0, |c| c.depth)
    }

    /// `subagents.maxDepth` (settings, then the policy cap), 1..=3.
    pub fn max_depth(&self, s: &SessionSettings) -> u32 {
        let mut d = self.cfg.max_depth;
        if let Some(hub) = &self.config {
            let root = s.workspace.as_deref().map(project_root);
            if let Some(v) = hub
                .value(root.as_deref(), "subagents.maxDepth")
                .and_then(|v| v.as_u64())
            {
                d = u32::try_from(v).unwrap_or(MAX_DEPTH_CAP);
            }
            if let Some(cap) = hub
                .policy()
                .get("subagents.maxDepth")
                .and_then(serde_json::Value::as_u64)
            {
                d = d.min(u32::try_from(cap).unwrap_or(MAX_DEPTH_CAP));
            }
        }
        d.clamp(1, MAX_DEPTH_CAP)
    }

    /// Whether `session_id` may call `task` (Code, below the depth limit).
    pub fn may_delegate(&self, session_id: &str) -> bool {
        if self.engine.get().is_none() {
            return false;
        }
        let Some(s) = self.settings(session_id) else {
            return false;
        };
        s.kind == SessionKind::Code && self.depth(session_id) < self.max_depth(&s)
    }

    /// Children of `parent` still running or queued.
    pub fn running(&self, parent: &str) -> Vec<String> {
        let mut v: Vec<String> = self
            .children()
            .iter()
            .filter(|(_, c)| c.parent == parent && c.running)
            .map(|(k, _)| k.clone())
            .collect();
        v.sort();
        v
    }

    pub fn has_running(&self, parent: &str) -> bool {
        self.children()
            .values()
            .any(|c| c.parent == parent && c.running)
    }

    /// `session.interrupt {stop_subagents: true}`: interrupt every running child of
    /// `parent`; their reports are dropped (the parent was stopped). Returns how many.
    pub fn stop_children(&self, parent: &str) -> usize {
        let g = self.children();
        let mut n = 0;
        for c in g.values().filter(|c| c.parent == parent && c.running) {
            c.stop.cancel();
            n += 1;
        }
        n
    }

    /// A session actor ended. Deleting a parent closes its children (t19); a deleted
    /// child is forgotten. Eviction keeps everything (state lives in the store).
    pub fn session_ended(&self, id: &str, evicted: bool) {
        if evicted {
            return;
        }
        let kids: Vec<String> = {
            let mut g = self.children();
            g.remove(id);
            let kids: Vec<String> = g
                .iter()
                .filter(|(_, c)| c.parent == id)
                .map(|(k, _)| k.clone())
                .collect();
            for k in &kids {
                if let Some(c) = g.get(k) {
                    c.stop.cancel();
                }
            }
            kids
        };
        self.parents
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(id);
        for k in kids {
            if let Some(r) = self.registrar.get() {
                r.unregister(&k);
            }
            if let Some(e) = self.engine.get() {
                e.close_session(&k);
            }
        }
    }

    /// The parent turn ended with children still running (D47): wait until one of them
    /// reports. Returns when the running set changed.
    pub async fn wait_change(&self, turn: &TurnCtx) -> Result<(), TurnError> {
        let (_, v) = self.parent_state(&turn.session_id);
        let mut rx = v.subscribe();
        rx.borrow_and_update();
        if !self.has_running(&turn.session_id) {
            return Ok(());
        }
        tokio::select! {
            r = rx.changed() => { let _ = r; Ok(()) }
            () = turn.token().cancelled() => Err(TurnError::Cancelled),
        }
    }

    /// `waiting_for_children` on the parent (ephemeral `status`).
    pub fn show_waiting(turn: &TurnCtx, on: bool) {
        turn.publish_ephemeral(SessionEvent::Status(StatusEvent {
            state: LiveStatus::Running,
            activity: on.then_some(Activity::WaitingForChildren),
        }));
    }

    /// Final assistant text of the child's active branch.
    fn final_text(&self, child: &str) -> String {
        let Ok(path) = self.conversation.path(child) else {
            return String::new();
        };
        path.iter()
            .rev()
            .find(|m| {
                m.message.role == Role::Assistant
                    && m.message.parts.iter().any(|p| matches!(p, Part::Text(_)))
            })
            .map(|m| {
                m.message
                    .parts
                    .iter()
                    .filter_map(|p| match p {
                        Part::Text(t) => Some(t.text.as_str()),
                        _ => None,
                    })
                    .collect::<Vec<_>>()
                    .join("\n")
            })
            .unwrap_or_default()
    }

    /// The `task` tool body.
    async fn start(self: &Arc<Self>, input: TaskInput, ctx: &ToolCtx) -> Result<Value, ToolError> {
        let engine = self
            .engine
            .get()
            .cloned()
            .ok_or_else(|| ToolError::failed("subagents are not available in this engine"))?;
        let parent_id = ctx.env.session_id.clone();
        let parent_settings = self
            .settings(&parent_id)
            .ok_or_else(|| ToolError::failed("no settings for this session"))?;
        if !self.may_delegate(&parent_id) {
            return Err(ToolError::failed(
                "this agent can't start subagents (depth limit)",
            ));
        }
        let (child_id, agent, stop) = match input.session_id.as_deref().filter(|s| !s.is_empty()) {
            Some(cid) => {
                let mut g = self.children();
                let c = g
                    .get_mut(cid)
                    .filter(|c| c.parent == parent_id)
                    .ok_or_else(|| {
                        ToolError::invalid_input(vec![format!(
                            "`session_id` {cid} is not a subagent of this session"
                        )])
                    })?;
                if c.running {
                    return Err(ToolError::failed(format!(
                        "subagent {cid} is still running; its report arrives automatically (use task_wait to block)"
                    )));
                }
                c.running = true;
                c.stop = CancellationToken::new();
                (cid.to_string(), c.agent.clone(), c.stop.clone())
            }
            None => {
                let name = input.agent.trim();
                let def = self
                    .agents
                    .get(name, parent_settings.workspace.as_deref())
                    .ok_or_else(|| {
                        let names: Vec<String> = self
                            .agents
                            .list(parent_settings.workspace.as_deref())
                            .into_iter()
                            .map(|d| d.name)
                            .collect();
                        ToolError::invalid_input(vec![format!(
                            "unknown agent `{name}`; available: {}",
                            names.join(", ")
                        )])
                    })?;
                let mut s = parent_settings.clone();
                s.kind = SessionKind::Code;
                s.agent = Some(def.name.clone());
                s.title = Some(input.description.clone());
                s.mode = narrow_mode(ctx.env.mode, def.permission_mode);
                AgentCatalog::apply(&def, &mut s, self.models.as_ref());
                if let Some(m) = input.model.as_deref().filter(|m| !m.trim().is_empty()) {
                    s.model = m.trim().to_string();
                }
                let h = engine.create_session();
                let id = h.id().to_string();
                let stop = CancellationToken::new();
                let depth = self.depth(&parent_id) + 1;
                self.children().insert(
                    id.clone(),
                    Child {
                        parent: parent_id.clone(),
                        agent: def.name.clone(),
                        depth,
                        settings: s.clone(),
                        running: true,
                        stop: stop.clone(),
                        last: None,
                    },
                );
                if let Some(r) = self.registrar.get() {
                    r.register(&id, &parent_id, &s);
                }
                if let Err(e) = h.publish_wire(SessionEvent::SessionCreated(SessionCreated {
                    kind: s.kind,
                    project_id: s.project_id.clone(),
                    workspace_id: None,
                    model: s.model.clone(),
                    effort: s.effort.clone(),
                    mode: s.mode,
                    agent: s.agent.clone(),
                    parent_session_id: Some(parent_id.clone()),
                })) {
                    tracing::error!(error = %e, "publish child session.created failed");
                }
                (id, def.name, stop)
            }
        };
        ctx.events
            .publish(SessionEvent::SubagentStarted(SubagentStarted {
                child_session_id: child_id.clone(),
                agent: agent.clone(),
                description: input.description.clone(),
            }));
        let (sem, _) = self.parent_state(&parent_id);
        let queued = sem.available_permits() == 0;
        self.bump(&parent_id);
        let hub = self.clone();
        let (cid, pid, ag, prompt) = (
            child_id.clone(),
            parent_id.clone(),
            agent.clone(),
            input.prompt.clone(),
        );
        self.scope.spawn(async move {
            hub.supervise(engine, pid, cid, ag, prompt, stop).await;
        });
        Ok(json!({
            "status": "running",
            "session_id": child_id,
            "agent": agent,
            "queued": queued,
            "note": "The subagent runs in the background. Its report arrives automatically as a <subagent> message; don't poll. Use task_wait only if you need the result before you can continue.",
        }))
    }

    /// Run one child turn and report back (see the module docs).
    async fn supervise(
        self: Arc<Self>,
        engine: Engine,
        parent: String,
        child: String,
        agent: String,
        prompt: String,
        stop: CancellationToken,
    ) {
        let (sem, _) = self.parent_state(&parent);
        let permit = tokio::select! {
            p = sem.acquire_owned() => p.ok(),
            () = stop.cancelled() => None,
        };
        let mut status = TurnStatus::Cancelled;
        let mut cost = 0.0;
        let mut error: Option<String> = None;
        let stopped_early = permit.is_none();
        if !stopped_early {
            match self
                .run_child(&engine, &parent, &child, &agent, prompt, &stop)
                .await
            {
                Ok((s, c, e)) => {
                    status = s;
                    cost = c;
                    error = e;
                }
                Err(e) => {
                    status = TurnStatus::Failed;
                    error = Some(e);
                }
            }
        }
        let stopped = stop.is_cancelled();
        let mut text = self.final_text(&child);
        if text.trim().is_empty()
            && let Some(e) = &error
        {
            text = format!("The subagent failed: {e}");
        }
        let parent_h = engine.session(&parent).ok();
        if !stopped && let Some(ph) = &parent_h {
            // `subagent.end` hooks (Code) may add context to the report.
            if let Some(hooks) = &self.hooks
                && let Some(ps) = self.settings(&parent)
            {
                let specs = hooks.hooks_for(&ps);
                if specs.iter().any(|h| h.event == HookEvent::SubagentEnd) {
                    let mut hctx = HookCtx::for_session(&parent, &ps);
                    hctx.agent.clone_from(&agent);
                    let ph2 = ph.clone();
                    let publish = move |r| {
                        let _ = ph2.publish_wire(SessionEvent::HookRan(r));
                    };
                    let o = hooks
                        .run(
                            &specs,
                            &hctx,
                            HookEvent::SubagentEnd,
                            json!({ "child_session_id": child, "agent": agent,
                                    "status": status_word(status), "output": text }),
                            &HookTarget::default(),
                            &CancellationToken::new(),
                            &publish,
                        )
                        .await;
                    if let Some(c) = o.context_text(HookEvent::SubagentEnd) {
                        text = format!("{text}\n\n{c}");
                    }
                }
            }
            let report = prompts::render(
                fragments::SUBAGENT_REPORT.trim_end(),
                &[
                    ("name", &agent),
                    ("session", &child),
                    ("status", status_word(status)),
                    ("text", text.trim()),
                ],
                &[],
            );
            // Outbox: persisted first; marked delivered once the parent's inbox has it.
            let saved = self.save_report(&parent, &child, &report);
            match ph.push_synthetic("subagent", report, Delivery::Steer) {
                Ok(_) => self.mark_delivered(saved),
                Err(e) => {
                    tracing::warn!(error = %e, child = %child, "subagent report not delivered");
                }
            }
        }
        if let Some(ph) = &parent_h
            && let Err(e) = ph.publish_wire(SessionEvent::SubagentEnded(SubagentEnded {
                child_session_id: child.clone(),
                status,
                summary: (!text.trim().is_empty()).then(|| text.trim().to_string()),
                cost: Cost { usd: cost },
            }))
        {
            tracing::error!(error = %e, "publish subagent.ended failed");
        }
        if let Some(c) = self.children().get_mut(&child) {
            c.running = false;
            c.last = Some(status);
        }
        drop(permit);
        self.bump(&parent);
    }

    /// Prompt the child and wait for its turn to settle: `(status, cost, error)`.
    async fn run_child(
        &self,
        engine: &Engine,
        parent: &str,
        child: &str,
        agent: &str,
        prompt: String,
        stop: &CancellationToken,
    ) -> Result<(TurnStatus, f64, Option<String>), String> {
        let ch = engine.session(child).map_err(|d| d.to_string())?;
        let ph = engine.session(parent).map_err(|d| d.to_string())?;
        ch.set_ask_relay(ph, agent);
        let bus = engine.bus();
        let head = bus.head_seq(child).map_err(|d| d.to_string())?;
        let mut sub = bus.subscribe(child, head).map_err(|d| d.to_string())?;
        ch.prompt_parts(vec![InputPart::text(prompt)], Delivery::Queue)
            .map_err(|e| e.to_string())?;
        let mut cost = 0.0;
        let mut interrupted = false;
        loop {
            let item = tokio::select! {
                i = sub.next() => i,
                () = stop.cancelled(), if !interrupted => {
                    interrupted = true;
                    ch.interrupt();
                    continue;
                }
            };
            match item {
                Some(SubItem::Event(e)) => match e.to_wire().event {
                    SessionEvent::StepEnded(s) => cost += s.cost.usd,
                    SessionEvent::TurnSettled(t) => {
                        return Ok((t.status, cost, t.error.map(|e| e.message)));
                    }
                    _ => {}
                },
                Some(SubItem::Synced) => {}
                Some(SubItem::ResyncRequired { from_seq }) => {
                    sub = bus.subscribe(child, from_seq).map_err(|d| d.to_string())?;
                }
                None => return Err("the subagent session closed".into()),
            }
        }
    }

    /// `task_wait`: block until the named children (default: all running) finish.
    async fn wait(
        &self,
        parent: &str,
        ids: Option<Vec<String>>,
        cancel: &CancellationToken,
    ) -> Result<Vec<ChildStatus>, ToolError> {
        let ids: Vec<String> = match ids.filter(|v| !v.is_empty()) {
            Some(v) => {
                let g = self.children();
                for id in &v {
                    if g.get(id).is_none_or(|c| c.parent != parent) {
                        return Err(ToolError::invalid_input(vec![format!(
                            "{id} is not a subagent of this session"
                        )]));
                    }
                }
                v
            }
            None => self.running(parent),
        };
        let (_, v) = self.parent_state(parent);
        loop {
            let mut rx = v.subscribe();
            rx.borrow_and_update();
            let still = {
                let g = self.children();
                ids.iter().any(|id| g.get(id).is_some_and(|c| c.running))
            };
            if !still {
                break;
            }
            tokio::select! {
                r = rx.changed() => { let _ = r; }
                () = cancel.cancelled() => return Err(ToolError::Cancelled),
            }
        }
        let g = self.children();
        Ok(ids
            .iter()
            .filter_map(|id| {
                g.get(id).map(|c| ChildStatus {
                    session_id: id.clone(),
                    agent: c.agent.clone(),
                    running: c.running,
                    status: c.last,
                })
            })
            .collect())
    }
}

/// The session registry seen through the hub: children first (01 §3 port).
pub struct WithChildren(pub Arc<SubagentHub>);

impl SettingsSource for WithChildren {
    fn settings(&self, session_id: &str) -> Option<SessionSettings> {
        self.0.settings(session_id)
    }

    fn set_title(&self, session_id: &str, title: &str) {
        self.0.base.set_title(session_id, title);
    }
}

// ------------------------------------------------------------------ tools

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct TaskInput {
    /// The agent to run (see the list in the description).
    pub agent: String,
    /// 3–7 words describing the sub-task (shown to the user).
    pub description: String,
    /// The full instructions for the subagent. It sees nothing else of this conversation.
    pub prompt: String,
    /// Optional catalog model id overriding the agent's model.
    #[serde(default)]
    pub model: Option<String>,
    /// Continue an existing subagent (its context is kept) instead of starting a new one.
    #[serde(default)]
    pub session_id: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct TaskWaitInput {
    /// Subagent session ids to wait for; default: every running subagent of this session.
    #[serde(default)]
    pub session_ids: Option<Vec<String>>,
}

/// `task` (06 §11).
pub struct TaskTool {
    hub: Arc<SubagentHub>,
    schema: Value,
}

impl TaskTool {
    pub fn new(hub: Arc<SubagentHub>) -> TaskTool {
        TaskTool {
            hub,
            schema: schema_of::<TaskInput>(),
        }
    }
}

#[async_trait]
impl Tool for TaskTool {
    fn name(&self) -> &str {
        "task"
    }

    fn description(&self, env: &ToolEnv) -> Cow<'_, str> {
        let ws = self.hub.settings(&env.session_id).and_then(|s| s.workspace);
        let mut d = String::from(
            "Start a subagent for a broad search or an independent sub-task. It runs in the background in its own session; this call returns at once with its session_id. Its final report arrives automatically as a <subagent> message, so don't poll. Give it a complete prompt: it sees nothing else of this conversation. Pass session_id to send a follow-up to an earlier subagent.\n\nAgents:",
        );
        for a in self.hub.agents.list(ws.as_deref()) {
            d.push_str(&format!("\n- {}: {}", a.name, a.description));
        }
        Cow::Owned(d)
    }

    fn input_schema(&self) -> &Value {
        &self.schema
    }

    fn kind(&self) -> ToolKind {
        // Starting a child asks nothing by itself; the child asks for its own calls.
        ToolKind::ReadOnly
    }

    fn availability(&self, env: &ToolEnv) -> Availability {
        if env.kind == SessionKind::Code && self.hub.may_delegate(&env.session_id) {
            Availability::Available
        } else {
            Availability::Unavailable("subagents run in Code sessions below the depth limit".into())
        }
    }

    async fn execute(&self, input: Value, ctx: ToolCtx) -> Result<ToolOutput, ToolError> {
        let input: TaskInput = parse_input(input, &self.schema)?;
        self.hub.start(input, &ctx).await.map(ToolOutput::Json)
    }
}

/// `task_wait` (06 §11).
pub struct TaskWaitTool {
    hub: Arc<SubagentHub>,
    schema: Value,
}

impl TaskWaitTool {
    pub fn new(hub: Arc<SubagentHub>) -> TaskWaitTool {
        TaskWaitTool {
            hub,
            schema: schema_of::<TaskWaitInput>(),
        }
    }
}

#[async_trait]
impl Tool for TaskWaitTool {
    fn name(&self) -> &str {
        "task_wait"
    }

    fn description(&self, _env: &ToolEnv) -> Cow<'_, str> {
        Cow::Borrowed(
            "Block until the given subagents (default: all running ones) have finished. Use it only when you need their reports before you can continue; the reports arrive as <subagent> messages right after this call.",
        )
    }

    fn input_schema(&self) -> &Value {
        &self.schema
    }

    fn kind(&self) -> ToolKind {
        ToolKind::ReadOnly
    }

    fn availability(&self, env: &ToolEnv) -> Availability {
        if env.kind == SessionKind::Code && self.hub.may_delegate(&env.session_id) {
            Availability::Available
        } else {
            Availability::Unavailable("subagents run in Code sessions below the depth limit".into())
        }
    }

    async fn execute(&self, input: Value, ctx: ToolCtx) -> Result<ToolOutput, ToolError> {
        let input: TaskWaitInput = parse_input(input, &self.schema)?;
        let done = self
            .hub
            .wait(&ctx.env.session_id, input.session_ids, &ctx.cancel)
            .await?;
        if done.is_empty() {
            return Ok(ToolOutput::text("No subagents are running."));
        }
        let lines: Vec<String> = done
            .iter()
            .map(|c| {
                format!(
                    "- {} ({}): {}",
                    c.session_id,
                    c.agent,
                    if c.running {
                        "running"
                    } else {
                        c.status.map_or("finished", status_word)
                    }
                )
            })
            .collect();
        Ok(ToolOutput::text(format!(
            "Finished. Reports arrive as <subagent> messages.\n{}",
            lines.join("\n")
        )))
    }
}
