//! Checkpoints (D49, 04 §10, 02 §6.10 `checkpoint.*`): shadow-git snapshots around every
//! step that can mutate files, and two-phase rewind of files and/or the conversation.
//!
//! - **Where:** one shadow repo per (project, working tree) under
//!   `<data>/snapshots/<projectId|_>/<hash(work tree)>/` (0700), shared by every session
//!   on that tree ([`genie_tools::code::snapshot::ShadowRepo`]).
//! - **When:** the turn loop asks [`Checkpoints::step`] for a [`StepCheckpoint`] at each
//!   step. The **before** snapshot is taken lazily, just before the first capturing tool
//!   (`edit`, `write`, `multi_edit`, `patch`, `shell`) runs; the **after** snapshot when
//!   the step's calls have joined. A step that changed files writes a [`Record`]
//!   (`genie/sessions/<session>.jsonl` in the shadow dir), keeps both trees alive with
//!   refs `refs/genie/<session>/<n>/{before,after}` and publishes `checkpoint.created`
//!   (`step_ref` = the step's assistant message id).
//! - **Rewind** to message M (on the active path): files go back to the `before` tree of
//!   the first checkpoint at or after M, path by path, for the paths those checkpoints
//!   changed; changes made outside Genie in between are reported first
//!   (`external_changes`). The conversation's active leaf moves to M's parent (a new
//!   in-place branch starts there). The rewind is **staged**: [`Checkpoints::undo`] puts
//!   files and leaf back; the next turn commits it ([`Checkpoints::commit_staged`]).
//! - **Retention:** until the session is deleted ([`Checkpoints::drop_session`]: refs
//!   dropped, `git gc --prune=now`).

use std::collections::{BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use genie_protocol::api::code::{CheckpointEntry, CheckpointPreview, FileChange, RestoreWhat};
use genie_protocol::common::SessionKind;
use genie_protocol::event::CheckpointCreated;
use genie_tools::code::snapshot::{Change, ShadowRepo, Snapshot};
use serde::{Deserialize, Serialize};

use crate::runtime::conversation::ConversationStore;
use crate::runtime::ports::{SessionSettings, SettingsSource};

/// Tools whose steps are snapshotted (04 §10).
pub const CAPTURED: &[&str] = &["edit", "write", "multi_edit", "patch", "shell"];

/// One checkpoint: a step that changed files.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Record {
    pub seq: u32,
    pub session_id: String,
    pub turn_id: String,
    /// The step's assistant message.
    pub message_id: String,
    pub before: String,
    pub after: String,
    #[serde(default)]
    pub skipped: Vec<String>,
    /// `(path, added|modified|deleted)`.
    #[serde(default)]
    pub files: Vec<(String, String)>,
    pub created_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum CheckpointError {
    #[error("{0} not found")]
    NotFound(String),
    #[error("{0}")]
    Invalid(String),
    #[error("{0}")]
    Failed(String),
}

/// What a rewind or undo did.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct RewindOutcome {
    pub files: Vec<FileChange>,
    /// `Some(leaf)` when the conversation's active leaf moved (`None` inside = empty path).
    pub leaf: Option<Option<String>>,
}

/// A rewind waiting for undo or the next prompt.
#[derive(Debug, Clone)]
struct Staged {
    pre_tree: Option<String>,
    paths: Vec<String>,
    prev_leaf: Option<Option<String>>,
}

pub struct Checkpoints {
    dir: PathBuf,
    settings: Arc<dyn SettingsSource>,
    /// Session → its shadow repo.
    by_session: Mutex<HashMap<String, Arc<ShadowRepo>>>,
    /// Shadow git dir → repo (one instance per tree, so its lock is shared).
    repos: tokio::sync::Mutex<HashMap<PathBuf, Arc<ShadowRepo>>>,
    staged: Mutex<HashMap<String, Staged>>,
    io: Mutex<()>,
}

/// FNV-1a 64: stable across builds (the folder name must not change between versions).
fn fnv(s: &str) -> String {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in s.as_bytes() {
        h ^= u64::from(*b);
        h = h.wrapping_mul(0x0100_0000_01b3);
    }
    format!("{h:016x}")
}

fn safe(s: &str) -> String {
    s.chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                c
            } else {
                '_'
            }
        })
        .collect()
}

impl Checkpoints {
    /// `dir` is `<data>/snapshots`.
    pub fn new(dir: PathBuf, settings: Arc<dyn SettingsSource>) -> Arc<Checkpoints> {
        Arc::new(Checkpoints {
            dir,
            settings,
            by_session: Mutex::new(HashMap::new()),
            repos: tokio::sync::Mutex::new(HashMap::new()),
            staged: Mutex::new(HashMap::new()),
            io: Mutex::new(()),
        })
    }

    /// The session's shadow repo (`None`: not a Code session with a workspace, or git
    /// can't run).
    pub async fn repo(&self, sid: &str) -> Option<Arc<ShadowRepo>> {
        let cached = self
            .by_session
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(sid)
            .cloned();
        if cached.is_some() {
            return cached;
        }
        let s = self.settings.settings(sid)?;
        self.repo_for(sid, &s).await
    }

    async fn repo_for(&self, sid: &str, s: &SessionSettings) -> Option<Arc<ShadowRepo>> {
        if s.kind != SessionKind::Code {
            return None;
        }
        let ws = s.workspace.as_ref()?;
        let (tree, info) = ShadowRepo::work_tree_of(ws).await;
        let project = s.project_id.as_deref().map_or_else(|| "_".into(), safe);
        let git_dir = self
            .dir
            .join(project)
            .join(fnv(&tree.display().to_string()));
        let repo = {
            let mut repos = self.repos.lock().await;
            match repos.get(&git_dir) {
                Some(r) => r.clone(),
                None => match ShadowRepo::open(git_dir.clone(), tree.clone(), info).await {
                    Ok(r) => {
                        let _ = std::fs::create_dir_all(git_dir.join("genie").join("sessions"));
                        let _ = std::fs::write(
                            git_dir.join("genie").join("work_tree"),
                            tree.display().to_string(),
                        );
                        let r = Arc::new(r);
                        repos.insert(git_dir, r.clone());
                        r
                    }
                    Err(e) => {
                        tracing::warn!(error = %e, "checkpoints unavailable for this workspace");
                        return None;
                    }
                },
            }
        };
        self.by_session
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(sid.to_string(), repo.clone());
        Some(repo)
    }

    fn log_path(repo: &ShadowRepo, sid: &str) -> PathBuf {
        repo.git_dir
            .join("genie")
            .join("sessions")
            .join(format!("{}.jsonl", safe(sid)))
    }

    /// Every checkpoint of the session, oldest first.
    pub fn records_in(repo: &ShadowRepo, sid: &str) -> Vec<Record> {
        std::fs::read_to_string(Self::log_path(repo, sid))
            .unwrap_or_default()
            .lines()
            .filter_map(|l| serde_json::from_str(l).ok())
            .collect()
    }

    fn append(&self, repo: &ShadowRepo, r: &Record) -> Result<(), String> {
        use std::io::Write;
        let _g = self.io.lock().unwrap_or_else(|e| e.into_inner());
        let p = Self::log_path(repo, &r.session_id);
        if let Some(d) = p.parent() {
            std::fs::create_dir_all(d).map_err(|e| e.to_string())?;
        }
        let mut f = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&p)
            .map_err(|e| e.to_string())?;
        let line = serde_json::to_string(r).map_err(|e| e.to_string())?;
        writeln!(f, "{line}").map_err(|e| e.to_string())
    }

    /// The per-step hook for the turn loop.
    pub async fn step(
        self: &Arc<Self>,
        sid: &str,
        settings: &SessionSettings,
        turn_id: &str,
    ) -> Option<Arc<StepCheckpoint>> {
        let cached = self
            .by_session
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(sid)
            .cloned();
        let repo = match cached {
            Some(r) => r,
            None => self.repo_for(sid, settings).await?,
        };
        Some(Arc::new(StepCheckpoint {
            cps: self.clone(),
            repo,
            sid: sid.to_string(),
            turn_id: turn_id.to_string(),
            before: tokio::sync::OnceCell::new(),
        }))
    }

    /// A new prompt commits a staged rewind (D49 two-phase).
    pub fn commit_staged(&self, sid: &str) {
        self.staged
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(sid);
    }

    pub fn has_staged(&self, sid: &str) -> bool {
        self.staged
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .contains_key(sid)
    }

    /// Checkpoints on the session's active path, oldest first.
    async fn on_path(
        &self,
        sid: &str,
        conv: &dyn ConversationStore,
    ) -> Result<(Arc<ShadowRepo>, Vec<(usize, Record)>, Vec<String>), CheckpointError> {
        let repo = self
            .repo(sid)
            .await
            .ok_or_else(|| CheckpointError::Invalid("this session has no checkpoints".into()))?;
        let path: Vec<String> = conv
            .path(sid)
            .map_err(|d| CheckpointError::Failed(format!("incident {}", d.incident_id)))?
            .iter()
            .map(|m| m.id().to_string())
            .collect();
        let pos: HashMap<&str, usize> = path
            .iter()
            .enumerate()
            .map(|(i, id)| (id.as_str(), i))
            .collect();
        let mut recs: Vec<(usize, Record)> = Self::records_in(&repo, sid)
            .into_iter()
            .filter_map(|r| pos.get(r.message_id.as_str()).map(|&i| (i, r)))
            .collect();
        recs.sort_by_key(|(i, r)| (*i, r.seq));
        Ok((repo, recs, path))
    }

    /// `checkpoint.list`: one entry per checkpointed message on the active path.
    pub async fn list(
        &self,
        sid: &str,
        conv: &dyn ConversationStore,
    ) -> Result<Vec<CheckpointEntry>, CheckpointError> {
        let (_, recs, _) = self.on_path(sid, conv).await?;
        Ok(recs
            .into_iter()
            .map(|(_, r)| CheckpointEntry {
                message_id: r.message_id,
                files_changed: u32::try_from(r.files.len()).unwrap_or(u32::MAX),
            })
            .collect())
    }

    /// What restoring files to `message_id` would do, plus the current snapshot.
    async fn plan(
        &self,
        sid: &str,
        conv: &dyn ConversationStore,
        message_id: &str,
    ) -> Result<Plan, CheckpointError> {
        let (repo, recs, path) = self.on_path(sid, conv).await?;
        let at = path
            .iter()
            .position(|m| m == message_id)
            .ok_or_else(|| CheckpointError::NotFound(format!("message {message_id}")))?;
        let later: Vec<Record> = recs
            .into_iter()
            .filter(|(i, _)| *i >= at)
            .map(|(_, r)| r)
            .collect();
        let fail = |e: String| CheckpointError::Failed(e);
        let Some(first) = later.first() else {
            return Ok(Plan {
                repo,
                target: None,
                current: None,
                files: vec![],
                external: vec![],
                skipped: vec![],
            });
        };
        let touched: BTreeSet<String> = later
            .iter()
            .flat_map(|r| r.files.iter().map(|(p, _)| p.clone()))
            .collect();
        let current = repo.snapshot().await.map_err(fail)?;
        let mut files = Vec::new();
        for (p, c) in repo
            .changes(&current.tree, &first.before)
            .await
            .map_err(fail)?
        {
            if touched.contains(&p) {
                files.push(FileChange {
                    path: p,
                    change: c.as_str().into(),
                });
            }
        }
        // Changes made outside Genie: between consecutive checkpoints and since the last.
        let mut external = BTreeSet::new();
        let mut pairs: Vec<(String, String)> = later
            .windows(2)
            .map(|w| (w[0].after.clone(), w[1].before.clone()))
            .collect();
        if let Some(last) = later.last() {
            pairs.push((last.after.clone(), current.tree.clone()));
        }
        for (a, b) in pairs {
            for (p, _) in repo.changes(&a, &b).await.map_err(fail)? {
                if touched.contains(&p) {
                    external.insert(p);
                }
            }
        }
        let skipped: BTreeSet<String> = later
            .iter()
            .flat_map(|r| r.skipped.iter().cloned())
            .chain(current.skipped.iter().cloned())
            .collect();
        Ok(Plan {
            repo,
            target: Some(first.before.clone()),
            current: Some(current),
            files,
            external: external.into_iter().collect(),
            skipped: skipped.into_iter().collect(),
        })
    }

    /// `checkpoint.preview`.
    pub async fn preview(
        &self,
        sid: &str,
        conv: &dyn ConversationStore,
        message_id: &str,
        restore: RestoreWhat,
    ) -> Result<CheckpointPreview, CheckpointError> {
        check_restore(restore)?;
        if restore == RestoreWhat::Conversation {
            message_on_path(sid, conv, message_id)?;
            return Ok(CheckpointPreview {
                files: vec![],
                external_changes: vec![],
                skipped: vec![],
            });
        }
        let p = self.plan(sid, conv, message_id).await?;
        Ok(CheckpointPreview {
            files: p.files,
            external_changes: p.external,
            skipped: p.skipped,
        })
    }

    /// `checkpoint.rewind`: restore files, the conversation or both, staged for undo.
    pub async fn rewind(
        &self,
        sid: &str,
        conv: &dyn ConversationStore,
        message_id: &str,
        restore: RestoreWhat,
    ) -> Result<RewindOutcome, CheckpointError> {
        check_restore(restore)?;
        let parent = message_on_path(sid, conv, message_id)?;
        // A second rewind commits the first.
        self.commit_staged(sid);
        let mut staged = Staged {
            pre_tree: None,
            paths: vec![],
            prev_leaf: None,
        };
        let mut out = RewindOutcome::default();
        if restore != RestoreWhat::Conversation {
            let plan = self.plan(sid, conv, message_id).await?;
            if let (Some(target), Some(current)) = (&plan.target, &plan.current) {
                let paths: Vec<String> = plan.files.iter().map(|f| f.path.clone()).collect();
                plan.repo
                    .keep(&format!("refs/genie/{}/staged", safe(sid)), &current.tree)
                    .await
                    .map_err(CheckpointError::Failed)?;
                plan.repo
                    .restore(target, &paths)
                    .await
                    .map_err(CheckpointError::Failed)?;
                staged.pre_tree = Some(current.tree.clone());
                staged.paths = paths;
            }
            out.files = plan.files;
        }
        if restore != RestoreWhat::Files {
            let prev = conv
                .path(sid)
                .map_err(|d| CheckpointError::Failed(format!("incident {}", d.incident_id)))?
                .last()
                .map(|m| m.id().to_string());
            conv.set_leaf(sid, parent.as_deref())
                .map_err(|d| CheckpointError::Failed(format!("incident {}", d.incident_id)))?;
            staged.prev_leaf = Some(prev);
            out.leaf = Some(parent);
        }
        self.staged
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(sid.to_string(), staged);
        Ok(out)
    }

    /// `checkpoint.undo`: clear the staged rewind (files and leaf back).
    pub async fn undo(
        &self,
        sid: &str,
        conv: &dyn ConversationStore,
    ) -> Result<RewindOutcome, CheckpointError> {
        let staged = self
            .staged
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(sid)
            .ok_or_else(|| CheckpointError::Invalid("there is no rewind to undo".into()))?;
        let mut out = RewindOutcome::default();
        if let Some(pre) = &staged.pre_tree {
            let repo = self
                .repo(sid)
                .await
                .ok_or_else(|| CheckpointError::Invalid("no checkpoints".into()))?;
            let current = repo.snapshot().await.map_err(CheckpointError::Failed)?;
            for (p, c) in repo
                .changes(&current.tree, pre)
                .await
                .map_err(CheckpointError::Failed)?
            {
                if staged.paths.contains(&p) {
                    out.files.push(FileChange {
                        path: p,
                        change: c.as_str().into(),
                    });
                }
            }
            repo.restore(pre, &staged.paths)
                .await
                .map_err(CheckpointError::Failed)?;
        }
        if let Some(prev) = staged.prev_leaf {
            conv.set_leaf(sid, prev.as_deref())
                .map_err(|d| CheckpointError::Failed(format!("incident {}", d.incident_id)))?;
            out.leaf = Some(prev);
        }
        Ok(out)
    }

    /// Session deleted: drop its refs and log in every shadow repo, then gc (D49).
    pub async fn drop_session(&self, sid: &str) {
        self.commit_staged(sid);
        self.by_session
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(sid);
        let name = format!("{}.jsonl", safe(sid));
        for git_dir in shadow_dirs(&self.dir) {
            let log = git_dir.join("genie").join("sessions").join(&name);
            if !log.exists() {
                continue;
            }
            let repo = {
                let repos = self.repos.lock().await;
                repos.get(&git_dir).cloned()
            };
            let repo = match repo {
                Some(r) => r,
                None => {
                    let tree = std::fs::read_to_string(git_dir.join("genie").join("work_tree"))
                        .map(PathBuf::from)
                        .unwrap_or_else(|_| git_dir.clone());
                    match ShadowRepo::open(git_dir.clone(), tree, None).await {
                        Ok(r) => Arc::new(r),
                        Err(e) => {
                            tracing::warn!(error = %e, "checkpoint cleanup skipped");
                            continue;
                        }
                    }
                }
            };
            if let Err(e) = repo.drop_refs(&format!("refs/genie/{}/", safe(sid))).await {
                tracing::warn!(error = %e, "dropping checkpoint refs failed");
            }
            let _ = std::fs::remove_file(&log);
            if let Err(e) = repo.gc().await {
                tracing::warn!(error = %e, "checkpoint gc failed");
            }
        }
    }

    /// Disk used by the shadow repos, per project folder (Settings → Privacy & data).
    pub fn disk_usage(&self) -> Vec<(String, u64)> {
        let Ok(rd) = std::fs::read_dir(&self.dir) else {
            return vec![];
        };
        let mut out: Vec<(String, u64)> = rd
            .flatten()
            .filter(|e| e.path().is_dir())
            .map(|e| {
                (
                    e.file_name().to_string_lossy().into_owned(),
                    dir_size(&e.path()),
                )
            })
            .collect();
        out.sort();
        out
    }
}

fn dir_size(p: &Path) -> u64 {
    let Ok(rd) = std::fs::read_dir(p) else {
        return 0;
    };
    rd.flatten()
        .map(|e| match e.metadata() {
            Ok(m) if m.is_dir() => dir_size(&e.path()),
            Ok(m) => m.len(),
            Err(_) => 0,
        })
        .sum()
}

/// `<dir>/<project>/<hash>` folders.
fn shadow_dirs(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    for p in std::fs::read_dir(dir).into_iter().flatten().flatten() {
        for r in std::fs::read_dir(p.path()).into_iter().flatten().flatten() {
            if r.path().join("HEAD").exists() {
                out.push(r.path());
            }
        }
    }
    out
}

fn check_restore(r: RestoreWhat) -> Result<(), CheckpointError> {
    if r == RestoreWhat::Unknown {
        return Err(CheckpointError::Invalid(
            "restore must be files, conversation or both".into(),
        ));
    }
    Ok(())
}

/// `message_id`'s parent on the active path (`Err` when it isn't on the path).
fn message_on_path(
    sid: &str,
    conv: &dyn ConversationStore,
    message_id: &str,
) -> Result<Option<String>, CheckpointError> {
    let path = conv
        .path(sid)
        .map_err(|d| CheckpointError::Failed(format!("incident {}", d.incident_id)))?;
    let i = path
        .iter()
        .position(|m| m.id() == message_id)
        .ok_or_else(|| CheckpointError::NotFound(format!("message {message_id}")))?;
    Ok(if i == 0 {
        None
    } else {
        Some(path[i - 1].id().to_string())
    })
}

struct Plan {
    repo: Arc<ShadowRepo>,
    target: Option<String>,
    current: Option<Snapshot>,
    files: Vec<FileChange>,
    external: Vec<String>,
    skipped: Vec<String>,
}

/// One step's checkpoint (see the module docs).
pub struct StepCheckpoint {
    cps: Arc<Checkpoints>,
    repo: Arc<ShadowRepo>,
    sid: String,
    turn_id: String,
    before: tokio::sync::OnceCell<Option<Snapshot>>,
}

impl StepCheckpoint {
    /// Whether a call of `tool` is snapshotted.
    pub fn captures(tool: &str) -> bool {
        CAPTURED.contains(&tool)
    }

    /// Take the before snapshot once (called by the first capturing tool, before it
    /// runs).
    pub async fn before(&self) {
        self.before
            .get_or_init(|| async {
                match self.repo.snapshot().await {
                    Ok(s) => Some(s),
                    Err(e) => {
                        tracing::warn!(error = %e, "checkpoint snapshot failed");
                        None
                    }
                }
            })
            .await;
    }

    /// After the step's calls joined: record the checkpoint if files changed.
    pub async fn finish(&self, message_id: &str) -> Option<CheckpointCreated> {
        let before = self.before.get()?.as_ref()?;
        let after = match self.repo.snapshot().await {
            Ok(s) => s,
            Err(e) => {
                tracing::warn!(error = %e, "checkpoint snapshot failed");
                return None;
            }
        };
        if after.tree == before.tree {
            return None;
        }
        let files: Vec<(String, String)> = match self.repo.changes(&before.tree, &after.tree).await
        {
            Ok(c) => c
                .into_iter()
                .map(|(p, c): (String, Change)| (p, c.as_str().to_string()))
                .collect(),
            Err(e) => {
                tracing::warn!(error = %e, "checkpoint diff failed");
                vec![]
            }
        };
        let seq = u32::try_from(Checkpoints::records_in(&self.repo, &self.sid).len())
            .unwrap_or(u32::MAX)
            + 1;
        let skipped: Vec<String> = before
            .skipped
            .iter()
            .chain(after.skipped.iter())
            .cloned()
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect();
        let rec = Record {
            seq,
            session_id: self.sid.clone(),
            turn_id: self.turn_id.clone(),
            message_id: message_id.to_string(),
            before: before.tree.clone(),
            after: after.tree.clone(),
            skipped: skipped.clone(),
            files,
            created_ms: crate::bus::now_ms(),
        };
        let base = format!("refs/genie/{}/{seq}", safe(&self.sid));
        for (name, tree) in [("before", &rec.before), ("after", &rec.after)] {
            if let Err(e) = self.repo.keep(&format!("{base}/{name}"), tree).await {
                tracing::warn!(error = %e, "checkpoint ref not kept");
            }
        }
        if let Err(e) = self.cps.append(&self.repo, &rec) {
            tracing::warn!(error = %e, "checkpoint not recorded");
            return None;
        }
        Some(CheckpointCreated {
            step_ref: message_id.to_string(),
            before_tree: rec.before,
            after_tree: rec.after,
            skipped,
        })
    }
}
