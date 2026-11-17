//! Managed policy (08 §5, D38): delivery sources, signature check, merge, effective view.
//!
//! - **Sources** are ports ([`PolicySource`]): the system file ([`FilePolicySource`], refuses
//!   a user-writable file), MDM (macOS profile / Windows `HKLM\Software\Policies\Genie` —
//!   OS readers implement the trait; [`StaticPolicySource`] serves tests and adapters) and
//!   the signed remote URL ([`PolicyFetcher`]), whose URL and Ed25519 keys may only come
//!   from the file or MDM.
//! - **Merge** ([`merge`]): most-restrictive-wins for allow/deny lists, disabling booleans
//!   and ordered levels; other scalars by remote > MDM > file.
//! - **Last good copy**: a source that fails after a valid load keeps its previous values
//!   and raises a Problem; the remote copy is cached on disk and re-verified on load.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use base64::Engine;
use ed25519_dalek::{Signature, Verifier, VerifyingKey};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use genie_protocol::api::config::PolicyEffectiveResult;
use genie_protocol::common::{Diagnostic, Mode, Severity};

use crate::fsutil::{read_optional, write_private};
use crate::settings::ModeSetting;

/// Flat policy: dotted key → value (`permissions.deny`, `org.name`, …).
pub type PolicyValues = BTreeMap<String, Value>;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum PolicySourceKind {
    File,
    Mdm,
    Remote,
}

impl PolicySourceKind {
    pub fn as_str(self) -> &'static str {
        match self {
            PolicySourceKind::File => "file",
            PolicySourceKind::Mdm => "mdm",
            PolicySourceKind::Remote => "remote",
        }
    }
}

/// Keys that configure delivery itself; honoured only from the file or MDM.
pub const KEY_POLICY_URL: &str = "policyUrl";
pub const KEY_POLICY_KEYS: &str = "policyKeys";

#[derive(Debug, thiserror::Error)]
pub enum PolicyError {
    #[error("{0} is writable by non-admin users; ignored")]
    UserWritable(String),
    #[error("{0}: {1}")]
    Parse(String, String),
    #[error("{0}: {1}")]
    Io(String, String),
    #[error("remote policy signature: {0}")]
    Signature(&'static str),
    #[error("remote policy fetch: {0}")]
    Fetch(String),
}

/// One delivery source. `Ok(None)` = not configured / absent.
pub trait PolicySource: Send + Sync {
    fn kind(&self) -> PolicySourceKind;
    fn load(&self) -> Result<Option<PolicyValues>, PolicyError>;
}

/// The platform's system policy path (08 §5.1).
pub fn system_policy_path() -> PathBuf {
    if cfg!(windows) {
        PathBuf::from(r"C:\ProgramData\Genie\policy.json")
    } else if cfg!(target_os = "macos") {
        PathBuf::from("/Library/Application Support/Genie/policy.json")
    } else {
        PathBuf::from("/etc/genie/policy.json")
    }
}

/// `policy.json` in the system location. Must be admin-writable only.
#[derive(Debug, Clone)]
pub struct FilePolicySource {
    pub path: PathBuf,
    /// Enforce the owner/mode check (off only in tests). On Windows the ACL check is
    /// not implemented yet (README gaps); the ProgramData default ACL applies.
    pub require_admin: bool,
}

impl FilePolicySource {
    pub fn system() -> FilePolicySource {
        FilePolicySource {
            path: system_policy_path(),
            require_admin: true,
        }
    }
}

/// Unix: owned by root and not group/world-writable.
pub fn admin_only(path: &Path) -> std::io::Result<bool> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let m = std::fs::metadata(path)?;
        Ok(m.uid() == 0 && m.mode() & 0o022 == 0)
    }
    #[cfg(not(unix))]
    {
        std::fs::metadata(path).map(|_| true)
    }
}

impl PolicySource for FilePolicySource {
    fn kind(&self) -> PolicySourceKind {
        PolicySourceKind::File
    }

    fn load(&self) -> Result<Option<PolicyValues>, PolicyError> {
        let name = self.path.display().to_string();
        let Some(bytes) =
            read_optional(&self.path).map_err(|e| PolicyError::Io(name.clone(), e.to_string()))?
        else {
            return Ok(None);
        };
        if self.require_admin
            && !admin_only(&self.path).map_err(|e| PolicyError::Io(name.clone(), e.to_string()))?
        {
            return Err(PolicyError::UserWritable(name));
        }
        parse_policy(&bytes)
            .map(Some)
            .map_err(|e| PolicyError::Parse(name, e))
    }
}

/// Fixed values: tests, and the output of OS-specific MDM readers.
#[derive(Debug, Clone)]
pub struct StaticPolicySource {
    pub kind: PolicySourceKind,
    pub values: Option<PolicyValues>,
}

impl PolicySource for StaticPolicySource {
    fn kind(&self) -> PolicySourceKind {
        self.kind
    }
    fn load(&self) -> Result<Option<PolicyValues>, PolicyError> {
        Ok(self.values.clone())
    }
}

/// A signed remote policy: the exact body bytes and a detached base64 Ed25519 signature.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SignedPolicy {
    pub body: String,
    pub signature: String,
}

/// HTTP port for the remote policy URL (the daemon implements it with its HTTP client).
pub trait PolicyFetcher: Send + Sync {
    fn fetch(&self, url: &str) -> Result<SignedPolicy, String>;
}

/// Parse policy JSON (nested or dotted keys) into flat dotted keys. Arrays and scalars are
/// leaves.
pub fn parse_policy(bytes: &[u8]) -> Result<PolicyValues, String> {
    match serde_json::from_slice::<Value>(bytes) {
        Ok(Value::Object(m)) => {
            let mut out = BTreeMap::new();
            flatten(&m, "", &mut out);
            Ok(out)
        }
        Ok(_) => Err("expected a JSON object".into()),
        Err(e) => Err(e.to_string()),
    }
}

fn flatten(m: &Map<String, Value>, prefix: &str, out: &mut PolicyValues) {
    for (k, v) in m {
        let key = if prefix.is_empty() {
            k.clone()
        } else {
            format!("{prefix}.{k}")
        };
        match v {
            Value::Object(sub) if !sub.is_empty() => flatten(sub, &key, out),
            _ => {
                out.insert(key, v.clone());
            }
        }
    }
}

pub fn parse_key(s: &str) -> Option<VerifyingKey> {
    let s = s.trim();
    let bytes = if s.len() == 64 && s.chars().all(|c| c.is_ascii_hexdigit()) {
        hex::decode(s).ok()?
    } else {
        base64::engine::general_purpose::STANDARD.decode(s).ok()?
    };
    VerifyingKey::from_bytes(&bytes.try_into().ok()?).ok()
}

/// Verify a signed policy against the pinned keys (any one may verify, for rotation).
pub fn verify(keys: &[VerifyingKey], p: &SignedPolicy) -> Result<PolicyValues, PolicyError> {
    if keys.is_empty() {
        return Err(PolicyError::Signature("no pinned keys in file/MDM"));
    }
    let raw = base64::engine::general_purpose::STANDARD
        .decode(p.signature.trim())
        .map_err(|_| PolicyError::Signature("malformed signature"))?;
    let sig =
        Signature::from_slice(&raw).map_err(|_| PolicyError::Signature("malformed signature"))?;
    if !keys
        .iter()
        .any(|k| k.verify(p.body.as_bytes(), &sig).is_ok())
    {
        return Err(PolicyError::Signature("no pinned key verifies it"));
    }
    let mut v = parse_policy(p.body.as_bytes())
        .map_err(|e| PolicyError::Parse("remote policy".into(), e))?;
    // Delivery settings can't be changed remotely.
    v.remove(KEY_POLICY_URL);
    v.remove(KEY_POLICY_KEYS);
    Ok(v)
}

/// How a key combines across sources (08 §5.1).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MergeRule {
    /// Allow-list: intersection.
    AllowList,
    /// Deny/ask list: union.
    DenyList,
    /// `*.enabled`, `*.allowed` booleans: `false` wins.
    Enable,
    /// `disable*`, `*Only`, `encryption.required`: `true` wins.
    Disable,
    /// Lowest number wins.
    Min,
    /// Ordered levels, earliest (most restrictive) wins.
    Level(&'static [&'static str]),
    /// remote > MDM > file.
    Scalar,
}

const ALLOW_LISTS: &[&str] = &[
    "providers.allowed",
    "endpoints.allowed",
    "models.allowed",
    "auth.methods",
    "mcp.allowed",
    "skills.allowed",
    "webSearch.allowedDomains",
    "webFetch.domains",
    "browser.siteAllowlist",
    "trust.allowedRoots",
];
const DENY_LISTS: &[&str] = &[
    "models.blocked",
    "permissions.deny",
    "permissions.ask",
    "webSearch.blockedDomains",
    "browser.siteBlocklist",
];
const DISABLE: &[&str] = &[
    "disableBypassMode",
    "mcp.directoryOnly",
    "mcp.disableLocal",
    "skills.directoryOnly",
    "skills.disableUserUpload",
    "hooks.managedOnly",
    "encryption.required",
];
const ENABLE: &[&str] = &[
    "incognito.allowed",
    "export.allowed",
    "skills.scriptsAllowed",
];
const MIN: &[&str] = &["retention.maxDays", "subagents.maxDepth"];

pub fn merge_rule(key: &str) -> MergeRule {
    if ALLOW_LISTS.contains(&key) {
        MergeRule::AllowList
    } else if DENY_LISTS.contains(&key) {
        MergeRule::DenyList
    } else if DISABLE.contains(&key) {
        MergeRule::Disable
    } else if ENABLE.contains(&key) || key.ends_with(".enabled") {
        MergeRule::Enable
    } else if MIN.contains(&key) {
        MergeRule::Min
    } else {
        match key {
            "telemetry.level" => MergeRule::Level(&["off", "crash-only", "usage"]),
            "mcp.sampling" => MergeRule::Level(&["off", "ask"]),
            "trust.mode" => MergeRule::Level(&["never", "prompt"]),
            "browser.profile" => MergeRule::Level(&["ephemeral", "persistent"]),
            "defaultMode" => MergeRule::Level(&["plan", "default", "accept_edits", "bypass"]),
            _ => MergeRule::Scalar,
        }
    }
}

fn level_index(levels: &[&str], v: &Value) -> Option<usize> {
    let s = v.as_str()?;
    // Accept Claude's mode spellings too.
    let s = match ModeSetting::parse(s) {
        Some(ModeSetting::AcceptEdits) => "accept_edits",
        Some(ModeSetting::Bypass) => "bypass",
        _ => s,
    };
    levels.iter().position(|l| *l == s)
}

/// Combine sources (any order) into the effective policy.
pub fn merge(sources: &[(PolicySourceKind, &PolicyValues)]) -> EffectivePolicy {
    let mut sorted: Vec<_> = sources.to_vec();
    sorted.sort_by_key(|(k, _)| *k); // file < mdm < remote
    let keys: BTreeSet<&String> = sorted.iter().flat_map(|(_, v)| v.keys()).collect();
    let mut values = BTreeMap::new();
    let mut src = BTreeMap::new();
    for key in keys {
        let present: Vec<(PolicySourceKind, &Value)> = sorted
            .iter()
            .filter_map(|(k, v)| v.get(key).map(|x| (*k, x)))
            .collect();
        let joined = || {
            present
                .iter()
                .map(|(k, _)| k.as_str())
                .collect::<Vec<_>>()
                .join("+")
        };
        let (value, from) = match merge_rule(key) {
            MergeRule::AllowList => {
                let mut acc: Option<Vec<Value>> = None;
                for (_, v) in &present {
                    let list = v.as_array().cloned().unwrap_or_default();
                    acc = Some(match acc {
                        None => list,
                        Some(a) => a.into_iter().filter(|x| list.contains(x)).collect(),
                    });
                }
                (Value::Array(acc.unwrap_or_default()), joined())
            }
            MergeRule::DenyList => {
                let mut acc: Vec<Value> = Vec::new();
                for (_, v) in &present {
                    for x in v.as_array().into_iter().flatten() {
                        if !acc.contains(x) {
                            acc.push(x.clone());
                        }
                    }
                }
                (Value::Array(acc), joined())
            }
            MergeRule::Enable | MergeRule::Disable => {
                let restrictive = matches!(merge_rule(key), MergeRule::Disable);
                match present
                    .iter()
                    .find(|(_, v)| v.as_bool() == Some(restrictive))
                {
                    Some((k, v)) => ((*v).clone(), k.as_str().to_string()),
                    None => scalar(&present),
                }
            }
            MergeRule::Min => match present
                .iter()
                .filter_map(|(k, v)| v.as_f64().map(|n| (n, k, v)))
                .min_by(|a, b| a.0.total_cmp(&b.0))
            {
                Some((_, k, v)) => ((*v).clone(), k.as_str().to_string()),
                None => scalar(&present),
            },
            MergeRule::Level(levels) => match present
                .iter()
                .filter_map(|(k, v)| level_index(levels, v).map(|i| (i, k, v)))
                .min_by_key(|x| x.0)
            {
                Some((_, k, v)) => ((*v).clone(), k.as_str().to_string()),
                None => scalar(&present),
            },
            MergeRule::Scalar => scalar(&present),
        };
        values.insert(key.clone(), value);
        src.insert(key.clone(), from);
    }
    EffectivePolicy {
        values,
        sources: src,
    }
}

fn scalar(present: &[(PolicySourceKind, &Value)]) -> (Value, String) {
    present
        .iter()
        .max_by_key(|(k, _)| *k)
        .map(|(k, v)| ((*v).clone(), k.as_str().to_string()))
        .unwrap_or((Value::Null, String::new()))
}

/// The merged policy with the source(s) of each key (Settings → Developer).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct EffectivePolicy {
    values: PolicyValues,
    sources: BTreeMap<String, String>,
}

impl EffectivePolicy {
    pub fn is_empty(&self) -> bool {
        self.values.is_empty()
    }

    pub fn get(&self, key: &str) -> Option<&Value> {
        self.values.get(key)
    }

    pub fn source(&self, key: &str) -> Option<&str> {
        self.sources.get(key).map(String::as_str)
    }

    pub fn bool(&self, key: &str) -> Option<bool> {
        self.get(key).and_then(Value::as_bool)
    }

    pub fn strings(&self, key: &str) -> Vec<String> {
        self.get(key)
            .and_then(Value::as_array)
            .map(|a| {
                a.iter()
                    .filter_map(|v| v.as_str().map(str::to_string))
                    .collect()
            })
            .unwrap_or_default()
    }

    /// `org.name`, shown as "Managed by <org>" / "Blocked by <org> policy".
    pub fn org_name(&self) -> Option<&str> {
        self.get("org.name").and_then(Value::as_str)
    }

    pub fn disable_bypass(&self) -> bool {
        self.bool("disableBypassMode").unwrap_or(false)
    }

    /// The `defaultMode` floor: the most permissive mode sessions may use.
    pub fn mode_floor(&self) -> Option<Mode> {
        self.get("defaultMode")
            .and_then(Value::as_str)
            .and_then(ModeSetting::parse)
            .map(ModeSetting::to_mode)
    }

    /// `audit.includeContent` (shown with a visible notice, 08 §5.3).
    pub fn include_content(&self) -> bool {
        self.bool("audit.includeContent").unwrap_or(false)
    }

    /// The notice the UI must show when content auditing is on.
    pub fn content_audit_notice(&self) -> Option<String> {
        self.include_content().then(|| {
            format!(
                "Content auditing is enabled by {}",
                self.org_name().unwrap_or("your organization")
            )
        })
    }

    /// `policy.effective` (02 §6.4).
    pub fn to_result(&self) -> PolicyEffectiveResult {
        PolicyEffectiveResult {
            values: self.values.clone(),
            source_per_key: self.sources.clone(),
        }
    }
}

/// Owns the sources and the last good copy of each.
pub struct PolicyManager {
    sources: Vec<Box<dyn PolicySource>>,
    fetcher: Option<Arc<dyn PolicyFetcher>>,
    /// Where the last verified remote policy is cached (`<data dir>/policy-remote.json`).
    remote_cache: Option<PathBuf>,
    last_good: BTreeMap<PolicySourceKind, PolicyValues>,
    problems: BTreeMap<PolicySourceKind, Diagnostic>,
}

impl std::fmt::Debug for PolicyManager {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PolicyManager")
            .field("last_good", &self.last_good)
            .field("problems", &self.problems)
            .finish_non_exhaustive()
    }
}

impl PolicyManager {
    pub fn new(sources: Vec<Box<dyn PolicySource>>) -> PolicyManager {
        PolicyManager {
            sources,
            fetcher: None,
            remote_cache: None,
            last_good: BTreeMap::new(),
            problems: BTreeMap::new(),
        }
    }

    pub fn with_remote(mut self, fetcher: Arc<dyn PolicyFetcher>) -> PolicyManager {
        self.fetcher = Some(fetcher);
        self
    }

    /// Where the last verified remote copy is cached (`<data dir>/policy-remote.json`).
    pub fn with_remote_cache(mut self, path: PathBuf) -> PolicyManager {
        self.remote_cache = Some(path);
        self
    }

    fn problem(&mut self, kind: PolicySourceKind, message: String) {
        self.problems.insert(
            kind,
            Diagnostic {
                severity: Severity::Error,
                message,
                source: Some(format!("policy:{}", kind.as_str())),
                key: None,
            },
        );
    }

    /// Re-read the file and MDM sources (at start and on change).
    pub fn refresh_local(&mut self) {
        let mut results = Vec::new();
        for s in &self.sources {
            results.push((s.kind(), s.load()));
        }
        for (kind, r) in results {
            match r {
                Ok(Some(v)) => {
                    self.last_good.insert(kind, v);
                    self.problems.remove(&kind);
                }
                Ok(None) if !self.last_good.contains_key(&kind) => {
                    self.problems.remove(&kind);
                }
                Ok(None) => self.problem(
                    kind,
                    format!(
                        "{} policy disappeared; keeping the last good copy",
                        kind.as_str()
                    ),
                ),
                Err(e) => {
                    let keep = if self.last_good.contains_key(&kind) {
                        "; keeping the last good copy"
                    } else {
                        ""
                    };
                    self.problem(kind, format!("{e}{keep}"));
                }
            }
        }
    }

    fn delivery(&self) -> (Option<String>, Vec<VerifyingKey>) {
        let mut url = None;
        let mut keys = Vec::new();
        for kind in [PolicySourceKind::File, PolicySourceKind::Mdm] {
            if let Some(v) = self.last_good.get(&kind) {
                if let Some(u) = v.get(KEY_POLICY_URL).and_then(Value::as_str) {
                    url = Some(u.to_string());
                }
                for k in v
                    .get(KEY_POLICY_KEYS)
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
                {
                    if let Some(k) = k.as_str().and_then(parse_key) {
                        keys.push(k);
                    }
                }
            }
        }
        (url, keys)
    }

    /// Load the cached remote copy (re-verified against today's pinned keys).
    pub fn load_remote_cache(&mut self) {
        let (_, keys) = self.delivery();
        let Some(path) = &self.remote_cache else {
            return;
        };
        let Ok(Some(bytes)) = read_optional(path) else {
            return;
        };
        match serde_json::from_slice::<SignedPolicy>(&bytes) {
            Ok(sp) => match verify(&keys, &sp) {
                Ok(v) => {
                    self.last_good.insert(PolicySourceKind::Remote, v);
                }
                Err(e) => self.problem(
                    PolicySourceKind::Remote,
                    format!("cached copy rejected: {e}"),
                ),
            },
            Err(e) => self.problem(
                PolicySourceKind::Remote,
                format!("cached copy unreadable: {e}"),
            ),
        }
    }

    /// Fetch the remote policy (at start and ~hourly). On failure the last good copy stays.
    pub fn refresh_remote(&mut self) {
        let (url, keys) = self.delivery();
        let (Some(url), Some(fetcher)) = (url, self.fetcher.clone()) else {
            return;
        };
        let result = fetcher
            .fetch(&url)
            .map_err(PolicyError::Fetch)
            .and_then(|sp| verify(&keys, &sp).map(|v| (sp, v)));
        match result {
            Ok((sp, v)) => {
                self.last_good.insert(PolicySourceKind::Remote, v);
                self.problems.remove(&PolicySourceKind::Remote);
                if let Some(path) = &self.remote_cache
                    && let Ok(bytes) = serde_json::to_vec(&sp)
                    && let Err(e) = write_private(path, &bytes)
                {
                    self.problem(
                        PolicySourceKind::Remote,
                        format!("cannot cache remote policy: {e}"),
                    );
                }
            }
            Err(e) => {
                let keep = if self.last_good.contains_key(&PolicySourceKind::Remote) {
                    "; keeping the last good copy"
                } else {
                    ""
                };
                self.problem(PolicySourceKind::Remote, format!("{e}{keep}"));
            }
        }
    }

    pub fn effective(&self) -> EffectivePolicy {
        let list: Vec<(PolicySourceKind, &PolicyValues)> =
            self.last_good.iter().map(|(k, v)| (*k, v)).collect();
        let mut e = merge(&list);
        e.values.remove(KEY_POLICY_KEYS);
        e.sources.remove(KEY_POLICY_KEYS);
        e
    }

    pub fn problems(&self) -> Vec<Diagnostic> {
        self.problems.values().cloned().collect()
    }
}
