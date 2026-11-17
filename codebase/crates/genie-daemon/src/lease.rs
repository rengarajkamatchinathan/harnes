//! Driver leases (02 §5, SA-11).
//!
//! A session has at most one driver, bound to a `client_instance_id`. The first
//! mutating command on a session with no driver acquires the lease; `take_over` moves
//! it; `release` gives it up. When the driver's connection drops, the lease stays bound
//! to its instance for the grace period so a reconnecting client keeps it; after that
//! it is vacant.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use genie_protocol::ClientRef;

/// 02 §5, SA-11.
pub const LEASE_GRACE: Duration = Duration::from_secs(60);

#[derive(Debug, Clone)]
struct Lease {
    driver: ClientRef,
    /// The live connection holding it, if connected.
    conn_id: Option<u64>,
    disconnected_at: Option<Instant>,
}

impl Lease {
    fn vacant(&self, grace: Duration) -> bool {
        self.conn_id.is_none() && self.disconnected_at.is_some_and(|t| t.elapsed() >= grace)
    }
}

/// Outcome of a lease check for a mutating command.
#[derive(Debug, Clone, PartialEq)]
pub enum LeaseCheck {
    /// The caller already held it (or reclaimed it after a reconnect).
    Held,
    /// The caller just acquired a vacant lease; publish `lease.changed`.
    Acquired,
    /// Someone else drives.
    NotDriver(ClientRef),
}

/// Result of `take_over`.
#[derive(Debug, Clone, PartialEq)]
pub struct TakeOver {
    /// Whether the driver changed (publish `lease.changed`).
    pub changed: bool,
    /// The previous driver's live connection, to send `lease.lost` to.
    pub old_conn: Option<u64>,
}

pub struct Leases {
    map: Mutex<HashMap<String, Lease>>,
    grace: Duration,
}

impl Leases {
    pub fn new(grace: Duration) -> Self {
        Leases {
            map: Mutex::new(HashMap::new()),
            grace,
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<String, Lease>> {
        self.map.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// The current driver, if the lease is not vacant.
    pub fn driver(&self, session: &str) -> Option<ClientRef> {
        self.lock()
            .get(session)
            .filter(|l| !l.vacant(self.grace))
            .map(|l| l.driver.clone())
    }

    pub fn is_driver(&self, session: &str, conn_id: u64) -> bool {
        self.lock()
            .get(session)
            .is_some_and(|l| l.conn_id == Some(conn_id))
    }

    /// Check (and acquire if vacant) before a mutating command.
    pub fn check(&self, session: &str, me: &ClientRef, conn_id: u64) -> LeaseCheck {
        let mut map = self.lock();
        match map.get_mut(session) {
            Some(l) if l.driver.client_instance_id == me.client_instance_id => {
                l.conn_id = Some(conn_id);
                l.disconnected_at = None;
                LeaseCheck::Held
            }
            Some(l) if !l.vacant(self.grace) => LeaseCheck::NotDriver(l.driver.clone()),
            _ => {
                map.insert(session.to_string(), held(me, conn_id));
                LeaseCheck::Acquired
            }
        }
    }

    pub fn take_over(&self, session: &str, me: &ClientRef, conn_id: u64) -> TakeOver {
        let mut map = self.lock();
        let old = map.insert(session.to_string(), held(me, conn_id));
        match old {
            Some(l) if l.driver.client_instance_id == me.client_instance_id => TakeOver {
                changed: false,
                old_conn: l.conn_id.filter(|c| *c != conn_id),
            },
            Some(l) => TakeOver {
                changed: true,
                old_conn: l.conn_id,
            },
            None => TakeOver {
                changed: true,
                old_conn: None,
            },
        }
    }

    /// Give the lease up. Returns whether `me` held it.
    pub fn release(&self, session: &str, me: &ClientRef) -> bool {
        let mut map = self.lock();
        if map
            .get(session)
            .is_some_and(|l| l.driver.client_instance_id == me.client_instance_id)
        {
            map.remove(session);
            true
        } else {
            false
        }
    }

    /// A connection closed: its leases enter the reconnect grace period.
    pub fn disconnected(&self, conn_id: u64) {
        let now = Instant::now();
        for l in self.lock().values_mut() {
            if l.conn_id == Some(conn_id) {
                l.conn_id = None;
                l.disconnected_at = Some(now);
            }
        }
    }

    /// Drop all state for a deleted session.
    pub fn forget(&self, session: &str) {
        self.lock().remove(session);
    }
}

fn held(me: &ClientRef, conn_id: u64) -> Lease {
    Lease {
        driver: me.clone(),
        conn_id: Some(conn_id),
        disconnected_at: None,
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;
    use genie_protocol::ClientKind;

    fn c(id: &str) -> ClientRef {
        ClientRef {
            client_instance_id: id.into(),
            client_kind: ClientKind::Desktop,
            label: None,
        }
    }

    #[test]
    fn acquire_reject_takeover_release() {
        let l = Leases::new(Duration::from_secs(60));
        assert_eq!(l.check("s", &c("a"), 1), LeaseCheck::Acquired);
        assert_eq!(l.check("s", &c("a"), 1), LeaseCheck::Held);
        assert_eq!(l.check("s", &c("b"), 2), LeaseCheck::NotDriver(c("a")));
        let t = l.take_over("s", &c("b"), 2);
        assert_eq!(
            t,
            TakeOver {
                changed: true,
                old_conn: Some(1)
            }
        );
        assert!(l.is_driver("s", 2));
        assert!(!l.release("s", &c("a")));
        assert!(l.release("s", &c("b")));
        assert_eq!(l.driver("s"), None);
    }

    #[test]
    fn reconnect_grace() {
        let l = Leases::new(Duration::from_millis(30));
        l.check("s", &c("a"), 1);
        l.disconnected(1);
        // Within grace: still a's, and a reconnecting `a` reclaims it.
        assert_eq!(l.check("s", &c("b"), 2), LeaseCheck::NotDriver(c("a")));
        assert_eq!(l.check("s", &c("a"), 3), LeaseCheck::Held);
        assert!(l.is_driver("s", 3));
        l.disconnected(3);
        std::thread::sleep(Duration::from_millis(40));
        assert_eq!(l.driver("s"), None);
        assert_eq!(l.check("s", &c("b"), 2), LeaseCheck::Acquired);
    }
}
