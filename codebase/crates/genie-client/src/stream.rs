//! Client-side session stream state (02 §3.1): the resume cursor, resync handling and
//! dropping of stale ephemeral events. Pure state, no I/O, so the CLI, the Tauri side
//! and tests share it.
//!
//! Feed every notification for the session to [`SessionTracker::on_notification`] and
//! act on the returned [`Action`].

use std::collections::HashSet;

use genie_protocol::Notification;
use genie_protocol::event::{ResyncRequired, SessionEvent, SessionEventEnvelope, SessionSynced};
use genie_protocol::methods::{self as m, Method};

#[derive(Debug, Clone, PartialEq)]
pub enum Action {
    /// Apply this event to the view.
    Event(Box<SessionEventEnvelope>),
    /// Replay finished; live from here.
    Synced,
    /// Re-subscribe with `after_seq` (overflow or lag; nothing was lost).
    Resubscribe { after_seq: u64 },
    /// The cursor is below the retained log: reload with `session.get`, then subscribe
    /// from its `head_seq`.
    Reload,
    /// Not for this session, a duplicate, or a stale ephemeral event.
    Ignore,
}

#[derive(Debug, Clone)]
pub struct SessionTracker {
    session_id: String,
    last_seq: u64,
    synced: bool,
    ended_parts: HashSet<String>,
    ended_calls: HashSet<String>,
}

impl SessionTracker {
    /// Start tracking; `after_seq` is the cursor passed to `session.subscribe`.
    pub fn new(session_id: impl Into<String>, after_seq: u64) -> Self {
        SessionTracker {
            session_id: session_id.into(),
            last_seq: after_seq,
            synced: false,
            ended_parts: HashSet::new(),
            ended_calls: HashSet::new(),
        }
    }

    /// The resume cursor: the last durable seq applied.
    pub fn last_seq(&self) -> u64 {
        self.last_seq
    }

    pub fn is_synced(&self) -> bool {
        self.synced
    }

    /// Reset after a `session.get` reload.
    pub fn reset_to(&mut self, head_seq: u64) {
        self.last_seq = head_seq;
        self.synced = false;
        self.ended_parts.clear();
        self.ended_calls.clear();
    }

    pub fn on_notification(&mut self, n: &Notification) -> Action {
        match n.method.as_str() {
            x if x == m::Event::NAME => match serde_json::from_value(n.params.clone()) {
                Ok(env) => self.on_event(env),
                Err(_) => Action::Ignore,
            },
            x if x == m::SessionSyncedNote::NAME => {
                match serde_json::from_value::<SessionSynced>(n.params.clone()) {
                    Ok(s) if s.session_id == self.session_id => {
                        self.synced = true;
                        Action::Synced
                    }
                    _ => Action::Ignore,
                }
            }
            x if x == m::SessionResyncRequired::NAME => {
                match serde_json::from_value::<ResyncRequired>(n.params.clone()) {
                    Ok(r) if r.session_id == self.session_id => {
                        self.synced = false;
                        if r.snapshot {
                            Action::Reload
                        } else {
                            // Never move the cursor backwards past what we applied.
                            Action::Resubscribe {
                                after_seq: self.last_seq.max(r.from_seq),
                            }
                        }
                    }
                    _ => Action::Ignore,
                }
            }
            _ => Action::Ignore,
        }
    }

    pub fn on_event(&mut self, env: SessionEventEnvelope) -> Action {
        if env.session_id != self.session_id {
            return Action::Ignore;
        }
        match env.seq {
            Some(seq) => {
                if seq <= self.last_seq {
                    return Action::Ignore; // duplicate after a resubscribe
                }
                self.last_seq = seq;
                match &env.event {
                    SessionEvent::PartEnded(p) => {
                        self.ended_parts.insert(p.part_id.clone());
                    }
                    SessionEvent::ToolEnded(t) => {
                        self.ended_calls.insert(t.call_id.clone());
                    }
                    SessionEvent::TurnSettled(_) => {
                        self.ended_parts.clear();
                        self.ended_calls.clear();
                    }
                    _ => {}
                }
                Action::Event(Box::new(env))
            }
            None => {
                let stale = match &env.event {
                    SessionEvent::PartDelta(d) => self.ended_parts.contains(&d.part_id),
                    SessionEvent::ToolProgress(p) => self.ended_calls.contains(&p.call_id),
                    SessionEvent::Unknown => true,
                    _ => false,
                };
                if stale {
                    Action::Ignore
                } else {
                    Action::Event(Box::new(env))
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;
    use genie_protocol::event::{PartDelta, PartEnded, SessionUpdated};
    use genie_protocol::message::Part;
    use serde_json::json;

    fn env(seq: Option<u64>, event: SessionEvent) -> SessionEventEnvelope {
        SessionEventEnvelope {
            session_id: "ses_1".into(),
            seq,
            ref_seq: if seq.is_none() { Some(1) } else { None },
            ts: "2026-01-01T00:00:00.000Z".into(),
            turn_id: None,
            step: None,
            subagent_of: None,
            event,
        }
    }

    #[test]
    fn dedupes_and_drops_stale_deltas() {
        let mut t = SessionTracker::new("ses_1", 0);
        let upd = SessionEvent::SessionUpdated(SessionUpdated::default());
        assert!(matches!(
            t.on_event(env(Some(1), upd.clone())),
            Action::Event(_)
        ));
        assert_eq!(t.on_event(env(Some(1), upd)), Action::Ignore);
        let delta = SessionEvent::PartDelta(PartDelta {
            part_id: "prt_1".into(),
            text: Some("x".into()),
            json: None,
        });
        assert!(matches!(
            t.on_event(env(None, delta.clone())),
            Action::Event(_)
        ));
        let ended = SessionEvent::PartEnded(PartEnded {
            part_id: "prt_1".into(),
            part: Part::Text {
                id: "prt_1".into(),
                text: "x".into(),
                citations: vec![],
            },
        });
        t.on_event(env(Some(2), ended));
        assert_eq!(t.on_event(env(None, delta)), Action::Ignore);
        assert_eq!(t.last_seq(), 2);
    }

    #[test]
    fn resync_actions() {
        let mut t = SessionTracker::new("ses_1", 5);
        let n = Notification::new(
            m::SessionResyncRequired::NAME,
            json!({"session_id": "ses_1", "from_seq": 3}),
        );
        assert_eq!(t.on_notification(&n), Action::Resubscribe { after_seq: 5 });
        let n = Notification::new(
            m::SessionResyncRequired::NAME,
            json!({"session_id": "ses_1", "from_seq": 0, "snapshot": true}),
        );
        assert_eq!(t.on_notification(&n), Action::Reload);
        let n = Notification::new(
            m::Event::NAME,
            json!({"session_id": "ses_1", "seq": 6, "ts": "t", "event": {"type": "brand.new", "x": 1}}),
        );
        // Unknown durable types still advance the cursor (02 §10).
        assert!(matches!(t.on_notification(&n), Action::Event(_)));
        assert_eq!(t.last_seq(), 6);
    }
}
