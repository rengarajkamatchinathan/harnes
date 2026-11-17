//! Memory entries per scope (04 §3.4, D31) and `memory_search` over `memory_fts` (§6).
//! Incognito sessions never reach this table (they are never stored).

use rusqlite::{Connection, OptionalExtension, Row, params};
use serde::{Deserialize, Serialize};

use crate::error::{Result, StoreError};
use crate::util::{fts_query, new_id, now_ms};

/// The scope value for global memory; otherwise a project or workspace id.
pub const GLOBAL: &str = "global";

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Memory {
    pub id: String,
    pub scope: String,
    pub text: String,
    pub source_session: Option<String>,
    pub source_message: Option<String>,
    pub created: i64,
    pub updated: i64,
    pub last_used: Option<i64>,
}

const COLS: &str =
    "m.id, m.scope, m.text, m.source_session, m.source_message, m.created, m.updated, m.last_used";

fn row(r: &Row<'_>) -> rusqlite::Result<Memory> {
    Ok(Memory {
        id: r.get(0)?,
        scope: r.get(1)?,
        text: r.get(2)?,
        source_session: r.get(3)?,
        source_message: r.get(4)?,
        created: r.get(5)?,
        updated: r.get(6)?,
        last_used: r.get(7)?,
    })
}

pub fn add(
    conn: &Connection,
    scope: &str,
    text: &str,
    source_session: Option<&str>,
    source_message: Option<&str>,
) -> Result<Memory> {
    let id = new_id("mem");
    let now = now_ms();
    conn.execute(
        "INSERT INTO memory (id, scope, text, source_session, source_message, created, updated)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?6)",
        params![id, scope, text, source_session, source_message, now],
    )?;
    get(conn, &id)?.ok_or(StoreError::NotFound(id))
}

pub fn get(conn: &Connection, id: &str) -> Result<Option<Memory>> {
    Ok(conn
        .query_row(
            &format!("SELECT {COLS} FROM memory m WHERE m.id = ?1"),
            [id],
            row,
        )
        .optional()?)
}

pub fn update_text(conn: &Connection, id: &str, text: &str) -> Result<()> {
    let n = conn.execute(
        "UPDATE memory SET text = ?2, updated = ?3 WHERE id = ?1",
        params![id, text, now_ms()],
    )?;
    if n == 0 {
        return Err(StoreError::NotFound(id.to_string()));
    }
    Ok(())
}

pub fn touch(conn: &Connection, id: &str) -> Result<()> {
    conn.execute(
        "UPDATE memory SET last_used = ?2 WHERE id = ?1",
        params![id, now_ms()],
    )?;
    Ok(())
}

pub fn delete(conn: &Connection, id: &str) -> Result<bool> {
    Ok(conn.execute("DELETE FROM memory WHERE id = ?1", [id])? > 0)
}

/// Entries of a scope, most recently used first.
pub fn list(conn: &Connection, scope: &str) -> Result<Vec<Memory>> {
    let mut st = conn.prepare(&format!(
        "SELECT {COLS} FROM memory m WHERE m.scope = ?1
         ORDER BY m.last_used DESC NULLS LAST, m.updated DESC"
    ))?;
    let rows = st.query_map([scope], row)?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

/// Full-text search within the given scopes, best match first.
pub fn search(conn: &Connection, scopes: &[&str], query: &str, limit: u32) -> Result<Vec<Memory>> {
    let Some(q) = fts_query(query) else {
        return Ok(vec![]);
    };
    let placeholders = (0..scopes.len())
        .map(|i| format!("?{}", i + 2))
        .collect::<Vec<_>>()
        .join(", ");
    let sql = format!(
        "SELECT {COLS} FROM memory_fts f JOIN memory m ON m.rowid = f.rowid
         WHERE memory_fts MATCH ?1 AND m.scope IN ({placeholders})
         ORDER BY bm25(memory_fts) LIMIT {limit}"
    );
    let mut args: Vec<&dyn rusqlite::ToSql> = vec![&q];
    for s in scopes {
        args.push(s);
    }
    let mut st = conn.prepare(&sql)?;
    let rows = st.query_map(args.as_slice(), row)?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}
