//! Export all (SA-17) and session import with id remapping (04 §9).
//!
//! **Export all** writes a plaintext `.zip`:
//!
//! ```text
//! manifest.json                 {format: "genie-export", version: 1, exported_at, counts}
//! sessions/<id>.json            the single-session JSON {info, messages, parts, artifacts}
//! projects/<id>.json            {project, files}
//! memory.json                   [memory]
//! blobs/<sha256>                decrypted blob contents (with `include_blobs`)
//! artifacts/<id>/v<n>.<ext>     every artifact version's content
//! ```
//!
//! **Import** takes one session JSON (from `session.export`, `genie export` or a
//! `sessions/<id>.json` of an export-all zip) and inserts it in one transaction. Any id that
//! already exists here (session, message, part, artifact) gets a fresh one and every
//! reference to it is rewritten; the result says whether anything was remapped. Blob
//! references to blobs this store doesn't have (or `[redacted]` ones) are dropped, sanitized
//! artifact versions without content are skipped, and event-log seqs are cleared (an
//! imported session has no event log; it is served from its messages).

use std::collections::{HashMap, HashSet};
use std::io::{Seek, Write};

use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use zip::write::SimpleFileOptions;

use crate::artifacts::{self, Artifact, ArtifactVersion};
use crate::error::{Result, StoreError};
use crate::messages::{self, Message, Part};
use crate::sessions::{self, NewSession, Session};
use crate::util::{new_id, now_ms};
use crate::{Store, memory, projects};

/// What an export-all zip holds.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExportCounts {
    pub sessions: u64,
    pub projects: u64,
    pub memories: u64,
    pub blobs: u64,
    pub artifacts: u64,
}

/// The outcome of [`import_session`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Imported {
    pub session_id: String,
    /// Some id already existed here and was replaced.
    pub remapped: bool,
    pub messages: u64,
}

fn zip_err(e: zip::result::ZipError) -> StoreError {
    StoreError::Io(std::io::Error::other(e.to_string()))
}

/// File extension for an artifact type (D28's six types).
fn artifact_ext(ty: &str) -> &'static str {
    match ty {
        "document" => "md",
        "code" => "txt",
        "svg" => "svg",
        "mermaid" => "mmd",
        "html" => "html",
        "react" => "jsx",
        _ => "txt",
    }
}

impl Store {
    /// Write every session, project, memory, artifact version (and, with `include_blobs`,
    /// every blob, decrypted) into a zip (SA-17). The caller asked the user first: the
    /// output is plaintext.
    pub fn export_all<W: Write + Seek>(&self, out: W, include_blobs: bool) -> Result<ExportCounts> {
        let mut zip = zip::ZipWriter::new(out);
        let opts = SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated);
        let mut counts = ExportCounts::default();
        let put = |zip: &mut zip::ZipWriter<W>, name: &str, data: &[u8]| -> Result<()> {
            zip.start_file(name, opts).map_err(zip_err)?;
            zip.write_all(data)?;
            Ok(())
        };

        let session_ids: Vec<String> = self.read(|c| {
            let mut st = c.prepare("SELECT id FROM session ORDER BY created")?;
            let rows = st.query_map([], |r| r.get::<_, String>(0))?;
            Ok(rows.collect::<rusqlite::Result<_>>()?)
        })?;
        for id in &session_ids {
            let v = self.read(|c| crate::export::session_json(c, id, false))?;
            put(&mut zip, &format!("sessions/{id}.json"), &serde_json::to_vec_pretty(&v)?)?;
            counts.sessions += 1;
            let arts = self.read(|c| artifacts::for_session(c, id))?;
            for a in arts {
                counts.artifacts += 1;
                for v in self.read(|c| artifacts::versions(c, &a.id))? {
                    let data = match (&v.content, &v.blob_id) {
                        (Some(t), _) => t.clone().into_bytes(),
                        (None, Some(b)) => self.read_blob(b)?,
                        (None, None) => continue,
                    };
                    let name = format!("artifacts/{}/v{}.{}", a.id, v.n, artifact_ext(&a.ty));
                    put(&mut zip, &name, &data)?;
                }
            }
        }
        for p in self.read(projects::list)? {
            let files = self.read(|c| projects::files(c, &p.id))?;
            let v = json!({ "project": p, "files": files });
            put(&mut zip, &format!("projects/{}.json", p.id), &serde_json::to_vec_pretty(&v)?)?;
            counts.projects += 1;
        }
        let mems = self.read(memory::all)?;
        counts.memories = mems.len() as u64;
        put(&mut zip, "memory.json", &serde_json::to_vec_pretty(&mems)?)?;
        if include_blobs {
            let ids: Vec<String> = self.read(|c| {
                let mut st = c.prepare("SELECT id FROM blob ORDER BY id")?;
                let rows = st.query_map([], |r| r.get::<_, String>(0))?;
                Ok(rows.collect::<rusqlite::Result<_>>()?)
            })?;
            for id in ids {
                // A row whose file was already collected is skipped, not fatal.
                let Ok(data) = self.read_blob(&id) else {
                    continue;
                };
                put(&mut zip, &format!("blobs/{id}"), &data)?;
                counts.blobs += 1;
            }
        }
        let manifest = json!({
            "format": "genie-export",
            "version": 1,
            "exported_at": genie_protocol::rfc3339_ms(u64::try_from(now_ms()).unwrap_or(0)),
            "counts": counts,
        });
        put(&mut zip, "manifest.json", &serde_json::to_vec_pretty(&manifest)?)?;
        zip.finish().map_err(zip_err)?;
        Ok(counts)
    }

    /// [`import_session`] in one write transaction.
    pub fn import_session(&self, json: &Value) -> Result<Imported> {
        self.write(|tx| import_session(tx, json))
    }
}

#[derive(Deserialize)]
struct ArtifactEntry {
    artifact: Artifact,
    #[serde(default)]
    versions: Vec<ArtifactVersion>,
}

fn exists(conn: &Connection, table: &str, id: &str) -> Result<bool> {
    Ok(conn
        .query_row(&format!("SELECT 1 FROM {table} WHERE id = ?1"), [id], |_| Ok(()))
        .optional()?
        .is_some())
}

struct Ids<'c> {
    conn: &'c Connection,
    map: HashMap<String, String>,
    remapped: bool,
}

impl Ids<'_> {
    /// The id to use for `old` in `table`: itself, or a fresh one when taken.
    fn claim(&mut self, table: &str, prefix: &str, old: &str) -> Result<String> {
        let taken = old.is_empty()
            || exists(self.conn, table, old)?
            || self.map.values().any(|v| v == old);
        let id = if taken {
            self.remapped = true;
            new_id(prefix)
        } else {
            old.to_string()
        };
        self.map.insert(old.to_string(), id.clone());
        Ok(id)
    }

    fn get(&self, old: &str) -> Option<String> {
        self.map.get(old).cloned()
    }
}

/// Insert one exported session (see the module docs).
pub fn import_session(conn: &Connection, json: &Value) -> Result<Imported> {
    let info: Session = serde_json::from_value(json.get("info").cloned().unwrap_or(Value::Null))
        .map_err(|e| StoreError::Format(format!("session info: {e}")))?;
    let msgs: Vec<Message> =
        serde_json::from_value(json.get("messages").cloned().unwrap_or(json!([])))
            .map_err(|e| StoreError::Format(format!("messages: {e}")))?;
    let parts: Vec<Part> = serde_json::from_value(json.get("parts").cloned().unwrap_or(json!([])))
        .map_err(|e| StoreError::Format(format!("parts: {e}")))?;
    let arts: Vec<ArtifactEntry> =
        serde_json::from_value(json.get("artifacts").cloned().unwrap_or(json!([])))
            .map_err(|e| StoreError::Format(format!("artifacts: {e}")))?;

    let mut ids = Ids {
        conn,
        map: HashMap::new(),
        remapped: false,
    };
    let sid = ids.claim("session", "ses", &info.id)?;
    let project_id = match &info.project_id {
        Some(p) if exists(conn, "project", p)? => Some(p.clone()),
        _ => None,
    };
    sessions::create(
        conn,
        &NewSession {
            id: Some(sid.clone()),
            kind: info.kind,
            project_id,
            workspace_id: None,
            worktree_id: None,
            parent_session_id: None,
            agent: info.agent.clone(),
            title: info.title.clone(),
            model: info.model.clone(),
            effort: info.effort.clone(),
            mode: info.mode.clone(),
        },
    )?;
    conn.execute(
        "UPDATE session SET cost_usd = ?2, tokens_in = ?3, tokens_out = ?4, pinned = ?5,
            archived = ?6, created = ?7 WHERE id = ?1",
        params![
            sid,
            info.cost_usd,
            info.tokens_in,
            info.tokens_out,
            info.pinned,
            info.archived,
            info.created
        ],
    )?;

    // Messages parent-first, so each parent exists when its children go in.
    for m in &msgs {
        ids.claim("message", "msg", &m.id)?;
    }
    let known: HashSet<&str> = msgs.iter().map(|m| m.id.as_str()).collect();
    let mut inserted: HashSet<String> = HashSet::new();
    let mut pending: Vec<&Message> = msgs.iter().collect();
    while !pending.is_empty() {
        let before = pending.len();
        let mut rest = vec![];
        for m in pending {
            let parent_ready = match &m.parent_id {
                None => true,
                Some(p) => !known.contains(p.as_str()) || inserted.contains(p),
            };
            if !parent_ready {
                rest.push(m);
                continue;
            }
            let mut nm = m.clone();
            nm.id = ids.get(&m.id).unwrap_or_else(|| m.id.clone());
            nm.session_id = sid.clone();
            // A parent outside the export starts a new root.
            nm.parent_id = m.parent_id.as_ref().and_then(|p| ids.get(p));
            nm.seq = None;
            messages::insert(conn, &nm)?;
            inserted.insert(m.id.clone());
        }
        if rest.len() == before {
            return Err(StoreError::Format("messages form a cycle".into()));
        }
        pending = rest;
    }
    if let Some(leaf) = info.active_leaf_id.as_deref().and_then(|l| ids.get(l)) {
        sessions::set_active_leaf(conn, &sid, Some(&leaf))?;
    }

    let blob_ok = |b: &Option<String>| -> Result<Option<String>> {
        match b {
            Some(b) if exists(conn, "blob", b)? => Ok(Some(b.clone())),
            _ => Ok(None),
        }
    };
    for p in &parts {
        let Some(mid) = ids.get(&p.message_id) else {
            continue; // a part of a message that is not in the export
        };
        let mut np = p.clone();
        np.id = ids.claim("part", "prt", &p.id)?;
        np.message_id = mid;
        np.blob_id = blob_ok(&p.blob_id)?;
        messages::insert_part(conn, &np)?;
    }

    for a in &arts {
        let aid = ids.claim("artifact", "art", &a.artifact.id)?;
        conn.execute(
            "INSERT INTO artifact (id, session_id, type, title, created) VALUES (?1, ?2, ?3, ?4, ?5)",
            params![aid, sid, a.artifact.ty, a.artifact.title, a.artifact.created],
        )?;
        for v in &a.versions {
            let Some(mid) = ids.get(&v.message_id) else {
                continue;
            };
            let blob = blob_ok(&v.blob_id)?;
            let content = v
                .content
                .clone()
                .filter(|c| c != crate::export::REDACTED);
            if content.is_none() && blob.is_none() {
                continue; // sanitized or missing content
            }
            conn.execute(
                "INSERT INTO artifact_version (artifact_id, n, message_id, content, blob_id, created)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                params![
                    aid,
                    v.n,
                    mid,
                    if blob.is_some() { None } else { content },
                    blob,
                    v.created
                ],
            )?;
        }
    }
    Ok(Imported {
        session_id: sid,
        remapped: ids.remapped,
        messages: msgs.len() as u64,
    })
}
