//! Chat search (04 §6): `message_fts` for sidebar search and the `chat_search` tool,
//! `session_title_fts` (trigram) for substring title search.

use rusqlite::{Connection, params};
use serde::{Deserialize, Serialize};

use crate::error::Result;
use crate::util::fts_query;

/// `chat_search` scopes (D31).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Scope {
    /// Chats of one project.
    Project(String),
    /// Chats outside any project.
    NonProject,
    All,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Hit {
    pub session_id: String,
    /// `None` for a title match.
    pub message_id: Option<String>,
    /// Matched text with `[` `]` around hits.
    pub snippet: String,
    /// bm25 score (lower is better).
    pub rank: f64,
}

pub fn chat_search(conn: &Connection, query: &str, scope: &Scope, limit: u32) -> Result<Vec<Hit>> {
    let Some(q) = fts_query(query) else {
        return Ok(vec![]);
    };
    let (filter, arg) = match scope {
        Scope::Project(p) => ("s.project_id = ?2", Some(p.as_str())),
        Scope::NonProject => ("s.project_id IS NULL AND ?2 IS NULL", None),
        Scope::All => ("?2 IS NULL", None),
    };
    let sql = format!(
        "SELECT f.session_id, f.message_id,
                coalesce(snippet(message_fts, 0, '[', ']', '…', 12),
                         snippet(message_fts, 1, '[', ']', '…', 12)),
                bm25(message_fts) AS rank
         FROM message_fts f JOIN session s ON s.id = f.session_id
         WHERE message_fts MATCH ?1 AND s.archived = 0 AND {filter}
         ORDER BY rank LIMIT {limit}"
    );
    let mut st = conn.prepare(&sql)?;
    let rows = st.query_map(params![q, arg], |r| {
        Ok(Hit {
            session_id: r.get(0)?,
            message_id: r.get(1)?,
            snippet: r.get::<_, Option<String>>(2)?.unwrap_or_default(),
            rank: r.get(3)?,
        })
    })?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

/// Session ids whose title contains `needle` (case-insensitive substring), newest first.
pub fn title_search(conn: &Connection, needle: &str, limit: u32) -> Result<Vec<String>> {
    let needle = needle.trim();
    if needle.is_empty() {
        return Ok(vec![]);
    }
    // The trigram tokenizer needs at least 3 characters; shorter needles scan.
    let rows: Vec<String> = if needle.chars().count() >= 3 {
        let q = format!("\"{}\"", needle.replace('"', "\"\""));
        let mut st = conn.prepare(&format!(
            "SELECT s.id FROM session_title_fts f JOIN session s ON s.rowid = f.rowid
             WHERE session_title_fts MATCH ?1 ORDER BY s.updated DESC LIMIT {limit}"
        ))?;
        st.query_map([q], |r| r.get(0))?
            .collect::<rusqlite::Result<_>>()?
    } else {
        let pat = format!(
            "%{}%",
            needle
                .replace('\\', "\\\\")
                .replace('%', "\\%")
                .replace('_', "\\_")
        );
        let mut st = conn.prepare(&format!(
            "SELECT id FROM session WHERE title LIKE ?1 ESCAPE '\\' ORDER BY updated DESC LIMIT {limit}"
        ))?;
        st.query_map([pat], |r| r.get(0))?
            .collect::<rusqlite::Result<_>>()?
    };
    Ok(rows)
}

/// Rebuild every FTS index from its source tables (after VACUUM, which may renumber
/// rowids of tables without an INTEGER PRIMARY KEY).
pub fn rebuild_all(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        "DELETE FROM message_fts;
         INSERT INTO message_fts (rowid, text, title, session_id, message_id)
           SELECT p.rowid, json_extract(p.body, '$.text'), NULL, m.session_id, m.id
           FROM part p JOIN message m ON m.id = p.message_id WHERE p.kind = 'text';
         INSERT INTO message_fts (rowid, text, title, session_id, message_id)
           SELECT -rowid, NULL, title, id, NULL FROM session WHERE title IS NOT NULL;
         DELETE FROM session_title_fts;
         INSERT INTO session_title_fts (rowid, title)
           SELECT rowid, title FROM session WHERE title IS NOT NULL;
         INSERT INTO memory_fts (memory_fts) VALUES ('rebuild');
         INSERT INTO kchunk_fts (kchunk_fts) VALUES ('rebuild');",
    )?;
    Ok(())
}
