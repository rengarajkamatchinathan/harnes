//! Code-mode methods (02 §6.10): `checkpoint.list/preview/rewind/undo` (D49),
//! `git.status/diff/commit` (Changes panel, D54), `worktree.finish` (D50) and
//! `files.tree/read` (Files panel); worktree creation on `session.create`
//! (`worktree: new|auto`, `worktree_name`); checkpoint and worktree cleanup on
//! `session.delete`.
//!
//! Everything here runs git as a child process, so the methods answer from a task
//! (`Handled::Later` style). Lease-gated methods check the lease first (02 §5).

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use genie_core::runtime::ConversationStore;
use genie_core::runtime::checkpoint::{CheckpointError, Checkpoints, RewindOutcome};
use genie_protocol::api::code::{
    CheckpointListResult, CommitThen, FileChange, FileNode, FilesReadResult, FilesTreeResult,
    GitCommitResult, GitStatusResult, TextResult, WorktreeAction, WorktreeFinishResult,
};
use genie_protocol::api::session::{CreateParams, CreateResult, UpdateParams, WorktreeChoice};
use genie_protocol::common::{Empty, SessionKind};
use genie_protocol::event::{BranchSwitched, SessionEvent};
use genie_protocol::methods::{self as m, Method};
use genie_protocol::session::LiveStatus;
use genie_protocol::{ErrorKind, Request, Response, RpcError};
use genie_tools::code::CodeConfig;
use genie_tools::code::git::{Git, nul_split, repo_info};
use genie_tools::code::worktree::{self, CreateSpec, Finish, WorktreeInfo};
use serde::Serialize;
use serde_json::{Value, json};

use crate::registry::Registry;
use crate::server::{Conn, DaemonState};

/// `files.read` returns text up to this size; larger or binary files go to a blob.
const MAX_TEXT_READ: u64 = 2 * 1024 * 1024;

const METHODS: &[&str] = &[
    m::CheckpointList::NAME,
    m::CheckpointPreviewMethod::NAME,
    m::CheckpointRewind::NAME,
    m::CheckpointUndo::NAME,
    m::GitStatus::NAME,
    m::GitDiff::NAME,
    m::GitCommit::NAME,
    m::WorktreeFinish::NAME,
    m::FilesTree::NAME,
    m::FilesRead::NAME,
];

/// Code-mode services (see the module docs).
pub struct CodeServices {
    pub registry: Arc<Registry>,
    pub conversation: Option<Arc<dyn ConversationStore>>,
    pub checkpoints: Option<Arc<Checkpoints>>,
    /// The code tools' state (read stamps, todo lists), cleared on delete.
    pub code: Option<CodeConfig>,
    /// Blobs for `files.read` of binary/large files.
    pub store: Option<Arc<genie_store::Store>>,
    /// `worktrees.dir`; default `../<repo>.genie-worktrees`.
    pub worktrees_dir: Option<PathBuf>,
    worktrees: Mutex<HashMap<String, WorktreeInfo>>,
}

impl CodeServices {
    pub fn new(registry: Arc<Registry>) -> CodeServices {
        CodeServices {
            registry,
            conversation: None,
            checkpoints: None,
            code: None,
            store: None,
            worktrees_dir: None,
            worktrees: Mutex::new(HashMap::new()),
        }
    }

    /// The worktree Genie created for `session`, if any.
    pub fn worktree(&self, session: &str) -> Option<WorktreeInfo> {
        self.worktrees
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(session)
            .cloned()
    }

    /// The session's worktree, rebuilt from git when this daemon has no record of it (the
    /// worktree was created before a restart). Remembered once found.
    async fn worktree_or_recover(&self, session: &str) -> Option<WorktreeInfo> {
        if let Some(w) = self.worktree(session) {
            return Some(w);
        }
        let ws = self.workspace(session).ok()?;
        let w = worktree::recover(&ws, &CreateSpec::new(&ws, "").branch_prefix).await?;
        self.set_worktree(session, Some(w.clone()));
        Some(w)
    }

    fn set_worktree(&self, session: &str, w: Option<WorktreeInfo>) {
        let mut g = self.worktrees.lock().unwrap_or_else(|e| e.into_inner());
        match w {
            Some(w) => g.insert(session.to_string(), w),
            None => g.remove(session),
        };
    }

    fn workspace(&self, sid: &str) -> Result<PathBuf, RpcError> {
        if !self.registry.exists(sid) {
            return Err(RpcError::new(ErrorKind::NotFound, "session not found"));
        }
        self.registry.extra(sid).workspace.ok_or_else(|| {
            RpcError::new(
                ErrorKind::InvalidState,
                "this session has no workspace folder",
            )
        })
    }

    fn conversation(&self) -> Result<&Arc<dyn ConversationStore>, RpcError> {
        self.conversation
            .as_ref()
            .ok_or_else(|| crate::services::not_implemented("checkpoint"))
    }

    fn checkpoints(&self) -> Result<&Arc<Checkpoints>, RpcError> {
        self.checkpoints
            .as_ref()
            .ok_or_else(|| crate::services::not_implemented("checkpoint"))
    }
}

fn to_result<T: Serialize>(v: &T) -> Result<Value, RpcError> {
    serde_json::to_value(v).map_err(|e| RpcError::internal(e.to_string()))
}

fn params<M: Method>(r: &Request) -> Result<M::Params, RpcError> {
    let v = if r.params.is_null() {
        json!({})
    } else {
        r.params.clone()
    };
    serde_json::from_value(v).map_err(|e| RpcError::invalid_params(e.to_string()))
}

fn state_err(msg: impl Into<String>) -> RpcError {
    RpcError::new(ErrorKind::InvalidState, msg)
}

fn cp_err(e: CheckpointError) -> RpcError {
    match e {
        CheckpointError::NotFound(_) => RpcError::new(ErrorKind::NotFound, e.to_string()),
        CheckpointError::Invalid(_) => state_err(e.to_string()),
        CheckpointError::Failed(m) => RpcError::new(ErrorKind::InternalError, m),
    }
}

/// Code-mode requests. `None`: not ours (the server handles it); `Some(None)`: answered
/// later from a task.
pub(crate) fn dispatch(
    state: &Arc<DaemonState>,
    conn: &Arc<Conn>,
    r: &Request,
) -> Option<Option<Response>> {
    let me = state.services()?.code.clone()?;
    let name = r.method.as_str();
    if name == m::SessionCreate::NAME {
        return create_with_worktree(&me, state, conn, r);
    }
    if !METHODS.contains(&name) {
        return None;
    }
    if let Some(info) = m::find(name)
        && info.needs_lease
    {
        let sid = r
            .params
            .get("session_id")
            .and_then(Value::as_str)
            .unwrap_or("");
        if let Err(e) = crate::server::with_lease(state, conn, sid) {
            return Some(Some(Response::err(r.id.clone(), e)));
        }
    }
    let r = r.clone();
    let c = conn.clone();
    let st = state.clone();
    conn.scope()
        .child("code-call")
        .spawn_cancellable(async move {
            let res = call(&me, &st, &r).await;
            c.respond(match res {
                Ok(v) => Response::ok(r.id, v),
                Err(e) => Response::err(r.id, e),
            });
        });
    Some(None)
}

async fn call(me: &CodeServices, st: &Arc<DaemonState>, r: &Request) -> Result<Value, RpcError> {
    match r.method.as_str() {
        n if n == m::CheckpointList::NAME => {
            let p = params::<m::CheckpointList>(r)?;
            let list = me
                .checkpoints()?
                .list(&p.session_id, me.conversation()?.as_ref())
                .await
                .map_err(cp_err)?;
            to_result(&CheckpointListResult { checkpoints: list })
        }
        n if n == m::CheckpointPreviewMethod::NAME => {
            let p = params::<m::CheckpointPreviewMethod>(r)?;
            let pv = me
                .checkpoints()?
                .preview(
                    &p.session_id,
                    me.conversation()?.as_ref(),
                    &p.message_id,
                    p.restore,
                )
                .await
                .map_err(cp_err)?;
            to_result(&pv)
        }
        n if n == m::CheckpointRewind::NAME => {
            let p = params::<m::CheckpointRewind>(r)?;
            idle(st, &p.session_id)?;
            let out = me
                .checkpoints()?
                .rewind(
                    &p.session_id,
                    me.conversation()?.as_ref(),
                    &p.message_id,
                    p.restore,
                )
                .await
                .map_err(cp_err)?;
            leaf_moved(me, st, &p.session_id, &out);
            to_result(&Empty {})
        }
        n if n == m::CheckpointUndo::NAME => {
            let p = params::<m::CheckpointUndo>(r)?;
            idle(st, &p.session_id)?;
            let out = me
                .checkpoints()?
                .undo(&p.session_id, me.conversation()?.as_ref())
                .await
                .map_err(cp_err)?;
            leaf_moved(me, st, &p.session_id, &out);
            to_result(&Empty {})
        }
        n if n == m::GitStatus::NAME => {
            let p = params::<m::GitStatus>(r)?;
            let ws = me.workspace(&p.session_id)?;
            to_result(&git_status(&ws, p.path.as_deref()).await?)
        }
        n if n == m::GitDiff::NAME => {
            let p = params::<m::GitDiff>(r)?;
            let ws = me.workspace(&p.session_id)?;
            to_result(&git_diff(&ws, p.path.as_deref()).await?)
        }
        n if n == m::GitCommit::NAME => {
            let p = params::<m::GitCommit>(r)?;
            let ws = me.workspace(&p.session_id)?;
            to_result(&git_commit(&ws, &p.message, p.paths.as_deref(), p.then).await?)
        }
        n if n == m::WorktreeFinish::NAME => {
            let p = params::<m::WorktreeFinish>(r)?;
            to_result(&worktree_finish(me, &p.session_id, p.action).await?)
        }
        n if n == m::FilesTree::NAME => {
            let p = params::<m::FilesTree>(r)?;
            let ws = me.workspace(&p.session_id)?;
            to_result(&files_tree(&ws, p.path.as_deref())?)
        }
        n if n == m::FilesRead::NAME => {
            let p = params::<m::FilesRead>(r)?;
            let ws = me.workspace(&p.session_id)?;
            let path = p
                .path
                .as_deref()
                .ok_or_else(|| RpcError::invalid_params("path is required"))?;
            to_result(&files_read(me, &ws, path)?)
        }
        other => Err(crate::services::not_implemented(other)),
    }
}

/// Rewind and undo need an idle (or interrupted) session.
fn idle(st: &DaemonState, sid: &str) -> Result<(), RpcError> {
    if let Some(h) = st.engine().loaded(sid)
        && !matches!(h.status(), LiveStatus::Idle | LiveStatus::Interrupted)
    {
        return Err(RpcError::new(
            ErrorKind::Busy,
            "the session is running; interrupt it before rewinding",
        ));
    }
    Ok(())
}

/// Publish `branch.switched` and keep the registry's active leaf in step.
fn leaf_moved(me: &CodeServices, st: &DaemonState, sid: &str, out: &RewindOutcome) {
    let Some(leaf) = &out.leaf else { return };
    let id = leaf.clone().unwrap_or_default();
    if !id.is_empty() {
        me.registry.update(&UpdateParams {
            session_id: sid.to_string(),
            active_leaf_id: Some(id.clone()),
            ..UpdateParams::default()
        });
    }
    match st.engine().session(sid) {
        Ok(h) => {
            if let Err(e) = h.publish_wire(SessionEvent::BranchSwitched(BranchSwitched {
                active_leaf_id: id,
            })) {
                tracing::warn!(error = %e, "branch.switched not published");
            }
        }
        Err(d) => tracing::warn!(incident_id = %d.incident_id, "branch.switched: no session"),
    }
}

// ------------------------------------------------------------------ worktrees

/// `session.create` with `worktree: new|auto` or a `worktree_name` in a Code session:
/// create the worktree first (git, so in a task), then the session inside it.
fn create_with_worktree(
    me: &Arc<CodeServices>,
    state: &Arc<DaemonState>,
    conn: &Arc<Conn>,
    r: &Request,
) -> Option<Option<Response>> {
    let p: CreateParams = params::<m::SessionCreate>(r).ok()?;
    let ws = p.workspace_path.clone().filter(|w| !w.is_empty())?;
    let choice = match (p.worktree, &p.worktree_name) {
        (Some(WorktreeChoice::None), _) => return None,
        (_, Some(_)) | (Some(WorktreeChoice::New), _) => WorktreeChoice::New,
        (Some(WorktreeChoice::Auto), None) => WorktreeChoice::Auto,
        _ => return None,
    };
    if p.kind != SessionKind::Code {
        return None;
    }
    let headless = conn.is_headless();
    let (me, st, c, id) = (me.clone(), state.clone(), conn.clone(), r.id.clone());
    conn.scope()
        .child("session-create")
        .spawn_cancellable(async move {
            let res = async {
                let ws = PathBuf::from(crate::services::canonical(&ws));
                let want = match choice {
                    WorktreeChoice::New => true,
                    _ => other_session_on_repo(&me, &ws).await,
                };
                let mut p2 = p.clone();
                let mut created = None;
                if want {
                    let spec = CreateSpec {
                        dir: me.worktrees_dir.clone(),
                        ..CreateSpec::new(
                            &ws,
                            p.worktree_name.clone().unwrap_or_else(|| "session".into()),
                        )
                    };
                    match worktree::create(&spec).await {
                        Ok(w) => {
                            p2.workspace_path = Some(w.workspace.display().to_string());
                            created = Some(w);
                        }
                        // Auto never fails the session; it falls back to the main checkout.
                        Err(e) if choice == WorktreeChoice::Auto => {
                            tracing::info!(error = %e, "no automatic worktree");
                        }
                        Err(e) => return Err(state_err(e)),
                    }
                }
                let v = crate::server::session_create(&st, &p2, None, headless)?;
                if let Some(w) = created
                    && let Ok(cr) = serde_json::from_value::<CreateResult>(v.clone())
                {
                    // Listed under the folder the user opened, not the worktree path.
                    me.registry.set_project_folder(&cr.session_id, &ws);
                    me.set_worktree(&cr.session_id, Some(w));
                }
                Ok(v)
            }
            .await;
            c.respond(match res {
                Ok(v) => Response::ok(id, v),
                Err(e) => Response::err(id, e),
            });
        });
    Some(None)
}

/// D50 "auto": another Code session already works in this repository's main checkout.
async fn other_session_on_repo(me: &CodeServices, ws: &Path) -> bool {
    let Some(info) = repo_info(ws).await else {
        return false;
    };
    me.registry
        .all_summaries()
        .iter()
        .filter(|s| s.kind == SessionKind::Code)
        .filter_map(|s| me.registry.extra(&s.session_id).workspace)
        .any(|w| w.starts_with(&info.top))
}

async fn worktree_finish(
    me: &CodeServices,
    sid: &str,
    action: WorktreeAction,
) -> Result<WorktreeFinishResult, RpcError> {
    let w = me
        .worktree_or_recover(sid)
        .await
        .ok_or_else(|| state_err("this session is not working in a Genie worktree"))?;
    let how = match action {
        WorktreeAction::Merge => Finish::Merge,
        WorktreeAction::Pr => Finish::Pr,
        WorktreeAction::Keep => Finish::Keep,
        WorktreeAction::Discard => Finish::Discard,
        WorktreeAction::Unknown => {
            return Err(RpcError::invalid_params(
                "action must be merge, pr, keep or discard",
            ));
        }
    };
    let done = worktree::finish(&w, how).await.map_err(state_err)?;
    if done.removed {
        // The session continues in the matching folder of the main checkout.
        let rel = w.workspace.strip_prefix(&w.path).unwrap_or(Path::new(""));
        me.registry.set_workspace(sid, Some(w.main.join(rel)));
        me.set_worktree(sid, None);
    }
    Ok(WorktreeFinishResult {
        removed: done.removed,
        branch_deleted: done.branch_deleted,
        merge_commit: done.merge_commit,
        message: done.message,
    })
}

/// `session.delete`: drop the session's checkpoints (refs + gc) and remove its worktree
/// when it is clean and nothing is unmerged/unpushed (D49, D50 auto-cleanup).
pub(crate) fn session_deleted(state: &Arc<DaemonState>, sid: &str) {
    let Some(me) = state.services().and_then(|s| s.code.clone()) else {
        return;
    };
    if let Some(c) = &me.code {
        c.end_session(sid);
    }
    let sid = sid.to_string();
    state
        .root_scope()
        .child("code-cleanup")
        .spawn(async move {
            if let Some(c) = &me.checkpoints {
                c.drop_session(&sid).await;
            }
            if let Some(w) = me.worktree_or_recover(&sid).await {
                let base = w.base_branch.clone().unwrap_or_else(|| w.base_ref.clone());
                let clean = worktree::is_clean(&w.path).await.unwrap_or(false);
                let unpushed = worktree::unpushed(&w.main, &w.branch, &base)
                    .await
                    .unwrap_or(1);
                if clean && unpushed == 0 {
                    if let Err(e) = worktree::finish(&w, Finish::Discard).await {
                        tracing::warn!(error = %e, "worktree cleanup failed");
                    }
                } else {
                    tracing::info!(path = %w.path.display(), "kept a dirty worktree of a deleted session");
                }
                me.set_worktree(&sid, None);
            }
        });
}

// ------------------------------------------------------------------ git

async fn repo_root(ws: &Path) -> Result<PathBuf, RpcError> {
    repo_info(ws)
        .await
        .map(|i| i.top)
        .ok_or_else(|| state_err("the session's folder is not a git repository"))
}

fn with_path(mut args: Vec<String>, path: Option<&str>) -> Vec<String> {
    if let Some(p) = path.filter(|p| !p.is_empty()) {
        args.push("--".into());
        args.push(p.to_string());
    }
    args
}

async fn git_status(ws: &Path, path: Option<&str>) -> Result<GitStatusResult, RpcError> {
    repo_root(ws).await?;
    let args = with_path(
        [
            "status",
            "--porcelain=v1",
            "-z",
            "-b",
            "--untracked-files=all",
        ]
        .iter()
        .map(|s| (*s).to_string())
        .collect(),
        path,
    );
    let o = Git::new(ws).run(&args, None).await.map_err(state_err)?;
    if !o.ok() {
        return Err(state_err(o.stderr));
    }
    let entries = nul_split(&o.stdout);
    let mut branch = None;
    let mut files = Vec::new();
    let mut it = entries.iter();
    while let Some(e) = it.next() {
        if let Some(b) = e.strip_prefix("## ") {
            let b = b.strip_prefix("No commits yet on ").unwrap_or(b);
            let b = b.split("...").next().unwrap_or(b);
            let b = b.split(' ').next().unwrap_or(b);
            if b != "HEAD" {
                branch = Some(b.to_string());
            }
            continue;
        }
        if e.len() < 4 {
            continue;
        }
        let (xy, p) = e.split_at(3);
        let x = xy.as_bytes()[0];
        let y = xy.as_bytes()[1];
        if x == b'R' || x == b'C' {
            it.next(); // the original path
        }
        let change = if xy.starts_with("??") || x == b'A' {
            "added"
        } else if x == b'D' || y == b'D' {
            "deleted"
        } else {
            "modified"
        };
        files.push(FileChange {
            path: p.to_string(),
            change: change.into(),
        });
    }
    Ok(GitStatusResult { branch, files })
}

async fn git_diff(ws: &Path, path: Option<&str>) -> Result<TextResult, RpcError> {
    repo_root(ws).await?;
    let git = Git::new(ws);
    let base: Vec<String> = ["diff", "--no-color", "--no-ext-diff"]
        .iter()
        .map(|s| (*s).to_string())
        .collect();
    let mut with_head = base.clone();
    with_head.push("HEAD".into());
    let o = git
        .run(&with_path(with_head, path), None)
        .await
        .map_err(state_err)?;
    let o = if o.ok() {
        o
    } else {
        // No commits yet: staged + unstaged against the empty tree.
        git.run(&with_path(base, path), None)
            .await
            .map_err(state_err)?
    };
    if !o.ok() {
        return Err(state_err(o.stderr));
    }
    Ok(TextResult { text: o.text() })
}

async fn git_commit(
    ws: &Path,
    message: &str,
    paths: Option<&[String]>,
    then: Option<CommitThen>,
) -> Result<GitCommitResult, RpcError> {
    if message.trim().is_empty() {
        return Err(RpcError::invalid_params("message is required"));
    }
    repo_root(ws).await?;
    let git = Git::new(ws);
    let mut add = vec!["add".to_string(), "-A".into()];
    let mut commit = vec![
        "commit".to_string(),
        "-q".into(),
        "-m".into(),
        message.into(),
    ];
    if let Some(ps) = paths.filter(|p| !p.is_empty()) {
        add.push("--".into());
        add.extend(ps.iter().cloned());
        commit.push("--".into());
        commit.extend(ps.iter().cloned());
    }
    git.ok(&add).await.map_err(state_err)?;
    let o = git.run(&commit, None).await.map_err(state_err)?;
    if !o.ok() {
        let why = if o.text().contains("nothing to commit") || o.stderr.is_empty() {
            "nothing to commit".to_string()
        } else {
            o.stderr
        };
        return Err(state_err(why));
    }
    let sha = git.ok(&["rev-parse", "HEAD"]).await.map_err(state_err)?;
    let branch = git
        .ok(&["rev-parse", "--abbrev-ref", "HEAD"])
        .await
        .unwrap_or_else(|_| "HEAD".into());
    let note = match then {
        Some(CommitThen::Push) => {
            git.ok(&["push", "-u", "origin", "HEAD"])
                .await
                .map_err(state_err)?;
            None
        }
        Some(CommitThen::Pr) => Some(format!(
            "To open a pull request, run:\n  git push -u origin {branch}\n  gh pr create \
             --head {branch} --fill"
        )),
        _ => None,
    };
    Ok(GitCommitResult {
        commit: sha,
        pr_url: None,
        note,
    })
}

// ------------------------------------------------------------------ files

/// `rel` inside the session folder; refuses anything that resolves outside it
/// (`..`, absolute paths, symlinks pointing out).
fn confine(ws: &Path, rel: Option<&str>) -> Result<PathBuf, RpcError> {
    let root = std::fs::canonicalize(ws).unwrap_or_else(|_| ws.to_path_buf());
    let p = match rel.filter(|r| !r.is_empty() && *r != ".") {
        Some(r) => genie_tools::code::resolve_in(&root, r),
        None => root.clone(),
    };
    let real = std::fs::canonicalize(&p).unwrap_or_else(|_| p.clone());
    if !real.starts_with(&root) {
        return Err(RpcError::new(
            ErrorKind::PermissionDenied,
            "the path is outside the session's folder",
        ));
    }
    Ok(p)
}

fn files_tree(ws: &Path, rel: Option<&str>) -> Result<FilesTreeResult, RpcError> {
    let root = std::fs::canonicalize(ws).unwrap_or_else(|_| ws.to_path_buf());
    let dir = confine(ws, rel)?;
    if !dir.is_dir() {
        return Err(RpcError::new(ErrorKind::NotFound, "no such folder"));
    }
    let mut nodes: Vec<FileNode> = ignore::WalkBuilder::new(&dir)
        .max_depth(Some(1))
        .hidden(false)
        .filter_entry(|e| e.file_name() != ".git")
        .build()
        .flatten()
        .filter(|e| e.depth() == 1)
        .map(|e| {
            let is_dir = e.file_type().is_some_and(|t| t.is_dir());
            FileNode {
                path: genie_tools::code::display(&root, e.path()),
                dir: is_dir,
                size: if is_dir {
                    None
                } else {
                    e.metadata().ok().map(|m| m.len())
                },
            }
        })
        .collect();
    nodes.sort_by(|a, b| b.dir.cmp(&a.dir).then(a.path.cmp(&b.path)));
    Ok(FilesTreeResult { nodes })
}

fn files_read(me: &CodeServices, ws: &Path, rel: &str) -> Result<FilesReadResult, RpcError> {
    let p = confine(ws, Some(rel))?;
    let meta =
        std::fs::metadata(&p).map_err(|_| RpcError::new(ErrorKind::NotFound, "no such file"))?;
    if meta.is_dir() {
        return Err(RpcError::invalid_params("that is a folder; use files.tree"));
    }
    let bytes = std::fs::read(&p).map_err(|e| state_err(e.to_string()))?;
    let size = meta.len();
    if size <= MAX_TEXT_READ
        && !bytes[..bytes.len().min(8192)].contains(&0)
        && let Ok(text) = String::from_utf8(bytes.clone())
    {
        return Ok(FilesReadResult {
            text: Some(text),
            blob_id: None,
            size,
        });
    }
    let store = me
        .store
        .as_ref()
        .ok_or_else(|| state_err("binary or large files need the store"))?;
    let id = store
        .put_blob(&bytes, None)
        .map_err(|e| RpcError::internal(e.to_string()))?;
    Ok(FilesReadResult {
        text: None,
        blob_id: Some(id),
        size,
    })
}
