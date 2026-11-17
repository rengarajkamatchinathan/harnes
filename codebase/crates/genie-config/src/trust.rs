//! Workspace trust (06 §14, 08 §4, D43): `~/.genie/trust.json` and definition-hash
//! approvals for project MCP servers, hooks and worktree setup commands.
//!
//! - Keyed by canonical path + project identity (git remote hash / root commit, computed
//!   by the caller): a different repo at the same path is not trusted.
//! - Subfolders inherit the nearest ancestor's decision.
//! - "Trust once" lives in memory only.
//! - Home, `/`, drive roots and system folders can't be trusted as a whole.
//! - Policy: `trust.mode: never` disables prompting; `trust.allowedRoots` trusts every
//!   folder beneath them (also what headless runs accept without `--trust`).

use std::collections::BTreeMap;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use genie_protocol::api::config::{
    ApproveDecision, DefinitionKind, PendingDefinition, TrustDecision, TrustState,
};

use crate::fsutil::{read_optional, sha256_hex, write_private};
use crate::policy::EffectivePolicy;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StoredDecision {
    Trust,
    Deny,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Approval {
    pub kind: DefinitionKind,
    pub name: String,
    pub hash: String,
    /// The approved definition, kept so a later change can be shown as a diff.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub definition: Option<Value>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TrustEntry {
    pub canonical_path: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub identity: Option<String>,
    pub decision: StoredDecision,
    pub decided_at: String,
    #[serde(default)]
    pub approvals: Vec<Approval>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct TrustFile {
    #[serde(default = "one")]
    pub version: u32,
    #[serde(default)]
    pub entries: Vec<TrustEntry>,
}

fn one() -> u32 {
    1
}

/// Policy inputs (08 §4).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TrustPolicy {
    pub never: bool,
    pub allowed_roots: Vec<String>,
}

impl TrustPolicy {
    pub fn from_policy(p: &EffectivePolicy) -> TrustPolicy {
        TrustPolicy {
            never: p.get("trust.mode").and_then(Value::as_str) == Some("never"),
            allowed_roots: p.strings("trust.allowedRoots"),
        }
    }
}

/// A project definition that needs approval (one MCP server, hook or setup command).
#[derive(Debug, Clone, PartialEq)]
pub struct Definition {
    pub kind: DefinitionKind,
    pub name: String,
    pub value: Value,
}

#[derive(Debug, thiserror::Error)]
pub enum TrustError {
    #[error("{0} can't be trusted as a whole; open a project folder inside it")]
    TooBroad(String),
    #[error("workspace trust is disabled by policy")]
    PolicyNever,
    #[error("the folder is not trusted")]
    NotTrusted,
    #[error("unknown trust decision")]
    BadDecision,
    #[error("trust.json: {0}")]
    Io(#[from] std::io::Error),
    #[error("trust.json: {0}")]
    Json(#[from] serde_json::Error),
}

#[derive(Debug, Clone, Default)]
struct OnceEntry {
    identity: Option<String>,
    approvals: Vec<Approval>,
}

#[derive(Debug)]
pub struct TrustStore {
    path: PathBuf,
    file: TrustFile,
    once: BTreeMap<String, OnceEntry>,
    home: Option<String>,
    case_insensitive: bool,
}

/// `/`-separated, no trailing slash, lower-cased when case-insensitive.
fn key(p: &str, ci: bool) -> String {
    let mut s = p.replace('\\', "/");
    while s.len() > 1 && s.ends_with('/') && !s.ends_with(":/") {
        s.pop();
    }
    if ci { s.to_lowercase() } else { s }
}

fn is_under(path: &str, base: &str) -> bool {
    path == base
        || base == "/"
        || (path.starts_with(base) && (base.ends_with('/') || path[base.len()..].starts_with('/')))
}

const SYSTEM_DIRS: &[&str] = &[
    "/bin",
    "/sbin",
    "/usr",
    "/etc",
    "/var",
    "/tmp",
    "/opt",
    "/System",
    "/Library",
    "/Applications",
    "/Users",
    "/home",
    "/private",
    "/Volumes",
    "c:/windows",
    "c:/program files",
    "c:/program files (x86)",
    "c:/programdata",
    "c:/users",
];

/// `/`, drive roots, the home folder and system folders (D43).
pub fn too_broad(path: &str, home: Option<&str>, ci: bool) -> bool {
    let p = key(path, ci);
    if p == "/" || p.is_empty() {
        return true;
    }
    let b = p.as_bytes();
    if b.len() <= 3 && b.len() >= 2 && b[1] == b':' {
        return true;
    }
    if home.is_some_and(|h| key(h, ci) == p) {
        return true;
    }
    SYSTEM_DIRS
        .iter()
        .any(|s| key(s, ci).eq_ignore_ascii_case(&p))
}

/// `sha256:<hex>` of the canonical JSON (sorted keys, no whitespace) of a definition.
pub fn definition_hash(v: &Value) -> String {
    format!("sha256:{}", sha256_hex(canonical(v).as_bytes()))
}

fn canonical(v: &Value) -> String {
    match v {
        Value::Object(m) => {
            let mut keys: Vec<&String> = m.keys().collect();
            keys.sort();
            let parts: Vec<String> = keys
                .iter()
                .map(|k| format!("{}:{}", Value::String((*k).clone()), canonical(&m[*k])))
                .collect();
            format!("{{{}}}", parts.join(","))
        }
        Value::Array(a) => format!(
            "[{}]",
            a.iter().map(canonical).collect::<Vec<_>>().join(",")
        ),
        other => other.to_string(),
    }
}

/// A small line diff of pretty JSON (`-`/`+`/` ` prefixed) for the approval card.
pub fn definition_diff(old: Option<&Value>, new: &Value) -> String {
    let pretty = |v: &Value| serde_json::to_string_pretty(v).unwrap_or_default();
    let a: Vec<String> = old
        .map(pretty)
        .unwrap_or_default()
        .lines()
        .map(str::to_string)
        .collect();
    let b: Vec<String> = pretty(new).lines().map(str::to_string).collect();
    // LCS table.
    let mut t = vec![vec![0usize; b.len() + 1]; a.len() + 1];
    for i in (0..a.len()).rev() {
        for j in (0..b.len()).rev() {
            t[i][j] = if a[i] == b[j] {
                t[i + 1][j + 1] + 1
            } else {
                t[i + 1][j].max(t[i][j + 1])
            };
        }
    }
    let (mut i, mut j, mut out) = (0, 0, Vec::new());
    while i < a.len() || j < b.len() {
        if i < a.len() && j < b.len() && a[i] == b[j] {
            out.push(format!(" {}", a[i]));
            i += 1;
            j += 1;
        } else if j < b.len() && (i == a.len() || t[i][j + 1] >= t[i + 1][j]) {
            out.push(format!("+{}", b[j]));
            j += 1;
        } else {
            out.push(format!("-{}", a[i]));
            i += 1;
        }
    }
    out.join("\n")
}

impl TrustStore {
    /// Open `~/.genie/trust.json` (missing = empty).
    pub fn open(path: PathBuf, home: Option<String>) -> Result<TrustStore, TrustError> {
        let file = match read_optional(&path)? {
            Some(b) => serde_json::from_slice(&b)?,
            None => TrustFile {
                version: 1,
                entries: Vec::new(),
            },
        };
        Ok(TrustStore {
            path,
            file,
            once: BTreeMap::new(),
            home,
            case_insensitive: cfg!(windows),
        })
    }

    pub fn with_case_insensitive(mut self, ci: bool) -> TrustStore {
        self.case_insensitive = ci;
        self
    }

    pub fn entries(&self) -> &[TrustEntry] {
        &self.file.entries
    }

    fn save(&self) -> Result<(), TrustError> {
        let mut text = serde_json::to_string_pretty(&self.file)?;
        text.push('\n');
        write_private(&self.path, text.as_bytes())?;
        Ok(())
    }

    fn k(&self, p: &str) -> String {
        key(p, self.case_insensitive)
    }

    /// Nearest ancestor entry whose identity matches (an entry with no identity matches any).
    fn nearest(&self, path: &str, identity: Option<&str>) -> Option<usize> {
        let p = self.k(path);
        self.file
            .entries
            .iter()
            .enumerate()
            .filter(|(_, e)| is_under(&p, &self.k(&e.canonical_path)))
            .filter(|(_, e)| {
                e.identity.is_none() || identity.is_none() || e.identity.as_deref() == identity
            })
            .max_by_key(|(_, e)| e.canonical_path.len())
            .map(|(i, _)| i)
    }

    fn nearest_once(&self, path: &str, identity: Option<&str>) -> Option<String> {
        let p = self.k(path);
        self.once
            .iter()
            .filter(|(base, e)| {
                is_under(&p, base)
                    && (e.identity.is_none()
                        || identity.is_none()
                        || e.identity.as_deref() == identity)
            })
            .max_by_key(|(base, _)| base.len())
            .map(|(b, _)| b.clone())
    }

    /// `trust.get`.
    pub fn state(&self, path: &str, identity: Option<&str>, policy: &TrustPolicy) -> TrustState {
        let p = self.k(path);
        if policy
            .allowed_roots
            .iter()
            .any(|r| is_under(&p, &self.k(r)))
        {
            return TrustState::Trusted;
        }
        if policy.never {
            return TrustState::Denied;
        }
        let file = self.nearest(path, identity).map(|i| &self.file.entries[i]);
        let once = self.nearest_once(path, identity);
        // The deeper decision wins; a stored deny beats a shallower "once".
        match (file, once) {
            (Some(e), Some(o)) if self.k(&e.canonical_path).len() >= o.len() => stored(e.decision),
            (_, Some(_)) => TrustState::Once,
            (Some(e), None) => stored(e.decision),
            (None, None) => TrustState::Unknown,
        }
    }

    /// Whether the client should ask "Do you trust the files in this folder?".
    pub fn should_prompt(&self, path: &str, identity: Option<&str>, policy: &TrustPolicy) -> bool {
        self.state(path, identity, policy) == TrustState::Unknown
    }

    /// Headless (`genie -p`): project config only with `--trust` or a trusted/allowed root.
    pub fn headless_trusted(
        &self,
        path: &str,
        identity: Option<&str>,
        policy: &TrustPolicy,
        trust_flag: bool,
    ) -> bool {
        if policy.never
            && !policy
                .allowed_roots
                .iter()
                .any(|r| is_under(&self.k(path), &self.k(r)))
        {
            return false;
        }
        trust_flag
            || matches!(
                self.state(path, identity, policy),
                TrustState::Trusted | TrustState::Once
            )
    }

    /// `trust.set`. `now` is an RFC 3339 timestamp.
    pub fn set(
        &mut self,
        path: &str,
        identity: Option<&str>,
        decision: TrustDecision,
        policy: &TrustPolicy,
        now: &str,
    ) -> Result<TrustState, TrustError> {
        let p = self.k(path);
        match decision {
            TrustDecision::Trust | TrustDecision::Once => {
                if policy.never {
                    return Err(TrustError::PolicyNever);
                }
                if too_broad(path, self.home.as_deref(), self.case_insensitive) {
                    return Err(TrustError::TooBroad(path.to_string()));
                }
            }
            TrustDecision::Unknown => return Err(TrustError::BadDecision),
            _ => {}
        }
        match decision {
            TrustDecision::Once => {
                self.once.insert(
                    p,
                    OnceEntry {
                        identity: identity.map(str::to_string),
                        approvals: Vec::new(),
                    },
                );
            }
            TrustDecision::Trust | TrustDecision::Deny => {
                self.once.remove(&p);
                let d = if decision == TrustDecision::Trust {
                    StoredDecision::Trust
                } else {
                    StoredDecision::Deny
                };
                let ci = self.case_insensitive;
                match self
                    .file
                    .entries
                    .iter_mut()
                    .find(|e| key(&e.canonical_path, ci) == p)
                {
                    Some(e) => {
                        if e.decision != d || e.identity.as_deref() != identity {
                            e.approvals.clear();
                        }
                        e.decision = d;
                        e.identity = identity.map(str::to_string);
                        e.decided_at = now.to_string();
                    }
                    None => self.file.entries.push(TrustEntry {
                        canonical_path: path.to_string(),
                        identity: identity.map(str::to_string),
                        decision: d,
                        decided_at: now.to_string(),
                        approvals: Vec::new(),
                    }),
                }
                self.save()?;
            }
            TrustDecision::Revoke => {
                self.once.remove(&p);
                let ci = self.case_insensitive;
                self.file
                    .entries
                    .retain(|e| key(&e.canonical_path, ci) != p);
                self.save()?;
            }
            TrustDecision::Unknown => {}
        }
        Ok(self.state(path, identity, policy))
    }

    fn approvals(&self, path: &str, identity: Option<&str>) -> Result<&[Approval], TrustError> {
        if let Some(o) = self.nearest_once(path, identity) {
            return Ok(&self.once[&o].approvals);
        }
        match self.nearest(path, identity).map(|i| &self.file.entries[i]) {
            Some(e) if e.decision == StoredDecision::Trust => Ok(&e.approvals),
            _ => Err(TrustError::NotTrusted),
        }
    }

    fn approvals_mut(
        &mut self,
        path: &str,
        identity: Option<&str>,
    ) -> Result<(&mut Vec<Approval>, bool), TrustError> {
        if let Some(o) = self.nearest_once(path, identity) {
            return Ok((
                &mut self
                    .once
                    .get_mut(&o)
                    .ok_or(TrustError::NotTrusted)?
                    .approvals,
                false,
            ));
        }
        match self.nearest(path, identity) {
            Some(i) if self.file.entries[i].decision == StoredDecision::Trust => {
                Ok((&mut self.file.entries[i].approvals, true))
            }
            _ => Err(TrustError::NotTrusted),
        }
    }

    /// Whether this exact definition was approved (a changed one is not started).
    pub fn is_approved(&self, path: &str, identity: Option<&str>, def: &Definition) -> bool {
        let h = definition_hash(&def.value);
        self.approvals(path, identity).is_ok_and(|a| {
            a.iter()
                .any(|x| x.kind == def.kind && x.name == def.name && x.hash == h)
        })
    }

    /// `trust.pending_definitions`: new or changed definitions, with a diff.
    pub fn pending(
        &self,
        path: &str,
        identity: Option<&str>,
        current: &[Definition],
    ) -> Result<Vec<PendingDefinition>, TrustError> {
        let approvals = self.approvals(path, identity)?;
        Ok(current
            .iter()
            .filter_map(|d| {
                let hash = definition_hash(&d.value);
                let prev = approvals
                    .iter()
                    .find(|a| a.kind == d.kind && a.name == d.name);
                if prev.is_some_and(|a| a.hash == hash) {
                    return None;
                }
                Some(PendingDefinition {
                    kind: d.kind,
                    name: d.name.clone(),
                    hash,
                    diff: definition_diff(prev.and_then(|a| a.definition.as_ref()), &d.value),
                })
            })
            .collect())
    }

    /// `trust.approve_definition`. The hash must match the current definition, so an
    /// approval can't be granted for something the user didn't see.
    pub fn approve(
        &mut self,
        path: &str,
        identity: Option<&str>,
        def: &Definition,
        hash: &str,
        decision: ApproveDecision,
    ) -> Result<bool, TrustError> {
        if decision != ApproveDecision::Approve || definition_hash(&def.value) != hash {
            return Ok(false);
        }
        let (list, persist) = self.approvals_mut(path, identity)?;
        list.retain(|a| !(a.kind == def.kind && a.name == def.name));
        list.push(Approval {
            kind: def.kind,
            name: def.name.clone(),
            hash: hash.to_string(),
            definition: Some(def.value.clone()),
        });
        if persist {
            self.save()?;
        }
        Ok(true)
    }

    /// Removing an entry from the project drops its approval (D43).
    pub fn prune(
        &mut self,
        path: &str,
        identity: Option<&str>,
        current: &[Definition],
    ) -> Result<usize, TrustError> {
        let (list, persist) = self.approvals_mut(path, identity)?;
        let before = list.len();
        list.retain(|a| current.iter().any(|d| d.kind == a.kind && d.name == a.name));
        let removed = before - list.len();
        if persist && removed > 0 {
            self.save()?;
        }
        Ok(removed)
    }
}

fn stored(d: StoredDecision) -> TrustState {
    match d {
        StoredDecision::Trust => TrustState::Trusted,
        StoredDecision::Deny => TrustState::Denied,
    }
}
