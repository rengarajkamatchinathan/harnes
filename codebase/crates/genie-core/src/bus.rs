//! The event bus (01 §3, D20, KB ch.03).
//!
//! `publish` takes the bus lock, assigns the next per-session `seq` to a durable event,
//! writes it through the [`Journal`] (in the real store: insert + projectors + commit
//! hooks in one SQLite transaction), and only then fans it out. Ephemeral events skip
//! the journal and carry `ref_seq`, the seq of the durable item they belong to.
//!
//! Subscribers replay from the journal after a cursor, get a `Synced` marker, then live
//! events. A subscriber that falls behind gets `ResyncRequired` instead of silently
//! losing events (D22).

use std::collections::{HashMap, VecDeque};
use std::sync::Arc;
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

use genie_protocol::handshake::AskKind;
use genie_protocol::session::{Delivery, LiveStatus, TurnFinish};
use serde::{Deserialize, Serialize};
use tokio::sync::broadcast;

use crate::error::Defect;
use crate::ids::SessionId;
use crate::journal::Journal;

/// Core-level session events. The wire `SessionEvent` (02 §4) is a superset added by
/// the protocol work; these are the ones the actor itself produces.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum CoreEvent {
    InboxAdded {
        item_id: String,
        delivery: Delivery,
    },
    InboxRemoved {
        item_id: String,
    },
    TurnStarted {
        turn_id: String,
        item_id: String,
        resume: bool,
    },
    SteerInjected {
        turn_id: String,
        item_ids: Vec<String>,
    },
    AskCreated {
        ask_id: String,
        turn_id: String,
        kind: AskKind,
    },
    AskResolved {
        ask_id: String,
        outcome: AskOutcome,
    },
    TurnSettled {
        turn_id: String,
        finish: TurnFinish,
        error_code: Option<String>,
    },
    TurnInterrupted {
        turn_id: String,
    },
    /// Ephemeral.
    Status {
        status: LiveStatus,
    },
    /// Ephemeral, e.g. token deltas.
    Delta {
        text: String,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AskOutcome {
    Answered,
    /// Interrupt resolves pending asks as cancelled, not declined (01 §5.3).
    Cancelled,
}

/// 02 §3.3.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Envelope {
    pub session_id: SessionId,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub seq: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ref_seq: Option<u64>,
    /// Unix milliseconds.
    pub ts_ms: u64,
    pub event: CoreEvent,
}

#[derive(Debug, Clone, PartialEq)]
pub enum SubItem {
    Event(Envelope),
    /// Replay done; live events follow.
    Synced,
    /// The subscriber fell behind; re-subscribe from `from_seq`.
    ResyncRequired {
        from_seq: u64,
    },
}

pub struct Bus {
    journal: Arc<dyn Journal>,
    seqs: Mutex<HashMap<SessionId, u64>>,
    tx: broadcast::Sender<Envelope>,
}

/// Live buffer per bus before a slow subscriber is told to resync.
pub const LIVE_BUFFER: usize = 4096;

impl Bus {
    pub fn new(journal: Arc<dyn Journal>) -> Self {
        let (tx, _) = broadcast::channel(LIVE_BUFFER);
        Bus {
            journal,
            seqs: Mutex::new(HashMap::new()),
            tx,
        }
    }

    pub fn journal(&self) -> &Arc<dyn Journal> {
        &self.journal
    }

    /// Publish a durable event: seq + journal under the lock, then fan out.
    pub fn publish(&self, session: &str, event: CoreEvent) -> Result<Envelope, Defect> {
        let mut seqs = self
            .seqs
            .lock()
            .map_err(|_| Defect::new("bus lock poisoned"))?;
        let next = match seqs.get(session) {
            Some(s) => s + 1,
            None => self.journal.last_seq(session)? + 1,
        };
        let env = Envelope {
            session_id: session.to_string(),
            seq: Some(next),
            ref_seq: None,
            ts_ms: now_ms(),
            event,
        };
        self.journal.append(&env)?;
        seqs.insert(session.to_string(), next);
        // Fan out while still holding the lock so subscribers see seq order.
        let _ = self.tx.send(env.clone());
        Ok(env)
    }

    /// Publish an ephemeral event (never stored).
    pub fn publish_ephemeral(&self, session: &str, event: CoreEvent) {
        let ref_seq = self.seqs.lock().ok().and_then(|s| s.get(session).copied());
        let env = Envelope {
            session_id: session.to_string(),
            seq: None,
            ref_seq,
            ts_ms: now_ms(),
            event,
        };
        let _ = self.tx.send(env);
    }

    /// Subscribe to `session` resuming after `after_seq`.
    pub fn subscribe(&self, session: &str, after_seq: u64) -> Result<Subscription, Defect> {
        // Subscribe to live first so nothing published during replay is lost.
        let rx = self.tx.subscribe();
        let replay: VecDeque<Envelope> = self.journal.events_after(session, after_seq)?.into();
        let last_seq = replay.back().and_then(|e| e.seq).unwrap_or(after_seq);
        Ok(Subscription {
            session: session.to_string(),
            replay,
            rx,
            last_seq,
            synced_sent: false,
            dead: false,
        })
    }
}

pub struct Subscription {
    session: SessionId,
    replay: VecDeque<Envelope>,
    rx: broadcast::Receiver<Envelope>,
    last_seq: u64,
    synced_sent: bool,
    dead: bool,
}

impl Subscription {
    /// Next item. `None` when the bus is gone or after a resync notice.
    pub async fn next(&mut self) -> Option<SubItem> {
        if self.dead {
            return None;
        }
        if let Some(e) = self.replay.pop_front() {
            return Some(SubItem::Event(e));
        }
        if !self.synced_sent {
            self.synced_sent = true;
            return Some(SubItem::Synced);
        }
        loop {
            match self.rx.recv().await {
                Ok(env) if env.session_id != self.session => continue,
                Ok(env) => match env.seq {
                    Some(seq) if seq <= self.last_seq => continue, // already replayed
                    Some(seq) => {
                        self.last_seq = seq;
                        return Some(SubItem::Event(env));
                    }
                    None => return Some(SubItem::Event(env)),
                },
                Err(broadcast::error::RecvError::Lagged(_)) => {
                    self.dead = true;
                    return Some(SubItem::ResyncRequired {
                        from_seq: self.last_seq,
                    });
                }
                Err(broadcast::error::RecvError::Closed) => return None,
            }
        }
    }

    pub fn last_seq(&self) -> u64 {
        self.last_seq
    }
}

fn now_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX))
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::journal::MemoryJournal;

    fn ev(n: u32) -> CoreEvent {
        CoreEvent::InboxRemoved {
            item_id: n.to_string(),
        }
    }

    #[tokio::test]
    async fn seq_is_gap_free_and_resume_replays() {
        let bus = Bus::new(Arc::new(MemoryJournal::new()));
        for i in 0..3 {
            bus.publish("ses_a", ev(i)).unwrap();
        }
        bus.publish("ses_b", ev(9)).unwrap();
        let mut sub = bus.subscribe("ses_a", 1).unwrap();
        let mut got = vec![];
        loop {
            match sub.next().await.unwrap() {
                SubItem::Event(e) => got.push(e.seq.unwrap()),
                SubItem::Synced => break,
                SubItem::ResyncRequired { .. } => panic!(),
            }
        }
        assert_eq!(got, [2, 3]);
        bus.publish_ephemeral("ses_a", CoreEvent::Delta { text: "x".into() });
        bus.publish("ses_a", ev(4)).unwrap();
        let SubItem::Event(e) = sub.next().await.unwrap() else {
            panic!()
        };
        assert_eq!((e.seq, e.ref_seq), (None, Some(3)));
        let SubItem::Event(e) = sub.next().await.unwrap() else {
            panic!()
        };
        assert_eq!(e.seq, Some(4));
    }

    #[tokio::test]
    async fn slow_subscriber_gets_resync_not_loss() {
        let bus = Bus::new(Arc::new(MemoryJournal::new()));
        let mut sub = bus.subscribe("s", 0).unwrap();
        assert_eq!(sub.next().await, Some(SubItem::Synced));
        for i in 0..(LIVE_BUFFER as u32 + 10) {
            bus.publish("s", ev(i)).unwrap();
        }
        assert!(matches!(
            sub.next().await,
            Some(SubItem::ResyncRequired { from_seq: 0 })
        ));
        assert_eq!(sub.next().await, None);
    }
}
