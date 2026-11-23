//! Session metadata for `session.create/list/get/update/delete` and the workspace feed.
//!
//! The map is the in-memory cache. With a store attached ([`Registry::attach_store`])
//! every change is written through to the session row's `meta` column (04 §3.2,
//! migration 2) and the map is reloaded from it on start, so the session list survives
//! a daemon restart. Incognito sessions are never written, and their rows are dropped on
//! start (D24).

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, OnceLock};

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
    store: OnceLock<Arc<genie_store::Store>>,
}

/// kv flag: rows from before migration 2 were given a registry record (done once).
const BACKFILLED_KV: &str = "registry.backfilled";
const META_VERSION: u32 = 1;

/// What [`Registry::attach_store`] found.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct LoadReport {
    pub loaded: usize,
    /// Old rows (messages but no registry record) listed again as chats.
    pub recovered: usize,
    /// Incognito rows removed.
    pub dropped_incognito: usize,
}

#[derive(serde::Serialize, serde::Deserialize)]
struct Meta {
    v: u32,
    info: SessionInfo,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    workspace: Option<PathBuf>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    worktree_name: Option<String>,
    #[serde(default)]
    headless: bool,
}

fn store_kind(k: genie_protocol::common::SessionKind) -> genie_store::sessions::SessionKind {
    match k {
        genie_protocol::common::SessionKind::Code => genie_store::sessions::SessionKind::Code,
        _ => genie_store::sessions::SessionKind::Chat,
    }
}

/// A title for a recovered chat: the first line of its first prompt, shortened.
fn title_from(text: &str) -> Option<String> {
    let line = text.lines().map(str::trim).find(|l| !l.is_empty())?;
    let mut t: String = line.chars().take(60).collect();
    if line.chars().count() > 60 {
        t.push('…');
    }
    Some(t)
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

    /// Use `store` from now on and load the sessions saved in it. Call once, before
    /// clients connect.
    pub fn attach_store(&self, store: Arc<genie_store::Store>) -> genie_store::Result<LoadReport> {
        let mut report = LoadReport::default();
        let rows = store.read(genie_store::sessions::all_meta)?;
        let mut incognito = vec![];
        {
            let mut map = self.lock();
            let mut extra = self.extra.lock().unwrap_or_else(|e| e.into_inner());
            for (id, v) in rows {
                let m: Meta = match serde_json::from_value(v) {
                    Ok(m) => m,
                    Err(e) => {
                        tracing::warn!(session = %id, error = %e, "session record unreadable; skipped");
                        continue;
                    }
                };
                if m.info.incognito {
                    incognito.push(id);
                    continue;
                }
                let mut info = m.info;
                info.summary.status = LiveStatus::Idle;
                info.driver = None;
                extra.insert(
                    id.clone(),
                    Extra {
                        workspace: m.workspace,
                        overlay: None,
                        worktree_name: m.worktree_name,
                        headless: m.headless,
                    },
                );
                map.insert(id, info);
                report.loaded += 1;
            }
        }
        if !incognito.is_empty() {
            store.write(|tx| {
                for id in &incognito {
                    genie_store::sessions::delete(tx, id)?;
                }
                Ok(())
            })?;
            report.dropped_incognito = incognito.len();
        }
        let _ = self.store.set(store.clone());
        report.recovered = self.backfill(&store)?;
        Ok(report)
    }

    /// Once per DB: rows written before the registry was persisted hold messages but no
    /// record, so they never showed up after a restart. List them again as chats.
    fn backfill(&self, store: &genie_store::Store) -> genie_store::Result<usize> {
        if store
            .read(|c| genie_store::kv::get(c, BACKFILLED_KV))?
            .is_some()
        {
            return Ok(0);
        }
        let old = store.read(genie_store::sessions::without_meta)?;
        let mut n = 0;
        for (row, first) in old {
            if self.exists(&row.id) {
                continue;
            }
            let kind = match row.kind {
                genie_store::sessions::SessionKind::Code => {
                    genie_protocol::common::SessionKind::Code
                }
                genie_store::sessions::SessionKind::Chat => {
                    genie_protocol::common::SessionKind::Chat
                }
            };
            let info = SessionInfo {
                summary: SessionSummary {
                    session_id: row.id.clone(),
                    kind,
                    title: row
                        .title
                        .clone()
                        .or_else(|| first.as_deref().and_then(title_from)),
                    status: LiveStatus::Idle,
                    pinned: row.pinned,
                    archived: row.archived,
                    project_id: None,
                    workspace_id: None,
                    updated_at: rfc3339_ms(u64::try_from(row.updated).unwrap_or(0)),
                },
                model: row.model.clone().unwrap_or_default(),
                effort: row.effort.clone(),
                mode: row
                    .mode
                    .as_deref()
                    .and_then(|m| serde_json::from_value(serde_json::json!(m)).ok())
                    .unwrap_or(Mode::Default),
                agent: row.agent.clone(),
                parent_session_id: None,
                active_leaf_id: row.active_leaf_id.clone(),
                driver: None,
                head_seq: 0,
                incognito: false,
                created_at: rfc3339_ms(u64::try_from(row.created).unwrap_or(0)),
                web_search: true,
                code_execution: false,
            };
            self.lock().insert(row.id.clone(), info);
            self.extra
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .insert(row.id.clone(), Extra::default());
            self.persist_with_times(&row.id, row.created, row.updated);
            n += 1;
        }
        store.write(|tx| genie_store::kv::set(tx, BACKFILLED_KV, &serde_json::json!(true)))?;
        if n > 0 {
            tracing::info!(
                recovered = n,
                "listed chats saved before the session registry was persisted"
            );
        }
        Ok(n)
    }

    /// Write `id`'s record through to the store (no-op without one). A failed write is
    /// logged, never surfaced: the in-memory record stays correct for this run.
    ///
    /// Incognito sessions get a record too, only so that [`Self::attach_store`] finds and
    /// deletes their rows on the next start: the journal and conversation store still
    /// write their events and messages while they run.
    fn persist(&self, id: &str) {
        let now = i64::try_from(now_ms()).unwrap_or(i64::MAX);
        self.persist_with_times(id, now, now);
    }

    fn persist_with_times(&self, id: &str, created: i64, updated: i64) {
        let Some(store) = self.store.get() else {
            return;
        };
        let Some(info) = self.get(id) else { return };
        let x = self.extra(id);
        let row = genie_store::sessions::MetaRow {
            id: id.to_string(),
            kind: store_kind(info.summary.kind),
            project_id: info.summary.project_id.clone(),
            parent_session_id: info.parent_session_id.clone(),
            agent: info.agent.clone(),
            title: info.summary.title.clone(),
            model: Some(info.model.clone()).filter(|m| !m.is_empty()),
            effort: info.effort.clone(),
            mode: serde_json::to_value(info.mode)
                .ok()
                .and_then(|v| v.as_str().map(String::from)),
            pinned: info.summary.pinned,
            archived: info.summary.archived,
            created,
            updated,
            meta: serde_json::Value::Null,
        };
        let meta = Meta {
            v: META_VERSION,
            info,
            workspace: x.workspace,
            worktree_name: x.worktree_name,
            headless: x.headless,
        };
        let row = match serde_json::to_value(&meta) {
            Ok(meta) => genie_store::sessions::MetaRow { meta, ..row },
            Err(e) => {
                tracing::error!(session = %id, error = %e, "session record not serialisable");
                return;
            }
        };
        if let Err(e) = store.write(|tx| genie_store::sessions::upsert_meta(tx, &row)) {
            tracing::error!(session = %id, error = %e, "session record not saved");
        }
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
        let folder = extra.workspace.as_ref().map(|w| w.display().to_string());
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
                // The folder a Code session works in, so clients can group sessions by project.
                workspace_id: folder,
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
        self.persist(id);
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
        let info = self.update_in_memory(p)?;
        self.persist(&p.session_id);
        Some(info)
    }

    fn update_in_memory(&self, p: &UpdateParams) -> Option<SessionInfo> {
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
        } else {
            return;
        }
        self.persist(id);
    }

    /// Forget a session. With a store this is the real delete (04 §8): it cascades to
    /// the session's messages, events and child sessions.
    pub fn remove(&self, id: &str) -> Option<SessionInfo> {
        self.extra
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(id);
        let out = self.lock().remove(id);
        if let Some(store) = self.store.get()
            && let Err(e) = store.write(|tx| genie_store::sessions::delete(tx, id).map(|_| ()))
        {
            tracing::error!(session = %id, error = %e, "session not deleted from the store");
        }
        out
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
        } else {
            return;
        }
        self.persist(id);
    }

    /// The project folder clients group a session under (`summary.workspace_id`): the folder the
    /// user opened, even when the session runs in a worktree of it (D50).
    pub fn set_project_folder(&self, id: &str, folder: &std::path::Path) {
        let summary = {
            let mut map = self.lock();
            let Some(s) = map.get_mut(id) else { return };
            s.summary.workspace_id = Some(folder.display().to_string());
            s.summary.clone()
        };
        self.persist(id);
        self.notify(ListedChange::Updated, summary);
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

    /// Forget every session (`data.delete_all.confirm`); returns what was there, incognito
    /// sessions included.
    pub fn clear(&self) -> Vec<SessionInfo> {
        self.extra.lock().unwrap_or_else(|e| e.into_inner()).clear();
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
        } else {
            return;
        }
        self.persist(session_id);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store(dir: &std::path::Path) -> Arc<genie_store::Store> {
        let opts = genie_store::StoreOptions::new(dir, "genie.db");
        let keys = genie_store::MemoryKeyStore::new();
        keys.set(genie_store::keys::DATA_KEY_ACCOUNT, &[3u8; 32])
            .unwrap();
        Arc::new(genie_store::Store::open_with_keystore(&opts, &keys).unwrap())
    }

    use genie_store::KeyStore as _;

    /// Chats written before the registry was persisted (bare `session` rows with
    /// messages) are listed again once, titled from their first prompt.
    #[test]
    fn old_chats_are_backfilled_once() {
        let dir = tempfile::tempdir().unwrap();
        let s = store(dir.path());
        s.write(|tx| {
            genie_store::sessions::ensure(tx, "ses_old")?;
            genie_store::sessions::ensure(tx, "ses_empty")?;
            let m = genie_store::messages::Message::new("ses_old", None, "user");
            genie_store::messages::insert(tx, &m)?;
            genie_store::messages::insert_part(
                tx,
                &genie_store::messages::Part::text(
                    &m.id,
                    0,
                    "\n  How do I rotate the Azure key?\nmore",
                ),
            )
        })
        .unwrap();

        let r = Registry::default();
        let rep = r.attach_store(s.clone()).unwrap();
        assert_eq!(rep.recovered, 1, "only rows with messages");
        let info = r.get("ses_old").unwrap();
        assert_eq!(
            info.summary.title.as_deref(),
            Some("How do I rotate the Azure key?")
        );
        assert!(!r.exists("ses_empty"));

        // Now a normal record: loaded, not recovered again.
        let r2 = Registry::default();
        let rep = r2.attach_store(s).unwrap();
        assert_eq!((rep.loaded, rep.recovered), (1, 0));
        assert!(r2.exists("ses_old"));
    }

    #[test]
    fn titles_are_trimmed_to_one_short_line() {
        assert_eq!(title_from("  \n hi \n there").as_deref(), Some("hi"));
        let long = "x".repeat(80);
        assert_eq!(title_from(&long).unwrap().chars().count(), 61);
        assert_eq!(title_from("   "), None);
    }
}
