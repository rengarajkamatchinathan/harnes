//! Layered settings (D23): default < user < project < local < CLI < policy.
//!
//! - Only the exact paths: `~/.genie/settings.json`, `<root>/.genie/settings.json`,
//!   `<root>/.genie/settings.local.json` (no walk up to `/`).
//! - Project and local layers load only after workspace trust (D43).
//! - Merge: scalars → highest layer wins; objects → deep-merged per key; arrays →
//!   concatenated (lowest first, duplicates folded). Each domain may also fold the layer
//!   list itself ([`ConfigStore::permission_layers`]).
//! - Per-key diagnostics; malformed JSON keeps the last good copy of that layer.
//! - Hot reload: [`ConfigStore::refresh`] re-reads files and reports only layers whose
//!   content changed; the daemon drives it from a debounced watcher on
//!   [`ConfigStore::watched_paths`].

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde_json::{Map, Value};

use genie_protocol::api::config::{ConfigDiagnosticsResult, ConfigGetParams, ConfigGetResult};
use genie_protocol::common::{ConfigScope, ConfigSource, Diagnostic, Severity};

use crate::fsutil::{read_optional, sha256_hex, write_private};
use crate::policy::EffectivePolicy;
use crate::settings::{PermissionsSettings, validate};

/// A settings layer, lowest precedence first.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum LayerKind {
    Default,
    User,
    Project,
    Local,
    /// `--settings <file>` / CLI flags (session only, D23 layer 2).
    Cli,
    Policy,
}

impl LayerKind {
    /// The protocol has no `cli` source; CLI values report as `unknown` (see README gaps).
    pub fn source(self) -> ConfigSource {
        match self {
            LayerKind::Default => ConfigSource::Default,
            LayerKind::User => ConfigSource::User,
            LayerKind::Project => ConfigSource::Project,
            LayerKind::Local => ConfigSource::Local,
            LayerKind::Cli => ConfigSource::Unknown,
            LayerKind::Policy => ConfigSource::Policy,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            LayerKind::Default => "default",
            LayerKind::User => "user",
            LayerKind::Project => "project",
            LayerKind::Local => "local",
            LayerKind::Cli => "cli",
            LayerKind::Policy => "policy",
        }
    }

    fn from_scope(s: ConfigScope) -> Option<LayerKind> {
        match s {
            ConfigScope::User => Some(LayerKind::User),
            ConfigScope::Project => Some(LayerKind::Project),
            ConfigScope::Local => Some(LayerKind::Local),
            ConfigScope::Unknown => None,
        }
    }
}

/// Where the settings files live.
#[derive(Debug, Clone)]
pub struct ConfigPaths {
    /// `~/.genie` (or the channel's dir, e.g. `~/.genie-beta`).
    pub user_dir: PathBuf,
    /// Git root, or the opened folder. `None` for chat.
    pub project_root: Option<PathBuf>,
}

impl ConfigPaths {
    pub fn file(&self, kind: LayerKind) -> Option<PathBuf> {
        match kind {
            LayerKind::User => Some(self.user_dir.join("settings.json")),
            LayerKind::Project => self
                .project_root
                .as_ref()
                .map(|r| r.join(".genie").join("settings.json")),
            LayerKind::Local => self
                .project_root
                .as_ref()
                .map(|r| r.join(".genie").join("settings.local.json")),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Default)]
struct Layer {
    values: Map<String, Value>,
    diagnostics: Vec<Diagnostic>,
    /// Hash of the last bytes read (change suppression, D23 hot reload).
    hash: Option<String>,
}

#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("`{key}` is managed by {org}")]
    Locked { key: String, org: String },
    #[error("scope `{0}` needs a project folder")]
    NoProject(&'static str),
    #[error("the project folder is not trusted")]
    Untrusted,
    #[error("unknown scope")]
    BadScope,
    #[error("invalid `{key}`: {message}")]
    Invalid { key: String, message: String },
    #[error("{0} is not valid JSON; fix or remove it before saving")]
    Malformed(String),
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

/// The daemon's single source of truth for settings (D23).
#[derive(Debug)]
pub struct ConfigStore {
    paths: ConfigPaths,
    trusted: bool,
    layers: BTreeMap<LayerKind, Layer>,
    policy: EffectivePolicy,
}

impl ConfigStore {
    /// Load every layer. `trusted`: the project folder is trusted (D43).
    pub fn load(paths: ConfigPaths, trusted: bool) -> ConfigStore {
        let mut s = ConfigStore {
            paths,
            trusted,
            layers: BTreeMap::new(),
            policy: EffectivePolicy::default(),
        };
        s.refresh();
        s
    }

    pub fn paths(&self) -> &ConfigPaths {
        &self.paths
    }

    /// Trust changed: project/local layers appear or disappear.
    pub fn set_trusted(&mut self, trusted: bool) -> Vec<LayerKind> {
        self.trusted = trusted;
        if !trusted {
            self.layers.remove(&LayerKind::Project);
            self.layers.remove(&LayerKind::Local);
        }
        self.refresh()
    }

    /// The session-only CLI layer (`--settings <file>`), validated like a file.
    pub fn set_cli_layer(&mut self, value: Map<String, Value>) {
        let (values, diagnostics) = validate(&value, "cli");
        self.layers.insert(
            LayerKind::Cli,
            Layer {
                values,
                diagnostics,
                hash: None,
            },
        );
    }

    pub fn set_policy(&mut self, policy: EffectivePolicy) {
        self.policy = policy;
    }

    pub fn policy(&self) -> &EffectivePolicy {
        &self.policy
    }

    /// Files the daemon should watch (including ones that don't exist yet).
    pub fn watched_paths(&self) -> Vec<PathBuf> {
        [LayerKind::User, LayerKind::Project, LayerKind::Local]
            .into_iter()
            .filter(|k| *k == LayerKind::User || self.trusted)
            .filter_map(|k| self.paths.file(k))
            .collect()
    }

    /// Re-read the files. Returns the layers whose content actually changed.
    pub fn refresh(&mut self) -> Vec<LayerKind> {
        let mut changed = Vec::new();
        for kind in [LayerKind::User, LayerKind::Project, LayerKind::Local] {
            if kind != LayerKind::User && !self.trusted {
                continue;
            }
            let Some(path) = self.paths.file(kind) else {
                continue;
            };
            if self.reload_layer(kind, &path) {
                changed.push(kind);
            }
        }
        changed
    }

    fn reload_layer(&mut self, kind: LayerKind, path: &Path) -> bool {
        let src = path.display().to_string();
        let bytes = match read_optional(path) {
            Ok(b) => b,
            Err(e) => {
                let layer = self.layers.entry(kind).or_default();
                layer.diagnostics = vec![problem(
                    Severity::Error,
                    &src,
                    None,
                    format!("cannot read: {e}"),
                )];
                return false;
            }
        };
        let hash = bytes.as_deref().map(sha256_hex);
        let prev = self.layers.get(&kind);
        if prev.is_some_and(|l| l.hash == hash) {
            return false;
        }
        let new = match bytes {
            None => Layer::default(),
            Some(b) => match serde_json::from_slice::<Value>(&b) {
                Ok(Value::Object(m)) => {
                    let (values, diagnostics) = validate(&m, &src);
                    Layer {
                        values,
                        diagnostics,
                        hash: hash.clone(),
                    }
                }
                Ok(_) | Err(_) => {
                    // Malformed JSON keeps the last good copy of this layer (D23).
                    let mut keep = prev.cloned().unwrap_or_default();
                    keep.hash = hash.clone();
                    keep.diagnostics = vec![problem(
                        Severity::Error,
                        &src,
                        None,
                        "malformed JSON (expected an object); keeping the last good copy".into(),
                    )];
                    self.layers.insert(kind, keep);
                    return true;
                }
            },
        };
        let changed = prev.is_none_or(|p| p.values != new.values);
        self.layers.insert(kind, new);
        changed
    }

    /// Merged values below policy, with the highest contributing layer per leaf path.
    fn merged(&self) -> (Value, BTreeMap<String, LayerKind>) {
        let mut out = Value::Object(Map::new());
        let mut sources = BTreeMap::new();
        for (kind, layer) in &self.layers {
            merge_into(
                &mut out,
                &Value::Object(layer.values.clone()),
                *kind,
                "",
                &mut sources,
            );
        }
        // Policy: exact settings paths are locked to the policy value; policy permission
        // rules are added on top (08 §5.1).
        for key in self.locked() {
            if let Some(v) = self.policy.get(&key) {
                set_path(&mut out, &key, v.clone());
                sources.retain(|k, _| !(k == &key || k.starts_with(&format!("{key}."))));
                sources.insert(key, LayerKind::Policy);
            }
        }
        for class in ["deny", "ask"] {
            let pkey = format!("permissions.{class}");
            if let Some(Value::Array(rules)) = self.policy.get(&pkey) {
                let mut base = get_path(&out, &pkey)
                    .cloned()
                    .unwrap_or(Value::Array(vec![]));
                merge_into(
                    &mut base,
                    &Value::Array(rules.clone()),
                    LayerKind::Policy,
                    &pkey,
                    &mut sources,
                );
                set_path(&mut out, &pkey, base);
            }
        }
        (out, sources)
    }

    /// Settings keys locked by managed policy (exact settings paths set by the policy).
    pub fn locked(&self) -> Vec<String> {
        crate::settings::UI_META
            .iter()
            .map(|m| m.key)
            .filter(|k| !k.starts_with("permissions."))
            .filter(|k| self.policy.get(k).is_some())
            .map(str::to_string)
            .collect()
    }

    /// One merged value by dotted key.
    pub fn value(&self, key: &str) -> Option<Value> {
        get_path(&self.merged().0, key).cloned()
    }

    /// `config.get` (02 §6.4). `scope` restricts the view to one file layer.
    pub fn get(&self, params: &ConfigGetParams) -> ConfigGetResult {
        let (merged, leaf_sources) = match params.scope.and_then(LayerKind::from_scope) {
            Some(kind) => {
                let mut src = BTreeMap::new();
                let mut v = Value::Object(Map::new());
                if let Some(l) = self.layers.get(&kind) {
                    merge_into(&mut v, &Value::Object(l.values.clone()), kind, "", &mut src);
                }
                (v, src)
            }
            None => self.merged(),
        };
        let keys: Vec<String> = match &params.keys {
            Some(k) => k.clone(),
            None => merged
                .as_object()
                .map(|m| m.keys().cloned().collect())
                .unwrap_or_default(),
        };
        let mut values = BTreeMap::new();
        let mut sources = BTreeMap::new();
        for k in keys {
            let Some(v) = get_path(&merged, &k) else {
                continue;
            };
            values.insert(k.clone(), v.clone());
            let src = leaf_sources
                .iter()
                .filter(|(p, _)| {
                    *p == &k || p.starts_with(&format!("{k}.")) || k.starts_with(&format!("{p}."))
                })
                .map(|(_, s)| *s)
                .max()
                .unwrap_or(LayerKind::Default);
            sources.insert(k, src.source());
        }
        ConfigGetResult {
            values,
            sources,
            locked: self.locked(),
        }
    }

    /// Problems for Settings → Problems and `genie config doctor`.
    pub fn diagnostics(&self) -> ConfigDiagnosticsResult {
        let mut problems: Vec<Diagnostic> = self
            .layers
            .values()
            .flat_map(|l| l.diagnostics.iter().cloned())
            .collect();
        if !self.trusted
            && let Some(root) = &self.paths.project_root
        {
            let skipped: Vec<_> = [LayerKind::Project, LayerKind::Local]
                .into_iter()
                .filter_map(|k| self.paths.file(k))
                .filter(|p| p.exists())
                .collect();
            if !skipped.is_empty() {
                problems.push(problem(
                    Severity::Info,
                    &root.display().to_string(),
                    None,
                    "project settings are not loaded until the folder is trusted".into(),
                ));
            }
        }
        ConfigDiagnosticsResult { problems }
    }

    /// The `permissions` block of each loaded layer, lowest first, for the permission
    /// evaluator to fold with its own deny → ask → allow semantics (t08). Policy rules are
    /// not included: read them from [`EffectivePolicy`].
    pub fn permission_layers(&self) -> Vec<(LayerKind, PermissionsSettings)> {
        self.layers
            .iter()
            .filter_map(|(k, l)| {
                let v = l.values.get("permissions")?;
                serde_json::from_value(v.clone()).ok().map(|p| (*k, p))
            })
            .collect()
    }

    /// `config.set` (02 §6.4): write one dotted key to a file scope.
    pub fn set(&mut self, key: &str, value: Value, scope: ConfigScope) -> Result<(), ConfigError> {
        if let Some(l) = self.locked().into_iter().find(|l| {
            key == l || key.starts_with(&format!("{l}.")) || l.starts_with(&format!("{key}."))
        }) {
            return Err(ConfigError::Locked {
                key: l,
                org: self
                    .policy
                    .org_name()
                    .unwrap_or("your organization")
                    .to_string(),
            });
        }
        self.edit_file(scope, |obj| {
            set_path(obj, key, value);
            Ok(())
        })
    }

    /// Persist a saved "always" (06 §5.4) or any rule edit: append `rule` to
    /// `permissions.<class>` in `scope`. Returns false if it was already there.
    pub fn add_permission_rule(
        &mut self,
        scope: ConfigScope,
        class: &str,
        rule: &str,
    ) -> Result<bool, ConfigError> {
        let key = format!("permissions.{class}");
        let mut added = false;
        self.edit_file(scope, |obj| {
            let mut list = get_path(obj, &key).cloned().unwrap_or(Value::Array(vec![]));
            let Value::Array(a) = &mut list else {
                return Err(ConfigError::Invalid {
                    key: key.clone(),
                    message: "not a list".into(),
                });
            };
            if !a.iter().any(|v| v.as_str() == Some(rule)) {
                a.push(Value::String(rule.to_string()));
                added = true;
            }
            set_path(obj, &key, list);
            Ok(())
        })?;
        Ok(added)
    }

    /// Revoke a rule (Settings → Permissions).
    pub fn remove_permission_rule(
        &mut self,
        scope: ConfigScope,
        class: &str,
        rule: &str,
    ) -> Result<bool, ConfigError> {
        let key = format!("permissions.{class}");
        let mut removed = false;
        self.edit_file(scope, |obj| {
            if let Some(Value::Array(a)) = get_path_mut(obj, &key) {
                let before = a.len();
                a.retain(|v| v.as_str() != Some(rule));
                removed = a.len() != before;
            }
            Ok(())
        })?;
        Ok(removed)
    }

    fn edit_file(
        &mut self,
        scope: ConfigScope,
        f: impl FnOnce(&mut Value) -> Result<(), ConfigError>,
    ) -> Result<(), ConfigError> {
        let kind = LayerKind::from_scope(scope).ok_or(ConfigError::BadScope)?;
        let path = self
            .paths
            .file(kind)
            .ok_or(ConfigError::NoProject(kind.name()))?;
        if kind != LayerKind::User && !self.trusted {
            return Err(ConfigError::Untrusted);
        }
        let mut doc = match read_optional(&path)? {
            None => Value::Object(Map::new()),
            Some(b) => match serde_json::from_slice::<Value>(&b) {
                Ok(v @ Value::Object(_)) => v,
                _ => return Err(ConfigError::Malformed(path.display().to_string())),
            },
        };
        f(&mut doc)?;
        let obj = doc.as_object().cloned().unwrap_or_default();
        let (_, diags) = validate(&obj, &path.display().to_string());
        if let Some(d) = diags
            .into_iter()
            .find(|d| d.severity == Severity::Error || d.severity == Severity::Warning)
        {
            // Pre-existing problems in other keys are tolerated only if they were there before.
            let had = self
                .layers
                .get(&kind)
                .is_some_and(|l| l.diagnostics.iter().any(|x| x.key == d.key));
            if !had {
                return Err(ConfigError::Invalid {
                    key: d.key.unwrap_or_default(),
                    message: d.message,
                });
            }
        }
        let mut text = serde_json::to_string_pretty(&doc).map_err(|e| ConfigError::Invalid {
            key: String::new(),
            message: e.to_string(),
        })?;
        text.push('\n');
        write_private(&path, text.as_bytes())?;
        self.reload_layer(kind, &path);
        Ok(())
    }
}

fn problem(severity: Severity, source: &str, key: Option<&str>, message: String) -> Diagnostic {
    Diagnostic {
        severity,
        message,
        source: Some(source.to_string()),
        key: key.map(str::to_string),
    }
}

/// Deep merge `src` (from layer `kind`) into `dst`, recording leaf sources.
fn merge_into(
    dst: &mut Value,
    src: &Value,
    kind: LayerKind,
    prefix: &str,
    sources: &mut BTreeMap<String, LayerKind>,
) {
    match (dst, src) {
        (Value::Object(d), Value::Object(s)) => {
            for (k, v) in s {
                let path = if prefix.is_empty() {
                    k.clone()
                } else {
                    format!("{prefix}.{k}")
                };
                match d.get_mut(k) {
                    Some(existing) if existing.is_object() && v.is_object() => {
                        merge_into(existing, v, kind, &path, sources)
                    }
                    Some(existing) if existing.is_array() && v.is_array() => {
                        merge_into(existing, v, kind, &path, sources)
                    }
                    _ => {
                        d.insert(k.clone(), v.clone());
                        sources.retain(|p, _| !p.starts_with(&format!("{path}.")));
                        record(v, kind, &path, sources);
                    }
                }
            }
        }
        (Value::Array(d), Value::Array(s)) => {
            for v in s {
                if !d.contains(v) {
                    d.push(v.clone());
                }
            }
            sources.insert(prefix.to_string(), kind);
        }
        (d, s) => {
            *d = s.clone();
            sources.insert(prefix.to_string(), kind);
        }
    }
}

fn record(v: &Value, kind: LayerKind, path: &str, sources: &mut BTreeMap<String, LayerKind>) {
    match v {
        Value::Object(m) if !m.is_empty() => {
            for (k, sv) in m {
                record(sv, kind, &format!("{path}.{k}"), sources);
            }
        }
        _ => {
            sources.insert(path.to_string(), kind);
        }
    }
}

pub(crate) fn get_path<'a>(v: &'a Value, key: &str) -> Option<&'a Value> {
    key.split('.')
        .try_fold(v, |cur, seg| cur.as_object()?.get(seg))
}

fn get_path_mut<'a>(v: &'a mut Value, key: &str) -> Option<&'a mut Value> {
    key.split('.')
        .try_fold(v, |cur, seg| cur.as_object_mut()?.get_mut(seg))
}

pub(crate) fn set_path(v: &mut Value, key: &str, value: Value) {
    let segs: Vec<&str> = key.split('.').collect();
    let mut cur = v;
    for (i, seg) in segs.iter().enumerate() {
        if !cur.is_object() {
            *cur = Value::Object(Map::new());
        }
        let Value::Object(m) = cur else { return };
        if i == segs.len() - 1 {
            m.insert((*seg).to_string(), value);
            return;
        }
        cur = m
            .entry((*seg).to_string())
            .or_insert_with(|| Value::Object(Map::new()));
    }
}
