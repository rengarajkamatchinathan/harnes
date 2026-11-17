//! Durable event log, execution claims and the inbox (04 §3.3; 02 §3, §9; 01 §4.3).
//!
//! * Seqs are per session, gap-free from 1; `session.head_seq` is the last one written.
//!   [`append`] refuses anything but `head_seq + 1`.
//! * Pruning keeps `session.log_floor_seq`: events with `seq <= floor` are gone, and a
//!   cursor below the floor gets [`StoreError::BelowFloor`] (the daemon answers with
//!   `resync_required {snapshot: true}`).
//! * Call these inside [`crate::Store::write`] together with the projections so the
//!   event and its effects commit in one transaction.

use genie_protocol::session::{Delivery, TurnFinish};
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};

use crate::error::{Result, StoreError};
use crate::util::{new_id, now_ms, to_i64, to_u64};

/// Event types after which history may be pruned (02 §9 "up to its last
/// `turn.settled`"). Both the wire name and genie-core's serde tag are accepted.
pub const TURN_SETTLED_TYPES: &[&str] = &["turn.settled", "turn_settled"];

/// Idle time before a session's log may be pruned (SA-12).
pub const PRUNE_IDLE_MS: i64 = 30 * 24 * 60 * 60 * 1000;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EventRow {
    pub session_id: String,
    pub seq: u64,
    #[serde(rename = "type")]
    pub ty: String,
    /// JSON text, as written.
    pub payload: String,
    pub created: i64,
}

/// `(head_seq, log_floor_seq)` of a session.
pub fn head(conn: &Connection, session: &str) -> Result<Option<(u64, u64)>> {
    Ok(conn
        .query_row(
            "SELECT head_seq, log_floor_seq FROM session WHERE id = ?1",
            [session],
            |r| Ok((to_u64(r.get(0)?), to_u64(r.get(1)?))),
        )
        .optional()?)
}

/// Append one durable event; `seq` must be `head_seq + 1`. Advances `head_seq`.
pub fn append(
    conn: &Connection,
    session: &str,
    seq: u64,
    ty: &str,
    payload: &str,
    created: i64,
) -> Result<()> {
    let (head_seq, _) =
        head(conn, session)?.ok_or_else(|| StoreError::NotFound(session.to_string()))?;
    if seq != head_seq + 1 {
        return Err(StoreError::SeqGap {
            session: session.to_string(),
            head: head_seq,
            got: seq,
        });
    }
    let seq_i = to_i64(seq)?;
    conn.execute(
        "INSERT INTO event (session_id, seq, type, payload, created) VALUES (?1, ?2, ?3, ?4, ?5)",
        params![session, seq_i, ty, payload, created],
    )?;
    conn.execute(
        "UPDATE session SET head_seq = ?2, updated = max(updated, ?3) WHERE id = ?1",
        params![session, seq_i, created],
    )?;
    Ok(())
}

/// Events with `seq > after_seq`, in order. Errors with [`StoreError::BelowFloor`] when
/// some of those were pruned.
pub fn events_after(conn: &Connection, session: &str, after_seq: u64) -> Result<Vec<EventRow>> {
    let Some((_, floor)) = head(conn, session)? else {
        return Ok(vec![]);
    };
    if after_seq < floor {
        return Err(StoreError::BelowFloor {
            session: session.to_string(),
            floor,
        });
    }
    retained_after(conn, session, after_seq)
}

/// Like [`events_after`] but returns what is retained without checking the floor.
pub fn retained_after(conn: &Connection, session: &str, after_seq: u64) -> Result<Vec<EventRow>> {
    let mut st = conn.prepare(
        "SELECT session_id, seq, type, payload, created FROM event
         WHERE session_id = ?1 AND seq > ?2 ORDER BY seq",
    )?;
    let rows = st.query_map(params![session, to_i64(after_seq)?], |r| {
        Ok(EventRow {
            session_id: r.get(0)?,
            seq: to_u64(r.get(1)?),
            ty: r.get(2)?,
            payload: r.get(3)?,
            created: r.get(4)?,
        })
    })?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

/// Delete events with `seq <= up_to` and raise the floor. `up_to` is clamped to
/// `head_seq`; the floor never goes down. Returns the number of events deleted.
pub fn prune(conn: &Connection, session: &str, up_to: u64) -> Result<usize> {
    let (head_seq, floor) =
        head(conn, session)?.ok_or_else(|| StoreError::NotFound(session.to_string()))?;
    let up_to = up_to.min(head_seq);
    if up_to <= floor {
        return Ok(0);
    }
    let n = conn.execute(
        "DELETE FROM event WHERE session_id = ?1 AND seq <= ?2",
        params![session, to_i64(up_to)?],
    )?;
    conn.execute(
        "UPDATE session SET log_floor_seq = ?2 WHERE id = ?1",
        params![session, to_i64(up_to)?],
    )?;
    Ok(n)
}

/// 02 §9 / SA-12: for every idle session not updated for `idle_ms`, prune events up
/// to its last settled turn. Returns `(session, new floor)` for each pruned session.
pub fn prune_idle(conn: &Connection, now: i64, idle_ms: i64) -> Result<Vec<(String, u64)>> {
    let placeholders = TURN_SETTLED_TYPES
        .iter()
        .enumerate()
        .map(|(i, _)| format!("?{}", i + 2))
        .collect::<Vec<_>>()
        .join(", ");
    let sql = format!(
        "SELECT s.id, (SELECT max(e.seq) FROM event e
                       WHERE e.session_id = s.id AND e.type IN ({placeholders}))
         FROM session s
         WHERE s.status = 'idle' AND s.updated < ?1"
    );
    let mut args: Vec<Box<dyn rusqlite::ToSql>> = vec![Box::new(now - idle_ms)];
    for t in TURN_SETTLED_TYPES {
        args.push(Box::new(*t));
    }
    let candidates: Vec<(String, Option<i64>)> = {
        let mut st = conn.prepare(&sql)?;
        let rows = st.query_map(rusqlite::params_from_iter(args.iter()), |r| {
            Ok((r.get(0)?, r.get(1)?))
        })?;
        rows.collect::<rusqlite::Result<_>>()?
    };
    let mut out = vec![];
    for (id, last) in candidates {
        let Some(last) = last else { continue };
        if prune(conn, &id, to_u64(last))? > 0 {
            out.push((id, to_u64(last)));
        }
    }
    Ok(out)
}

/// The latest turn of a session as recorded by its execution claim.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClaimRow {
    pub session_id: String,
    pub turn_id: String,
    pub started: i64,
    pub daemon_pid: Option<i64>,
    pub daemon_boot_id: Option<String>,
    /// `None` while the turn is running.
    pub finish: Option<TurnFinish>,
}

fn finish_str(f: TurnFinish) -> Result<String> {
    match serde_json::to_value(f)? {
        serde_json::Value::String(s) => Ok(s),
        v => Err(StoreError::Format(format!("turn finish {v}"))),
    }
}

/// Write the execution claim for a starting turn (D20). Replaces the previous turn's row.
pub fn claim_turn(
    conn: &Connection,
    session: &str,
    turn: &str,
    daemon_pid: Option<u32>,
    daemon_boot_id: Option<&str>,
) -> Result<()> {
    conn.execute(
        "INSERT INTO execution_claim (session_id, turn_id, started, daemon_pid, daemon_boot_id, finish)
         VALUES (?1, ?2, ?3, ?4, ?5, NULL)
         ON CONFLICT (session_id) DO UPDATE SET turn_id = excluded.turn_id,
             started = excluded.started, daemon_pid = excluded.daemon_pid,
             daemon_boot_id = excluded.daemon_boot_id, finish = NULL",
        params![session, turn, now_ms(), daemon_pid, daemon_boot_id],
    )?;
    Ok(())
}

/// Settle the claim. A different turn id than the claimed one inserts/replaces.
pub fn settle_turn(conn: &Connection, session: &str, turn: &str, finish: TurnFinish) -> Result<()> {
    let f = finish_str(finish)?;
    conn.execute(
        "INSERT INTO execution_claim (session_id, turn_id, started, finish)
         VALUES (?1, ?2, ?3, ?4)
         ON CONFLICT (session_id) DO UPDATE SET finish = excluded.finish,
             turn_id = excluded.turn_id,
             started = CASE WHEN execution_claim.turn_id = excluded.turn_id
                            THEN execution_claim.started ELSE excluded.started END",
        params![session, turn, now_ms(), f],
    )?;
    Ok(())
}

fn claim_row(r: &rusqlite::Row<'_>) -> rusqlite::Result<(ClaimRow, Option<String>)> {
    Ok((
        ClaimRow {
            session_id: r.get(0)?,
            turn_id: r.get(1)?,
            started: r.get(2)?,
            daemon_pid: r.get(3)?,
            daemon_boot_id: r.get(4)?,
            finish: None,
        },
        r.get(5)?,
    ))
}

fn parse_claim((mut c, f): (ClaimRow, Option<String>)) -> Result<ClaimRow> {
    c.finish = f
        .map(|s| serde_json::from_value(serde_json::Value::String(s)))
        .transpose()?;
    Ok(c)
}

const CLAIM_COLS: &str = "session_id, turn_id, started, daemon_pid, daemon_boot_id, finish";

pub fn last_turn(conn: &Connection, session: &str) -> Result<Option<ClaimRow>> {
    conn.query_row(
        &format!("SELECT {CLAIM_COLS} FROM execution_claim WHERE session_id = ?1"),
        [session],
        claim_row,
    )
    .optional()?
    .map(parse_claim)
    .transpose()
}

/// Claims still open (scanned on daemon start → `turn.interrupted`, 01 §4.3).
pub fn unsettled_claims(conn: &Connection) -> Result<Vec<ClaimRow>> {
    let mut st = conn.prepare(&format!(
        "SELECT {CLAIM_COLS} FROM execution_claim WHERE finish IS NULL ORDER BY started"
    ))?;
    let rows = st.query_map([], claim_row)?;
    rows.map(|r| parse_claim(r?)).collect()
}

// ---- inbox (01 §5.2) ----

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InboxState {
    Pending,
    Admitted,
    Removed,
}

impl InboxState {
    fn as_str(self) -> &'static str {
        match self {
            InboxState::Pending => "pending",
            InboxState::Admitted => "admitted",
            InboxState::Removed => "removed",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct InboxItem {
    pub id: String,
    pub session_id: String,
    pub delivery: Delivery,
    pub parts: serde_json::Value,
    pub source: Option<String>,
    pub client_msg_id: Option<String>,
    pub state: InboxState,
    pub created: i64,
}

/// Add an inbox item. A repeated `client_msg_id` for the same session returns the
/// existing item (idempotent send) with `false`.
pub fn inbox_add(
    conn: &Connection,
    session: &str,
    delivery: Delivery,
    parts: &serde_json::Value,
    source: Option<&str>,
    client_msg_id: Option<&str>,
) -> Result<(InboxItem, bool)> {
    if let Some(cm) = client_msg_id
        && let Some(existing) = inbox_by_client_id(conn, session, cm)?
    {
        return Ok((existing, false));
    }
    let item = InboxItem {
        id: new_id("inb"),
        session_id: session.to_string(),
        delivery,
        parts: parts.clone(),
        source: source.map(String::from),
        client_msg_id: client_msg_id.map(String::from),
        state: InboxState::Pending,
        created: now_ms(),
    };
    conn.execute(
        "INSERT INTO inbox (id, session_id, delivery, parts, source, client_msg_id, state, created)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, 'pending', ?7)",
        params![
            item.id,
            item.session_id,
            delivery_str(delivery),
            serde_json::to_string(parts)?,
            item.source,
            item.client_msg_id,
            item.created
        ],
    )?;
    Ok((item, true))
}

fn delivery_str(d: Delivery) -> &'static str {
    match d {
        Delivery::Steer => "steer",
        Delivery::Queue => "queue",
    }
}

const INBOX_COLS: &str = "id, session_id, delivery, parts, source, client_msg_id, state, created";

fn inbox_row(r: &rusqlite::Row<'_>) -> rusqlite::Result<InboxItem> {
    let delivery: String = r.get(2)?;
    let parts: String = r.get(3)?;
    let state: String = r.get(6)?;
    let bad = |i, name: &str| {
        rusqlite::Error::InvalidColumnType(i, name.to_string(), rusqlite::types::Type::Text)
    };
    Ok(InboxItem {
        id: r.get(0)?,
        session_id: r.get(1)?,
        delivery: match delivery.as_str() {
            "steer" => Delivery::Steer,
            "queue" => Delivery::Queue,
            _ => return Err(bad(2, "delivery")),
        },
        parts: serde_json::from_str(&parts).map_err(|_| bad(3, "parts"))?,
        source: r.get(4)?,
        client_msg_id: r.get(5)?,
        state: match state.as_str() {
            "pending" => InboxState::Pending,
            "admitted" => InboxState::Admitted,
            "removed" => InboxState::Removed,
            _ => return Err(bad(6, "state")),
        },
        created: r.get(7)?,
    })
}

fn inbox_by_client_id(conn: &Connection, session: &str, cm: &str) -> Result<Option<InboxItem>> {
    Ok(conn
        .query_row(
            &format!("SELECT {INBOX_COLS} FROM inbox WHERE session_id = ?1 AND client_msg_id = ?2"),
            [session, cm],
            inbox_row,
        )
        .optional()?)
}

pub fn inbox_set_state(conn: &Connection, id: &str, state: InboxState) -> Result<()> {
    let n = conn.execute(
        "UPDATE inbox SET state = ?2 WHERE id = ?1",
        params![id, state.as_str()],
    )?;
    if n == 0 {
        return Err(StoreError::NotFound(id.to_string()));
    }
    Ok(())
}

/// Pending items in arrival order (rebuilt into the actor's inbox on restart).
pub fn inbox_pending(conn: &Connection, session: &str) -> Result<Vec<InboxItem>> {
    let mut st = conn.prepare(&format!(
        "SELECT {INBOX_COLS} FROM inbox WHERE session_id = ?1 AND state = 'pending'
         ORDER BY created, id"
    ))?;
    let rows = st.query_map([session], inbox_row)?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}
