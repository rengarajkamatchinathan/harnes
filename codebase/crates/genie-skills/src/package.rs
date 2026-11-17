//! `.zip` / `.skill` package import (06 §9 "Sources", D37): size caps, path-traversal and
//! zip-bomb guards, `SKILL.md` required, frontmatter schema; unpacked atomically into the
//! user skills root.
//!
//! Accepted layouts: `SKILL.md` at the archive root, or one top-level folder containing
//! `SKILL.md`. The skill id is the frontmatter `name`. Hidden entries (`.DS_Store`,
//! `__MACOSX/`, dotfiles) are skipped; symlinks, encrypted entries, absolute paths, `..`,
//! drive letters and backslashes are refused.

use std::collections::BTreeSet;
use std::fs;
use std::io::{self, Cursor, Read};
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

use crate::catalog::safe_relative;
use crate::error::SkillError;
use crate::manifest::{self, MAX_SKILL_MD};
use crate::scan::{InstallMarker, MARKER_FILE, SKILL_FILE, SkillSource};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImportLimits {
    /// Archive size.
    pub max_archive: u64,
    /// Sum of uncompressed sizes (counted while inflating, not trusted from headers).
    pub max_total: u64,
    pub max_file: u64,
    pub max_entries: usize,
    /// Largest uncompressed/compressed ratio allowed for entries over 1 MiB.
    pub max_ratio: u64,
}

impl Default for ImportLimits {
    fn default() -> Self {
        ImportLimits {
            max_archive: 20 * 1024 * 1024,
            max_total: 64 * 1024 * 1024,
            max_file: 16 * 1024 * 1024,
            max_entries: 1000,
            max_ratio: 200,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImportOptions {
    /// Replace an installed skill with the same id (updates).
    pub replace: bool,
    pub source: SkillSource,
    pub limits: ImportLimits,
    /// When set (directory installs), the archive's file list must equal it exactly, so the
    /// "what will run" manifest the user approved is what gets installed.
    pub expected_files: Option<BTreeSet<String>>,
    pub version: Option<String>,
}

impl Default for ImportOptions {
    fn default() -> Self {
        ImportOptions {
            replace: false,
            source: SkillSource::Upload,
            limits: ImportLimits::default(),
            expected_files: None,
            version: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstalledSkill {
    pub id: String,
    pub path: PathBuf,
    /// SHA-256 (hex) of the archive, for the audit event.
    pub sha256: String,
    /// Installed files, relative and `/`-separated.
    pub files: Vec<String>,
    pub replaced: bool,
}

fn bad(msg: impl Into<String>) -> SkillError {
    SkillError::Package(msg.into())
}

fn junk(name: &str) -> bool {
    name.split('/')
        .any(|c| c.starts_with('.') || c == "__MACOSX")
}

/// Everything about an archive that can be checked without writing to disk.
#[derive(Debug)]
pub struct Inspected {
    pub id: String,
    pub manifest: manifest::SkillManifest,
    /// `(index in archive, relative path inside the skill)`.
    entries: Vec<(usize, String)>,
    pub sha256: String,
}

impl Inspected {
    pub fn files(&self) -> Vec<String> {
        self.entries.iter().map(|(_, p)| p.clone()).collect()
    }
}

/// Validates an archive (layout, names, sizes, `SKILL.md`) without extracting it.
pub fn inspect(archive: &[u8], limits: &ImportLimits) -> Result<Inspected, SkillError> {
    if archive.len() as u64 > limits.max_archive {
        return Err(SkillError::TooLarge {
            what: "skill package".into(),
            size: archive.len() as u64,
            limit: limits.max_archive,
        });
    }
    let mut zip = zip::ZipArchive::new(Cursor::new(archive))
        .map_err(|e| bad(format!("not a zip archive ({e})")))?;
    if zip.len() > limits.max_entries {
        return Err(bad(format!("more than {} entries", limits.max_entries)));
    }
    let mut files: Vec<(usize, String)> = vec![];
    let mut declared: u64 = 0;
    for i in 0..zip.len() {
        let f = zip.by_index_raw(i).map_err(|e| bad(e.to_string()))?;
        let name = f.name().to_string();
        if f.encrypted() {
            return Err(bad(format!("`{name}` is encrypted")));
        }
        if f.is_symlink() {
            return Err(bad(format!("`{name}` is a symbolic link")));
        }
        if name.contains('\\') || name.contains(':') || name.contains('\0') || name.starts_with('/')
        {
            return Err(bad(format!("unsafe path `{name}`")));
        }
        if f.enclosed_name().is_none() || name.split('/').any(|c| c == "..") {
            return Err(bad(format!("path `{name}` escapes the skill folder")));
        }
        if f.is_dir() || junk(&name) {
            continue;
        }
        let size = f.size();
        if size > limits.max_file {
            return Err(SkillError::TooLarge {
                what: name,
                size,
                limit: limits.max_file,
            });
        }
        if size > 1024 * 1024 && size / f.compressed_size().max(1) > limits.max_ratio {
            return Err(bad(format!("`{name}` has a suspicious compression ratio")));
        }
        declared = declared.saturating_add(size);
        if declared > limits.max_total {
            return Err(SkillError::TooLarge {
                what: "unpacked skill".into(),
                size: declared,
                limit: limits.max_total,
            });
        }
        files.push((i, name));
    }

    let prefix = if files.iter().any(|(_, n)| n == SKILL_FILE) {
        String::new()
    } else {
        let tops: BTreeSet<&str> = files
            .iter()
            .filter_map(|(_, n)| n.split('/').next())
            .collect();
        match tops.iter().next() {
            Some(top)
                if tops.len() == 1
                    && files
                        .iter()
                        .any(|(_, n)| *n == format!("{top}/{SKILL_FILE}")) =>
            {
                format!("{top}/")
            }
            _ => return Err(bad("SKILL.md is required at the top of the package")),
        }
    };
    let mut entries = vec![];
    for (i, n) in files {
        let rel = n
            .strip_prefix(&prefix)
            .ok_or_else(|| bad(format!("`{n}` is outside the skill folder")))?
            .to_string();
        safe_relative(&rel).map_err(|_| bad(format!("unsafe path `{n}`")))?;
        entries.push((i, rel));
    }

    let idx = entries
        .iter()
        .find(|(_, r)| r == SKILL_FILE)
        .map(|(i, _)| *i)
        .ok_or_else(|| bad("SKILL.md is required"))?;
    let mut text = String::new();
    zip.by_index(idx)
        .map_err(|e| bad(e.to_string()))?
        .take(MAX_SKILL_MD + 1)
        .read_to_string(&mut text)
        .map_err(|e| bad(format!("SKILL.md: {e}")))?;
    if text.len() as u64 > MAX_SKILL_MD {
        return Err(SkillError::TooLarge {
            what: SKILL_FILE.into(),
            size: text.len() as u64,
            limit: MAX_SKILL_MD,
        });
    }
    let (manifest, _) = manifest::parse_skill_md(&text).map_err(SkillError::Invalid)?;
    entries.sort_by(|a, b| a.1.cmp(&b.1));
    Ok(Inspected {
        id: manifest.name.clone(),
        manifest,
        entries,
        sha256: hex::encode(Sha256::digest(archive)),
    })
}

/// Validates and installs an archive into `dest_root/<id>`. `permits(id)` is the policy
/// allowlist check.
pub fn import(
    archive: &[u8],
    dest_root: &Path,
    opts: &ImportOptions,
    permits: &dyn Fn(&str) -> bool,
) -> Result<InstalledSkill, SkillError> {
    let ins = inspect(archive, &opts.limits)?;
    if !permits(&ins.id) {
        return Err(SkillError::Blocked(ins.id));
    }
    let files = ins.files();
    if let Some(expected) = &opts.expected_files {
        let got: BTreeSet<String> = files.iter().cloned().collect();
        if &got != expected {
            return Err(bad(
                "the package's files differ from the directory manifest",
            ));
        }
    }
    let dest = dest_root.join(&ins.id);
    let exists = dest.exists();
    if exists && !opts.replace {
        return Err(SkillError::AlreadyExists(ins.id));
    }

    fs::create_dir_all(dest_root)?;
    let stamp = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let tmp = dest_root.join(format!(".tmp-{}-{}-{stamp}", ins.id, std::process::id()));
    let result = extract(archive, &ins, &tmp, opts).and_then(|()| {
        let marker = InstallMarker {
            source: opts.source,
            sha256: ins.sha256.clone(),
            version: opts.version.clone(),
        };
        fs::write(
            tmp.join(MARKER_FILE),
            serde_json::to_vec_pretty(&marker).map_err(io::Error::other)?,
        )?;
        swap_in(&tmp, &dest)
    });
    if let Err(e) = result {
        let _ = fs::remove_dir_all(&tmp);
        return Err(e);
    }
    Ok(InstalledSkill {
        id: ins.id,
        path: dest,
        sha256: ins.sha256,
        files,
        replaced: exists,
    })
}

fn extract(
    archive: &[u8],
    ins: &Inspected,
    tmp: &Path,
    opts: &ImportOptions,
) -> Result<(), SkillError> {
    let mut zip = zip::ZipArchive::new(Cursor::new(archive)).map_err(|e| bad(e.to_string()))?;
    fs::create_dir_all(tmp)?;
    let mut total: u64 = 0;
    for (i, rel) in &ins.entries {
        let out_path = tmp.join(safe_relative(rel)?);
        if let Some(parent) = out_path.parent() {
            fs::create_dir_all(parent)?;
        }
        let f = zip.by_index(*i).map_err(|e| bad(e.to_string()))?;
        let mut out = fs::File::create(&out_path)?;
        // Count real bytes: header sizes can lie (zip bomb).
        let n = io::copy(&mut f.take(opts.limits.max_file + 1), &mut out)
            .map_err(|e| bad(format!("`{rel}`: {e}")))?;
        if n > opts.limits.max_file {
            return Err(SkillError::TooLarge {
                what: rel.clone(),
                size: n,
                limit: opts.limits.max_file,
            });
        }
        total += n;
        if total > opts.limits.max_total {
            return Err(SkillError::TooLarge {
                what: "unpacked skill".into(),
                size: total,
                limit: opts.limits.max_total,
            });
        }
    }
    Ok(())
}

/// Moves `tmp` to `dest`, replacing an existing folder via a rename aside.
fn swap_in(tmp: &Path, dest: &Path) -> Result<(), SkillError> {
    if dest.exists() {
        let old = tmp.with_extension("old");
        fs::rename(dest, &old)?;
        if let Err(e) = fs::rename(tmp, dest) {
            let _ = fs::rename(&old, dest);
            return Err(e.into());
        }
        let _ = fs::remove_dir_all(&old);
    } else {
        fs::rename(tmp, dest)?;
    }
    Ok(())
}
