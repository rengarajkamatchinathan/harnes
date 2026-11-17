//! Messages, parts and wire records; the in-place branch tree (04 §3.2, D24).
//!
//! Editing or regenerating inserts a sibling (same `parent_id`). The model context is
//! the path root → `session.active_leaf_id`.

use rusqlite::{Connection, OptionalExtension, Row, params};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::error::{Result, StoreError};
use crate::util::{new_id, now_ms, to_i64, to_u64};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum MessageKind {
    #[default]
    Normal,
    /// D33: a compaction summary node.
    Compaction,
    /// Subagent report, background job, fix-with-genie.
    Synthetic,
}

impl MessageKind {
    pub fn as_str(self) -> &'static str {
        match self {
            MessageKind::Normal => "normal",
            MessageKind::Compaction => "compaction",
            MessageKind::Synthetic => "synthetic",
        }
    }

    fn parse(s: &str) -> rusqlite::Result<Self> {
        match s {
            "normal" => Ok(MessageKind::Normal),
            "compaction" => Ok(MessageKind::Compaction),
            "synthetic" => Ok(MessageKind::Synthetic),
            _ => Err(rusqlite::Error::InvalidColumnType(
                13,
                "kind".into(),
                rusqlite::types::Type::Text,
            )),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Message {
    pub id: String,
    pub session_id: String,
    pub parent_id: Option<String>,
    pub role: String,
    /// Seq of `message.admitted` / the message's first durable event.
    pub seq: Option<u64>,
    pub created: i64,
    pub model: Option<String>,
    pub provider: Option<String>,
    pub origin: Option<Value>,
    pub usage: Option<Value>,
    pub cost_usd: Option<f64>,
    pub finish: Option<String>,
    pub error: Option<Value>,
    pub kind: MessageKind,
    pub snapshot_before: Option<String>,
    pub snapshot_after: Option<String>,
}

impl Message {
    /// A new message with a fresh `msg_…` id, created now.
    pub fn new(session_id: &str, parent_id: Option<&str>, role: &str) -> Self {
        Message {
            id: new_id("msg"),
            session_id: session_id.to_string(),
            parent_id: parent_id.map(String::from),
            role: role.to_string(),
            seq: None,
            created: now_ms(),
            model: None,
            provider: None,
            origin: None,
            usage: None,
            cost_usd: None,
            finish: None,
            error: None,
            kind: MessageKind::Normal,
            snapshot_before: None,
            snapshot_after: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Part {
    pub id: String,
    pub message_id: String,
    pub ordinal: i64,
    /// text | reasoning | media | tool_call | tool_result | citation | compaction |
    /// effort_marker | unknown | artifact_ref | memory_chip | subagent_ref | attachment
    pub kind: String,
    pub body: Value,
    pub blob_id: Option<String>,
    pub call_id: Option<String>,
}

impl Part {
    pub fn new(message_id: &str, ordinal: i64, kind: &str, body: Value) -> Self {
        Part {
            id: new_id("prt"),
            message_id: message_id.to_string(),
            ordinal,
            kind: kind.to_string(),
            body,
            blob_id: None,
            call_id: None,
        }
    }

    /// A `text` part: body `{"text": …}` (indexed by `message_fts`).
    pub fn text(message_id: &str, ordinal: i64, text: &str) -> Self {
        Part::new(
            message_id,
            ordinal,
            "text",
            serde_json::json!({ "text": text }),
        )
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WireRecord {
    pub id: String,
    pub message_id: String,
    pub step: i64,
    pub provider: String,
    pub protocol: String,
    pub route: Option<String>,
    pub account_tag: Option<Vec<u8>>,
    pub request_blob: Option<String>,
    pub response_blob: Option<String>,
    pub id_map: Option<Value>,
    pub created: i64,
}

fn json_opt(v: &Option<Value>) -> Result<Option<String>> {
    v.as_ref()
        .map(serde_json::to_string)
        .transpose()
        .map_err(Into::into)
}

fn parse_json(s: Option<String>) -> rusqlite::Result<Option<Value>> {
    s.map(|s| {
        serde_json::from_str(&s).map_err(|e| {
            rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Text, Box::new(e))
        })
    })
    .transpose()
}

const MSG_COLS: &str = "id, session_id, parent_id, role, seq, created, model, provider, origin,
    usage, cost_usd, finish, error, kind, snapshot_before, snapshot_after";

fn msg_row(r: &Row<'_>) -> rusqlite::Result<Message> {
    let seq: Option<i64> = r.get(4)?;
    let kind: String = r.get(13)?;
    Ok(Message {
        id: r.get(0)?,
        session_id: r.get(1)?,
        parent_id: r.get(2)?,
        role: r.get(3)?,
        seq: seq.map(to_u64),
        created: r.get(5)?,
        model: r.get(6)?,
        provider: r.get(7)?,
        origin: parse_json(r.get(8)?)?,
        usage: parse_json(r.get(9)?)?,
        cost_usd: r.get(10)?,
        finish: r.get(11)?,
        error: parse_json(r.get(12)?)?,
        kind: MessageKind::parse(&kind)?,
        snapshot_before: r.get(14)?,
        snapshot_after: r.get(15)?,
    })
}

/// Insert a message. Its parent (if any) must be in the same session.
pub fn insert(conn: &Connection, m: &Message) -> Result<()> {
    if let Some(p) = &m.parent_id {
        let parent_session: Option<String> = conn
            .query_row("SELECT session_id FROM message WHERE id = ?1", [p], |r| {
                r.get(0)
            })
            .optional()?;
        if parent_session.as_deref() != Some(m.session_id.as_str()) {
            return Err(StoreError::Invalid(format!(
                "parent {p} is not a message of {}",
                m.session_id
            )));
        }
    }
    conn.execute(
        &format!("INSERT INTO message ({MSG_COLS}) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16)"),
        params![
            m.id,
            m.session_id,
            m.parent_id,
            m.role,
            m.seq.map(to_i64).transpose()?,
            m.created,
            m.model,
            m.provider,
            json_opt(&m.origin)?,
            json_opt(&m.usage)?,
            m.cost_usd,
            m.finish,
            json_opt(&m.error)?,
            m.kind.as_str(),
            m.snapshot_before,
            m.snapshot_after
        ],
    )?;
    Ok(())
}

/// Update the fields a message gets when it finishes.
pub fn finish(
    conn: &Connection,
    id: &str,
    finish: Option<&str>,
    usage: Option<&Value>,
    cost_usd: Option<f64>,
    error: Option<&Value>,
) -> Result<()> {
    conn.execute(
        "UPDATE message SET finish = ?2, usage = ?3, cost_usd = ?4, error = ?5 WHERE id = ?1",
        params![
            id,
            finish,
            usage.map(serde_json::to_string).transpose()?,
            cost_usd,
            error.map(serde_json::to_string).transpose()?
        ],
    )?;
    Ok(())
}

/// Record the usage (JSON) and cost of the step that produced an assistant message.
pub fn set_usage(conn: &Connection, id: &str, usage: &Value, cost_usd: f64) -> Result<()> {
    conn.execute(
        "UPDATE message SET usage = ?2, cost_usd = ?3 WHERE id = ?1",
        params![id, serde_json::to_string(usage)?, cost_usd],
    )?;
    Ok(())
}

pub fn set_snapshots(
    conn: &Connection,
    id: &str,
    before: Option<&str>,
    after: Option<&str>,
) -> Result<()> {
    conn.execute(
        "UPDATE message SET snapshot_before = coalesce(?2, snapshot_before),
                            snapshot_after = coalesce(?3, snapshot_after) WHERE id = ?1",
        params![id, before, after],
    )?;
    Ok(())
}

pub fn get(conn: &Connection, id: &str) -> Result<Option<Message>> {
    Ok(conn
        .query_row(
            &format!("SELECT {MSG_COLS} FROM message WHERE id = ?1"),
            [id],
            msg_row,
        )
        .optional()?)
}

/// Children of `parent` (`None` = roots), oldest first. Siblings of a message are
/// `children(session, message.parent_id)`.
pub fn children(conn: &Connection, session: &str, parent: Option<&str>) -> Result<Vec<Message>> {
    let mut st = conn.prepare(&format!(
        "SELECT {MSG_COLS} FROM message WHERE session_id = ?1 AND parent_id IS ?2
         ORDER BY created, id"
    ))?;
    let rows = st.query_map(params![session, parent], msg_row)?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

/// All messages of a session in creation order.
pub fn all(conn: &Connection, session: &str) -> Result<Vec<Message>> {
    let mut st = conn.prepare(&format!(
        "SELECT {MSG_COLS} FROM message WHERE session_id = ?1 ORDER BY created, id"
    ))?;
    let rows = st.query_map([session], msg_row)?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

/// The path root → `leaf`.
pub fn path_to(conn: &Connection, leaf: &str) -> Result<Vec<Message>> {
    let mut st = conn.prepare(&format!(
        "WITH RECURSIVE up(id, depth) AS (
             SELECT ?1, 0
             UNION ALL
             SELECT m.parent_id, up.depth + 1 FROM message m JOIN up ON m.id = up.id
             WHERE m.parent_id IS NOT NULL
         )
         SELECT {} FROM up JOIN message m ON m.id = up.id ORDER BY up.depth DESC",
        MSG_COLS
            .split(',')
            .map(|c| format!("m.{}", c.trim()))
            .collect::<Vec<_>>()
            .join(", ")
    ))?;
    let rows = st.query_map([leaf], msg_row)?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

/// The model context: root → `session.active_leaf_id` (empty if unset).
pub fn active_path(conn: &Connection, session: &str) -> Result<Vec<Message>> {
    let leaf: Option<String> = conn
        .query_row(
            "SELECT active_leaf_id FROM session WHERE id = ?1",
            [session],
            |r| r.get(0),
        )
        .optional()?
        .flatten();
    match leaf {
        Some(l) => path_to(conn, &l),
        None => Ok(vec![]),
    }
}

/// The newest leaf under `id` following the latest child at each level. Used to pick
/// the active leaf when the user switches to another sibling.
pub fn latest_leaf(conn: &Connection, id: &str) -> Result<String> {
    let mut cur = id.to_string();
    loop {
        let next: Option<String> = conn
            .query_row(
                "SELECT id FROM message WHERE parent_id = ?1 ORDER BY created DESC, id DESC LIMIT 1",
                [&cur],
                |r| r.get(0),
            )
            .optional()?;
        match next {
            Some(n) => cur = n,
            None => return Ok(cur),
        }
    }
}

const PART_COLS: &str = "id, message_id, ordinal, kind, body, blob_id, call_id";

fn part_row(r: &Row<'_>) -> rusqlite::Result<Part> {
    let body: String = r.get(4)?;
    Ok(Part {
        id: r.get(0)?,
        message_id: r.get(1)?,
        ordinal: r.get(2)?,
        kind: r.get(3)?,
        body: parse_json(Some(body))?.unwrap_or(Value::Null),
        blob_id: r.get(5)?,
        call_id: r.get(6)?,
    })
}

pub fn insert_part(conn: &Connection, p: &Part) -> Result<()> {
    conn.execute(
        &format!("INSERT INTO part ({PART_COLS}) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)"),
        params![
            p.id,
            p.message_id,
            p.ordinal,
            p.kind,
            serde_json::to_string(&p.body)?,
            p.blob_id,
            p.call_id
        ],
    )?;
    Ok(())
}

/// Replace a part's body (e.g. a streamed text part settling).
pub fn update_part_body(conn: &Connection, id: &str, body: &Value) -> Result<()> {
    conn.execute(
        "UPDATE part SET body = ?2 WHERE id = ?1",
        params![id, serde_json::to_string(body)?],
    )?;
    Ok(())
}

pub fn parts(conn: &Connection, message_id: &str) -> Result<Vec<Part>> {
    let mut st = conn.prepare(&format!(
        "SELECT {PART_COLS} FROM part WHERE message_id = ?1 ORDER BY ordinal"
    ))?;
    let rows = st.query_map([message_id], part_row)?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

/// Parts by tool `call_id` (tool_call + tool_result).
pub fn parts_by_call(conn: &Connection, call_id: &str) -> Result<Vec<Part>> {
    let mut st = conn.prepare(&format!(
        "SELECT {PART_COLS} FROM part WHERE call_id = ?1 ORDER BY message_id, ordinal"
    ))?;
    let rows = st.query_map([call_id], part_row)?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

pub fn insert_wire_record(conn: &Connection, w: &WireRecord) -> Result<()> {
    conn.execute(
        "INSERT INTO wire_record (id, message_id, step, provider, protocol, route, account_tag,
             request_blob, response_blob, id_map, created)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
        params![
            w.id,
            w.message_id,
            w.step,
            w.provider,
            w.protocol,
            w.route,
            w.account_tag,
            w.request_blob,
            w.response_blob,
            json_opt(&w.id_map)?,
            w.created
        ],
    )?;
    Ok(())
}

pub fn wire_records(conn: &Connection, message_id: &str) -> Result<Vec<WireRecord>> {
    let mut st = conn.prepare(
        "SELECT id, message_id, step, provider, protocol, route, account_tag, request_blob,
                response_blob, id_map, created
         FROM wire_record WHERE message_id = ?1 ORDER BY step",
    )?;
    let rows = st.query_map([message_id], |r| {
        Ok(WireRecord {
            id: r.get(0)?,
            message_id: r.get(1)?,
            step: r.get(2)?,
            provider: r.get(3)?,
            protocol: r.get(4)?,
            route: r.get(5)?,
            account_tag: r.get(6)?,
            request_blob: r.get(7)?,
            response_blob: r.get(8)?,
            id_map: parse_json(r.get(9)?)?,
            created: r.get(10)?,
        })
    })?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}
