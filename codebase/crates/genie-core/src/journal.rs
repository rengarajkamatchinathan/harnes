//! The durable side of the bus: event log + execution claims (01 §3, §4.3).
//!
//! [`StoreJournal`] implements [`Journal`] on `genie-store` (spec 04, SQLCipher); each
//! call is one store transaction. [`MemoryJournal`] is the in-process implementation
//! used by tests, incognito sessions and the daemon until it is wired to the store.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

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
    /// `log_floor_seq` (02 §9): durable events up to here were pruned. 0 = none.
    fn log_floor(&self, _session: &str) -> Result<u64, Defect> {
        Ok(0)
    }
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

/// [`Journal`] on the SQLCipher store (spec 04).
///
/// Events are stored as `(type, payload = the serialized CoreEvent)`; `ts_ms` is the
/// row's `created`. Sessions are created on first use as bare chat rows until session
/// creation is routed through the store. `events_after` returns what is retained:
/// callers check `genie_store::events::head` for the log floor (02 §9) before replaying.
pub struct StoreJournal {
    store: Arc<genie_store::Store>,
    boot_id: String,
}

impl StoreJournal {
    pub fn new(store: Arc<genie_store::Store>) -> Self {
        StoreJournal {
            store,
            boot_id: crate::ids::new_id("boot"),
        }
    }

    pub fn store(&self) -> &Arc<genie_store::Store> {
        &self.store
    }
}

fn store_defect(e: genie_store::StoreError) -> Defect {
    Defect::new(format!("store: {e}"))
}

impl Journal for StoreJournal {
    fn append(&self, env: &Envelope) -> Result<(), Defect> {
        let seq = env
            .seq
            .ok_or_else(|| Defect::new("journal append without seq"))?;
        let payload = serde_json::to_value(&env.event)
            .map_err(|e| Defect::new(format!("serialize event: {e}")))?;
        let ty = payload
            .get("type")
            .and_then(|t| t.as_str())
            .unwrap_or("unknown")
            .to_string();
        let created = i64::try_from(env.ts_ms).unwrap_or(i64::MAX);
        let payload = payload.to_string();
        self.store
            .write(|tx| {
                genie_store::sessions::ensure(tx, &env.session_id)?;
                genie_store::events::append(tx, &env.session_id, seq, &ty, &payload, created)
            })
            .map_err(store_defect)
    }

    fn events_after(&self, session: &str, after_seq: u64) -> Result<Vec<Envelope>, Defect> {
        let rows = self
            .store
            .read(|c| genie_store::events::retained_after(c, session, after_seq))
            .map_err(store_defect)?;
        rows.into_iter()
            .map(|r| {
                let event = serde_json::from_str(&r.payload).map_err(|e| {
                    Defect::new(format!("stored event {}#{}: {e}", r.session_id, r.seq))
                })?;
                Ok(Envelope {
                    session_id: r.session_id,
                    seq: Some(r.seq),
                    ref_seq: None,
                    ts_ms: u64::try_from(r.created).unwrap_or(0),
                    event,
                })
            })
            .collect()
    }

    fn last_seq(&self, session: &str) -> Result<u64, Defect> {
        Ok(self
            .store
            .read(|c| genie_store::events::head(c, session))
            .map_err(store_defect)?
            .map(|(head, _)| head)
            .unwrap_or(0))
    }

    fn claim_turn(&self, session: &str, turn: &str) -> Result<(), Defect> {
        self.store
            .write(|tx| {
                genie_store::sessions::ensure(tx, session)?;
                genie_store::events::claim_turn(
                    tx,
                    session,
                    turn,
                    Some(std::process::id()),
                    Some(&self.boot_id),
                )
            })
            .map_err(store_defect)
    }

    fn settle_turn(&self, session: &str, turn: &str, finish: TurnFinish) -> Result<(), Defect> {
        self.store
            .write(|tx| {
                genie_store::sessions::ensure(tx, session)?;
                genie_store::events::settle_turn(tx, session, turn, finish)
            })
            .map_err(store_defect)
    }

    fn last_turn(&self, session: &str) -> Result<Option<TurnRecord>, Defect> {
        let row = self
            .store
            .read(|c| genie_store::events::last_turn(c, session))
            .map_err(store_defect)?;
        Ok(row.map(|r| match r.finish {
            None => TurnRecord::Claimed { turn_id: r.turn_id },
            Some(finish) => TurnRecord::Settled {
                turn_id: r.turn_id,
                finish,
            },
        }))
    }

    fn unsettled_claims(&self) -> Result<Vec<(SessionId, TurnId)>, Defect> {
        Ok(self
            .store
            .read(genie_store::events::unsettled_claims)
            .map_err(store_defect)?
            .into_iter()
            .map(|c| (c.session_id, c.turn_id))
            .collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bus::{Bus, CoreEvent, SubItem};

    fn store(dir: &tempfile::TempDir) -> Arc<genie_store::Store> {
        let opts = genie_store::StoreOptions::new(dir.path(), "genie.db");
        let keys = genie_store::MemoryKeyStore::new();
        Arc::new(genie_store::Store::open_with_keystore(&opts, &keys).unwrap())
    }

    fn ev(i: &str) -> CoreEvent {
        CoreEvent::InboxRemoved {
            item_id: i.to_string(),
            items: vec![],
        }
    }

    #[tokio::test]
    async fn store_journal_backs_the_bus() {
        let dir = tempfile::tempdir().unwrap();
        let st = store(&dir);
        let journal: Arc<dyn Journal> = Arc::new(StoreJournal::new(st.clone()));
        let bus = Bus::new(journal.clone());
        for i in 0..3 {
            bus.publish("ses_a", ev(&i.to_string())).unwrap();
        }
        assert_eq!(journal.last_seq("ses_a").unwrap(), 3);
        assert_eq!(journal.last_seq("ses_none").unwrap(), 0);

        // A new bus over the same store continues the seq (daemon restart).
        let bus2 = Bus::new(Arc::new(StoreJournal::new(st)));
        let env = bus2.publish("ses_a", ev("x")).unwrap();
        assert_eq!(env.seq, Some(4));
        let mut sub = bus2.subscribe("ses_a", 2).unwrap();
        let mut got = vec![];
        while let Some(SubItem::Event(e)) = sub.next().await {
            assert!(matches!(e.event, CoreEvent::InboxRemoved { .. }));
            got.push(e.seq.unwrap());
        }
        assert_eq!(got, vec![3, 4]);
    }

    #[test]
    fn store_journal_claims() {
        let dir = tempfile::tempdir().unwrap();
        let j = StoreJournal::new(store(&dir));
        j.claim_turn("ses_a", "trn_1").unwrap();
        j.claim_turn("ses_b", "trn_2").unwrap();
        j.settle_turn("ses_b", "trn_2", TurnFinish::Cancelled)
            .unwrap();
        assert_eq!(
            j.unsettled_claims().unwrap(),
            vec![("ses_a".to_string(), "trn_1".to_string())]
        );
        assert_eq!(
            j.last_turn("ses_b").unwrap(),
            Some(TurnRecord::Settled {
                turn_id: "trn_2".into(),
                finish: TurnFinish::Cancelled
            })
        );
        assert_eq!(
            j.last_turn("ses_a").unwrap(),
            Some(TurnRecord::Claimed {
                turn_id: "trn_1".into()
            })
        );
        assert_eq!(j.last_turn("ses_c").unwrap(), None);
    }
}
