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

use genie_protocol::ask::{Ask, AskKind, AutoResolver, Decision, ResolvedBy};
use genie_protocol::event::{
    self as wire, Activity, InboxItemView, InterruptReason, SessionEvent, SessionEventEnvelope,
    StatusEvent, TurnStatus,
};
use genie_protocol::session::{Delivery, LiveStatus, TurnFinish};
use serde::{Deserialize, Serialize};
use tokio::sync::broadcast;

use crate::error::Defect;
use crate::ids::SessionId;
use crate::journal::Journal;

/// Core-level session events. Every durable core event maps to exactly one wire
/// [`SessionEvent`] ([`Envelope::to_wire`]), so the wire `seq` stays gap-free (02 §3.1).
/// Subsystems (turn loop, tools, lease) publish wire events directly via [`CoreEvent::Wire`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum CoreEvent {
    InboxAdded {
        item_id: String,
        delivery: Delivery,
        /// The full queue after the change (`inbox.changed`).
        #[serde(default)]
        items: Vec<InboxItemView>,
    },
    InboxRemoved {
        item_id: String,
        #[serde(default)]
        items: Vec<InboxItemView>,
    },
    TurnStarted {
        turn_id: String,
        item_id: String,
        resume: bool,
        #[serde(default)]
        message_id: Option<String>,
    },
    SteerInjected {
        turn_id: String,
        item_ids: Vec<String>,
        #[serde(default)]
        items: Vec<InboxItemView>,
    },
    AskCreated {
        ask_id: String,
        turn_id: String,
        kind: AskKind,
        #[serde(default)]
        ask: Option<Box<Ask>>,
    },
    AskResolved {
        ask_id: String,
        outcome: AskOutcome,
        #[serde(default)]
        decision: Option<Decision>,
        #[serde(default)]
        by: Option<ResolvedBy>,
        /// The rule an `always` answer saved (06 §5.4).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        saved_rule: Option<String>,
    },
    TurnSettled {
        turn_id: String,
        finish: TurnFinish,
        error_code: Option<String>,
        /// User-facing message for `error_code` (defaults to the code).
        #[serde(default)]
        error_message: Option<String>,
        /// Fix link (e.g. `genie://settings/providers`, 01 §7).
        #[serde(default)]
        error_link: Option<String>,
    },
    TurnInterrupted {
        turn_id: String,
        /// Found unsettled at daemon start (crash) rather than settled by a clean
        /// shutdown.
        #[serde(default)]
        crash: bool,
    },
    /// Ephemeral.
    Status { status: LiveStatus },
    /// Ephemeral, e.g. token deltas.
    Delta {
        text: String,
        #[serde(default)]
        part_id: String,
    },
    /// A wire event published as-is by a subsystem.
    Wire { event: Box<SessionEvent> },
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

impl Envelope {
    /// The wire envelope for this event (02 §3.3).
    pub fn to_wire(&self) -> SessionEventEnvelope {
        let (event, turn_id) = self.event.to_wire();
        SessionEventEnvelope {
            session_id: self.session_id.clone(),
            seq: self.seq,
            ref_seq: self.ref_seq,
            ts: genie_protocol::rfc3339_ms(self.ts_ms),
            turn_id,
            step: None,
            subagent_of: None,
            event,
        }
    }
}

impl CoreEvent {
    /// Map to the wire event and the turn it belongs to.
    pub fn to_wire(&self) -> (SessionEvent, Option<String>) {
        match self {
            CoreEvent::InboxAdded { items, .. } | CoreEvent::InboxRemoved { items, .. } => (
                SessionEvent::InboxChanged(wire::InboxChanged {
                    items: items.clone(),
                }),
                None,
            ),
            CoreEvent::SteerInjected { turn_id, items, .. } => (
                SessionEvent::InboxChanged(wire::InboxChanged {
                    items: items.clone(),
                }),
                Some(turn_id.clone()),
            ),
            CoreEvent::TurnStarted {
                turn_id,
                item_id,
                resume,
                message_id,
            } => (
                SessionEvent::TurnStarted(wire::TurnStarted {
                    turn_id: turn_id.clone(),
                    message_id: message_id.clone().unwrap_or_else(|| item_id.clone()),
                    resume: *resume,
                }),
                Some(turn_id.clone()),
            ),
            CoreEvent::AskCreated {
                ask_id,
                turn_id,
                kind,
                ask,
            } => {
                let ask = match ask {
                    Some(a) => (**a).clone(),
                    None => {
                        let mut a = Ask::new(ask_id.clone(), "", *kind, "");
                        a.turn_id = Some(turn_id.clone());
                        a
                    }
                };
                (
                    SessionEvent::PermissionAsked(wire::PermissionAsked { ask: Box::new(ask) }),
                    Some(turn_id.clone()),
                )
            }
            CoreEvent::AskResolved {
                ask_id,
                outcome,
                decision,
                by,
                saved_rule,
            } => (
                SessionEvent::PermissionResolved(wire::PermissionResolved {
                    ask_id: ask_id.clone(),
                    decision: decision.unwrap_or(match outcome {
                        AskOutcome::Answered => Decision::Once,
                        AskOutcome::Cancelled => Decision::Cancel,
                    }),
                    by: by.clone().unwrap_or(ResolvedBy::Auto(AutoResolver::System)),
                    saved_rule: saved_rule.clone(),
                }),
                None,
            ),
            CoreEvent::TurnSettled {
                turn_id,
                finish,
                error_code,
                error_message,
                error_link,
            } => {
                let status = match finish {
                    TurnFinish::Completed => TurnStatus::Completed,
                    TurnFinish::Failed => TurnStatus::Failed,
                    TurnFinish::Cancelled | TurnFinish::Interrupted => TurnStatus::Cancelled,
                };
                let error = match (finish, error_code) {
                    (_, Some(code)) => Some(wire::ErrorInfo {
                        code: code.clone(),
                        message: error_message.clone().unwrap_or_else(|| code.clone()),
                        link: error_link.clone(),
                    }),
                    (TurnFinish::Interrupted, None) => Some(wire::ErrorInfo {
                        code: "interrupted".into(),
                        message: "the engine stopped while this turn was running".into(),
                        link: None,
                    }),
                    _ => None,
                };
                (
                    SessionEvent::TurnSettled(wire::TurnSettled {
                        turn_id: turn_id.clone(),
                        status,
                        error,
                    }),
                    Some(turn_id.clone()),
                )
            }
            CoreEvent::TurnInterrupted { turn_id, crash } => (
                SessionEvent::TurnInterrupted(wire::TurnInterrupted {
                    turn_id: turn_id.clone(),
                    reason: if *crash {
                        InterruptReason::Crash
                    } else {
                        InterruptReason::DaemonRestart
                    },
                }),
                Some(turn_id.clone()),
            ),
            CoreEvent::Status { status } => (
                SessionEvent::Status(StatusEvent {
                    state: *status,
                    activity: match status {
                        LiveStatus::Compacting => Some(Activity::Compacting),
                        LiveStatus::Interrupting => Some(Activity::Interrupting),
                        _ => None,
                    },
                }),
                None,
            ),
            CoreEvent::Delta { text, part_id } => (
                SessionEvent::PartDelta(wire::PartDelta {
                    part_id: part_id.clone(),
                    text: Some(text.clone()),
                    json: None,
                }),
                None,
            ),
            CoreEvent::Wire { event } => ((**event).clone(), None),
        }
    }
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

    /// Durable events at or below this seq were pruned (02 §9). A subscribe with
    /// `after_seq` below it must reload a snapshot instead.
    pub fn log_floor(&self, session: &str) -> Result<u64, Defect> {
        self.journal.log_floor(session)
    }

    /// Highest durable seq of `session`.
    pub fn head_seq(&self, session: &str) -> Result<u64, Defect> {
        if let Some(s) = self
            .seqs
            .lock()
            .map_err(|_| Defect::new("bus lock poisoned"))?
            .get(session)
        {
            return Ok(*s);
        }
        self.journal.last_seq(session)
    }

    /// Every event of every session, live only (the workspace feed pump).
    pub fn subscribe_all(&self) -> broadcast::Receiver<Envelope> {
        self.tx.subscribe()
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

/// Unix milliseconds now.
pub fn now_ms() -> u64 {
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
            items: vec![],
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
        bus.publish_ephemeral(
            "ses_a",
            CoreEvent::Delta {
                text: "x".into(),
                part_id: String::new(),
            },
        );
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
