//! `EngineBuilder` and the process-wide engine (01 §3). Explicit wiring, no DI framework.

use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use crate::bus::{Bus, CoreEvent};
use crate::dir_context::DirContexts;
use crate::error::Defect;
use crate::ids::{SessionId, new_id};
use crate::journal::{Journal, MemoryJournal, TurnRecord};
use crate::locks::PathLocks;
use crate::session::{Deps, LogNotifier, Notifier, SessionHandle, TurnRunner, UnconfiguredRunner};
use crate::task::TaskScope;
use genie_protocol::session::{LiveStatus, TurnFinish};

/// Actor eviction after this long with no subscribers and no turn (01 §5.5, SA-05).
pub const ACTOR_IDLE_TTL: Duration = Duration::from_secs(15 * 60);

pub struct EngineBuilder {
    runner: Arc<dyn TurnRunner>,
    journal: Arc<dyn Journal>,
    notifier: Arc<dyn Notifier>,
    actor_idle_ttl: Duration,
    eviction_interval: Duration,
}

impl Default for EngineBuilder {
    fn default() -> Self {
        EngineBuilder {
            runner: Arc::new(UnconfiguredRunner),
            journal: Arc::new(MemoryJournal::new()),
            notifier: Arc::new(LogNotifier),
            actor_idle_ttl: ACTOR_IDLE_TTL,
            eviction_interval: Duration::from_secs(60),
        }
    }
}

impl EngineBuilder {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn turn_runner(mut self, r: Arc<dyn TurnRunner>) -> Self {
        self.runner = r;
        self
    }

    /// The store. `genie-store` provides the SQLCipher implementation (spec 04).
    pub fn journal(mut self, j: Arc<dyn Journal>) -> Self {
        self.journal = j;
        self
    }

    pub fn notifier(mut self, n: Arc<dyn Notifier>) -> Self {
        self.notifier = n;
        self
    }

    pub fn actor_idle_ttl(mut self, ttl: Duration, check_every: Duration) -> Self {
        self.actor_idle_ttl = ttl;
        self.eviction_interval = check_every;
        self
    }

    /// Build under the daemon root scope. Starts the actor-eviction background job.
    pub fn build(self, root: &TaskScope) -> Engine {
        let bus = Arc::new(Bus::new(self.journal));
        let inner = Arc::new(Inner {
            scope: root.child("engine"),
            deps: Arc::new(Deps {
                runner: self.runner,
                bus,
                notifier: self.notifier,
            }),
            sessions: Mutex::new(HashMap::new()),
            dirs: DirContexts::new(),
            locks: PathLocks::new(),
            background_jobs: Arc::new(AtomicUsize::new(0)),
            terminals: Arc::new(AtomicUsize::new(0)),
            actor_idle_ttl: self.actor_idle_ttl,
        });
        let engine = Engine { inner };
        let e = engine.clone();
        let every = self.eviction_interval;
        engine
            .inner
            .scope
            .child("jobs")
            .spawn_cancellable(async move {
                loop {
                    tokio::time::sleep(every).await;
                    let n = e.evict_idle();
                    if n > 0 {
                        tracing::debug!(evicted = n, "evicted idle session actors");
                    }
                }
            });
        engine
    }
}

struct Inner {
    scope: TaskScope,
    deps: Arc<Deps>,
    sessions: Mutex<HashMap<SessionId, SessionHandle>>,
    dirs: DirContexts,
    locks: PathLocks,
    background_jobs: Arc<AtomicUsize>,
    terminals: Arc<AtomicUsize>,
    actor_idle_ttl: Duration,
}

#[derive(Clone)]
pub struct Engine {
    inner: Arc<Inner>,
}

/// What keeps the daemon from idling out (01 §4.3).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct BusySummary {
    pub running_turns: u32,
    /// Asks waiting for an answer, including paused ones.
    pub pending_asks: u32,
    pub background_jobs: u32,
    pub terminals: u32,
}

impl BusySummary {
    /// Running or paused turns: drives the version-skew path (01 §4.2).
    pub fn has_turns(&self) -> bool {
        self.running_turns > 0 || self.pending_asks > 0
    }

    /// Anything that blocks idle exit.
    pub fn keeps_alive(&self) -> bool {
        self.has_turns() || self.background_jobs > 0 || self.terminals > 0
    }
}

/// Counts a background shell job or terminal while alive.
pub struct WorkGuard(Arc<AtomicUsize>);

impl Drop for WorkGuard {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}

impl Engine {
    fn sessions(&self) -> MutexGuard<'_, HashMap<SessionId, SessionHandle>> {
        self.inner
            .sessions
            .lock()
            .unwrap_or_else(|p| p.into_inner())
    }

    /// Create a new session actor.
    pub fn create_session(&self) -> SessionHandle {
        let id = new_id("ses");
        let h = SessionHandle::spawn(id.clone(), self.inner.deps.clone(), &self.inner.scope, None);
        self.sessions().insert(id, h.clone());
        h
    }

    /// The actor for `id` if it is loaded (no journal access, no spawn).
    pub fn loaded(&self, id: &str) -> Option<SessionHandle> {
        self.sessions().get(id).cloned()
    }

    /// Close and drop the actor for `id` (`session.delete`).
    pub fn close_session(&self, id: &str) {
        if let Some(h) = self.sessions().remove(id) {
            h.close();
        }
    }

    /// Get the actor for `id`, creating it on first subscribe or prompt (01 §5.5).
    pub fn session(&self, id: &str) -> Result<SessionHandle, Defect> {
        if let Some(h) = self.sessions().get(id) {
            return Ok(h.clone());
        }
        let interrupted = match self.inner.deps.bus.journal().last_turn(id)? {
            Some(TurnRecord::Settled {
                turn_id,
                finish: TurnFinish::Interrupted,
            })
            | Some(TurnRecord::Claimed { turn_id }) => Some(turn_id),
            _ => None,
        };
        let mut map = self.sessions();
        let h = map
            .entry(id.to_string())
            .or_insert_with(|| {
                SessionHandle::spawn(
                    id.to_string(),
                    self.inner.deps.clone(),
                    &self.inner.scope,
                    interrupted,
                )
            })
            .clone();
        Ok(h)
    }

    /// On daemon start: each unsettled execution claim becomes a durable
    /// `turn.interrupted`; the UI shows **Resume**. Never auto-resumed (01 §4.3, D20).
    pub fn recover_on_start(&self) -> Result<usize, Defect> {
        let journal = self.inner.deps.bus.journal().clone();
        let claims = journal.unsettled_claims()?;
        for (session, turn) in &claims {
            journal.settle_turn(session, turn, TurnFinish::Interrupted)?;
            self.inner.deps.bus.publish(
                session,
                CoreEvent::TurnInterrupted {
                    turn_id: turn.clone(),
                    crash: true,
                },
            )?;
            tracing::info!(session_id = %session, turn_id = %turn, "turn interrupted by daemon restart");
        }
        Ok(claims.len())
    }

    pub fn busy(&self) -> BusySummary {
        let mut s = BusySummary {
            background_jobs: u32::try_from(self.inner.background_jobs.load(Ordering::SeqCst))
                .unwrap_or(u32::MAX),
            terminals: u32::try_from(self.inner.terminals.load(Ordering::SeqCst))
                .unwrap_or(u32::MAX),
            ..Default::default()
        };
        for h in self.sessions().values() {
            if h.has_turn() {
                s.running_turns += 1;
            }
            s.pending_asks += u32::try_from(h.pending_asks()).unwrap_or(u32::MAX);
        }
        s
    }

    /// Sessions currently running a turn (for `daemon.status`).
    pub fn sessions_running(&self) -> u32 {
        self.sessions()
            .values()
            .filter(|h| !matches!(h.status(), LiveStatus::Idle | LiveStatus::Interrupted))
            .count()
            .try_into()
            .unwrap_or(u32::MAX)
    }

    /// Drop actors with no subscribers and no turn for the idle TTL. State lives in the
    /// store, so this is cheap.
    pub fn evict_idle(&self) -> usize {
        let ttl = self.inner.actor_idle_ttl;
        let mut map = self.sessions();
        let ids: Vec<SessionId> = map
            .iter()
            .filter(|(_, h)| h.evictable(ttl))
            .map(|(k, _)| k.clone())
            .collect();
        for id in &ids {
            if let Some(h) = map.remove(id) {
                h.close();
            }
        }
        ids.len()
    }

    pub fn background_job(&self) -> WorkGuard {
        self.inner.background_jobs.fetch_add(1, Ordering::SeqCst);
        WorkGuard(self.inner.background_jobs.clone())
    }

    pub fn terminal(&self) -> WorkGuard {
        self.inner.terminals.fetch_add(1, Ordering::SeqCst);
        WorkGuard(self.inner.terminals.clone())
    }

    pub fn bus(&self) -> &Arc<Bus> {
        &self.inner.deps.bus
    }

    pub fn dirs(&self) -> &DirContexts {
        &self.inner.dirs
    }

    pub fn path_locks(&self) -> &PathLocks {
        &self.inner.locks
    }

    /// Close every actor; running turns settle as `interrupted` (01 §4.3).
    pub fn shutdown(&self) {
        for h in self.sessions().drain().map(|(_, h)| h) {
            h.close();
        }
        self.inner.scope.cancel();
    }
}
