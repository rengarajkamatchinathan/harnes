//! Single-session export (04 §9): JSON `{info, messages, parts, artifacts}` and Markdown.
//! `sanitize` redacts file contents, attachments, tool outputs and blob references for
//! bug reports. Export-all (.zip) and import with id remapping: [`crate::transfer`].

use rusqlite::Connection;
use serde_json::{Value, json};

use crate::artifacts;
use crate::error::{Result, StoreError};
use crate::messages::{self, Part};
use crate::sessions;

pub const REDACTED: &str = "[redacted]";

/// Part kinds whose bodies `sanitize` replaces.
const SENSITIVE_PARTS: &[&str] = &["tool_result", "media", "attachment", "artifact_ref"];

fn sanitize_part(p: &mut Part) {
    p.blob_id = p.blob_id.as_ref().map(|_| REDACTED.to_string());
    if SENSITIVE_PARTS.contains(&p.kind.as_str()) {
        p.body = json!({ "redacted": true });
    } else if p.kind == "tool_call" {
        // Keep the tool name, drop arguments (they can hold file contents or secrets).
        let name = p.body.get("name").cloned().unwrap_or(Value::Null);
        p.body = json!({ "name": name, "input": REDACTED });
    }
}

pub fn session_json(conn: &Connection, session_id: &str, sanitize: bool) -> Result<Value> {
    let info = sessions::get(conn, session_id)?
        .ok_or_else(|| StoreError::NotFound(session_id.to_string()))?;
    let msgs = messages::all(conn, session_id)?;
    let mut parts = vec![];
    for m in &msgs {
        for mut p in messages::parts(conn, &m.id)? {
            if sanitize {
                sanitize_part(&mut p);
            }
            parts.push(p);
        }
    }
    let mut arts = vec![];
    for a in artifacts::for_session(conn, session_id)? {
        let mut versions = artifacts::versions(conn, &a.id)?;
        if sanitize {
            for v in &mut versions {
                v.content = v.content.as_ref().map(|_| REDACTED.to_string());
                v.blob_id = v.blob_id.as_ref().map(|_| REDACTED.to_string());
            }
        }
        arts.push(json!({ "artifact": a, "versions": versions }));
    }
    Ok(json!({
        "info": info,
        "messages": msgs,
        "parts": parts,
        "artifacts": arts,
    }))
}

/// Markdown transcript of the active path.
pub fn session_markdown(conn: &Connection, session_id: &str) -> Result<String> {
    let info = sessions::get(conn, session_id)?
        .ok_or_else(|| StoreError::NotFound(session_id.to_string()))?;
    let mut out = format!("# {}\n\n", info.title.as_deref().unwrap_or("Untitled chat"));
    for m in messages::active_path(conn, session_id)? {
        out.push_str(&format!("## {}\n\n", m.role));
        for p in messages::parts(conn, &m.id)? {
            match p.kind.as_str() {
                "text" => {
                    if let Some(t) = p.body.get("text").and_then(Value::as_str) {
                        out.push_str(t);
                        out.push_str("\n\n");
                    }
                }
                "tool_call" => {
                    let name = p.body.get("name").and_then(Value::as_str).unwrap_or("tool");
                    out.push_str(&format!("_Tool call: `{name}`_\n\n"));
                }
                _ => {}
            }
        }
    }
    Ok(out)
}
