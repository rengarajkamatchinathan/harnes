//! Session metadata for `session.create/list/get/update/delete` and the workspace feed.
//!
//! In-memory until `genie-store` (spec 04) owns the `session` table; the shape matches
//! the wire `SessionInfo` so the swap is local to this module.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Mutex;

use genie_core::bus::now_ms;
use genie_core::runtime::{SessionOverlay, SessionSettings, SettingsSource};
use genie_protocol::api::session::{CreateParams, ListParams, UpdateParams};
use genie_protocol::common::Mode;
use genie_protocol::message::{SessionInfo, SessionSummary};
use genie_protocol::rfc3339_ms;
use genie_protocol::session::LiveStatus;
use genie_protocol::workspace::ListedChange;

/// Engine-side facts that are not on `SessionInfo`.
#[derive(Debug, Clone, Default)]
pub struct Extra {
    pub workspace: Option<PathBuf>,
    pub overlay: Option<SessionOverlay>,
    /// `--worktree <name>` (kept until the worktree subsystem lands).
    pub worktree_name: Option<String>,
    /// Created by a headless client.
    pub headless: bool,
}

/// Called when a session appears or goes away outside `session.create/delete` (child
/// sessions of subagents).
pub type ListedFn = dyn Fn(ListedChange, SessionSummary) + Send + Sync;

#[derive(Default)]
pub struct Registry {
    map: Mutex<HashMap<String, SessionInfo>>,
    extra: Mutex<HashMap<String, Extra>>,
    listener: std::sync::OnceLock<std::sync::Arc<ListedFn>>,
}

impl Registry {
    /// Where child-session changes go (the workspace feed). Set once.
    pub fn set_listener(&self, f: std::sync::Arc<ListedFn>) {
        let _ = self.listener.set(f);
    }

    pub(crate) fn notify(&self, change: ListedChange, summary: SessionSummary) {
        if let Some(f) = self.listener.get() {
            f(change, summary);
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<String, SessionInfo>> {
        self.map.lock().unwrap_or_else(|e| e.into_inner())
    }

    pub fn create(&self, id: &str, p: &CreateParams, parent: Option<String>) -> SessionInfo {
        self.create_with(id, p, parent, Extra::default())
    }

    pub fn create_with(
        &self,
        id: &str,
        p: &CreateParams,
        parent: Option<String>,
        extra: Extra,
    ) -> SessionInfo {
        self.extra
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(id.to_string(), extra);
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
            web_search: p.web_search.unwrap_or(true),
            code_execution: p.code_execution.unwrap_or(false),
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

    /// Every session, archived and incognito included.
    pub fn count(&self) -> usize {
        self.lock().len()
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
        if let Some(v) = &p.project_id {
            s.summary.project_id = Some(v.clone()).filter(|v| !v.is_empty());
        }
        if let Some(v) = p.web_search {
            s.web_search = v;
        }
        if let Some(v) = p.code_execution {
            s.code_execution = v;
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
        self.extra
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(id);
        self.lock().remove(id)
    }

    pub fn extra(&self, id: &str) -> Extra {
        self.extra
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(id)
            .cloned()
            .unwrap_or_default()
    }

    /// Move a session to another folder (its worktree was merged or discarded, D50).
    pub fn set_workspace(&self, id: &str, workspace: Option<PathBuf>) {
        if let Some(x) = self
            .extra
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get_mut(id)
        {
            x.workspace = workspace;
        }
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

    /// Forget every session (`data.delete_all`); returns what was there, incognito
    /// sessions included.
    pub fn clear(&self) -> Vec<SessionInfo> {
        self.extra
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clear();
        self.lock().drain().map(|(_, v)| v).collect()
    }
}

/// The turn loop reads per-session settings from the registry (06 §1).
impl SettingsSource for Registry {
    fn settings(&self, session_id: &str) -> Option<SessionSettings> {
        let s = self.get(session_id)?;
        let x = self.extra(session_id);
        Some(SessionSettings {
            kind: s.summary.kind,
            model: s.model,
            effort: s.effort,
            mode: s.mode,
            agent: s.agent,
            project_id: s.summary.project_id,
            incognito: s.incognito,
            title: s.summary.title,
            workspace: x.workspace,
            overlay: x.overlay,
            headless: x.headless,
            web_search_off: !s.web_search,
            code_execution: s.code_execution,
        })
    }

    fn set_title(&self, session_id: &str, title: &str) {
        if let Some(s) = self.lock().get_mut(session_id) {
            s.summary.title = Some(title.to_string());
            s.summary.updated_at = rfc3339_ms(now_ms());
        }
    }
}
