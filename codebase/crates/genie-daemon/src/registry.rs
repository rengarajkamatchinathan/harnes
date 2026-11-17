//! Session metadata for `session.create/list/get/update/delete` and the workspace feed.
//!
//! In-memory until `genie-store` (spec 04) owns the `session` table; the shape matches
//! the wire `SessionInfo` so the swap is local to this module.

use std::collections::HashMap;
use std::sync::Mutex;

use genie_core::bus::now_ms;
use genie_protocol::api::session::{CreateParams, ListParams, UpdateParams};
use genie_protocol::common::Mode;
use genie_protocol::message::{SessionInfo, SessionSummary};
use genie_protocol::rfc3339_ms;
use genie_protocol::session::LiveStatus;

#[derive(Default)]
pub struct Registry {
    map: Mutex<HashMap<String, SessionInfo>>,
}

impl Registry {
    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<String, SessionInfo>> {
        self.map.lock().unwrap_or_else(|e| e.into_inner())
    }

    pub fn create(&self, id: &str, p: &CreateParams, parent: Option<String>) -> SessionInfo {
        let now = rfc3339_ms(now_ms());
        let info = SessionInfo {
            summary: SessionSummary {
                session_id: id.to_string(),
                kind: p.kind,
                title: None,
                status: LiveStatus::Idle,
                pinned: false,
                archived: false,
                project_id: p.project_id.clone(),
                workspace_id: None,
                updated_at: now.clone(),
            },
            model: p.model.clone(),
            effort: p.effort.clone(),
            mode: p.mode.unwrap_or(Mode::Default),
            agent: p.agent.clone(),
            parent_session_id: parent,
            active_leaf_id: None,
            driver: None,
            head_seq: 0,
            incognito: p.incognito,
            created_at: now,
        };
        self.lock().insert(id.to_string(), info.clone());
        info
    }

    pub fn get(&self, id: &str) -> Option<SessionInfo> {
        self.lock().get(id).cloned()
    }

    pub fn exists(&self, id: &str) -> bool {
        self.lock().contains_key(id)
    }

    /// Apply an update; returns the new record.
    pub fn update(&self, p: &UpdateParams) -> Option<SessionInfo> {
        let mut map = self.lock();
        let s = map.get_mut(&p.session_id)?;
        if let Some(t) = &p.title {
            s.summary.title = Some(t.clone());
        }
        if let Some(v) = p.pinned {
            s.summary.pinned = v;
        }
        if let Some(v) = p.archived {
            s.summary.archived = v;
        }
        if let Some(v) = &p.model {
            s.model.clone_from(v);
        }
        if let Some(v) = &p.effort {
            s.effort = Some(v.clone());
        }
        if let Some(v) = p.mode {
            s.mode = v;
        }
        if let Some(v) = &p.active_leaf_id {
            s.active_leaf_id = Some(v.clone());
        }
        s.summary.updated_at = rfc3339_ms(now_ms());
        Some(s.clone())
    }

    pub fn touch(&self, id: &str) {
        if let Some(s) = self.lock().get_mut(id) {
            s.summary.updated_at = rfc3339_ms(now_ms());
        }
    }

    pub fn remove(&self, id: &str) -> Option<SessionInfo> {
        self.lock().remove(id)
    }

    /// Newest first. Incognito sessions are never listed (D24).
    pub fn list(&self, p: &ListParams) -> Vec<SessionSummary> {
        let q = p.query.as_deref().map(str::to_lowercase);
        let mut v: Vec<SessionSummary> = self
            .lock()
            .values()
            .filter(|s| !s.incognito)
            .map(|s| &s.summary)
            .filter(|s| p.kind.is_none_or(|k| s.kind == k))
            .filter(|s| p.project_id.is_none() || s.project_id == p.project_id)
            .filter(|s| p.workspace_id.is_none() || s.workspace_id == p.workspace_id)
            .filter(|s| s.archived == p.archived.unwrap_or(false))
            .filter(|s| match &q {
                Some(q) => s
                    .title
                    .as_deref()
                    .is_some_and(|t| t.to_lowercase().contains(q.as_str())),
                None => true,
            })
            .cloned()
            .collect();
        v.sort_by(|a, b| {
            b.updated_at
                .cmp(&a.updated_at)
                .then(b.session_id.cmp(&a.session_id))
        });
        v
    }

    pub fn all_summaries(&self) -> Vec<SessionSummary> {
        self.list(&ListParams::default())
    }
}
