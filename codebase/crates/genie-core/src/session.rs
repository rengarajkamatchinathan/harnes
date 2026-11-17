//! Session actor (01 §5).
//!
//! One tokio task per open session drains a durable inbox; no two turns of one session
//! ever run at once. State machine (01 §5.1):
//!
//! ```text
//! Idle ──admit──▶ Running ──ask──▶ AwaitingAsk ──no client──▶ Paused
//!   ▲               │  ▲ step boundary: steer items injected      │
//!   │               │  └──────────── answered ◀───────────────────┘
//!   │               ├──▶ Compacting ──▶ Running
//!   ├── settled ────┤
//!   │               └─interrupt─▶ Interrupting ──cleanup──▶ Idle
//!   └── new prompt ◀── Interrupted ◀── daemon crash/shutdown;  Resume ──▶ Running
//! ```

use std::collections::{HashMap, VecDeque};
use std::panic::AssertUnwindSafe;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use futures::FutureExt;
use futures::future::BoxFuture;
use genie_protocol::handshake::AskKind;
use genie_protocol::session::{Delivery, LiveStatus, TurnFinish};
use serde_json::Value;
use tokio::sync::{Notify, oneshot, watch};
use tokio_util::sync::CancellationToken;

use crate::bus::{AskOutcome, Bus, CoreEvent};
use crate::error::{CoreError, Defect, TurnError};
use crate::ids::{AskId, InboxItemId, SessionId, TurnId, new_id};
use crate::task::TaskScope;

#[derive(Debug, Clone, PartialEq)]
pub enum InboxKind {
    User,
    /// Background shell completion (D45), subagent report (D47), "Fix with Genie" (D28),
    /// hook `turn.end` continuation (D48).
    Synthetic {
        source: String,
    },
    /// User-initiated resume of an interrupted turn (D20).
    Resume {
        turn_id: TurnId,
    },
}

#[derive(Debug, Clone, PartialEq)]
pub struct InboxItem {
    pub id: InboxItemId,
    pub kind: InboxKind,
    pub delivery: Delivery,
    pub text: String,
}

/// Runs one turn. Implemented by the turn loop (spec 06); tests use fakes.
///
/// The runner must watch [`TurnCtx::token`], call [`TurnCtx::step_boundary`] between
/// steps to pick up steer items, and spawn tool work on [`TurnCtx::scope`] children.
pub trait TurnRunner: Send + Sync + 'static {
    fn run(&self, turn: TurnCtx) -> BoxFuture<'static, Result<(), TurnError>>;
}

/// Raises an OS notification when an ask waits with no client attached (D22).
pub trait Notifier: Send + Sync + 'static {
    fn ask_paused(&self, session_id: &str, ask_id: &str, kind: AskKind);
}

pub struct LogNotifier;

impl Notifier for LogNotifier {
    fn ask_paused(&self, session_id: &str, ask_id: &str, kind: AskKind) {
        tracing::info!(
            session_id,
            ask_id,
            ?kind,
            "ask waiting with no client attached"
        );
    }
}

pub(crate) struct Deps {
    pub runner: Arc<dyn TurnRunner>,
    pub bus: Arc<Bus>,
    pub notifier: Arc<dyn Notifier>,
}

struct CurrentTurn {
    token: CancellationToken,
    interrupt_requested: bool,
}

struct PendingAsk {
    turn_id: TurnId,
    tx: oneshot::Sender<Value>,
}

struct State {
    inbox: VecDeque<InboxItem>,
    current: Option<CurrentTurn>,
    asks: HashMap<AskId, PendingAsk>,
    interrupted_turn: Option<TurnId>,
    last_activity: Instant,
}

struct Shared {
    id: SessionId,
    deps: Arc<Deps>,
    scope: TaskScope,
    state: Mutex<State>,
    doorbell: Notify,
    status_tx: watch::Sender<LiveStatus>,
    subscribers: AtomicUsize,
}

/// A cheap, cloneable handle to a session actor.
#[derive(Clone)]
pub struct SessionHandle {
    sh: Arc<Shared>,
}

/// Keeps a client counted as attached (subscribed) while alive.
pub struct AttachGuard {
    sh: Arc<Shared>,
}

impl Drop for AttachGuard {
    fn drop(&mut self) {
        self.sh.subscribers.fetch_sub(1, Ordering::SeqCst);
        self.sh.touch();
    }
}

impl SessionHandle {
    /// Create the actor. `interrupted_turn` comes from the journal (01 §4.3).
    pub(crate) fn spawn(
        id: SessionId,
        deps: Arc<Deps>,
        parent: &TaskScope,
        interrupted_turn: Option<TurnId>,
    ) -> SessionHandle {
        let initial = if interrupted_turn.is_some() {
            LiveStatus::Interrupted
        } else {
            LiveStatus::Idle
        };
        let (status_tx, _) = watch::channel(initial);
        let sh = Arc::new(Shared {
            id,
            deps,
            scope: parent.child("session"),
            state: Mutex::new(State {
                inbox: VecDeque::new(),
                current: None,
                asks: HashMap::new(),
                interrupted_turn,
                last_activity: Instant::now(),
            }),
            doorbell: Notify::new(),
            status_tx,
            subscribers: AtomicUsize::new(0),
        });
        let actor = sh.clone();
        sh.scope.spawn(async move {
            let id = actor.id.clone();
            if let Err(p) = AssertUnwindSafe(actor_loop(actor)).catch_unwind().await {
                let d = Defect::from_panic(&*p);
                tracing::error!(session_id = %id, incident_id = %d.incident_id, "session actor died");
            }
        });
        SessionHandle { sh }
    }

    pub fn id(&self) -> &str {
        &self.sh.id
    }

    /// Write an inbox item and ring the doorbell. Returns once the item is committed
    /// (01 §5.2).
    pub fn prompt(
        &self,
        text: impl Into<String>,
        delivery: Delivery,
    ) -> Result<InboxItemId, CoreError> {
        self.push(InboxKind::User, text.into(), delivery)
    }

    /// Synthetic items share the inbox (01 §5.2).
    pub fn push_synthetic(
        &self,
        source: impl Into<String>,
        text: impl Into<String>,
        delivery: Delivery,
    ) -> Result<InboxItemId, CoreError> {
        self.push(
            InboxKind::Synthetic {
                source: source.into(),
            },
            text.into(),
            delivery,
        )
    }

    fn push(
        &self,
        kind: InboxKind,
        text: String,
        delivery: Delivery,
    ) -> Result<InboxItemId, CoreError> {
        if self.sh.scope.is_cancelled() {
            return Err(CoreError::InvalidState("session is closed".into()));
        }
        let item = InboxItem {
            id: new_id("inb"),
            kind,
            delivery,
            text,
        };
        let id = item.id.clone();
        {
            let mut st = self.sh.lock();
            st.inbox.push_back(item);
            st.last_activity = Instant::now();
        }
        self.sh.publish(CoreEvent::InboxAdded {
            item_id: id.clone(),
            delivery,
        })?;
        self.sh.doorbell.notify_one();
        Ok(id)
    }

    /// Remove a waiting (not yet admitted) item (`session.inbox.remove`).
    pub fn remove_queued(&self, item_id: &str) -> Result<(), CoreError> {
        let removed = {
            let mut st = self.sh.lock();
            let pos = st.inbox.iter().position(|i| i.id == item_id);
            pos.and_then(|p| st.inbox.remove(p))
        };
        match removed {
            Some(_) => {
                self.sh.publish(CoreEvent::InboxRemoved {
                    item_id: item_id.to_string(),
                })?;
                Ok(())
            }
            None => Err(CoreError::NotFound(format!("inbox item {item_id}"))),
        }
    }

    /// Cancel the running turn. Returns immediately; cleanup is async (01 §5.3).
    /// Returns `false` when no turn is running.
    pub fn interrupt(&self) -> bool {
        let cancelled = {
            let mut st = self.sh.lock();
            match st.current.as_mut() {
                Some(cur) => {
                    cur.interrupt_requested = true;
                    cur.token.cancel();
                    true
                }
                None => false,
            }
        };
        if cancelled {
            self.sh.set_status(LiveStatus::Interrupting);
        }
        cancelled
    }

    /// Resume an interrupted turn (user-initiated only, D20).
    pub fn resume(&self) -> Result<(), CoreError> {
        let turn_id = {
            let mut st = self.sh.lock();
            let Some(turn_id) = st.interrupted_turn.clone() else {
                return Err(CoreError::InvalidState("session is not interrupted".into()));
            };
            if st.current.is_some()
                || st
                    .inbox
                    .iter()
                    .any(|i| matches!(i.kind, InboxKind::Resume { .. }))
            {
                return Err(CoreError::InvalidState("a turn is already running".into()));
            }
            let item = InboxItem {
                id: new_id("inb"),
                kind: InboxKind::Resume {
                    turn_id: turn_id.clone(),
                },
                delivery: Delivery::Queue,
                text: String::new(),
            };
            st.inbox.push_front(item);
            turn_id
        };
        tracing::debug!(session_id = %self.sh.id, %turn_id, "resume requested");
        self.sh.doorbell.notify_one();
        Ok(())
    }

    /// Answer a pending ask from any client.
    pub fn answer_ask(&self, ask_id: &str, answer: Value) -> Result<(), CoreError> {
        let pending = self.sh.lock().asks.remove(ask_id);
        let Some(p) = pending else {
            return Err(CoreError::NotFound(format!("ask {ask_id}")));
        };
        if p.tx.send(answer).is_err() {
            return Err(CoreError::Conflict(format!(
                "ask {ask_id} is no longer waiting"
            )));
        }
        self.sh.publish(CoreEvent::AskResolved {
            ask_id: ask_id.to_string(),
            outcome: AskOutcome::Answered,
        })?;
        Ok(())
    }

    pub fn status(&self) -> LiveStatus {
        *self.sh.status_tx.borrow()
    }

    pub fn watch_status(&self) -> watch::Receiver<LiveStatus> {
        self.sh.status_tx.subscribe()
    }

    /// Count a client as attached. A paused ask becomes `AwaitingAsk` again.
    pub fn attach(&self) -> AttachGuard {
        self.sh.subscribers.fetch_add(1, Ordering::SeqCst);
        self.sh.touch();
        if self.status() == LiveStatus::Paused {
            self.sh.set_status(LiveStatus::AwaitingAsk);
        }
        AttachGuard {
            sh: self.sh.clone(),
        }
    }

    pub fn pending_inbox(&self) -> Vec<InboxItem> {
        self.sh.lock().inbox.iter().cloned().collect()
    }

    pub fn pending_asks(&self) -> usize {
        self.sh.lock().asks.len()
    }

    pub(crate) fn has_turn(&self) -> bool {
        self.sh.lock().current.is_some()
    }

    /// Evictable: no turn, nothing waiting, no subscribers, quiet for `ttl` (01 §5.5).
    pub(crate) fn evictable(&self, ttl: Duration) -> bool {
        let st = self.sh.lock();
        st.current.is_none()
            && st.inbox.is_empty()
            && st.asks.is_empty()
            && self.sh.subscribers.load(Ordering::SeqCst) == 0
            && st.last_activity.elapsed() >= ttl
    }

    /// Stop the actor (eviction or daemon shutdown). A running turn becomes interrupted.
    pub(crate) fn close(&self) {
        self.sh.scope.cancel();
    }
}

impl Shared {
    fn lock(&self) -> MutexGuard<'_, State> {
        // Never held across an await; poisoning would only follow a panic in this module.
        self.state.lock().unwrap_or_else(|p| p.into_inner())
    }

    fn touch(&self) {
        self.lock().last_activity = Instant::now();
    }

    fn set_status(&self, s: LiveStatus) {
        let changed = self.status_tx.send_replace(s) != s;
        if changed {
            self.deps
                .bus
                .publish_ephemeral(&self.id, CoreEvent::Status { status: s });
        }
    }

    fn publish(&self, ev: CoreEvent) -> Result<(), Defect> {
        self.deps.bus.publish(&self.id, ev).map(|_| ())
    }

    fn publish_logged(&self, ev: CoreEvent) {
        if let Err(d) = self.publish(ev) {
            tracing::error!(session_id = %self.id, incident_id = %d.incident_id, "publish failed");
        }
    }
}

async fn actor_loop(sh: Arc<Shared>) {
    let token = sh.scope.token().clone();
    loop {
        if token.is_cancelled() {
            break;
        }
        let next = sh.lock().inbox.pop_front();
        match next {
            Some(item) => run_turn(&sh, item).await,
            None => {
                tokio::select! {
                    () = sh.doorbell.notified() => {}
                    () = token.cancelled() => break,
                }
            }
        }
    }
}

async fn run_turn(sh: &Arc<Shared>, item: InboxItem) {
    let (turn_id, resume) = match &item.kind {
        InboxKind::Resume { turn_id } => {
            if sh.lock().interrupted_turn.as_deref() != Some(turn_id.as_str()) {
                return; // a new prompt superseded it
            }
            (turn_id.clone(), true)
        }
        _ => (new_id("trn"), false),
    };
    let journal = sh.deps.bus.journal().clone();
    let turn_scope = sh.scope.child("turn");
    {
        let mut st = sh.lock();
        // A new prompt leaves the old turn interrupted; a resume picks it up.
        st.interrupted_turn = None;
        st.current = Some(CurrentTurn {
            token: turn_scope.token().clone(),
            interrupt_requested: false,
        });
        st.last_activity = Instant::now();
    }
    if let Err(d) = journal.claim_turn(&sh.id, &turn_id) {
        tracing::error!(incident_id = %d.incident_id, "claim_turn failed");
    }
    sh.set_status(LiveStatus::Running);
    sh.publish_logged(CoreEvent::TurnStarted {
        turn_id: turn_id.clone(),
        item_id: item.id.clone(),
        resume,
    });

    let ctx = TurnCtx {
        session_id: sh.id.clone(),
        turn_id: turn_id.clone(),
        input: item,
        resume,
        scope: turn_scope.clone(),
        sh: sh.clone(),
    };
    let result = AssertUnwindSafe(sh.deps.runner.run(ctx))
        .catch_unwind()
        .await;

    let interrupt_requested = {
        let mut st = sh.lock();
        let cur = st.current.take();
        st.last_activity = Instant::now();
        cur.is_some_and(|c| c.interrupt_requested)
    };
    let session_closing = sh.scope.is_cancelled();
    // Stop anything the turn left behind (tool tasks, process trees, subagents).
    turn_scope.cancel();

    let (finish, error_code) = match result {
        Err(panic) => {
            // Panics are caught per session; the daemon and other sessions live (01 §4.3).
            let d = Defect::from_panic(&*panic);
            (
                TurnFinish::Failed,
                Some(format!("GENIE-INTERNAL:{}", d.incident_id)),
            )
        }
        Ok(_) if session_closing => (TurnFinish::Interrupted, None),
        Ok(_) if interrupt_requested => (TurnFinish::Cancelled, None),
        Ok(Ok(())) => (TurnFinish::Completed, None),
        Ok(Err(TurnError::Cancelled)) => (TurnFinish::Cancelled, None),
        Ok(Err(e)) => (TurnFinish::Failed, Some(e.code().to_string())),
    };

    // Pending asks of this turn resolve as cancelled, not declined (01 §5.3).
    let orphaned: Vec<AskId> = {
        let mut st = sh.lock();
        let ids: Vec<AskId> = st
            .asks
            .iter()
            .filter(|(_, a)| a.turn_id == turn_id)
            .map(|(k, _)| k.clone())
            .collect();
        for id in &ids {
            st.asks.remove(id);
        }
        ids
    };
    for ask_id in orphaned {
        sh.publish_logged(CoreEvent::AskResolved {
            ask_id,
            outcome: AskOutcome::Cancelled,
        });
    }

    if let Err(d) = journal.settle_turn(&sh.id, &turn_id, finish) {
        tracing::error!(incident_id = %d.incident_id, "settle_turn failed");
    }
    sh.publish_logged(CoreEvent::TurnSettled {
        turn_id: turn_id.clone(),
        finish,
        error_code,
    });
    if finish == TurnFinish::Interrupted {
        sh.publish_logged(CoreEvent::TurnInterrupted {
            turn_id: turn_id.clone(),
        });
        sh.lock().interrupted_turn = Some(turn_id);
        sh.set_status(LiveStatus::Interrupted);
    } else {
        sh.set_status(LiveStatus::Idle);
    }
}

/// What a [`TurnRunner`] gets for one turn.
pub struct TurnCtx {
    pub session_id: SessionId,
    pub turn_id: TurnId,
    /// The admitted inbox item.
    pub input: InboxItem,
    /// Resuming an interrupted turn: completed tool results are kept, unsettled calls are
    /// failed with `interrupted` (01 §5.4).
    pub resume: bool,
    scope: TaskScope,
    sh: Arc<Shared>,
}

impl TurnCtx {
    /// The turn token. Cancelled by interrupt, session close or daemon shutdown.
    pub fn token(&self) -> &CancellationToken {
        self.scope.token()
    }

    /// The turn scope: make `child("step")` / `child("tool")` scopes from it.
    pub fn scope(&self) -> &TaskScope {
        &self.scope
    }

    /// Call between steps (after the step's tool calls are joined). Returns the steer
    /// items to inject as user messages, in inbox order (01 §5.2).
    pub fn step_boundary(&self) -> Vec<InboxItem> {
        let taken: Vec<InboxItem> = {
            let mut st = self.sh.lock();
            let (steer, rest): (VecDeque<_>, VecDeque<_>) = st.inbox.drain(..).partition(|i| {
                i.delivery == Delivery::Steer && !matches!(i.kind, InboxKind::Resume { .. })
            });
            st.inbox = rest;
            steer.into()
        };
        if !taken.is_empty() {
            self.sh.publish_logged(CoreEvent::SteerInjected {
                turn_id: self.turn_id.clone(),
                item_ids: taken.iter().map(|i| i.id.clone()).collect(),
            });
        }
        taken
    }

    pub fn set_compacting(&self, on: bool) {
        if !self.token().is_cancelled() {
            self.sh.set_status(if on {
                LiveStatus::Compacting
            } else {
                LiveStatus::Running
            });
        }
    }

    /// Ephemeral live text (token deltas).
    pub fn emit_delta(&self, text: impl Into<String>) {
        self.sh
            .deps
            .bus
            .publish_ephemeral(&self.session_id, CoreEvent::Delta { text: text.into() });
    }

    /// Ask the user. The wait is a `oneshot` raced against the turn token (01 §6).
    /// With no client attached the session shows as `Paused` and an OS notification is
    /// raised; the ask can be answered later from any client.
    pub async fn ask(&self, kind: AskKind) -> Result<Value, TurnError> {
        let ask_id = new_id("ask");
        let (tx, rx) = oneshot::channel();
        self.sh.lock().asks.insert(
            ask_id.clone(),
            PendingAsk {
                turn_id: self.turn_id.clone(),
                tx,
            },
        );
        self.sh
            .publish(CoreEvent::AskCreated {
                ask_id: ask_id.clone(),
                turn_id: self.turn_id.clone(),
                kind,
            })
            .map_err(TurnError::Internal)?;
        if self.sh.subscribers.load(Ordering::SeqCst) == 0 {
            self.sh.set_status(LiveStatus::Paused);
            self.sh
                .deps
                .notifier
                .ask_paused(&self.session_id, &ask_id, kind);
        } else {
            self.sh.set_status(LiveStatus::AwaitingAsk);
        }

        let res = tokio::select! {
            r = rx => r.map_err(|_| TurnError::Cancelled),
            () = self.token().cancelled() => Err(TurnError::Cancelled),
        };

        if res.is_err() && self.sh.lock().asks.remove(&ask_id).is_some() {
            self.sh.publish_logged(CoreEvent::AskResolved {
                ask_id,
                outcome: AskOutcome::Cancelled,
            });
        }
        if !self.token().is_cancelled() && self.sh.lock().asks.is_empty() {
            self.sh.set_status(LiveStatus::Running);
        }
        res
    }
}

/// Default runner until providers are wired (spec 03/06): every turn fails with
/// `no_provider_configured`.
pub struct UnconfiguredRunner;

impl TurnRunner for UnconfiguredRunner {
    fn run(&self, _turn: TurnCtx) -> BoxFuture<'static, Result<(), TurnError>> {
        Box::pin(async {
            Err(TurnError::Provider {
                class: "no_provider_configured".into(),
                fix_link: Some("genie://settings/providers".into()),
            })
        })
    }
}
