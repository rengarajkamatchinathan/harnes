//! The skill catalogue: discovery results + policy + enable/disable state, and the three
//! levels of progressive disclosure (06 §9 "Use"):
//!
//! 1. [`SkillCatalog::index`] — name + description for `<available_skills>` (baseline);
//! 2. [`SkillCatalog::load`] — `SKILL.md` body + base path + sampled file list (`skill` tool,
//!    `/skill-name`, "+");
//! 3. [`SkillCatalog::read_file`] — one file inside the skill folder (`skill_read`).

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use std::fs;
use std::io::{self, Read};
use std::path::{Component, Path, PathBuf};
use std::sync::{Arc, Mutex};

use genie_protocol::api::mcp::{SkillInfo, SkillListResult};
use serde::{Deserialize, Serialize};

use crate::error::SkillError;
use crate::manifest::{self, MAX_SKILL_MD};
use crate::package::{self, ImportOptions, InstalledSkill};
use crate::scan::{
    self, Diagnostic, MARKER_FILE, SKILL_FILE, Skill, SkillLocation, SkillRoots, SkillSource,
};

/// Largest slice of a file `skill_read` returns (bytes); the rest is reported as truncated.
pub const MAX_READ: u64 = 256 * 1024;
/// Files listed by `skill` (the list is a sample; `skill_read` can read any of them).
pub const MAX_LISTED_FILES: usize = 50;
const MAX_LIST_DEPTH: usize = 4;

/// Admin policy (08 §5.2 `skills.*`). `skill.use` deny is a permission rule and is checked by
/// the engine before calling [`SkillCatalog::load`].
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SkillPolicy {
    /// `skills.allowed`: only these ids are visible (when set).
    pub allowed: Option<Vec<String>>,
    /// `skills.directoryOnly`: only built-in and directory-installed skills.
    pub directory_only: bool,
    /// `skills.disableUserUpload`.
    pub disable_user_upload: bool,
}

impl SkillPolicy {
    pub fn permits(&self, s: &Skill) -> bool {
        if let Some(allowed) = &self.allowed
            && !allowed.iter().any(|a| a == &s.id)
        {
            return false;
        }
        !self.directory_only || matches!(s.source, SkillSource::Builtin | SkillSource::Directory)
    }

    pub fn permits_id(&self, id: &str) -> bool {
        self.allowed
            .as_ref()
            .is_none_or(|a| a.iter().any(|x| x == id))
    }
}

/// Persistence of the per-skill on/off switch (Settings → Capabilities → Skills, D34).
pub trait SkillStateStore: Send + Sync {
    fn disabled(&self) -> BTreeSet<String>;
    fn save_disabled(&self, ids: &BTreeSet<String>) -> io::Result<()>;
}

/// In-memory state (tests, incognito, or when settings own persistence elsewhere).
#[derive(Default)]
pub struct MemoryState(Mutex<BTreeSet<String>>);

impl SkillStateStore for MemoryState {
    fn disabled(&self) -> BTreeSet<String> {
        self.0.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }
    fn save_disabled(&self, ids: &BTreeSet<String>) -> io::Result<()> {
        *self.0.lock().unwrap_or_else(|e| e.into_inner()) = ids.clone();
        Ok(())
    }
}

/// `{"disabled": [...]}` in a JSON file (e.g. `~/.genie/skills-state.json`), written
/// atomically.
pub struct JsonStateFile {
    path: PathBuf,
}

#[derive(Default, Serialize, Deserialize)]
struct StateDoc {
    #[serde(default)]
    disabled: BTreeSet<String>,
}

impl JsonStateFile {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        JsonStateFile { path: path.into() }
    }
}

impl SkillStateStore for JsonStateFile {
    fn disabled(&self) -> BTreeSet<String> {
        fs::read(&self.path)
            .ok()
            .and_then(|b| serde_json::from_slice::<StateDoc>(&b).ok())
            .unwrap_or_default()
            .disabled
    }
    fn save_disabled(&self, ids: &BTreeSet<String>) -> io::Result<()> {
        if let Some(parent) = self.path.parent() {
            fs::create_dir_all(parent)?;
        }
        let doc = StateDoc {
            disabled: ids.clone(),
        };
        let tmp = self.path.with_extension("json.tmp");
        fs::write(
            &tmp,
            serde_json::to_vec_pretty(&doc).map_err(io::Error::other)?,
        )?;
        fs::rename(&tmp, &self.path)
    }
}

/// Who asked to load a skill.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Invoker {
    /// The `skill` tool. Refused for `disable-model-invocation` skills.
    Model,
    /// `/skill-name` or the "+" menu (`InputPart::Skill`). Policy still applies.
    User,
}

/// Session kind, for how scripts are presented (D44: chat v1 lists them, Code runs them
/// through `shell`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionKind {
    Chat,
    Code,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileKind {
    Reference,
    Asset,
    Script,
    Other,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SkillFileEntry {
    /// Relative to the skill folder, `/`-separated.
    pub path: String,
    pub size: u64,
    pub kind: FileKind,
}

/// Level 2: what the `skill` tool returns.
#[derive(Debug, Clone, PartialEq)]
pub struct LoadedSkill {
    pub id: String,
    pub name: String,
    pub description: String,
    pub source: SkillSource,
    /// Folder path (or `builtin:<id>`).
    pub base_path: String,
    pub body: String,
    pub files: Vec<SkillFileEntry>,
    pub files_truncated: bool,
    pub allowed_tools: Vec<String>,
    pub session: SessionKind,
}

impl LoadedSkill {
    /// Model-facing text. Also what the engine re-injects after compaction (06 §3).
    pub fn render(&self) -> String {
        let mut s = format!(
            "<skill name=\"{}\" base_path=\"{}\">\n{}\n",
            attr(&self.id),
            attr(&self.base_path),
            self.body.trim()
        );
        if !self.files.is_empty() {
            s.push_str("\n<skill_files>\n");
            for f in &self.files {
                let _ = write!(s, "- {} ({} bytes)", f.path, f.size);
                if f.kind == FileKind::Script {
                    s.push_str(match self.session {
                        SessionKind::Chat => " [script: runs in Code mode]",
                        SessionKind::Code => " [script: run with the shell tool]",
                    });
                }
                s.push('\n');
            }
            if self.files_truncated {
                s.push_str("- … more files not listed\n");
            }
            s.push_str("</skill_files>\nRead files with `skill_read` and a path relative to the skill folder.\n");
        }
        s.push_str("</skill>");
        s
    }
}

/// Level 3: one file read by `skill_read`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SkillFile {
    pub path: String,
    pub content: String,
    pub size: u64,
    pub truncated: bool,
}

pub(crate) fn attr(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('"', "&quot;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

pub struct SkillCatalog {
    roots: SkillRoots,
    policy: SkillPolicy,
    state: Arc<dyn SkillStateStore>,
    disabled: BTreeSet<String>,
    skills: BTreeMap<String, Skill>,
    diagnostics: Vec<Diagnostic>,
    fingerprint: String,
}

impl SkillCatalog {
    pub fn new(roots: SkillRoots, policy: SkillPolicy, state: Arc<dyn SkillStateStore>) -> Self {
        let mut c = SkillCatalog {
            roots,
            policy,
            disabled: state.disabled(),
            state,
            skills: BTreeMap::new(),
            diagnostics: vec![],
            fingerprint: String::new(),
        };
        c.rescan();
        c
    }

    /// Full rescan of every root. Later roots override earlier ones (built-in < user < project).
    pub fn rescan(&mut self) {
        self.fingerprint = scan::fingerprint(&self.roots);
        let mut skills = BTreeMap::new();
        let mut diags = vec![];
        if self.roots.builtin {
            for s in scan::builtin_skills() {
                skills.insert(s.id.clone(), s);
            }
        }
        for root in &self.roots.roots {
            let (found, d) = scan::scan_root(root);
            diags.extend(d);
            for s in found {
                if let (Some(prev), Some(dir)) = (skills.get(&s.id), s.dir()) {
                    diags.push(Diagnostic {
                        path: dir.to_path_buf(),
                        severity: scan::Severity::Warning,
                        message: format!(
                            "skill `{}` overrides the {} skill with the same name",
                            s.id,
                            prev.source.as_str()
                        ),
                    });
                }
                skills.insert(s.id.clone(), s);
            }
        }
        self.skills = skills;
        self.diagnostics = diags;
    }

    /// Hot reload: rescans when the fingerprint changed. Returns whether it did. The engine
    /// calls this from its file watcher (or on a timer) and diffs [`Self::index`].
    pub fn refresh(&mut self) -> bool {
        if scan::fingerprint(&self.roots) == self.fingerprint {
            return false;
        }
        self.rescan();
        true
    }

    /// Switches roots (e.g. the workspace became trusted) and rescans.
    pub fn set_roots(&mut self, roots: SkillRoots) {
        self.roots = roots;
        self.rescan();
    }

    pub fn set_policy(&mut self, policy: SkillPolicy) {
        self.policy = policy;
    }

    pub fn policy(&self) -> &SkillPolicy {
        &self.policy
    }

    pub fn diagnostics(&self) -> &[Diagnostic] {
        &self.diagnostics
    }

    /// Skills the policy permits (enabled or not).
    pub fn skills(&self) -> impl Iterator<Item = &Skill> {
        self.skills.values().filter(|s| self.policy.permits(s))
    }

    pub fn get(&self, id: &str) -> Option<&Skill> {
        self.skills.get(id).filter(|s| self.policy.permits(s))
    }

    pub fn is_enabled(&self, id: &str) -> bool {
        !self.disabled.contains(id)
    }

    /// `skill.list`.
    pub fn list(&self) -> SkillListResult {
        SkillListResult {
            skills: self
                .skills()
                .map(|s| SkillInfo {
                    id: s.id.clone(),
                    name: s.manifest.name.clone(),
                    description: s.manifest.description.clone(),
                    enabled: self.is_enabled(&s.id),
                    source: s.source.as_str().to_string(),
                })
                .collect(),
        }
    }

    /// `skill.set_enabled`. Persists through the state store.
    pub fn set_enabled(&mut self, id: &str, enabled: bool) -> Result<(), SkillError> {
        if self.get(id).is_none() {
            return Err(SkillError::NotFound(id.to_string()));
        }
        let mut next = self.disabled.clone();
        if enabled {
            next.remove(id);
        } else {
            next.insert(id.to_string());
        }
        self.state.save_disabled(&next)?;
        self.disabled = next;
        Ok(())
    }

    /// `(id, description)` of enabled, model-invocable skills, sorted by id: the input of
    /// `genie_core::runtime::context::InstructionSource::skills_index`. Empty → leave the
    /// source and the `feature.skills` fragment out.
    pub fn index(&self) -> Vec<(String, String)> {
        self.skills()
            .filter(|s| self.is_enabled(&s.id) && !s.manifest.disable_model_invocation)
            .map(|s| (s.id.clone(), s.manifest.description.clone()))
            .collect()
    }

    /// `(id, description)` of every enabled skill the user can attach (`/` menu, "+").
    pub fn user_invocable(&self) -> Vec<(String, String)> {
        self.skills()
            .filter(|s| self.is_enabled(&s.id))
            .map(|s| (s.id.clone(), s.manifest.description.clone()))
            .collect()
    }

    fn usable(&self, id: &str, invoker: Invoker) -> Result<&Skill, SkillError> {
        let s = self
            .skills
            .get(id)
            .ok_or_else(|| SkillError::NotFound(id.to_string()))?;
        if !self.policy.permits(s) {
            return Err(SkillError::Blocked(id.to_string()));
        }
        if !self.is_enabled(id) {
            return Err(SkillError::Disabled(id.to_string()));
        }
        if invoker == Invoker::Model && s.manifest.disable_model_invocation {
            return Err(SkillError::UserOnly(id.to_string()));
        }
        Ok(s)
    }

    /// Level 2. Reads `SKILL.md` fresh, so edits since the scan are picked up.
    pub fn load(
        &self,
        id: &str,
        invoker: Invoker,
        session: SessionKind,
    ) -> Result<LoadedSkill, SkillError> {
        let s = self.usable(id, invoker)?;
        let (text, base_path, files, truncated) = match &s.location {
            SkillLocation::Builtin(text) => (
                (*text).to_string(),
                format!("builtin:{}", s.id),
                vec![],
                false,
            ),
            SkillLocation::Dir(dir) => {
                let text = read_capped(&dir.join(SKILL_FILE), MAX_SKILL_MD)?;
                let (files, truncated) = list_files(dir);
                (text, dir.display().to_string(), files, truncated)
            }
        };
        let (m, body) = manifest::parse_skill_md(&text).map_err(SkillError::Invalid)?;
        Ok(LoadedSkill {
            id: s.id.clone(),
            name: m.name,
            description: m.description,
            source: s.source,
            base_path,
            body: body.to_string(),
            files,
            files_truncated: truncated,
            allowed_tools: m.allowed_tools,
            session,
        })
    }

    /// Level 3. `path` is relative to the skill folder; `..`, absolute paths, hidden files and
    /// symlinks leading outside the folder are refused.
    pub fn read_file(
        &self,
        id: &str,
        path: &str,
        invoker: Invoker,
    ) -> Result<SkillFile, SkillError> {
        let s = self.usable(id, invoker)?;
        let dir = match &s.location {
            SkillLocation::Builtin(text) if path == SKILL_FILE => {
                return Ok(SkillFile {
                    path: path.to_string(),
                    content: (*text).to_string(),
                    size: text.len() as u64,
                    truncated: false,
                });
            }
            SkillLocation::Builtin(_) => return Err(SkillError::PathEscape(path.to_string())),
            SkillLocation::Dir(d) => d,
        };
        let rel = safe_relative(path)?;
        let base = dir.canonicalize()?;
        let full = base.join(&rel).canonicalize().map_err(|_| {
            io::Error::new(
                io::ErrorKind::NotFound,
                format!("no file `{path}` in skill `{id}`"),
            )
        })?;
        if !full.starts_with(&base) {
            return Err(SkillError::PathEscape(path.to_string()));
        }
        let meta = fs::metadata(&full)?;
        if !meta.is_file() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("`{path}` is not a file"),
            )
            .into());
        }
        let mut buf = vec![];
        fs::File::open(&full)?
            .take(MAX_READ)
            .read_to_end(&mut buf)?;
        let truncated = meta.len() > MAX_READ;
        let content = match String::from_utf8(buf) {
            Ok(s) => s,
            // Cut in the middle of a character at the limit: drop the partial character.
            Err(e) if truncated && e.utf8_error().error_len().is_none() => {
                let valid = e.utf8_error().valid_up_to();
                let mut b = e.into_bytes();
                b.truncate(valid);
                String::from_utf8(b).unwrap_or_default()
            }
            Err(_) => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    format!(
                        "`{path}` is a binary file ({} bytes) and cannot be read as text",
                        meta.len()
                    ),
                )
                .into());
            }
        };
        Ok(SkillFile {
            path: path.to_string(),
            content,
            size: meta.len(),
            truncated,
        })
    }

    /// The root uploads and directory installs go to (`~/.genie/skills`).
    pub fn user_root(&self) -> Option<&Path> {
        self.roots
            .roots
            .iter()
            .find(|r| r.source == SkillSource::User)
            .map(|r| r.path.as_path())
    }

    /// `skill.upload`: validates and unpacks a `.zip`/`.skill` into the user root, then rescans.
    pub fn upload(&mut self, archive: &[u8], replace: bool) -> Result<InstalledSkill, SkillError> {
        if self.policy.disable_user_upload || self.policy.directory_only {
            return Err(SkillError::Policy(
                "skill upload is disabled by your organisation".into(),
            ));
        }
        let root = self
            .user_root()
            .ok_or_else(|| SkillError::Policy("no user skills folder is configured".into()))?
            .to_path_buf();
        let opts = ImportOptions {
            replace,
            source: SkillSource::Upload,
            ..ImportOptions::default()
        };
        let installed = package::import(archive, &root, &opts, &|id| self.policy.permits_id(id))?;
        self.rescan();
        Ok(installed)
    }

    /// Removes a user-scope skill folder (user, upload or directory source).
    pub fn uninstall(&mut self, id: &str) -> Result<(), SkillError> {
        let s = self
            .skills
            .get(id)
            .ok_or_else(|| SkillError::NotFound(id.to_string()))?;
        let (Some(dir), Some(root)) = (s.dir(), self.user_root()) else {
            return Err(SkillError::Policy(format!(
                "skill `{id}` cannot be removed here"
            )));
        };
        if !matches!(
            s.source,
            SkillSource::User | SkillSource::Upload | SkillSource::Directory
        ) || !dir.starts_with(root)
        {
            return Err(SkillError::Policy(format!(
                "skill `{id}` is not in the user skills folder"
            )));
        }
        fs::remove_dir_all(dir)?;
        self.rescan();
        Ok(())
    }
}

/// Validates a skill-relative path: no absolute, `..`, prefix or hidden components.
pub(crate) fn safe_relative(path: &str) -> Result<PathBuf, SkillError> {
    let esc = || SkillError::PathEscape(path.to_string());
    if path.is_empty() || path.contains('\\') || path.contains('\0') || path.contains(':') {
        return Err(esc());
    }
    let mut out = PathBuf::new();
    for c in Path::new(path).components() {
        match c {
            Component::Normal(n) => {
                if n.to_string_lossy().starts_with('.') {
                    return Err(esc());
                }
                out.push(n);
            }
            Component::CurDir => {}
            _ => return Err(esc()),
        }
    }
    if out.as_os_str().is_empty() {
        return Err(esc());
    }
    Ok(out)
}

fn read_capped(p: &Path, cap: u64) -> Result<String, SkillError> {
    let len = fs::metadata(p)?.len();
    if len > cap {
        return Err(SkillError::TooLarge {
            what: p.display().to_string(),
            size: len,
            limit: cap,
        });
    }
    Ok(fs::read_to_string(p)?)
}

fn kind_of(rel: &str) -> FileKind {
    let first = rel.split('/').next().unwrap_or_default();
    let ext = rel.rsplit('.').next().unwrap_or_default();
    match first {
        "scripts" => FileKind::Script,
        "references" => FileKind::Reference,
        "assets" => FileKind::Asset,
        _ if matches!(
            ext,
            "sh" | "py" | "js" | "mjs" | "ts" | "rb" | "ps1" | "bat" | "cmd"
        ) =>
        {
            FileKind::Script
        }
        _ => FileKind::Other,
    }
}

/// Sorted, breadth-limited file list of a skill folder (hidden files and `SKILL.md` left out).
fn list_files(dir: &Path) -> (Vec<SkillFileEntry>, bool) {
    let mut out = vec![];
    let mut truncated = false;
    let mut stack = vec![(dir.to_path_buf(), String::new(), 0usize)];
    while let Some((d, prefix, depth)) = stack.pop() {
        let Ok(rd) = fs::read_dir(&d) else {
            continue;
        };
        let mut entries: Vec<_> = rd.flatten().collect();
        entries.sort_by_key(|e| std::cmp::Reverse(e.file_name()));
        for e in entries {
            let name = e.file_name().to_string_lossy().into_owned();
            if name.starts_with('.') || (depth == 0 && (name == SKILL_FILE || name == MARKER_FILE))
            {
                continue;
            }
            let rel = format!("{prefix}{name}");
            let Ok(meta) = e.metadata() else {
                continue;
            };
            if meta.is_dir() {
                if depth + 1 < MAX_LIST_DEPTH {
                    stack.push((e.path(), format!("{rel}/"), depth + 1));
                }
            } else if out.len() < MAX_LISTED_FILES {
                out.push(SkillFileEntry {
                    kind: kind_of(&rel),
                    path: rel,
                    size: meta.len(),
                });
            } else {
                truncated = true;
            }
        }
    }
    out.sort_by(|a, b| a.path.cmp(&b.path));
    (out, truncated)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn relative_paths() {
        assert!(safe_relative("references/a.md").is_ok());
        assert!(safe_relative("./a.md").is_ok());
        for bad in [
            "",
            "/etc/passwd",
            "../x",
            "a/../../x",
            ".genie-skill.json",
            "a/.env",
            "C:\\x",
            "a\\..\\b",
            ".",
        ] {
            assert!(safe_relative(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn kinds() {
        assert_eq!(kind_of("scripts/run.txt"), FileKind::Script);
        assert_eq!(kind_of("tools/run.py"), FileKind::Script);
        assert_eq!(kind_of("references/a.md"), FileKind::Reference);
        assert_eq!(kind_of("assets/a.png"), FileKind::Asset);
        assert_eq!(kind_of("notes.md"), FileKind::Other);
    }
}
