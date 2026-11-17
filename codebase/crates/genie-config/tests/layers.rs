//! D23 layers: merge, sources, trust gating, diagnostics, last good copy, policy locks,
//! `config.set`, saving "always".

#![allow(clippy::unwrap_used, clippy::expect_used)] // test code

use std::fs;
use std::path::Path;

use genie_config::policy::{PolicyValues, merge};
use genie_config::*;
use genie_protocol::api::config::ConfigGetParams;
use genie_protocol::common::{ConfigScope, ConfigSource, Severity};
use serde_json::{Value, json};

fn write(p: &Path, v: &str) {
    fs::create_dir_all(p.parent().unwrap()).unwrap();
    fs::write(p, v).unwrap();
}

struct Fixture {
    _dir: tempfile::TempDir,
    paths: ConfigPaths,
}

fn fixture() -> Fixture {
    let dir = tempfile::tempdir().unwrap();
    let paths = ConfigPaths {
        user_dir: dir.path().join("home/.genie"),
        project_root: Some(dir.path().join("repo")),
    };
    write(
        &paths.file(LayerKind::User).unwrap(),
        r#"{"model":"user-model","theme":"dark","permissions":{"allow":["Bash(ls:*)"],"deny":["Read(./.env)"]},"memory":{"enabled":true}}"#,
    );
    write(
        &paths.file(LayerKind::Project).unwrap(),
        r#"{"model":"project-model","permissions":{"allow":["Bash(npm:*)"]},"shell":"bash"}"#,
    );
    write(
        &paths.file(LayerKind::Local).unwrap(),
        r#"{"permissions":{"allow":["Bash(ls:*)","Bash(git commit:*)"],"defaultMode":"acceptEdits"}}"#,
    );
    Fixture { _dir: dir, paths }
}

fn get(store: &ConfigStore, key: &str) -> (Option<Value>, Option<ConfigSource>) {
    let r = store.get(&ConfigGetParams {
        keys: Some(vec![key.into()]),
        ..Default::default()
    });
    (r.values.get(key).cloned(), r.sources.get(key).copied())
}

#[test]
fn merge_and_sources() {
    let f = fixture();
    let store = ConfigStore::load(f.paths.clone(), true);
    // (key, value, source)
    let cases = [
        ("model", json!("project-model"), ConfigSource::Project),
        ("theme", json!("dark"), ConfigSource::User),
        ("shell", json!("bash"), ConfigSource::Project),
        (
            "permissions.allow",
            json!(["Bash(ls:*)", "Bash(npm:*)", "Bash(git commit:*)"]),
            ConfigSource::Local,
        ),
        (
            "permissions.deny",
            json!(["Read(./.env)"]),
            ConfigSource::User,
        ),
        (
            "permissions.defaultMode",
            json!("acceptEdits"),
            ConfigSource::Local,
        ),
        (
            "permissions",
            store.value("permissions").unwrap(),
            ConfigSource::Local,
        ),
    ];
    for (key, want, src) in cases {
        assert_eq!(get(&store, key), (Some(want), Some(src)), "{key}");
    }
    let layers = store.permission_layers();
    assert_eq!(
        layers.iter().map(|(k, _)| *k).collect::<Vec<_>>(),
        vec![LayerKind::User, LayerKind::Project, LayerKind::Local]
    );
    // A scope-restricted view shows one file only.
    let r = store.get(&ConfigGetParams {
        scope: Some(ConfigScope::User),
        ..Default::default()
    });
    assert_eq!(r.values["model"], "user-model");
}

#[test]
fn untrusted_project_layers_are_skipped() {
    let f = fixture();
    let mut store = ConfigStore::load(f.paths.clone(), false);
    assert_eq!(get(&store, "model").0, Some(json!("user-model")));
    assert_eq!(store.watched_paths().len(), 1);
    assert!(
        store
            .diagnostics()
            .problems
            .iter()
            .any(|d| d.severity == Severity::Info)
    );
    assert!(matches!(
        store.add_permission_rule(ConfigScope::Local, "allow", "Bash(x:*)"),
        Err(ConfigError::Untrusted)
    ));
    let changed = store.set_trusted(true);
    assert_eq!(changed, vec![LayerKind::Project, LayerKind::Local]);
    assert_eq!(get(&store, "model").0, Some(json!("project-model")));
}

#[test]
fn diagnostics_and_last_good_copy() {
    let f = fixture();
    let user = f.paths.file(LayerKind::User).unwrap();
    write(&user, r#"{"model":"m1","bogus":1,"theme":"neon"}"#);
    let mut store = ConfigStore::load(f.paths.clone(), true);
    let problems = store.diagnostics().problems;
    let keys: Vec<_> = problems.iter().filter_map(|d| d.key.clone()).collect();
    assert!(keys.contains(&"bogus".into()) && keys.contains(&"theme".into()));
    assert_eq!(store.value("theme"), None);

    // Unchanged content: no change reported.
    assert!(store.refresh().is_empty());

    // Malformed JSON keeps the last good copy and raises a problem.
    write(&user, "{ not json");
    assert_eq!(store.refresh(), vec![LayerKind::User]);
    assert_eq!(store.value("memory"), None);
    let r = store.get(&ConfigGetParams {
        scope: Some(ConfigScope::User),
        ..Default::default()
    });
    assert_eq!(r.values["model"], "m1");
    assert!(
        store
            .diagnostics()
            .problems
            .iter()
            .any(|d| d.message.contains("malformed"))
    );

    // Fixed: picked up again.
    write(&user, r#"{"model":"m2"}"#);
    assert_eq!(store.refresh(), vec![LayerKind::User]);
    assert!(store.diagnostics().problems.is_empty());
}

fn policy(v: Value) -> EffectivePolicy {
    let mut flat = PolicyValues::new();
    for (k, x) in v.as_object().unwrap() {
        flat.insert(k.clone(), x.clone());
    }
    merge(&[(PolicySourceKind::File, &flat)])
}

#[test]
fn policy_locks_and_rules() {
    let f = fixture();
    let mut store = ConfigStore::load(f.paths.clone(), true);
    store.set_policy(policy(json!({
        "org.name": "Synergech",
        "memory.enabled": false,
        "permissions.deny": ["Bash(curl:*)"],
        "disableBypassMode": true
    })));
    assert_eq!(
        get(&store, "memory.enabled"),
        (Some(json!(false)), Some(ConfigSource::Policy))
    );
    assert_eq!(get(&store, "memory").1, Some(ConfigSource::Policy));
    let r = store.get(&ConfigGetParams::default());
    assert_eq!(r.locked, vec!["memory.enabled".to_string()]);
    // Policy rules are added on top of the layer rules.
    assert_eq!(
        get(&store, "permissions.deny"),
        (
            Some(json!(["Read(./.env)", "Bash(curl:*)"])),
            Some(ConfigSource::Policy)
        )
    );
    // Writing a locked key is refused with the org name.
    match store.set("memory.enabled", json!(true), ConfigScope::User) {
        Err(ConfigError::Locked { key, org }) => {
            assert_eq!(
                (key.as_str(), org.as_str()),
                ("memory.enabled", "Synergech")
            )
        }
        other => panic!("{other:?}"),
    }
    // Writing a parent object would overwrite the locked child.
    assert!(matches!(
        store.set("memory", json!({"enabled": true}), ConfigScope::User),
        Err(ConfigError::Locked { .. })
    ));
    assert_eq!(store.value("memory.enabled"), Some(json!(false)));
}

#[test]
fn set_validates_and_writes_scopes() {
    let f = fixture();
    let mut store = ConfigStore::load(f.paths.clone(), true);
    store
        .set("theme", json!("light"), ConfigScope::User)
        .unwrap();
    assert_eq!(
        get(&store, "theme"),
        (Some(json!("light")), Some(ConfigSource::User))
    );
    store
        .set("model", json!("local-model"), ConfigScope::Local)
        .unwrap();
    assert_eq!(
        get(&store, "model"),
        (Some(json!("local-model")), Some(ConfigSource::Local))
    );
    assert!(matches!(
        store.set("theme", json!("neon"), ConfigScope::User),
        Err(ConfigError::Invalid { .. })
    ));
    assert!(matches!(
        store.set("nope", json!(1), ConfigScope::User),
        Err(ConfigError::Invalid { .. })
    ));
    let text = fs::read_to_string(f.paths.file(LayerKind::User).unwrap()).unwrap();
    assert!(text.contains("\"light\"") && !text.contains("neon"));

    // Chat session without a project: project scope is refused.
    let mut chat = ConfigStore::load(
        ConfigPaths {
            user_dir: f.paths.user_dir.clone(),
            project_root: None,
        },
        false,
    );
    assert!(matches!(
        chat.set("model", json!("x"), ConfigScope::Project),
        Err(ConfigError::NoProject(_))
    ));
}

#[test]
fn saving_always_rules() {
    let f = fixture();
    let mut store = ConfigStore::load(f.paths.clone(), true);
    assert!(
        store
            .add_permission_rule(ConfigScope::Local, "allow", "Bash(cargo test:*)")
            .unwrap()
    );
    assert!(
        !store
            .add_permission_rule(ConfigScope::Local, "allow", "Bash(cargo test:*)")
            .unwrap()
    );
    let local = store
        .permission_layers()
        .into_iter()
        .find(|(k, _)| *k == LayerKind::Local)
        .unwrap()
        .1;
    assert!(
        local
            .allow
            .unwrap()
            .contains(&"Bash(cargo test:*)".to_string())
    );
    // Chat: user scope, file created on demand.
    let dir = tempfile::tempdir().unwrap();
    let mut chat = ConfigStore::load(
        ConfigPaths {
            user_dir: dir.path().join(".genie"),
            project_root: None,
        },
        false,
    );
    assert!(
        chat.add_permission_rule(ConfigScope::User, "allow", "mcp__gh__x")
            .unwrap()
    );
    assert_eq!(chat.value("permissions.allow"), Some(json!(["mcp__gh__x"])));
    assert!(
        chat.remove_permission_rule(ConfigScope::User, "allow", "mcp__gh__x")
            .unwrap()
    );
    assert_eq!(chat.value("permissions.allow"), Some(json!([])));
}

#[test]
fn cli_layer_beats_files() {
    let f = fixture();
    let mut store = ConfigStore::load(f.paths.clone(), true);
    store.set_cli_layer(json!({"model": "cli-model"}).as_object().unwrap().clone());
    assert_eq!(get(&store, "model").0, Some(json!("cli-model")));
}

#[test]
fn schema_is_exported() {
    let s = settings::schema();
    assert!(s.schema["properties"]["permissions"].is_object());
    assert!(s.ui["keys"]["permissions.defaultMode"].is_object());
}
