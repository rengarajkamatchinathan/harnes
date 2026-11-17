//! `kv` (migration cursors, flags, budget state) and `usage_daily` (04 §3.4, D34).

use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::error::Result;

pub fn get(conn: &Connection, key: &str) -> Result<Option<Value>> {
    let s: Option<String> = conn
        .query_row("SELECT value FROM kv WHERE key = ?1", [key], |r| r.get(0))
        .optional()?;
    Ok(s.map(|s| serde_json::from_str(&s)).transpose()?)
}

pub fn set(conn: &Connection, key: &str, value: &Value) -> Result<()> {
    conn.execute(
        "INSERT INTO kv (key, value) VALUES (?1, ?2)
         ON CONFLICT (key) DO UPDATE SET value = excluded.value",
        params![key, serde_json::to_string(value)?],
    )?;
    Ok(())
}

pub fn delete(conn: &Connection, key: &str) -> Result<()> {
    conn.execute("DELETE FROM kv WHERE key = ?1", [key])?;
    Ok(())
}

/// One usage increment. `project_id` empty = outside a project.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Usage {
    /// `YYYY-MM-DD` (local day, chosen by the caller).
    pub day: String,
    pub provider: String,
    pub model: String,
    pub project_id: String,
    /// chat | compaction | title | subagent | embedding | sampling | search
    pub kind: String,
    pub tokens_in: i64,
    pub tokens_out: i64,
    pub cache_read: i64,
    pub cache_write: i64,
    pub calls: i64,
    pub cost_usd: f64,
}

/// Add to the day's row (upsert).
pub fn add_usage(conn: &Connection, u: &Usage) -> Result<()> {
    conn.execute(
        "INSERT INTO usage_daily (day, provider, model, project_id, kind, tokens_in, tokens_out,
             cache_read, cache_write, calls, cost_usd)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)
         ON CONFLICT (day, provider, model, project_id, kind) DO UPDATE SET
             tokens_in = tokens_in + excluded.tokens_in,
             tokens_out = tokens_out + excluded.tokens_out,
             cache_read = cache_read + excluded.cache_read,
             cache_write = cache_write + excluded.cache_write,
             calls = calls + excluded.calls,
             cost_usd = cost_usd + excluded.cost_usd",
        params![
            u.day,
            u.provider,
            u.model,
            u.project_id,
            u.kind,
            u.tokens_in,
            u.tokens_out,
            u.cache_read,
            u.cache_write,
            u.calls,
            u.cost_usd
        ],
    )?;
    Ok(())
}

/// Rows with `from <= day <= to`, ordered by day.
pub fn usage_between(conn: &Connection, from: &str, to: &str) -> Result<Vec<Usage>> {
    let mut st = conn.prepare(
        "SELECT day, provider, model, project_id, kind, tokens_in, tokens_out, cache_read,
                cache_write, calls, cost_usd
         FROM usage_daily WHERE day BETWEEN ?1 AND ?2 ORDER BY day, provider, model, kind",
    )?;
    let rows = st.query_map([from, to], |r| {
        Ok(Usage {
            day: r.get(0)?,
            provider: r.get(1)?,
            model: r.get(2)?,
            project_id: r.get(3)?,
            kind: r.get(4)?,
            tokens_in: r.get(5)?,
            tokens_out: r.get(6)?,
            cache_read: r.get(7)?,
            cache_write: r.get(8)?,
            calls: r.get(9)?,
            cost_usd: r.get(10)?,
        })
    })?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}
