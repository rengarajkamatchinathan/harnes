//! 08 §5: policy merge (table), signed remote URL, last good copy, file checks.

#![allow(clippy::unwrap_used, clippy::expect_used)] // test code

use std::sync::{Arc, Mutex};

use base64::Engine;
use ed25519_dalek::{Signer, SigningKey};
use genie_config::policy::*;
use genie_protocol::common::Mode;
use serde_json::{Value, json};

fn vals(v: Value) -> PolicyValues {
    parse_policy(v.to_string().as_bytes()).unwrap()
}

#[test]
fn merge_table() {
    use PolicySourceKind::*;
    // (key, file, mdm, remote, expected value, expected source)
    let cases: Vec<(&str, Value, Value, Value, Value, &str)> = vec![
        (
            "models.allowed",
            json!(["a", "b", "c"]),
            json!(["b", "c"]),
            json!(["c", "d"]),
            json!(["c"]),
            "file+mdm+remote",
        ),
        (
            "permissions.deny",
            json!(["Bash(rm:*)"]),
            json!(["WebSearch", "Bash(rm:*)"]),
            Value::Null,
            json!(["Bash(rm:*)", "WebSearch"]),
            "file+mdm",
        ),
        (
            "memory.enabled",
            json!(true),
            json!(false),
            json!(true),
            json!(false),
            "mdm",
        ),
        (
            "incognito.allowed",
            json!(true),
            Value::Null,
            json!(true),
            json!(true),
            "remote",
        ),
        (
            "disableBypassMode",
            json!(false),
            json!(true),
            json!(false),
            json!(true),
            "mdm",
        ),
        (
            "retention.maxDays",
            json!(90),
            json!(30),
            json!(60),
            json!(30),
            "mdm",
        ),
        (
            "telemetry.level",
            json!("usage"),
            json!("off"),
            json!("crash-only"),
            json!("off"),
            "mdm",
        ),
        (
            "trust.mode",
            json!("prompt"),
            json!("never"),
            Value::Null,
            json!("never"),
            "mdm",
        ),
        (
            "defaultMode",
            json!("acceptEdits"),
            json!("plan"),
            json!("default"),
            json!("plan"),
            "mdm",
        ),
        (
            "browser.profile",
            json!("persistent"),
            Value::Null,
            json!("ephemeral"),
            json!("ephemeral"),
            "remote",
        ),
        (
            "org.name",
            json!("FileOrg"),
            json!("MdmOrg"),
            json!("RemoteOrg"),
            json!("RemoteOrg"),
            "remote",
        ),
        (
            "catalog.url",
            json!("https://f"),
            json!("https://m"),
            Value::Null,
            json!("https://m"),
            "mdm",
        ),
        (
            "updates.minVersion",
            json!("1.0.0"),
            Value::Null,
            Value::Null,
            json!("1.0.0"),
            "file",
        ),
    ];
    for (key, f, m, r, want, src) in cases {
        let mut sources = Vec::new();
        for (kind, v) in [(File, f), (Mdm, m), (Remote, r)] {
            if !v.is_null() {
                let mut pv = PolicyValues::new();
                pv.insert(key.to_string(), v);
                sources.push((kind, pv));
            }
        }
        // Order of the input must not matter.
        sources.reverse();
        let refs: Vec<_> = sources.iter().map(|(k, v)| (*k, v)).collect();
        let e = merge(&refs);
        assert_eq!(e.get(key), Some(&want), "{key}");
        assert_eq!(e.source(key), Some(src), "{key} source");
    }
}

#[test]
fn nested_and_dotted_keys_flatten() {
    let v = vals(
        json!({"permissions": {"deny": ["X"]}, "org.name": "S", "audit": {"otlp": {"endpoint": "e"}}}),
    );
    assert_eq!(v["permissions.deny"], json!(["X"]));
    assert_eq!(v["org.name"], "S");
    assert_eq!(v["audit.otlp.endpoint"], "e");
}

#[test]
fn effective_helpers() {
    let e = merge(&[(
        PolicySourceKind::File,
        &vals(
            json!({"org": {"name": "Synergech"}, "disableBypassMode": true, "defaultMode": "acceptEdits", "audit": {"includeContent": true}}),
        ),
    )]);
    assert_eq!(e.org_name(), Some("Synergech"));
    assert!(e.disable_bypass());
    assert_eq!(e.mode_floor(), Some(Mode::AcceptEdits));
    assert_eq!(
        e.content_audit_notice().as_deref(),
        Some("Content auditing is enabled by Synergech")
    );
    let r = e.to_result();
    assert_eq!(r.source_per_key["org.name"], "file");
}

struct Remote {
    key: SigningKey,
    body: Mutex<Option<String>>,
    tamper: Mutex<bool>,
}

impl PolicyFetcher for Remote {
    fn fetch(&self, _url: &str) -> Result<SignedPolicy, String> {
        let body = self.body.lock().unwrap().clone().ok_or("offline")?;
        let sig = base64::engine::general_purpose::STANDARD
            .encode(self.key.sign(body.as_bytes()).to_bytes());
        let body = if *self.tamper.lock().unwrap() {
            body.replace("30", "3000")
        } else {
            body
        };
        Ok(SignedPolicy {
            body,
            signature: sig,
        })
    }
}

#[test]
fn signed_remote_with_last_good_copy() {
    let key = SigningKey::from_bytes(&[9u8; 32]);
    let pk_hex = hex::encode(key.verifying_key().to_bytes());
    let remote = Arc::new(Remote {
        key,
        body: Mutex::new(Some(
            r#"{"retention":{"maxDays":30},"policyUrl":"https://evil","models":{"blocked":["x"]}}"#
                .into(),
        )),
        tamper: Mutex::new(false),
    });
    let file = StaticPolicySource {
        kind: PolicySourceKind::File,
        values: Some(vals(
            json!({"policyUrl": "https://policy.example/p.json", "policyKeys": [pk_hex], "retention": {"maxDays": 90}}),
        )),
    };
    let dir = tempfile::tempdir().unwrap();
    let cache = dir.path().join("policy-remote.json");
    let mut m = PolicyManager::new(vec![Box::new(file.clone())])
        .with_remote(remote.clone())
        .with_remote_cache(cache.clone());
    m.refresh_local();
    m.refresh_remote();
    let e = m.effective();
    assert_eq!(e.get("retention.maxDays"), Some(&json!(30)));
    assert_eq!(e.get("models.blocked"), Some(&json!(["x"])));
    // The remote can't move its own URL; keys are not exposed.
    assert_eq!(
        e.get("policyUrl"),
        Some(&json!("https://policy.example/p.json"))
    );
    assert_eq!(e.get("policyKeys"), None);
    assert!(m.problems().is_empty());
    assert!(cache.exists());

    // Fetch fails → last good copy kept + Problem.
    *remote.body.lock().unwrap() = None;
    m.refresh_remote();
    assert_eq!(m.effective().get("models.blocked"), Some(&json!(["x"])));
    assert_eq!(m.problems().len(), 1);

    // Tampered body → rejected, last good kept.
    *remote.body.lock().unwrap() = Some(r#"{"retention":{"maxDays":30}}"#.into());
    *remote.tamper.lock().unwrap() = true;
    m.refresh_remote();
    assert_eq!(m.effective().get("models.blocked"), Some(&json!(["x"])));
    assert!(m.problems()[0].message.contains("signature"));

    // A fresh daemon loads the cached copy (re-verified).
    let mut m2 = PolicyManager::new(vec![Box::new(file)]).with_remote_cache(cache);
    m2.refresh_local();
    m2.load_remote_cache();
    assert_eq!(m2.effective().get("models.blocked"), Some(&json!(["x"])));

    // Without pinned keys the remote is never trusted.
    let nokeys = StaticPolicySource {
        kind: PolicySourceKind::Mdm,
        values: Some(vals(json!({"policyUrl": "https://p"}))),
    };
    *remote.tamper.lock().unwrap() = false;
    let mut m3 = PolicyManager::new(vec![Box::new(nokeys)]).with_remote(remote);
    m3.refresh_local();
    m3.refresh_remote();
    assert_eq!(m3.effective().get("retention.maxDays"), None);
    assert!(m3.problems()[0].message.contains("no pinned keys"));
}

#[test]
fn local_source_disappearing_keeps_last_good() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("policy.json");
    std::fs::write(&path, r#"{"permissions":{"deny":["Bash"]}}"#).unwrap();
    let src = FilePolicySource {
        path: path.clone(),
        require_admin: false,
    };
    let mut m = PolicyManager::new(vec![Box::new(src)]);
    m.refresh_local();
    assert_eq!(m.effective().strings("permissions.deny"), vec!["Bash"]);
    std::fs::remove_file(&path).unwrap();
    m.refresh_local();
    assert_eq!(m.effective().strings("permissions.deny"), vec!["Bash"]);
    assert!(m.problems()[0].message.contains("last good"));
    std::fs::write(&path, "{broken").unwrap();
    m.refresh_local();
    assert_eq!(m.effective().strings("permissions.deny"), vec!["Bash"]);
    std::fs::write(&path, r#"{"permissions":{"deny":["WebSearch"]}}"#).unwrap();
    m.refresh_local();
    assert_eq!(m.effective().strings("permissions.deny"), vec!["WebSearch"]);
    assert!(m.problems().is_empty());
}

#[cfg(unix)]
#[test]
fn user_writable_policy_file_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("policy.json");
    std::fs::write(&path, r#"{"org":{"name":"x"}}"#).unwrap();
    // Owned by the test user, not root → refused.
    let src = FilePolicySource {
        path,
        require_admin: true,
    };
    assert!(matches!(src.load(), Err(PolicyError::UserWritable(_))));
}
