//! `session` rows (04 §3.2).

use rusqlite::{Connection, OptionalExtension, Row, params};
use serde::{Deserialize, Serialize};

use crate::error::{Result, StoreError};
use crate::util::{new_id, now_ms, to_u64};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum SessionKind {
    #[default]
    Chat,
    Code,
}

impl SessionKind {
    pub fn as_str(self) -> &'static str {
        match self {
            SessionKind::Chat => "chat",
            SessionKind::Code => "code",
        }
    }

    fn parse(s: &str) -> Result<Self> {
        match s {
            "chat" => Ok(SessionKind::Chat),
            "code" => Ok(SessionKind::Code),
            o => Err(StoreError::Format(format!("session kind {o:?}"))),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum SessionStatus {
    #[default]
    Idle,
    Running,
    AwaitingAsk,
    Interrupted,
}

impl SessionStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            SessionStatus::Idle => "idle",
            SessionStatus::Running => "running",
            SessionStatus::AwaitingAsk => "awaiting_ask",
            SessionStatus::Interrupted => "interrupted",
        }
    }

    fn parse(s: &str) -> Result<Self> {
        match s {
            "idle" => Ok(SessionStatus::Idle),
            "running" => Ok(SessionStatus::Running),
            "awaiting_ask" => Ok(SessionStatus::AwaitingAsk),
            "interrupted" => Ok(SessionStatus::Interrupted),
            o => Err(StoreError::Format(format!("session status {o:?}"))),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Session {
    pub id: String,
    pub kind: SessionKind,
    pub project_id: Option<String>,
    pub workspace_id: Option<String>,
    pub worktree_id: Option<String>,
    pub parent_session_id: Option<String>,
    pub agent: Option<String>,
    pub title: Option<String>,
    pub model: Option<String>,
    pub effort: Option<String>,
    pub mode: Option<String>,
    pub active_leaf_id: Option<String>,
    pub status: SessionStatus,
    pub driver_client: Option<String>,
    pub head_seq: u64,
    pub log_floor_seq: u64,
    pub cost_usd: f64,
    pub tokens_in: i64,
    pub tokens_out: i64,
    pub pinned: bool,
    pub archived: bool,
    pub created: i64,
    pub updated: i64,
}

/// Fields set at creation.
#[derive(Debug, Clone, Default)]
pub struct NewSession {
    /// `None` = a fresh `ses_…` id.
    pub id: Option<String>,
    pub kind: SessionKind,
    pub project_id: Option<String>,
    pub workspace_id: Option<String>,
    pub worktree_id: Option<String>,
    pub parent_session_id: Option<String>,
    pub agent: Option<String>,
    pub title: Option<String>,
    pub model: Option<String>,
    pub effort: Option<String>,
    pub mode: Option<String>,
}

const COLS: &str = "id, kind, project_id, workspace_id, worktree_id, parent_session_id, agent,
    title, model, effort, mode, active_leaf_id, status, driver_client, head_seq, log_floor_seq,
    cost_usd, tokens_in, tokens_out, pinned, archived, created, updated";

fn from_row(r: &Row<'_>) -> rusqlite::Result<(Session, String, String)> {
    let kind: String = r.get(1)?;
    let status: String = r.get(12)?;
    Ok((
        Session {
            id: r.get(0)?,
            kind: SessionKind::Chat,
            project_id: r.get(2)?,
            workspace_id: r.get(3)?,
            worktree_id: r.get(4)?,
            parent_session_id: r.get(5)?,
            agent: r.get(6)?,
            title: r.get(7)?,
            model: r.get(8)?,
            effort: r.get(9)?,
            mode: r.get(10)?,
            active_leaf_id: r.get(11)?,
            status: SessionStatus::Idle,
            driver_client: r.get(13)?,
            head_seq: to_u64(r.get(14)?),
            log_floor_seq: to_u64(r.get(15)?),
            cost_usd: r.get(16)?,
            tokens_in: r.get(17)?,
            tokens_out: r.get(18)?,
            pinned: r.get(19)?,
            archived: r.get(20)?,
            created: r.get(21)?,
            updated: r.get(22)?,
        },
        kind,
        status,
    ))
}

fn finish(t: (Session, String, String)) -> Result<Session> {
    let (mut s, kind, status) = t;
    s.kind = SessionKind::parse(&kind)?;
    s.status = SessionStatus::parse(&status)?;
    Ok(s)
}

pub fn create(conn: &Connection, new: &NewSession) -> Result<Session> {
    let id = new.id.clone().unwrap_or_else(|| new_id("ses"));
    let now = now_ms();
    conn.execute(
        "INSERT INTO session (id, kind, project_id, workspace_id, worktree_id, parent_session_id,
            agent, title, model, effort, mode, created, updated)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?12)",
        params![
            id,
            new.kind.as_str(),
            new.project_id,
            new.workspace_id,
            new.worktree_id,
            new.parent_session_id,
            new.agent,
            new.title,
            new.model,
            new.effort,
            new.mode,
            now
        ],
    )?;
    get(conn, &id)?.ok_or_else(|| StoreError::NotFound(id))
}

/// Make sure a session row exists for `id` (a bare chat session if not). Used by the
/// `Journal` adapter while session creation is not yet routed through the store.
pub fn ensure(conn: &Connection, id: &str) -> Result<()> {
    let now = now_ms();
    conn.execute(
        "INSERT OR IGNORE INTO session (id, kind, created, updated) VALUES (?1, 'chat', ?2, ?2)",
        params![id, now],
    )?;
    Ok(())
}

pub fn get(conn: &Connection, id: &str) -> Result<Option<Session>> {
    conn.query_row(
        &format!("SELECT {COLS} FROM session WHERE id = ?1"),
        [id],
        from_row,
    )
    .optional()?
    .map(finish)
    .transpose()
}

#[derive(Debug, Clone, Default)]
pub struct ListFilter {
    pub kind: Option<SessionKind>,
    pub project_id: Option<String>,
    pub workspace_id: Option<String>,
    pub archived: bool,
    /// Include subagent sessions (default: top-level only).
    pub include_children: bool,
    pub limit: Option<u32>,
}

/// Sessions, most recently updated first (pinned first).
pub fn list(conn: &Connection, f: &ListFilter) -> Result<Vec<Session>> {
    let mut sql = format!("SELECT {COLS} FROM session WHERE archived = ?1");
    let mut args: Vec<Box<dyn rusqlite::ToSql>> = vec![Box::new(f.archived)];
    if let Some(k) = f.kind {
        args.push(Box::new(k.as_str()));
        sql.push_str(&format!(" AND kind = ?{}", args.len()));
    }
    if let Some(p) = &f.project_id {
        args.push(Box::new(p.clone()));
        sql.push_str(&format!(" AND project_id = ?{}", args.len()));
    }
    if let Some(w) = &f.workspace_id {
        args.push(Box::new(w.clone()));
        sql.push_str(&format!(" AND workspace_id = ?{}", args.len()));
    }
    if !f.include_children {
        sql.push_str(" AND parent_session_id IS NULL");
    }
    sql.push_str(" ORDER BY pinned DESC, updated DESC");
    if let Some(l) = f.limit {
        sql.push_str(&format!(" LIMIT {l}"));
    }
    let mut st = conn.prepare(&sql)?;
    let rows = st.query_map(rusqlite::params_from_iter(args.iter()), from_row)?;
    rows.map(|r| finish(r?)).collect()
}

/// Subagent sessions of `id` (D47).
pub fn children(conn: &Connection, id: &str) -> Result<Vec<Session>> {
    let mut st = conn.prepare(&format!(
        "SELECT {COLS} FROM session WHERE parent_session_id = ?1 ORDER BY created"
    ))?;
    let rows = st.query_map([id], from_row)?;
    rows.map(|r| finish(r?)).collect()
}

fn update(conn: &Connection, id: &str, set: &str, v: &dyn rusqlite::ToSql) -> Result<()> {
    let n = conn.execute(
        &format!("UPDATE session SET {set} = ?2, updated = ?3 WHERE id = ?1"),
        params![id, v, now_ms()],
    )?;
    if n == 0 {
        return Err(StoreError::NotFound(id.to_string()));
    }
    Ok(())
}

pub fn set_title(conn: &Connection, id: &str, title: Option<&str>) -> Result<()> {
    update(conn, id, "title", &title)
}

pub fn set_status(conn: &Connection, id: &str, status: SessionStatus) -> Result<()> {
    update(conn, id, "status", &status.as_str())
}

pub fn set_active_leaf(conn: &Connection, id: &str, leaf: Option<&str>) -> Result<()> {
    update(conn, id, "active_leaf_id", &leaf)
}

pub fn set_pinned(conn: &Connection, id: &str, pinned: bool) -> Result<()> {
    update(conn, id, "pinned", &pinned)
}

pub fn set_archived(conn: &Connection, id: &str, archived: bool) -> Result<()> {
    update(conn, id, "archived", &archived)
}

pub fn set_driver(conn: &Connection, id: &str, client: Option<&str>) -> Result<()> {
    update(conn, id, "driver_client", &client)
}

/// Add a turn's usage to the session totals.
pub fn add_usage(
    conn: &Connection,
    id: &str,
    tokens_in: i64,
    tokens_out: i64,
    cost_usd: f64,
) -> Result<()> {
    conn.execute(
        "UPDATE session SET tokens_in = tokens_in + ?2, tokens_out = tokens_out + ?3,
             cost_usd = cost_usd + ?4, updated = ?5 WHERE id = ?1",
        params![id, tokens_in, tokens_out, cost_usd, now_ms()],
    )?;
    Ok(())
}

/// A real delete (04 §8): cascades to messages, parts, wire records, events, inbox,
/// asks, artifacts, browser actions and child sessions; triggers release blob refs and
/// FTS rows. Returns whether the session existed.
pub fn delete(conn: &Connection, id: &str) -> Result<bool> {
    Ok(conn.execute("DELETE FROM session WHERE id = ?1", [id])? > 0)
}
