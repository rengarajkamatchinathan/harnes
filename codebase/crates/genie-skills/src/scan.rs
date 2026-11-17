//! Skill discovery (06 §9 "Locations"): depth-limited scan of skill roots, built-in skills,
//! and a fingerprint for hot reload.
//!
//! The scanner reads only frontmatter-level metadata (the `SKILL.md` text is parsed and the
//! body dropped); bodies and files are read on use (progressive disclosure).

use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::manifest::{self, MAX_SKILL_MD, SkillManifest};

pub const SKILL_FILE: &str = "SKILL.md";
/// Written next to `SKILL.md` by upload and directory installs; hidden from file lists.
pub const MARKER_FILE: &str = ".genie-skill.json";
/// Folder levels below a root that are searched for `SKILL.md` (`root/a/b/c/SKILL.md`).
pub const MAX_DEPTH: usize = 3;

/// Where a skill came from (`SkillInfo.source` on the wire).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SkillSource {
    /// Shipped with the app (compiled in, so signed and updated with it).
    Builtin,
    /// A folder in `~/.genie/skills/`.
    User,
    /// `.genie/skills/` or `.agents/skills/` of a trusted workspace.
    Project,
    /// Unpacked from a `.zip`/`.skill` upload into the user root.
    Upload,
    /// Installed from the signed skills directory into the user root.
    Directory,
}

impl SkillSource {
    pub fn as_str(self) -> &'static str {
        match self {
            SkillSource::Builtin => "builtin",
            SkillSource::User => "user",
            SkillSource::Project => "project",
            SkillSource::Upload => "upload",
            SkillSource::Directory => "directory",
        }
    }
}

/// Contents of [`MARKER_FILE`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct InstallMarker {
    pub source: SkillSource,
    /// SHA-256 (hex) of the archive it was unpacked from (audit: "skill installs with hash").
    pub sha256: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum SkillLocation {
    /// The skill folder (contains `SKILL.md`).
    Dir(PathBuf),
    /// Compiled-in `SKILL.md` text.
    Builtin(&'static str),
}

/// Skill metadata (level 1 of progressive disclosure).
#[derive(Debug, Clone, PartialEq)]
pub struct Skill {
    /// The folder name (06 §9: "the id comes from the folder name").
    pub id: String,
    pub manifest: SkillManifest,
    pub source: SkillSource,
    pub location: SkillLocation,
    pub install: Option<InstallMarker>,
}

impl Skill {
    pub fn dir(&self) -> Option<&Path> {
        match &self.location {
            SkillLocation::Dir(d) => Some(d),
            SkillLocation::Builtin(_) => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Severity {
    Warning,
    Error,
}

/// A problem found while scanning (shown in Settings → Skills and `genie doctor`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Diagnostic {
    pub path: PathBuf,
    pub severity: Severity,
    pub message: String,
}

/// One directory searched for skills.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SkillRoot {
    pub path: PathBuf,
    pub source: SkillSource,
}

/// The roots of one workspace, lowest precedence first.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct SkillRoots {
    pub builtin: bool,
    pub roots: Vec<SkillRoot>,
}

impl SkillRoots {
    /// Standard layout: built-ins, `<genie_home>/skills`, and — only when the workspace is
    /// trusted (06 §14) — `<workspace>/.genie/skills` and `<workspace>/.agents/skills`.
    /// `.claude/skills` is never scanned (importer only). Later roots win on id clashes.
    pub fn standard(genie_home: &Path, workspace: Option<&Path>, trusted: bool) -> Self {
        let mut roots = vec![SkillRoot {
            path: genie_home.join("skills"),
            source: SkillSource::User,
        }];
        if let (Some(ws), true) = (workspace, trusted) {
            for rel in [".agents/skills", ".genie/skills"] {
                roots.push(SkillRoot {
                    path: ws.join(rel),
                    source: SkillSource::Project,
                });
            }
        }
        SkillRoots {
            builtin: true,
            roots,
        }
    }
}

/// Compiled-in skills: `(id, SKILL.md)`.
pub const BUILTIN: &[(&str, &str)] = &[
    (
        "create-skill",
        include_str!("../builtin/create-skill/SKILL.md"),
    ),
    (
        "review-document",
        include_str!("../builtin/review-document/SKILL.md"),
    ),
    (
        "summarise-meeting",
        include_str!("../builtin/summarise-meeting/SKILL.md"),
    ),
    ("write-spec", include_str!("../builtin/write-spec/SKILL.md")),
];

pub fn builtin_skills() -> Vec<Skill> {
    BUILTIN
        .iter()
        .filter_map(|(id, text)| {
            let (manifest, _) = manifest::parse_skill_md(text).ok()?;
            Some(Skill {
                id: (*id).to_string(),
                manifest,
                source: SkillSource::Builtin,
                location: SkillLocation::Builtin(text),
                install: None,
            })
        })
        .collect()
}

fn hidden(name: &str) -> bool {
    name.starts_with('.') || name == "node_modules" || name == "__MACOSX"
}

/// Skill folders under `root` (stops descending at a `SKILL.md`), sorted by path.
fn skill_dirs(root: &Path) -> Vec<PathBuf> {
    let mut out = vec![];
    let mut stack = vec![(root.to_path_buf(), 0usize)];
    while let Some((dir, depth)) = stack.pop() {
        let Ok(rd) = fs::read_dir(&dir) else {
            continue;
        };
        for e in rd.flatten() {
            let name = e.file_name().to_string_lossy().into_owned();
            let p = e.path();
            if hidden(&name) || !p.is_dir() {
                continue;
            }
            if p.join(SKILL_FILE).is_file() {
                out.push(p);
            } else if depth + 1 < MAX_DEPTH {
                stack.push((p, depth + 1));
            }
        }
    }
    out.sort();
    out
}

/// Reads one skill folder. `Err` carries diagnostics; warnings come back with `Ok`.
pub fn read_skill_dir(
    dir: &Path,
    source: SkillSource,
) -> Result<(Skill, Vec<Diagnostic>), Diagnostic> {
    let diag = |severity, message: String| Diagnostic {
        path: dir.to_path_buf(),
        severity,
        message,
    };
    let id = dir
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    if !manifest::is_valid_id(&id) {
        return Err(diag(
            Severity::Error,
            format!("folder name `{id}` is not a valid skill name (a-z, 0-9, -)"),
        ));
    }
    let file = dir.join(SKILL_FILE);
    let len = fs::metadata(&file).map(|m| m.len()).unwrap_or(0);
    if len > MAX_SKILL_MD {
        return Err(diag(
            Severity::Error,
            format!("SKILL.md is larger than {} KiB", MAX_SKILL_MD / 1024),
        ));
    }
    let text = fs::read_to_string(&file)
        .map_err(|e| diag(Severity::Error, format!("cannot read SKILL.md: {e}")))?;
    let (manifest, _) = manifest::parse_skill_md(&text).map_err(|p| {
        diag(
            Severity::Error,
            format!("invalid SKILL.md: {}", p.join("; ")),
        )
    })?;
    let mut warnings = vec![];
    if manifest.name != id {
        warnings.push(diag(
            Severity::Warning,
            format!(
                "`name: {}` differs from the folder name; the id is `{id}`",
                manifest.name
            ),
        ));
    }
    let install = fs::read(dir.join(MARKER_FILE))
        .ok()
        .and_then(|b| serde_json::from_slice::<InstallMarker>(&b).ok());
    let source = match (&install, source) {
        (Some(m), SkillSource::User) => m.source,
        _ => source,
    };
    Ok((
        Skill {
            id,
            manifest,
            source,
            location: SkillLocation::Dir(dir.to_path_buf()),
            install,
        },
        warnings,
    ))
}

/// Scans one root.
pub fn scan_root(root: &SkillRoot) -> (Vec<Skill>, Vec<Diagnostic>) {
    let mut skills = vec![];
    let mut diags = vec![];
    for dir in skill_dirs(&root.path) {
        match read_skill_dir(&dir, root.source) {
            Ok((s, w)) => {
                diags.extend(w);
                // Same folder name at different depths: the first (sorted) path wins.
                if skills.iter().any(|x: &Skill| x.id == s.id) {
                    diags.push(Diagnostic {
                        path: dir.clone(),
                        severity: Severity::Warning,
                        message: format!("duplicate skill id `{}`; this one is ignored", s.id),
                    });
                } else {
                    skills.push(s);
                }
            }
            Err(d) => diags.push(d),
        }
    }
    (skills, diags)
}

/// Hash of everything the scan depends on: folder listings and `SKILL.md`/marker sizes and
/// modification times. Cheap enough to poll; a change triggers a rescan.
pub fn fingerprint(roots: &SkillRoots) -> String {
    let mut h = Sha256::new();
    h.update([u8::from(roots.builtin)]);
    for root in &roots.roots {
        h.update(root.path.to_string_lossy().as_bytes());
        h.update([0]);
        stamp(&mut h, &root.path);
        let mut stack = vec![(root.path.clone(), 0usize)];
        while let Some((dir, depth)) = stack.pop() {
            let Ok(rd) = fs::read_dir(&dir) else {
                continue;
            };
            let mut entries: Vec<_> = rd.flatten().map(|e| e.path()).collect();
            entries.sort();
            for p in entries {
                let name = p
                    .file_name()
                    .map(|n| n.to_string_lossy().into_owned())
                    .unwrap_or_default();
                if hidden(&name) || !p.is_dir() {
                    continue;
                }
                h.update(p.to_string_lossy().as_bytes());
                stamp(&mut h, &p);
                let skill_md = p.join(SKILL_FILE);
                if skill_md.is_file() {
                    stamp(&mut h, &skill_md);
                    stamp(&mut h, &p.join(MARKER_FILE));
                } else if depth + 1 < MAX_DEPTH {
                    stack.push((p, depth + 1));
                }
            }
        }
    }
    hex::encode(h.finalize())
}

fn stamp(h: &mut Sha256, p: &Path) {
    match fs::metadata(p) {
        Ok(m) => {
            h.update(m.len().to_le_bytes());
            let t = m
                .modified()
                .ok()
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|d| d.as_nanos())
                .unwrap_or(0);
            h.update(t.to_le_bytes());
        }
        Err(_) => h.update([0xff]),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builtins_are_valid() {
        let b = builtin_skills();
        assert_eq!(b.len(), BUILTIN.len());
        for s in &b {
            assert_eq!(s.id, s.manifest.name);
        }
    }
}
