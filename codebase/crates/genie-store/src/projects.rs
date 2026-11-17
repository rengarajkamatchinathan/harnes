//! Chat projects, project files, workspaces and worktrees (04 §3.1).

use rusqlite::{Connection, OptionalExtension, Row, params};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::error::{Result, StoreError};
use crate::util::{new_id, now_ms};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Project {
    pub id: String,
    pub name: String,
    pub instructions: Option<String>,
    pub settings: Option<Value>,
    pub starred: bool,
    pub created: i64,
    pub updated: i64,
}

fn project_row(r: &Row<'_>) -> rusqlite::Result<Project> {
    let settings: Option<String> = r.get(3)?;
    Ok(Project {
        id: r.get(0)?,
        name: r.get(1)?,
        instructions: r.get(2)?,
        settings: settings.and_then(|s| serde_json::from_str(&s).ok()),
        starred: r.get(4)?,
        created: r.get(5)?,
        updated: r.get(6)?,
    })
}

const PROJECT_COLS: &str = "id, name, instructions, settings, starred, created, updated";

pub fn create(conn: &Connection, name: &str, instructions: Option<&str>) -> Result<Project> {
    let id = new_id("prj");
    let now = now_ms();
    conn.execute(
        "INSERT INTO project (id, name, instructions, created, updated) VALUES (?1, ?2, ?3, ?4, ?4)",
        params![id, name, instructions, now],
    )?;
    get(conn, &id)?.ok_or(StoreError::NotFound(id))
}

pub fn get(conn: &Connection, id: &str) -> Result<Option<Project>> {
    Ok(conn
        .query_row(
            &format!("SELECT {PROJECT_COLS} FROM project WHERE id = ?1"),
            [id],
            project_row,
        )
        .optional()?)
}

pub fn list(conn: &Connection) -> Result<Vec<Project>> {
    let mut st = conn.prepare(&format!(
        "SELECT {PROJECT_COLS} FROM project ORDER BY starred DESC, updated DESC"
    ))?;
    let rows = st.query_map([], project_row)?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

pub fn update(
    conn: &Connection,
    id: &str,
    name: &str,
    instructions: Option<&str>,
    settings: Option<&Value>,
    starred: bool,
) -> Result<()> {
    let n = conn.execute(
        "UPDATE project SET name = ?2, instructions = ?3, settings = ?4, starred = ?5, updated = ?6
         WHERE id = ?1",
        params![
            id,
            name,
            instructions,
            settings.map(serde_json::to_string).transpose()?,
            starred,
            now_ms()
        ],
    )?;
    if n == 0 {
        return Err(StoreError::NotFound(id.to_string()));
    }
    Ok(())
}

/// Deletes the project, its files and retrieval chunks; its chats stay as non-project
/// chats (`session.project_id` → NULL).
pub fn delete(conn: &Connection, id: &str) -> Result<bool> {
    Ok(conn.execute("DELETE FROM project WHERE id = ?1", [id])? > 0)
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProjectFile {
    pub id: String,
    pub project_id: String,
    pub name: String,
    pub mime: Option<String>,
    pub blob_id: Option<String>,
    pub tokens: Option<i64>,
    pub sha256: Option<String>,
    pub added: i64,
}

const FILE_COLS: &str = "id, project_id, name, mime, blob_id, tokens, sha256, added";

fn file_row(r: &Row<'_>) -> rusqlite::Result<ProjectFile> {
    Ok(ProjectFile {
        id: r.get(0)?,
        project_id: r.get(1)?,
        name: r.get(2)?,
        mime: r.get(3)?,
        blob_id: r.get(4)?,
        tokens: r.get(5)?,
        sha256: r.get(6)?,
        added: r.get(7)?,
    })
}

/// Add a file whose content is already a blob (`Store::put_blob`).
pub fn add_file(
    conn: &Connection,
    project_id: &str,
    name: &str,
    mime: Option<&str>,
    blob_id: &str,
    tokens: Option<i64>,
) -> Result<ProjectFile> {
    let id = new_id("pfl");
    conn.execute(
        &format!("INSERT INTO project_file ({FILE_COLS}) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?5, ?7)"),
        params![id, project_id, name, mime, blob_id, tokens, now_ms()],
    )?;
    conn.query_row(
        &format!("SELECT {FILE_COLS} FROM project_file WHERE id = ?1"),
        [&id],
        file_row,
    )
    .map_err(Into::into)
}

pub fn files(conn: &Connection, project_id: &str) -> Result<Vec<ProjectFile>> {
    let mut st = conn.prepare(&format!(
        "SELECT {FILE_COLS} FROM project_file WHERE project_id = ?1 ORDER BY added, id"
    ))?;
    let rows = st.query_map([project_id], file_row)?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

pub fn remove_file(conn: &Connection, id: &str) -> Result<bool> {
    Ok(conn.execute("DELETE FROM project_file WHERE id = ?1", [id])? > 0)
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Workspace {
    pub id: String,
    pub canonical_path: String,
    pub identity: Option<String>,
    pub git_root: Option<String>,
    pub created: i64,
    pub last_opened: i64,
}

/// Find or create the workspace for a canonical path, bumping `last_opened`.
pub fn open_workspace(
    conn: &Connection,
    canonical_path: &str,
    identity: Option<&str>,
    git_root: Option<&str>,
) -> Result<Workspace> {
    let now = now_ms();
    conn.execute(
        "INSERT INTO workspace (id, canonical_path, identity, git_root, created, last_opened)
         VALUES (?1, ?2, ?3, ?4, ?5, ?5)
         ON CONFLICT (canonical_path) DO UPDATE SET last_opened = excluded.last_opened,
             identity = coalesce(excluded.identity, workspace.identity),
             git_root = coalesce(excluded.git_root, workspace.git_root)",
        params![new_id("wsp"), canonical_path, identity, git_root, now],
    )?;
    conn.query_row(
        "SELECT id, canonical_path, identity, git_root, created, last_opened
         FROM workspace WHERE canonical_path = ?1",
        [canonical_path],
        |r| {
            Ok(Workspace {
                id: r.get(0)?,
                canonical_path: r.get(1)?,
                identity: r.get(2)?,
                git_root: r.get(3)?,
                created: r.get(4)?,
                last_opened: r.get(5)?,
            })
        },
    )
    .map_err(Into::into)
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Worktree {
    pub id: String,
    pub workspace_id: String,
    pub session_id: Option<String>,
    pub path: String,
    pub branch: Option<String>,
    pub base_ref: Option<String>,
    pub created_by_genie: bool,
    pub setup_hash: Option<String>,
    pub created: i64,
}

pub fn add_worktree(conn: &Connection, w: &Worktree) -> Result<()> {
    conn.execute(
        "INSERT INTO worktree (id, workspace_id, session_id, path, branch, base_ref,
             created_by_genie, setup_hash, created)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
        params![
            w.id,
            w.workspace_id,
            w.session_id,
            w.path,
            w.branch,
            w.base_ref,
            w.created_by_genie,
            w.setup_hash,
            w.created
        ],
    )?;
    Ok(())
}

pub fn worktrees(conn: &Connection, workspace_id: &str) -> Result<Vec<Worktree>> {
    let mut st = conn.prepare(
        "SELECT id, workspace_id, session_id, path, branch, base_ref, created_by_genie, setup_hash,
                created
         FROM worktree WHERE workspace_id = ?1 ORDER BY created",
    )?;
    let rows = st.query_map([workspace_id], |r| {
        Ok(Worktree {
            id: r.get(0)?,
            workspace_id: r.get(1)?,
            session_id: r.get(2)?,
            path: r.get(3)?,
            branch: r.get(4)?,
            base_ref: r.get(5)?,
            created_by_genie: r.get(6)?,
            setup_hash: r.get(7)?,
            created: r.get(8)?,
        })
    })?;
    Ok(rows.collect::<rusqlite::Result<_>>()?)
}

pub fn remove_worktree(conn: &Connection, id: &str) -> Result<bool> {
    Ok(conn.execute("DELETE FROM worktree WHERE id = ?1", [id])? > 0)
}
