//! The daemon's settings, policy and trust hub (D23, D38, D43; 08 §4–5).
//!
//! One [`ConfigStore`] per project root (plus one without a root for Chat and the user
//! layer), all sharing one [`EffectivePolicy`] and one [`TrustStore`]. The permission
//! authorizer, the MCP wiring and the daemon's `config.*` / `policy.*` / `trust.*`
//! methods all read through it. Changes are broadcast as [`HubEvent`]s, which the daemon
//! forwards to the workspace feed (`config.changed`, `policy.changed`).

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, MutexGuard};

use genie_config::fsutil::{read_optional, write_private};
use genie_config::trust::TrustStore;
use genie_config::{
    ConfigError, ConfigPaths, ConfigStore, Definition, EffectivePolicy, LayerKind,
    PermissionsSettings, TrustError, TrustPolicy,
};
use genie_protocol::api::config::{
    ApproveDecision, ConfigDiagnosticsResult, ConfigGetParams, ConfigGetResult, ConfigSchemaResult,
    DefinitionKind, PendingDefinition, PolicyEffectiveResult, TrustDecision, TrustState,
};
use genie_protocol::common::ConfigScope;
use serde_json::{Map, Value};
use tokio::sync::broadcast;

use crate::bus::now_ms;

/// Something other parts of the daemon should react to.
#[derive(Debug, Clone, PartialEq)]
pub enum HubEvent {
    /// Settings changed (`config.changed`). `root`: the project, `None` for user scope.
    Config {
        root: Option<PathBuf>,
        scope: ConfigScope,
        keys: Vec<String>,
    },
    /// Managed policy changed (`policy.changed`).
    Policy { keys: Vec<String> },
    /// A trust decision or definition approval changed for `path`.
    Trust { path: PathBuf },
}

struct Inner {
    policy: EffectivePolicy,
    trust: TrustStore,
    stores: HashMap<Option<PathBuf>, ConfigStore>,
}

/// See the module docs.
pub struct ConfigHub {
    user_dir: PathBuf,
    inner: Mutex<Inner>,
    events: broadcast::Sender<HubEvent>,
}

impl std::fmt::Debug for ConfigHub {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ConfigHub")
            .field("user_dir", &self.user_dir)
            .finish_non_exhaustive()
    }
}

/// The project root for a folder: the nearest ancestor with `.git`, else the folder.
pub fn project_root(cwd: &Path) -> PathBuf {
    let mut p = Some(cwd);
    while let Some(d) = p {
        if d.join(".git").exists() {
            return d.to_path_buf();
        }
        p = d.parent();
    }
    cwd.to_path_buf()
}

fn now() -> String {
    genie_protocol::rfc3339_ms(now_ms())
}

fn layer_scope(k: LayerKind) -> ConfigScope {
    match k {
        LayerKind::User => ConfigScope::User,
        LayerKind::Project => ConfigScope::Project,
        LayerKind::Local => ConfigScope::Local,
        _ => ConfigScope::Unknown,
    }
}

/// Top-level keys that differ between two objects.
fn changed_keys(a: &Map<String, Value>, b: &Map<String, Value>) -> Vec<String> {
    let keys: BTreeSet<&String> = a.keys().chain(b.keys()).collect();
    keys.into_iter()
        .filter(|k| a.get(*k) != b.get(*k))
        .cloned()
        .collect()
}

impl ConfigHub {
    /// `user_dir`: `~/.genie` (channel dir). `trust_path`: `~/.genie/trust.json`.
    /// `home`: the home folder (too-broad trust check, `~/` rules).
    pub fn new(
        user_dir: PathBuf,
        trust_path: PathBuf,
        home: Option<String>,
    ) -> Result<ConfigHub, TrustError> {
        let trust = TrustStore::open(trust_path.clone(), home.clone()).or_else(|e| {
            tracing::error!(error = %e, "trust.json unreadable; moved aside, starting empty");
            let _ = std::fs::rename(&trust_path, trust_path.with_extension("json.bad"));
            TrustStore::open(trust_path, home)
        })?;
        Ok(ConfigHub {
            user_dir,
            inner: Mutex::new(Inner {
                policy: EffectivePolicy::default(),
                trust,
                stores: HashMap::new(),
            }),
            events: broadcast::channel(256).0,
        })
    }

    pub fn subscribe(&self) -> broadcast::Receiver<HubEvent> {
        self.events.subscribe()
    }

    fn emit(&self, e: HubEvent) {
        let _ = self.events.send(e);
    }

    fn lock(&self) -> MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }

    pub fn user_dir(&self) -> &Path {
        &self.user_dir
    }

    fn trusted_in(inner: &Inner, root: &Path) -> bool {
        let tp = TrustPolicy::from_policy(&inner.policy);
        matches!(
            inner.trust.state(&root.display().to_string(), None, &tp),
            TrustState::Trusted | TrustState::Once
        )
    }

    /// Run `f` on the store for `root` (created on first use, trust-gated).
    pub fn with_store<R>(&self, root: Option<&Path>, f: impl FnOnce(&mut ConfigStore) -> R) -> R {
        let mut g = self.lock();
        let key = root.map(Path::to_path_buf);
        if !g.stores.contains_key(&key) {
            let trusted = root.is_some_and(|r| Self::trusted_in(&g, r));
            let mut s = ConfigStore::load(
                ConfigPaths {
                    user_dir: self.user_dir.clone(),
                    project_root: key.clone(),
                },
                trusted,
            );
            s.set_policy(g.policy.clone());
            g.stores.insert(key.clone(), s);
        }
        let Inner { stores, .. } = &mut *g;
        #[allow(clippy::expect_used)]
        let s = stores.get_mut(&key).expect("inserted above");
        f(s)
    }

    // ---------------------------------------------------------------- policy

    pub fn policy(&self) -> EffectivePolicy {
        self.lock().policy.clone()
    }

    /// Install a new effective policy; emits `policy.changed` with the keys that changed.
    pub fn set_policy(&self, policy: EffectivePolicy) -> Vec<String> {
        let keys = {
            let mut g = self.lock();
            let old = g.policy.to_result().values;
            let new = policy.to_result().values;
            let keys: BTreeSet<&String> = old.keys().chain(new.keys()).collect();
            let keys: Vec<String> = keys
                .into_iter()
                .filter(|k| old.get(*k) != new.get(*k))
                .cloned()
                .collect();
            g.policy = policy.clone();
            for s in g.stores.values_mut() {
                s.set_policy(policy.clone());
            }
            keys
        };
        if !keys.is_empty() {
            self.emit(HubEvent::Policy { keys: keys.clone() });
        }
        keys
    }

    pub fn policy_effective(&self) -> PolicyEffectiveResult {
        self.lock().policy.to_result()
    }

    // ---------------------------------------------------------------- settings

    /// Re-read every settings file (debounced watcher / poll). Emits `config.changed`.
    pub fn refresh(&self) {
        let mut out = Vec::new();
        {
            let mut g = self.lock();
            for (root, s) in &mut g.stores {
                let before: Vec<(LayerKind, Map<String, Value>)> = Self::layer_values(s);
                for kind in s.refresh() {
                    let after = Self::layer_values(s);
                    let get = |v: &[(LayerKind, Map<String, Value>)]| {
                        v.iter()
                            .find(|(k, _)| *k == kind)
                            .map(|(_, m)| m.clone())
                            .unwrap_or_default()
                    };
                    let keys = changed_keys(&get(&before), &get(&after));
                    out.push(HubEvent::Config {
                        root: root.clone(),
                        scope: layer_scope(kind),
                        keys,
                    });
                }
            }
        }
        // A user-scope change is reported once even though every store reloads it.
        let mut seen_user = false;
        for e in out {
            if let HubEvent::Config {
                scope: ConfigScope::User,
                ..
            } = &e
            {
                if seen_user {
                    continue;
                }
                seen_user = true;
            }
            self.emit(e);
        }
    }

    fn layer_values(s: &ConfigStore) -> Vec<(LayerKind, Map<String, Value>)> {
        [LayerKind::User, LayerKind::Project, LayerKind::Local]
            .into_iter()
            .map(|k| {
                let v = s.get(&ConfigGetParams {
                    keys: None,
                    scope: Some(layer_scope(k)),
                    cwd: None,
                });
                (k, v.values.into_iter().collect())
            })
            .collect()
    }

    fn root_of(cwd: Option<&str>) -> Option<PathBuf> {
        cwd.filter(|c| !c.is_empty())
            .map(|c| project_root(Path::new(c)))
    }

    /// `config.get`.
    pub fn get(&self, p: &ConfigGetParams) -> ConfigGetResult {
        let root = Self::root_of(p.cwd.as_deref());
        self.with_store(root.as_deref(), |s| s.get(p))
    }

    /// `config.set`. Other stores pick up a user-scope change on their next refresh.
    pub fn set(
        &self,
        key: &str,
        value: Value,
        scope: ConfigScope,
        cwd: Option<&str>,
    ) -> Result<(), ConfigError> {
        let root = Self::root_of(cwd);
        self.with_store(root.as_deref(), |s| s.set(key, value, scope))?;
        if scope == ConfigScope::User {
            self.refresh_others(root.as_deref());
        }
        self.emit(HubEvent::Config {
            root,
            scope,
            keys: vec![key.to_string()],
        });
        Ok(())
    }

    fn refresh_others(&self, except: Option<&Path>) {
        let mut g = self.lock();
        for (r, s) in &mut g.stores {
            if r.as_deref() != except {
                s.refresh();
            }
        }
    }

    pub fn schema(&self) -> ConfigSchemaResult {
        genie_config::settings::schema()
    }

    /// `config.diagnostics`: settings problems for the folder plus policy problems.
    pub fn diagnostics(
        &self,
        cwd: Option<&str>,
        policy_problems: Vec<genie_protocol::common::Diagnostic>,
    ) -> ConfigDiagnosticsResult {
        let root = Self::root_of(cwd);
        let mut r = self.with_store(root.as_deref(), |s| s.diagnostics());
        r.problems.extend(policy_problems);
        r
    }

    /// The `permissions` block per loaded layer (lowest first).
    pub fn permission_layers(&self, root: Option<&Path>) -> Vec<(LayerKind, PermissionsSettings)> {
        self.with_store(root, |s| s.permission_layers())
    }

    /// One merged value by dotted key.
    pub fn value(&self, root: Option<&Path>, key: &str) -> Option<Value> {
        self.with_store(root, |s| s.value(key))
    }

    /// One layer's value of a top-level key (e.g. `mcpServers` per scope).
    pub fn layer_value(&self, root: Option<&Path>, scope: ConfigScope, key: &str) -> Option<Value> {
        self.with_store(root, |s| {
            s.get(&ConfigGetParams {
                keys: Some(vec![key.to_string()]),
                scope: Some(scope),
                cwd: None,
            })
            .values
            .remove(key)
        })
    }

    /// Persist a saved "always" (06 §5.4).
    pub fn add_permission_rule(
        &self,
        root: Option<&Path>,
        scope: ConfigScope,
        class: &str,
        rule: &str,
    ) -> Result<bool, ConfigError> {
        let added = self.with_store(root, |s| s.add_permission_rule(scope, class, rule))?;
        if scope == ConfigScope::User {
            self.refresh_others(root);
        }
        if added {
            self.emit(HubEvent::Config {
                root: root.map(Path::to_path_buf),
                scope,
                keys: vec![format!("permissions.{class}")],
            });
        }
        Ok(added)
    }

    /// Edit a JSON file's top-level object in place (0600, atomic) and reload stores.
    /// Used for `mcpServers` entries (`mcp.add` / `mcp.remove`).
    pub fn edit_mcp_servers(
        &self,
        root: Option<&Path>,
        scope: ConfigScope,
        f: impl FnOnce(&mut Map<String, Value>) -> bool,
    ) -> Result<bool, ConfigError> {
        let path = match scope {
            ConfigScope::User => self.user_dir.join("settings.json"),
            ConfigScope::Local => root
                .ok_or(ConfigError::NoProject("local"))?
                .join(".genie")
                .join("settings.local.json"),
            ConfigScope::Project => root
                .ok_or(ConfigError::NoProject("project"))?
                .join(".mcp.json"),
            _ => return Err(ConfigError::BadScope),
        };
        if scope != ConfigScope::User && !root.is_some_and(|r| self.trusted(r)) {
            return Err(ConfigError::Untrusted);
        }
        let mut doc = match read_optional(&path)? {
            None => Map::new(),
            Some(b) => match serde_json::from_slice::<Value>(&b) {
                Ok(Value::Object(m)) => m,
                _ => return Err(ConfigError::Malformed(path.display().to_string())),
            },
        };
        let mut servers = match doc.remove("mcpServers") {
            Some(Value::Object(m)) => m,
            _ => Map::new(),
        };
        let changed = f(&mut servers);
        if !changed {
            return Ok(false);
        }
        doc.insert("mcpServers".into(), Value::Object(servers));
        let mut text = serde_json::to_string_pretty(&Value::Object(doc)).map_err(|e| {
            ConfigError::Invalid {
                key: "mcpServers".into(),
                message: e.to_string(),
            }
        })?;
        text.push('\n');
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        write_private(&path, text.as_bytes())?;
        self.refresh_others(None);
        if let Some(r) = root {
            self.with_store(Some(r), |s| {
                s.refresh();
            });
        }
        self.with_store(None, |s| {
            s.refresh();
        });
        self.emit(HubEvent::Config {
            root: root.map(Path::to_path_buf),
            scope,
            keys: vec!["mcpServers".into()],
        });
        Ok(true)
    }

    /// `<root>/.mcp.json` `mcpServers` (project scope, D43).
    pub fn mcp_json(&self, root: &Path) -> Map<String, Value> {
        let Ok(Some(b)) = read_optional(&root.join(".mcp.json")) else {
            return Map::new();
        };
        match serde_json::from_slice::<Value>(&b) {
            Ok(Value::Object(mut m)) => match m.remove("mcpServers") {
                Some(Value::Object(s)) => s,
                _ => Map::new(),
            },
            _ => Map::new(),
        }
    }

    // ---------------------------------------------------------------- trust

    pub fn trusted(&self, root: &Path) -> bool {
        Self::trusted_in(&self.lock(), root)
    }

    /// `trust.get`.
    pub fn trust_get(&self, path: &str) -> TrustState {
        let g = self.lock();
        let tp = TrustPolicy::from_policy(&g.policy);
        g.trust.state(path, None, &tp)
    }

    /// `trust.set`: stores under `path` reload with or without their project layers.
    pub fn trust_set(&self, path: &str, decision: TrustDecision) -> Result<TrustState, TrustError> {
        let state = {
            let mut g = self.lock();
            let tp = TrustPolicy::from_policy(&g.policy);
            let st = g.trust.set(path, None, decision, &tp, &now())?;
            let roots: Vec<Option<PathBuf>> = g.stores.keys().cloned().collect();
            for r in roots.into_iter().flatten() {
                if r.starts_with(path) || Path::new(path).starts_with(&r) {
                    let t = Self::trusted_in(&g, &r);
                    if let Some(s) = g.stores.get_mut(&Some(r)) {
                        s.set_trusted(t);
                    }
                }
            }
            st
        };
        self.emit(HubEvent::Trust {
            path: PathBuf::from(path),
        });
        Ok(state)
    }

    /// Project definitions that need approval (D43): `.mcp.json` and project-settings
    /// `mcpServers` entries, and project-settings `hooks`.
    pub fn project_definitions(&self, root: &Path) -> Vec<Definition> {
        let mut defs: BTreeMap<String, Value> = BTreeMap::new();
        if let Some(Value::Object(m)) =
            self.layer_value(Some(root), ConfigScope::Project, "mcpServers")
        {
            defs.extend(m);
        }
        defs.extend(self.mcp_json(root));
        let mut out: Vec<Definition> = defs
            .into_iter()
            .map(|(name, value)| Definition {
                kind: DefinitionKind::Mcp,
                name,
                value,
            })
            .collect();
        // Project hooks (06 §12, 08 §2.3) need the same approval.
        out.extend(crate::runtime::hooks::project_hook_definitions(self, root));
        out
    }

    /// `trust.pending_definitions`.
    pub fn pending_definitions(&self, path: &str) -> Result<Vec<PendingDefinition>, TrustError> {
        let root = PathBuf::from(path);
        let defs = self.project_definitions(&root);
        self.lock().trust.pending(path, None, &defs)
    }

    /// Whether this project definition was approved with its current hash.
    pub fn is_approved(&self, root: &Path, def: &Definition) -> bool {
        self.lock()
            .trust
            .is_approved(&root.display().to_string(), None, def)
    }

    /// `trust.approve_definition`. Returns false if the hash is stale or the decision skip.
    pub fn approve_definition(
        &self,
        path: &str,
        kind: DefinitionKind,
        name: &str,
        hash: &str,
        decision: ApproveDecision,
    ) -> Result<bool, TrustError> {
        let root = PathBuf::from(path);
        let Some(def) = self
            .project_definitions(&root)
            .into_iter()
            .find(|d| d.kind == kind && d.name == name)
        else {
            return Ok(false);
        };
        let ok = self
            .lock()
            .trust
            .approve(path, None, &def, hash, decision)?;
        if ok {
            self.emit(HubEvent::Trust { path: root });
        }
        Ok(ok)
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;
    use serde_json::json;

    fn hub(dir: &Path) -> ConfigHub {
        ConfigHub::new(
            dir.join("home/.genie"),
            dir.join("home/.genie/trust.json"),
            Some(dir.join("home").display().to_string()),
        )
        .unwrap()
    }

    #[test]
    fn set_get_and_events() {
        let d = tempfile::tempdir().unwrap();
        let h = hub(d.path());
        let mut rx = h.subscribe();
        h.set("model", json!("anthropic/x"), ConfigScope::User, None)
            .unwrap();
        let r = h.get(&ConfigGetParams::default());
        assert_eq!(r.values["model"], json!("anthropic/x"));
        assert!(matches!(
            rx.try_recv().unwrap(),
            HubEvent::Config {
                scope: ConfigScope::User,
                ..
            }
        ));
    }

    #[test]
    fn trust_gates_project_layers_and_approvals() {
        let d = tempfile::tempdir().unwrap();
        let h = hub(d.path());
        let proj = d.path().join("work/proj");
        std::fs::create_dir_all(proj.join(".genie")).unwrap();
        std::fs::write(
            proj.join(".genie/settings.json"),
            r#"{"permissions":{"allow":["Bash(ls:*)"]}}"#,
        )
        .unwrap();
        std::fs::write(
            proj.join(".mcp.json"),
            r#"{"mcpServers":{"x":{"command":"x"}}}"#,
        )
        .unwrap();
        let p = proj.display().to_string();
        assert!(h.permission_layers(Some(&proj)).is_empty());
        assert_eq!(h.trust_get(&p), TrustState::Unknown);
        h.trust_set(&p, TrustDecision::Trust).unwrap();
        assert_eq!(h.permission_layers(Some(&proj)).len(), 1);
        let pend = h.pending_definitions(&p).unwrap();
        assert_eq!(pend.len(), 1);
        assert!(
            h.approve_definition(
                &p,
                DefinitionKind::Mcp,
                "x",
                &pend[0].hash,
                ApproveDecision::Approve
            )
            .unwrap()
        );
        assert!(h.pending_definitions(&p).unwrap().is_empty());
    }
}
