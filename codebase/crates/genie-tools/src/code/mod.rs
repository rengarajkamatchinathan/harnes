//! Code-mode file tools (spec/06-agent-runtime.md §4.2) and the git plumbing that
//! checkpoints (D49, 04 §10) and worktrees (D50, 06 §15) are built on.
//!
//! | Tool | Behaviour |
//! |---|---|
//! | `read` | text with line numbers; 2,000 lines / 50 KiB / 2,000 chars per line; `offset`/`limit`; records **mtime + hash** ([`ReadState`]) for read-before-edit; pages spilled output refs |
//! | `write` | new files, or overwrite **only after read**; staleness check |
//! | `edit` | `old_string/new_string/replace_all`; 3-tier matcher ([`matcher`]: exact → Unicode-normalised → trailing-whitespace-tolerant); LF view preserving CRLF/BOM; "closest lines" hint on a miss; read-before-edit + staleness inside the per-path lock; approval asked **after** the match |
//! | `multi_edit` | several edits to one file, atomic |
//! | `patch` | Codex `*** Begin Patch` envelope; atomic with rollback |
//! | `grep`, `glob`, `list` | the ripgrep crates in-process (`ignore` walker + `regex`/`globset`): `.gitignore`/`.ignore`/git excludes and hidden files skipped like `rg`; 100 results / 30 s |
//! | `todo_write` | whole-list replace, kept per session |
//!
//! Ports: [`SessionDirs`](crate::shell::SessionDirs) gives the session's workspace root
//! (relative paths resolve against it); [`PathLocker`] serialises mutations per path
//! across all sessions (`genie_core::locks::PathLocks` in the daemon). Permissions:
//! every tool asks the gate with the absolute path as `resource` (`read`/`grep`/`glob`/
//! `list` → `Action::Read`, the editors → `Action::Edit` with a `Diff` preview).

pub mod diff;
pub mod edit;
pub mod git;
pub mod matcher;
pub mod patch;
pub mod read;
pub mod search;
pub mod snapshot;
pub mod todo;
pub mod worktree;
pub mod write;

use std::borrow::Cow;
use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::path::{Component, Path, PathBuf};
use std::sync::{Arc, Mutex, Weak};
use std::time::SystemTime;

use async_trait::async_trait;
use genie_protocol::ask::Preview;
use genie_protocol::common::SessionKind;
use serde_json::Value;

use crate::shell::SessionDirs;
use crate::tool::{Availability, PermissionRequest, Tool, ToolCtx, ToolEnv, ToolError};

/// 06 §4.2 read limits.
pub const MAX_READ_LINES: usize = 2_000;
pub const MAX_READ_BYTES: usize = 50 * 1024;
pub const MAX_LINE_CHARS: usize = 2_000;
/// 06 §4.2 search limits.
pub const MAX_RESULTS: usize = 100;
pub const SEARCH_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);

/// A held path lock; dropping it releases the path.
pub type PathGuard = Box<dyn Send + Sync>;

/// Per-path serialisation of file mutations across sessions and clients (06 §4.2).
/// Implemented by `genie-core` on `locks::PathLocks`.
#[async_trait]
pub trait PathLocker: Send + Sync {
    async fn lock(&self, path: &Path) -> PathGuard;
}

/// An in-crate locker (tests, hosts without the engine's shared locks).
#[derive(Default)]
pub struct LocalLocks {
    map: Mutex<HashMap<PathBuf, Weak<tokio::sync::Mutex<()>>>>,
}

#[async_trait]
impl PathLocker for LocalLocks {
    async fn lock(&self, path: &Path) -> PathGuard {
        let key = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
        let m = {
            let mut map = self.map.lock().unwrap_or_else(|e| e.into_inner());
            map.retain(|_, w| w.strong_count() > 0);
            match map.get(&key).and_then(Weak::upgrade) {
                Some(m) => m,
                None => {
                    let m = Arc::new(tokio::sync::Mutex::new(()));
                    map.insert(key, Arc::downgrade(&m));
                    m
                }
            }
        };
        Box::new(m.lock_owned().await)
    }
}

/// What `read` saw: the file's mtime and a content hash (06 §4.2 read-before-edit).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Stamp {
    pub mtime: Option<SystemTime>,
    pub hash: u64,
}

impl Stamp {
    pub fn of(bytes: &[u8], mtime: Option<SystemTime>) -> Stamp {
        let mut h = std::collections::hash_map::DefaultHasher::new();
        bytes.hash(&mut h);
        Stamp {
            mtime,
            hash: h.finish(),
        }
    }
}

/// Files each session has read, for read-before-edit and the staleness check.
#[derive(Default)]
pub struct ReadState {
    map: Mutex<HashMap<(String, PathBuf), Stamp>>,
}

impl ReadState {
    pub fn record(&self, session: &str, path: &Path, stamp: Stamp) {
        self.map
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert((session.to_string(), path.to_path_buf()), stamp);
    }

    pub fn get(&self, session: &str, path: &Path) -> Option<Stamp> {
        self.map
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(&(session.to_string(), path.to_path_buf()))
            .copied()
    }

    /// Forget a session (session end).
    pub fn end_session(&self, session: &str) {
        self.map
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .retain(|(s, _), _| s != session);
    }

    /// Read-before-edit + staleness: `Ok` when `path` was read by this session and its
    /// content has not changed since (a touched mtime with equal content still passes).
    pub fn check(&self, session: &str, path: &Path, current: &[u8]) -> Result<(), ToolError> {
        let Some(seen) = self.get(session, path) else {
            return Err(ToolError::failed(format!(
                "{} has not been read in this session. Read it first, then edit it.",
                path.display()
            )));
        };
        if Stamp::of(current, None).hash != seen.hash {
            return Err(ToolError::failed(format!(
                "{} changed since it was last read (by the user or another tool). Read it \
                 again before editing.",
                path.display()
            )));
        }
        Ok(())
    }
}

/// Shared configuration of the code tools.
#[derive(Clone)]
pub struct CodeConfig {
    pub dirs: Arc<dyn SessionDirs>,
    pub locks: Arc<dyn PathLocker>,
    pub reads: Arc<ReadState>,
    pub todos: Arc<todo::TodoStore>,
}

impl CodeConfig {
    pub fn new(dirs: Arc<dyn SessionDirs>, locks: Arc<dyn PathLocker>) -> CodeConfig {
        CodeConfig {
            dirs,
            locks,
            reads: Arc::new(ReadState::default()),
            todos: Arc::new(todo::TodoStore::default()),
        }
    }

    /// The session's workspace root.
    pub fn root(&self, session: &str) -> Result<PathBuf, ToolError> {
        self.dirs.initial_cwd(session).ok_or_else(|| {
            ToolError::failed("this session has no workspace folder, so file tools can't run")
        })
    }

    /// Resolve a model-given path against the workspace root (lexically normalised).
    pub fn resolve(&self, session: &str, path: &str) -> Result<PathBuf, ToolError> {
        let root = self.root(session)?;
        Ok(resolve_in(&root, path))
    }

    /// Forget per-session state (read stamps, todo list).
    pub fn end_session(&self, session: &str) {
        self.reads.end_session(session);
        self.todos.end_session(session);
    }
}

/// `path` joined to `root` unless absolute, with `.`/`..` removed lexically and `~/`
/// expanded.
pub fn resolve_in(root: &Path, path: &str) -> PathBuf {
    let path = path.trim();
    let p = match path.strip_prefix("~/") {
        Some(rest) => match std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE")) {
            Some(h) => PathBuf::from(h).join(rest),
            None => PathBuf::from(path),
        },
        None => PathBuf::from(path),
    };
    let joined = if p.is_absolute() { p } else { root.join(p) };
    normalize(&joined)
}

/// Lexical normalisation (no filesystem access).
pub fn normalize(p: &Path) -> PathBuf {
    let mut out = PathBuf::new();
    for c in p.components() {
        match c {
            Component::CurDir => {}
            Component::ParentDir => {
                if !out.pop() {
                    out.push("..");
                }
            }
            other => out.push(other.as_os_str()),
        }
    }
    out
}

/// `path` relative to `root` for display (forward slashes), or the absolute path.
pub fn display(root: &Path, path: &Path) -> String {
    match path.strip_prefix(root) {
        Ok(r) if r.as_os_str().is_empty() => ".".into(),
        Ok(r) => r.to_string_lossy().replace('\\', "/"),
        Err(_) => path.display().to_string(),
    }
}

/// Code tools show only in Code sessions.
pub(crate) fn code_only(env: &ToolEnv) -> Availability {
    if env.kind == SessionKind::Code {
        Availability::Available
    } else {
        Availability::Unavailable(Cow::Borrowed("code sessions only"))
    }
}

/// Rename alias keys models use (`file_path` → `path`) before validation.
pub(crate) fn alias_keys(mut input: Value, aliases: &[(&str, &str)]) -> Value {
    if let Value::Object(m) = &mut input {
        for (from, to) in aliases {
            if !m.contains_key(*to)
                && let Some(v) = m.remove(*from)
            {
                m.insert((*to).to_string(), v);
            }
        }
    }
    input
}

/// Ask the gate about reading `path`.
pub(crate) async fn authorize_read(ctx: &ToolCtx, path: &Path) -> Result<(), ToolError> {
    ctx.authorize(PermissionRequest {
        resource: Some(path.display().to_string()),
        preview: None,
        save_suggestions: Vec::new(),
    })
    .await
}

/// Ask the gate about editing `path`, showing the diff.
pub(crate) async fn authorize_edit(
    ctx: &ToolCtx,
    path: &Path,
    shown: &str,
    unified: String,
) -> Result<(), ToolError> {
    ctx.authorize(PermissionRequest {
        resource: Some(path.display().to_string()),
        preview: Some(Preview::Diff {
            path: shown.to_string(),
            unified,
        }),
        save_suggestions: Vec::new(),
    })
    .await
}

/// A UTF-8 text file in its LF view, remembering the BOM and CRLF line endings so a
/// write puts them back (06 §4.2).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TextFile {
    pub text: String,
    pub bom: bool,
    pub crlf: bool,
}

impl TextFile {
    pub fn decode(bytes: &[u8]) -> Result<TextFile, ToolError> {
        let s = std::str::from_utf8(bytes)
            .map_err(|_| ToolError::failed("the file is not UTF-8 text, so it can't be edited"))?;
        let (bom, s) = match s.strip_prefix('\u{feff}') {
            Some(rest) => (true, rest),
            None => (false, s),
        };
        let crlf_count = s.matches("\r\n").count();
        let lf_count = s.matches('\n').count();
        let crlf = crlf_count > 0 && crlf_count * 2 >= lf_count;
        Ok(TextFile {
            text: s.replace("\r\n", "\n"),
            bom,
            crlf,
        })
    }

    pub fn new_like(&self, text: String) -> TextFile {
        TextFile {
            text,
            bom: self.bom,
            crlf: self.crlf,
        }
    }

    pub fn encode(&self) -> Vec<u8> {
        let mut out = String::with_capacity(self.text.len() + 8);
        if self.bom {
            out.push('\u{feff}');
        }
        if self.crlf {
            out.push_str(&self.text.replace("\r\n", "\n").replace('\n', "\r\n"));
        } else {
            out.push_str(&self.text);
        }
        out.into_bytes()
    }
}

/// Write `bytes` to `path` atomically (temp file + rename), creating parent folders and
/// keeping the existing file's permissions.
pub(crate) fn write_atomic(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    std::fs::create_dir_all(parent)?;
    let perms = std::fs::metadata(path).ok().map(|m| m.permissions());
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "file".into());
    let tmp = parent.join(format!(".{name}.genie-tmp-{}", std::process::id()));
    std::fs::write(&tmp, bytes)?;
    if let Some(p) = perms {
        let _ = std::fs::set_permissions(&tmp, p);
    }
    match std::fs::rename(&tmp, path) {
        Ok(()) => Ok(()),
        Err(e) => {
            let _ = std::fs::remove_file(&tmp);
            Err(e)
        }
    }
}

pub(crate) fn mtime(path: &Path) -> Option<SystemTime> {
    std::fs::metadata(path).and_then(|m| m.modified()).ok()
}

/// All code tools (Code registry only).
pub fn tools(cfg: CodeConfig) -> Vec<Arc<dyn Tool>> {
    vec![
        Arc::new(read::ReadTool::new(cfg.clone())),
        Arc::new(write::WriteTool::new(cfg.clone())),
        Arc::new(edit::EditTool::new(cfg.clone())),
        Arc::new(edit::MultiEditTool::new(cfg.clone())),
        Arc::new(patch::PatchTool::new(cfg.clone())),
        Arc::new(search::GrepTool::new(cfg.clone())),
        Arc::new(search::GlobTool::new(cfg.clone())),
        Arc::new(search::ListTool::new(cfg.clone())),
        Arc::new(todo::TodoWriteTool::new(cfg)),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolve_and_display() {
        let root = Path::new("/w/repo");
        assert_eq!(resolve_in(root, "src/a.rs"), PathBuf::from("/w/repo/src/a.rs"));
        assert_eq!(resolve_in(root, "./src/../b"), PathBuf::from("/w/repo/b"));
        assert_eq!(resolve_in(root, "/etc/x"), PathBuf::from("/etc/x"));
        assert_eq!(display(root, Path::new("/w/repo/src/a.rs")), "src/a.rs");
        assert_eq!(display(root, Path::new("/etc/x")), "/etc/x");
    }

    #[test]
    fn text_file_keeps_bom_and_crlf() {
        let f = TextFile::decode("\u{feff}a\r\nb\r\n".as_bytes()).unwrap();
        assert!(f.bom && f.crlf);
        assert_eq!(f.text, "a\nb\n");
        let g = f.new_like("a\nc\n".into());
        assert_eq!(g.encode(), "\u{feff}a\r\nc\r\n".as_bytes());
        let plain = TextFile::decode(b"x\ny").unwrap();
        assert!(!plain.bom && !plain.crlf);
        assert_eq!(plain.encode(), b"x\ny");
        assert!(TextFile::decode(&[0xff, 0xfe, 0x00]).is_err());
    }

    #[test]
    fn read_state_detects_staleness() {
        let rs = ReadState::default();
        let p = Path::new("/x");
        assert!(rs.check("s", p, b"a").is_err());
        rs.record("s", p, Stamp::of(b"a", None));
        assert!(rs.check("s", p, b"a").is_ok());
        assert!(rs.check("s", p, b"b").is_err());
        assert!(rs.check("other", p, b"a").is_err());
    }
}
