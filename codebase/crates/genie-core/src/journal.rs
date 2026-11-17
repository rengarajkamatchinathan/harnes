//! The durable side of the bus: event log + execution claims (01 §3, §4.3).
//!
//! `genie-store` (spec 04) implements [`Journal`] on SQLCipher, running the insert, the
//! projectors and commit hooks in one SQLite transaction on its writer task.
//! [`MemoryJournal`] is the in-process implementation used by tests, incognito sessions
//! and this milestone's daemon.

use std::collections::HashMap;
use std::sync::Mutex;

use genie_protocol::session::TurnFinish;

use crate::bus::Envelope;
use crate::error::Defect;
use crate::ids::{SessionId, TurnId};

#[derive(Debug, Clone, PartialEq)]
pub enum TurnRecord {
    /// Execution claim written when the turn started; not settled yet.
    Claimed {
        turn_id: TurnId,
    },
    Settled {
        turn_id: TurnId,
        finish: TurnFinish,
    },
}

impl TurnRecord {
    pub fn turn_id(&self) -> &str {
        match self {
            TurnRecord::Claimed { turn_id } | TurnRecord::Settled { turn_id, .. } => turn_id,
        }
    }
}

pub trait Journal: Send + Sync + 'static {
    /// Append a durable event (already carrying its `seq`).
    fn append(&self, env: &Envelope) -> Result<(), Defect>;
    /// Durable events of `session` with `seq > after_seq`, in order.
    fn events_after(&self, session: &str, after_seq: u64) -> Result<Vec<Envelope>, Defect>;
    /// Highest durable seq written for `session` (0 if none).
    fn last_seq(&self, session: &str) -> Result<u64, Defect>;
    /// Write the execution claim for a turn (01 §4.3).
    fn claim_turn(&self, session: &str, turn: &str) -> Result<(), Defect>;
    fn settle_turn(&self, session: &str, turn: &str, finish: TurnFinish) -> Result<(), Defect>;
    /// The latest turn record of a session.
    fn last_turn(&self, session: &str) -> Result<Option<TurnRecord>, Defect>;
    /// Every session whose latest turn is still claimed (scanned on daemon start).
    fn unsettled_claims(&self) -> Result<Vec<(SessionId, TurnId)>, Defect>;
}

#[derive(Default)]
pub struct MemoryJournal {
    inner: Mutex<MemInner>,
}

#[derive(Default)]
struct MemInner {
    events: HashMap<SessionId, Vec<Envelope>>,
    turns: HashMap<SessionId, TurnRecord>,
}

impl MemoryJournal {
    pub fn new() -> Self {
        Self::default()
    }

    fn lock(&self) -> Result<std::sync::MutexGuard<'_, MemInner>, Defect> {
        self.inner
            .lock()
            .map_err(|_| Defect::new("memory journal lock poisoned"))
    }
}

impl Journal for MemoryJournal {
    fn append(&self, env: &Envelope) -> Result<(), Defect> {
        self.lock()?
            .events
            .entry(env.session_id.clone())
            .or_default()
            .push(env.clone());
        Ok(())
    }

    fn events_after(&self, session: &str, after_seq: u64) -> Result<Vec<Envelope>, Defect> {
        let g = self.lock()?;
        Ok(g.events
            .get(session)
            .map(|v| {
                v.iter()
                    .filter(|e| e.seq.unwrap_or(0) > after_seq)
                    .cloned()
                    .collect()
            })
            .unwrap_or_default())
    }

    fn last_seq(&self, session: &str) -> Result<u64, Defect> {
        let g = self.lock()?;
        Ok(g.events
            .get(session)
            .and_then(|v| v.last())
            .and_then(|e| e.seq)
            .unwrap_or(0))
    }

    fn claim_turn(&self, session: &str, turn: &str) -> Result<(), Defect> {
        self.lock()?.turns.insert(
            session.to_string(),
            TurnRecord::Claimed {
                turn_id: turn.to_string(),
            },
        );
        Ok(())
    }

    fn settle_turn(&self, session: &str, turn: &str, finish: TurnFinish) -> Result<(), Defect> {
        self.lock()?.turns.insert(
            session.to_string(),
            TurnRecord::Settled {
                turn_id: turn.to_string(),
                finish,
            },
        );
        Ok(())
    }

    fn last_turn(&self, session: &str) -> Result<Option<TurnRecord>, Defect> {
        Ok(self.lock()?.turns.get(session).cloned())
    }

    fn unsettled_claims(&self) -> Result<Vec<(SessionId, TurnId)>, Defect> {
        Ok(self
            .lock()?
            .turns
            .iter()
            .filter_map(|(s, t)| match t {
                TurnRecord::Claimed { turn_id } => Some((s.clone(), turn_id.clone())),
                TurnRecord::Settled { .. } => None,
            })
            .collect())
    }
}
