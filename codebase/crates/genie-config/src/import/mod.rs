//! The importer (07 §5, D53): a one-time copy from Claude Code, Claude Desktop and
//! OpenCode, never live compatibility.
//!
//! 1. [`scan`] known locations into [`Item`]s, each with the writes it would make.
//! 2. [`preview`] marks every item *import / skip / conflict* with per-key diagnostics.
//!    Nothing is written.
//! 3. [`apply`] writes the selected items; secrets go to a [`SecretSink`] (the keychain,
//!    D26), never into settings. A report is saved to `<genie home>/imports/<date>.json`;
//!    re-running is idempotent (source path + content hash).
//!
//! Every path comes from [`Roots`], so tests run on fixture directories.

pub mod perms;
mod sources;

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};
use sha2::{Digest, Sha256};

pub use sources::scan;

/// Where everything lives. [`Roots::detect`] for real runs; tests build their own.
#[derive(Debug, Clone)]
pub struct Roots {
    pub home: PathBuf,
    pub project: PathBuf,
    /// `~/.genie` (or `GENIE_HOME`).
    pub genie_home: PathBuf,
    /// Claude Desktop's config directory.
    pub claude_desktop: PathBuf,
    /// `~/.config/opencode`.
    pub opencode: PathBuf,
}

impl Roots {
    pub fn detect(home: PathBuf, project: PathBuf) -> Roots {
        let genie_home = std::env::var_os("GENIE_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| home.join(".genie"));
        let claude_desktop = if cfg!(target_os = "macos") {
            home.join("Library/Application Support/Claude")
        } else if cfg!(windows) {
            std::env::var_os("APPDATA")
                .map(PathBuf::from)
                .unwrap_or_else(|| home.join("AppData/Roaming"))
                .join("Claude")
        } else {
            std::env::var_os("XDG_CONFIG_HOME")
                .map(PathBuf::from)
                .unwrap_or_else(|| home.join(".config"))
                .join("Claude")
        };
        let opencode = std::env::var_os("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| home.join(".config"))
            .join("opencode");
        Roots {
            home,
            project,
            genie_home,
            claude_desktop,
            opencode,
        }
    }

    pub fn user_settings(&self) -> PathBuf {
        self.genie_home.join("settings.json")
    }
    pub fn project_settings(&self) -> PathBuf {
        self.project.join(".genie/settings.json")
    }
    pub fn local_settings(&self) -> PathBuf {
        self.project.join(".genie/settings.local.json")
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Source {
    ClaudeCode,
    ClaudeDesktop,
    Opencode,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Level {
    User,
    Project,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    Settings,
    Mcp,
    Instructions,
    Skill,
    Agent,
    Command,
    Hooks,
    Provider,
    ConnectorSuggestion,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DiagKind {
    Unsupported,
    SemanticDifference,
    SecretDetected,
    NeedsTrust,
    Info,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Diag {
    pub kind: DiagKind,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub key: Option<String>,
    pub message: String,
}

impl Diag {
    pub fn new(kind: DiagKind, key: Option<&str>, message: &str) -> Diag {
        Diag {
            kind,
            key: key.map(str::to_string),
            message: message.to_string(),
        }
    }
}

/// One write an item makes.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum Plan {
    /// Deep-merge `patch` into a JSON settings file (arrays union, existing scalars win).
    MergeSettings { target: PathBuf, patch: Value },
    /// Copy one file; never overwrites a different existing file.
    CopyFile { from: PathBuf, to: PathBuf },
    /// Copy a directory tree; existing files are kept.
    CopyDir { from: PathBuf, to: PathBuf },
    /// Write `content`; when the file exists without `marker`, append after confirmation.
    WriteText {
        to: PathBuf,
        content: String,
        marker: String,
    },
    /// Store a secret in the keychain under `name`.
    Secret {
        name: String,
        #[serde(skip_serializing)]
        value: String,
    },
    /// Nothing written; shown in the report (e.g. connector suggestions).
    Suggest { text: String },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    Import,
    Skip,
    Conflict,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Item {
    /// Stable id: `<source>:<kind>:<path>[#name]`.
    pub id: String,
    pub source: Source,
    pub level: Level,
    pub kind: Kind,
    pub name: String,
    pub src_path: PathBuf,
    pub hash: String,
    pub plans: Vec<Plan>,
    pub status: Status,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    pub diagnostics: Vec<Diag>,
}

impl Item {
    pub(crate) fn new(
        source: Source,
        level: Level,
        kind: Kind,
        name: impl Into<String>,
        src_path: &Path,
        hash: String,
    ) -> Item {
        let name = name.into();
        Item {
            id: format!(
                "{}:{}:{}#{name}",
                serde_plain(&source),
                serde_plain(&kind),
                src_path.display()
            ),
            source,
            level,
            kind,
            name,
            src_path: src_path.to_path_buf(),
            hash,
            plans: vec![],
            status: Status::Import,
            reason: None,
            diagnostics: vec![],
        }
    }
}

fn serde_plain<T: Serialize>(v: &T) -> String {
    serde_json::to_value(v)
        .ok()
        .and_then(|v| v.as_str().map(str::to_string))
        .unwrap_or_default()
}

/// Which items to consider.
#[derive(Debug, Clone, Copy, Default)]
pub struct Filter {
    pub source: Option<Source>,
    pub level: Option<Level>,
}

impl Filter {
    fn keeps(&self, i: &Item) -> bool {
        self.source.is_none_or(|s| s == i.source) && self.level.is_none_or(|l| l == i.level)
    }
}

pub fn hash_bytes(b: &[u8]) -> String {
    hex::encode(Sha256::digest(b))
}

pub fn hash_file(p: &Path) -> String {
    std::fs::read(p).map(|b| hash_bytes(&b)).unwrap_or_default()
}

/// Hash of a directory tree (relative paths + contents, sorted).
pub fn hash_dir(p: &Path) -> String {
    let mut files = vec![];
    walk(p, &mut files);
    files.sort();
    let mut h = Sha256::new();
    for f in files {
        if let Ok(rel) = f.strip_prefix(p) {
            h.update(rel.to_string_lossy().as_bytes());
        }
        h.update(std::fs::read(&f).unwrap_or_default());
    }
    hex::encode(h.finalize())
}

fn walk(p: &Path, out: &mut Vec<PathBuf>) {
    if let Ok(rd) = std::fs::read_dir(p) {
        for e in rd.flatten() {
            let path = e.path();
            if path.is_dir() {
                walk(&path, out);
            } else {
                out.push(path);
            }
        }
    }
}

/// Secret-looking env var / header?
pub fn looks_secret(key: &str, value: &str) -> bool {
    let v = value.trim();
    if v.is_empty() || v.starts_with("${") || v.starts_with("{env:") || v.starts_with("keychain:") {
        return false;
    }
    let k = key.to_uppercase();
    let key_hit = [
        "KEY",
        "TOKEN",
        "SECRET",
        "PASSWORD",
        "PASSWD",
        "CREDENTIAL",
        "AUTH",
    ]
    .iter()
    .any(|w| k.contains(w));
    let value_hit = [
        "sk-",
        "ghp_",
        "gho_",
        "github_pat_",
        "xoxb-",
        "xoxp-",
        "AKIA",
        "AIza",
        "Bearer ",
    ]
    .iter()
    .any(|p| v.starts_with(p))
        || (v.len() >= 32
            && v.chars()
                .all(|c| c.is_ascii_alphanumeric() || "-_".contains(c))
            && v.chars().any(|c| c.is_ascii_digit())
            && v.chars().any(|c| c.is_ascii_alphabetic()));
    key_hit || value_hit
}

/// Keychain reference written into settings in place of a secret value.
pub fn secret_ref(name: &str) -> String {
    format!("keychain:{name}")
}

// ---------------------------------------------------------------- preview

/// What earlier imports recorded (for idempotency).
#[derive(Debug, Default)]
pub struct Prior {
    /// id → hash of applied items.
    applied: BTreeMap<String, String>,
}

impl Prior {
    pub fn load(genie_home: &Path) -> Prior {
        let mut p = Prior::default();
        let Ok(rd) = std::fs::read_dir(genie_home.join("imports")) else {
            return p;
        };
        for e in rd.flatten() {
            let Ok(text) = std::fs::read_to_string(e.path()) else {
                continue;
            };
            let Ok(r) = serde_json::from_str::<Value>(&text) else {
                continue;
            };
            for it in r["items"].as_array().into_iter().flatten() {
                if it["applied"] == true
                    && let (Some(id), Some(h)) = (it["id"].as_str(), it["hash"].as_str())
                {
                    p.applied.insert(id.to_string(), h.to_string());
                }
            }
        }
        p
    }
}

/// Scan + classify. Nothing is written.
pub fn preview(roots: &Roots, filter: Filter) -> Vec<Item> {
    let prior = Prior::load(&roots.genie_home);
    let mut items: Vec<Item> = scan(roots)
        .into_iter()
        .filter(|i| filter.keeps(i))
        .collect();
    for it in &mut items {
        classify(it, &prior);
    }
    items
}

fn classify(it: &mut Item, prior: &Prior) {
    if prior.applied.get(&it.id) == Some(&it.hash) {
        it.status = Status::Skip;
        it.reason = Some("already imported (unchanged since the last import)".into());
        return;
    }
    if it.plans.iter().all(|p| matches!(p, Plan::Suggest { .. })) {
        it.status = Status::Skip;
        it.reason = Some("suggestion only".into());
        return;
    }
    let mut conflicts = vec![];
    let mut all_present = true;
    for p in &it.plans {
        match p {
            Plan::MergeSettings { target, patch } => {
                let existing = read_json(target).unwrap_or(Value::Object(Map::new()));
                let mut c = vec![];
                let changes = merge_preview(&existing, patch, "", &mut c);
                if changes {
                    all_present = false;
                }
                conflicts.extend(c);
            }
            Plan::CopyFile { from, to } => match std::fs::read(to) {
                Ok(b) if hash_bytes(&b) == hash_file(from) => {}
                Ok(_) => conflicts.push(format!("{} exists with different content", to.display())),
                Err(_) => all_present = false,
            },
            Plan::CopyDir { to, .. } => {
                if to.exists() {
                    conflicts.push(format!(
                        "{} already exists (existing files are kept)",
                        to.display()
                    ));
                } else {
                    all_present = false;
                }
            }
            Plan::WriteText { to, marker, .. } => match std::fs::read_to_string(to) {
                Ok(t) if t.contains(marker.as_str()) => {}
                Ok(_) => conflicts.push(format!(
                    "{} exists; the import line will be appended after confirmation",
                    to.display()
                )),
                Err(_) => all_present = false,
            },
            Plan::Secret { .. } => all_present = false,
            Plan::Suggest { .. } => {}
        }
    }
    if !conflicts.is_empty() {
        it.status = Status::Conflict;
        it.reason = Some(conflicts.join("; "));
    } else if all_present {
        it.status = Status::Skip;
        it.reason = Some("already present".into());
    } else {
        it.status = Status::Import;
    }
}

/// Would merging change anything? Records differing scalars as conflicts.
fn merge_preview(existing: &Value, patch: &Value, path: &str, conflicts: &mut Vec<String>) -> bool {
    match (existing, patch) {
        (Value::Object(e), Value::Object(p)) => {
            let mut changed = false;
            for (k, v) in p {
                let sub = if path.is_empty() {
                    k.clone()
                } else {
                    format!("{path}.{k}")
                };
                match e.get(k) {
                    None => changed = true,
                    Some(ev) => changed |= merge_preview(ev, v, &sub, conflicts),
                }
            }
            changed
        }
        (Value::Array(e), Value::Array(p)) => p.iter().any(|x| !e.contains(x)),
        (e, p) if e == p => false,
        _ => {
            conflicts.push(format!(
                "`{path}` already set to a different value (yours is kept)"
            ));
            false
        }
    }
}

/// Deep merge: objects recurse, arrays union, existing scalars win.
pub fn merge(existing: &mut Value, patch: &Value) {
    match (existing, patch) {
        (Value::Object(e), Value::Object(p)) => {
            for (k, v) in p {
                match e.get_mut(k) {
                    Some(ev) => merge(ev, v),
                    None => {
                        e.insert(k.clone(), v.clone());
                    }
                }
            }
        }
        (Value::Array(e), Value::Array(p)) => {
            for x in p {
                if !e.contains(x) {
                    e.push(x.clone());
                }
            }
        }
        _ => {}
    }
}

/// JSON, or JSON with comments / trailing commas (`.jsonc`).
pub fn read_json(p: &Path) -> Option<Value> {
    let text = std::fs::read_to_string(p).ok()?;
    parse_jsonc(&text)
}

pub fn parse_jsonc(text: &str) -> Option<Value> {
    serde_json::from_str(text)
        .ok()
        .or_else(|| serde_json::from_str(&strip_jsonc(text)).ok())
}

/// Remove `//` and `/* */` comments and trailing commas, leaving strings intact.
pub fn strip_jsonc(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let b: Vec<char> = s.chars().collect();
    let mut i = 0;
    let mut in_str = false;
    while i < b.len() {
        let c = b[i];
        if in_str {
            out.push(c);
            if c == '\\' && i + 1 < b.len() {
                out.push(b[i + 1]);
                i += 2;
                continue;
            }
            if c == '"' {
                in_str = false;
            }
            i += 1;
            continue;
        }
        match c {
            '"' => {
                in_str = true;
                out.push(c);
                i += 1;
            }
            '/' if b.get(i + 1) == Some(&'/') => {
                while i < b.len() && b[i] != '\n' {
                    i += 1;
                }
            }
            '/' if b.get(i + 1) == Some(&'*') => {
                i += 2;
                while i + 1 < b.len() && !(b[i] == '*' && b[i + 1] == '/') {
                    i += 1;
                }
                i += 2;
            }
            ',' => {
                let mut j = i + 1;
                while j < b.len() && b[j].is_whitespace() {
                    j += 1;
                }
                if !matches!(b.get(j), Some('}') | Some(']')) {
                    out.push(c);
                }
                i += 1;
            }
            _ => {
                out.push(c);
                i += 1;
            }
        }
    }
    out
}

// ---------------------------------------------------------------- apply

/// Where secrets go (D26). The CLI build has no keychain write path yet; see README.
pub trait SecretSink {
    fn store(&mut self, name: &str, value: &str) -> Result<(), String>;
}

/// No keychain reachable: secrets are reported, never written to disk.
pub struct NoKeychain;

impl SecretSink for NoKeychain {
    fn store(&mut self, _name: &str, _value: &str) -> Result<(), String> {
        Err(
            "the keychain is not reachable from this build; add it with `genie login` \
             or in Settings"
                .into(),
        )
    }
}

#[derive(Debug, Default)]
pub struct MemorySecrets(pub BTreeMap<String, String>);

impl SecretSink for MemorySecrets {
    fn store(&mut self, name: &str, value: &str) -> Result<(), String> {
        self.0.insert(name.to_string(), value.to_string());
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct ReportItem {
    pub id: String,
    pub source: Source,
    pub kind: Kind,
    pub level: Level,
    pub name: String,
    pub src_path: PathBuf,
    pub hash: String,
    pub status: Status,
    pub applied: bool,
    pub written: Vec<String>,
    pub diagnostics: Vec<Diag>,
}

#[derive(Debug, Clone, Serialize)]
pub struct Report {
    pub date: String,
    pub items: Vec<ReportItem>,
    #[serde(skip)]
    pub path: Option<PathBuf>,
}

pub struct ApplyOptions {
    /// Ids to apply; `None` = every item with status import or conflict.
    pub selected: Option<Vec<String>>,
    /// Confirmed appending to an existing GENIE.md.
    pub confirm_append: bool,
    /// `YYYY-MM-DD` for the report name (tests pin it).
    pub date: String,
}

pub fn today() -> String {
    let ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0);
    genie_protocol::rfc3339_ms(ms).chars().take(10).collect()
}

pub fn apply(
    roots: &Roots,
    items: &mut [Item],
    opts: &ApplyOptions,
    secrets: &mut dyn SecretSink,
) -> Result<Report> {
    let mut report = Report {
        date: opts.date.clone(),
        items: vec![],
        path: None,
    };
    for it in items.iter_mut() {
        let selected = match &opts.selected {
            Some(ids) => ids.contains(&it.id),
            None => it.status != Status::Skip,
        };
        let mut written = vec![];
        let mut applied = false;
        if selected && it.status != Status::Skip {
            // Recorded as imported only when nothing was deferred, so a later run retries
            // a declined append or a secret the keychain could not take.
            applied = true;
            for p in &it.plans {
                match apply_plan(p, opts, secrets, &mut it.diagnostics) {
                    Ok(Done::Wrote(w)) => written.push(w),
                    Ok(Done::Nothing) => {}
                    Ok(Done::Deferred) => applied = false,
                    Err(e) => {
                        applied = false;
                        it.diagnostics
                            .push(Diag::new(DiagKind::Info, None, &format!("{e:#}")));
                    }
                }
            }
        }
        report.items.push(ReportItem {
            id: it.id.clone(),
            source: it.source,
            kind: it.kind,
            level: it.level,
            name: it.name.clone(),
            src_path: it.src_path.clone(),
            hash: it.hash.clone(),
            status: it.status.clone(),
            applied,
            written,
            diagnostics: it.diagnostics.clone(),
        });
    }
    let dir = roots.genie_home.join("imports");
    std::fs::create_dir_all(&dir).with_context(|| format!("creating {}", dir.display()))?;
    let mut path = dir.join(format!("{}.json", opts.date));
    let mut n = 2;
    while path.exists() {
        path = dir.join(format!("{}-{n}.json", opts.date));
        n += 1;
    }
    std::fs::write(&path, serde_json::to_vec_pretty(&report)?)?;
    report.path = Some(path);
    Ok(report)
}

enum Done {
    Wrote(String),
    Nothing,
    Deferred,
}

fn apply_plan(
    p: &Plan,
    opts: &ApplyOptions,
    secrets: &mut dyn SecretSink,
    diags: &mut Vec<Diag>,
) -> Result<Done> {
    match p {
        Plan::MergeSettings { target, patch } => {
            let mut v = read_json(target).unwrap_or(Value::Object(Map::new()));
            if !v.is_object() {
                v = json!({});
            }
            merge(&mut v, patch);
            if let Some(parent) = target.parent() {
                std::fs::create_dir_all(parent)?;
            }
            std::fs::write(target, serde_json::to_vec_pretty(&v)?)?;
            Ok(Done::Wrote(target.display().to_string()))
        }
        Plan::CopyFile { from, to } => {
            if to.exists() {
                return Ok(Done::Nothing);
            }
            if let Some(parent) = to.parent() {
                std::fs::create_dir_all(parent)?;
            }
            std::fs::copy(from, to)?;
            Ok(Done::Wrote(to.display().to_string()))
        }
        Plan::CopyDir { from, to } => {
            let mut files = vec![];
            walk(from, &mut files);
            for f in files {
                let Ok(rel) = f.strip_prefix(from) else {
                    continue;
                };
                let dest = to.join(rel);
                if dest.exists() {
                    continue;
                }
                if let Some(parent) = dest.parent() {
                    std::fs::create_dir_all(parent)?;
                }
                std::fs::copy(&f, &dest)?;
            }
            Ok(Done::Wrote(to.display().to_string()))
        }
        Plan::WriteText {
            to,
            content,
            marker,
        } => match std::fs::read_to_string(to) {
            Ok(t) if t.contains(marker.as_str()) => Ok(Done::Nothing),
            Ok(mut t) => {
                if !opts.confirm_append {
                    diags.push(Diag::new(
                        DiagKind::Info,
                        None,
                        &format!("{} exists; not appended (confirm to append)", to.display()),
                    ));
                    return Ok(Done::Deferred);
                }
                if !t.ends_with('\n') {
                    t.push('\n');
                }
                t.push('\n');
                t.push_str(content);
                std::fs::write(to, t)?;
                Ok(Done::Wrote(to.display().to_string()))
            }
            Err(_) => {
                if let Some(parent) = to.parent() {
                    std::fs::create_dir_all(parent)?;
                }
                std::fs::write(to, content)?;
                Ok(Done::Wrote(to.display().to_string()))
            }
        },
        Plan::Secret { name, value } => match secrets.store(name, value) {
            Ok(()) => Ok(Done::Wrote(format!("keychain:{name}"))),
            Err(e) => {
                diags.push(Diag::new(
                    DiagKind::SecretDetected,
                    Some(name),
                    &format!("secret not imported: {e}"),
                ));
                Ok(Done::Deferred)
            }
        },
        Plan::Suggest { .. } => Ok(Done::Nothing),
    }
}

#[cfg(test)]
mod tests;
