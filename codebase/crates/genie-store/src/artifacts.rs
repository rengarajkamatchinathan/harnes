//! Artifacts and their versions (04 §3.4, D28).
//!
//! A version is tied to the message that produced it, so each branch has its own
//! versions; the panel shows the versions on the active path.

use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};

use crate::error::{Result, StoreError};
use crate::util::{new_id, now_ms};

/// Content above this size is stored as a blob (04 §4).
pub const INLINE_MAX: usize = 256 * 1024;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Artifact {
    pub id: String,
    pub session_id: String,
    #[serde(rename = "type")]
    pub ty: String,
    pub title: Option<String>,
    pub created: i64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ArtifactVersion {
    pub artifact_id: String,
    pub n: i64,
    pub message_id: String,
    /// Inline content (≤ [`INLINE_MAX`]); otherwise `blob_id` is set.
    pub content: Option<String>,
    pub blob_id: Option<String>,
    pub created: i64,
}

pub fn create(conn: &Connection, session: &str, ty: &str, title: Option<&str>) -> Result<Artifact> {
    let a = Artifact {
        id: new_id("art"),
        session_id: session.to_string(),
        ty: ty.to_string(),
        title: title.map(String::from),
        created: now_ms(),
    };
    conn.execute(
        "INSERT INTO artifact (id, session_id, type, title, created) VALUES (?1, ?2, ?3, ?4, ?5)",
        params![a.id, a.session_id, a.ty, a.title, a.created],
    )?;
    Ok(a)
}

pub fn get(conn: &Connection, id: &str) -> Result<Option<Artifact>> {
    Ok(conn
        .query_row(
            "SELECT id, session_id, type, title, created FROM artifact WHERE id = ?1",
            [id],
            |r| {
                Ok(Artifact {
                    id: r.get(0)?,
                    session_id: r.get(1)?,
                    ty: r.get(2)?,
                    title: r.get(3)?,
                    created: r.get(4)?,
                })
            },
        )
        .optional()?)
}

pub fn for_session(conn: &Connection, session: &str) -> Result<Vec<Artifact>> {
    let mut st = conn.prepare(
        "SELECT id, session_id, type, title, created FROM artifact WHERE session_id = ?1
         ORDER BY created, id",
    )?;
    let rows = st.query_map([session], |r| {
        Ok(Artifact {
            id: r.get(0)?,
            session_id: r.get(1)?,
            ty: r.get(2)?,
            title: r.get(3)?,
            created: r.get(4)?,
        })
    })?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

/// Append version `n = max + 1`. Exactly one of `content` / `blob_id` is set; use
/// `Store::add_artifact_version` to spill large content to a blob automatically.
pub fn add_version(
    conn: &Connection,
    artifact_id: &str,
    message_id: &str,
    content: Option<&str>,
    blob_id: Option<&str>,
) -> Result<ArtifactVersion> {
    if content.is_some() == blob_id.is_some() {
        return Err(StoreError::Invalid(
            "artifact version needs content or a blob, not both".into(),
        ));
    }
    let n: i64 = conn.query_row(
        "SELECT coalesce(max(n), 0) + 1 FROM artifact_version WHERE artifact_id = ?1",
        [artifact_id],
        |r| r.get(0),
    )?;
    let v = ArtifactVersion {
        artifact_id: artifact_id.to_string(),
        n,
        message_id: message_id.to_string(),
        content: content.map(String::from),
        blob_id: blob_id.map(String::from),
        created: now_ms(),
    };
    conn.execute(
        "INSERT INTO artifact_version (artifact_id, n, message_id, content, blob_id, created)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        params![
            v.artifact_id,
            v.n,
            v.message_id,
            v.content,
            v.blob_id,
            v.created
        ],
    )?;
    Ok(v)
}

const VERSION_COLS: &str = "v.artifact_id, v.n, v.message_id, v.content, v.blob_id, v.created";

fn version_row(r: &rusqlite::Row<'_>) -> rusqlite::Result<ArtifactVersion> {
    Ok(ArtifactVersion {
        artifact_id: r.get(0)?,
        n: r.get(1)?,
        message_id: r.get(2)?,
        content: r.get(3)?,
        blob_id: r.get(4)?,
        created: r.get(5)?,
    })
}

pub fn versions(conn: &Connection, artifact_id: &str) -> Result<Vec<ArtifactVersion>> {
    let mut st = conn.prepare(&format!(
        "SELECT {VERSION_COLS} FROM artifact_version v WHERE v.artifact_id = ?1 ORDER BY v.n"
    ))?;
    let rows = st.query_map([artifact_id], version_row)?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

/// Versions of `artifact_id` produced by messages on the session's active path.
pub fn versions_on_active_path(
    conn: &Connection,
    artifact_id: &str,
) -> Result<Vec<ArtifactVersion>> {
    let mut st = conn.prepare(&format!(
        "WITH RECURSIVE up(id) AS (
             SELECT s.active_leaf_id FROM artifact a JOIN session s ON s.id = a.session_id
             WHERE a.id = ?1 AND s.active_leaf_id IS NOT NULL
             UNION ALL
             SELECT m.parent_id FROM message m JOIN up ON m.id = up.id WHERE m.parent_id IS NOT NULL
         )
         SELECT {VERSION_COLS} FROM artifact_version v JOIN up ON v.message_id = up.id
         WHERE v.artifact_id = ?1 ORDER BY v.n"
    ))?;
    let rows = st.query_map([artifact_id], version_row)?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

/// Insert an artifact row with a caller-chosen id (the engine hands the id to the model
/// before the producing message is stored, D28). An existing id is left as it is.
pub fn insert(conn: &Connection, a: &Artifact) -> Result<()> {
    conn.execute(
        "INSERT OR IGNORE INTO artifact (id, session_id, type, title, created)
         VALUES (?1, ?2, ?3, ?4, ?5)",
        params![a.id, a.session_id, a.ty, a.title, a.created],
    )?;
    Ok(())
}
