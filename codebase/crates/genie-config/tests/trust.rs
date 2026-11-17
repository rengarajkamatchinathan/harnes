//! D43 / 08 §4: trust decisions (table), identity, inheritance, policy, approvals.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::type_complexity)] // test code

use genie_config::trust::*;
use genie_protocol::api::config::{ApproveDecision, DefinitionKind, TrustDecision, TrustState};
use serde_json::json;

const NOW: &str = "2026-11-17T00:00:00Z";

fn store(dir: &tempfile::TempDir) -> TrustStore {
    TrustStore::open(dir.path().join("trust.json"), Some("/home/me".into()))
        .unwrap()
        .with_case_insensitive(false)
}

#[test]
fn decisions_table() {
    let none = TrustPolicy::default();
    let never = TrustPolicy {
        never: true,
        allowed_roots: vec![],
    };
    let roots = TrustPolicy {
        never: true,
        allowed_roots: vec!["/ci/work".into()],
    };
    // (setup decisions, query path, identity, policy, expected)
    type Setup = Vec<(&'static str, Option<&'static str>, TrustDecision)>;
    let cases: Vec<(&str, Setup, &str, Option<&str>, &TrustPolicy, TrustState)> = vec![
        (
            "unknown by default",
            vec![],
            "/p/repo",
            None,
            &none,
            TrustState::Unknown,
        ),
        (
            "trusted",
            vec![("/p/repo", Some("id1"), TrustDecision::Trust)],
            "/p/repo",
            Some("id1"),
            &none,
            TrustState::Trusted,
        ),
        (
            "subfolder inherits",
            vec![("/p/repo", Some("id1"), TrustDecision::Trust)],
            "/p/repo/sub/x",
            Some("id1"),
            &none,
            TrustState::Trusted,
        ),
        (
            "sibling prefix doesn't",
            vec![("/p/repo", None, TrustDecision::Trust)],
            "/p/repo2",
            None,
            &none,
            TrustState::Unknown,
        ),
        (
            "different repo at same path",
            vec![("/p/repo", Some("id1"), TrustDecision::Trust)],
            "/p/repo",
            Some("id2"),
            &none,
            TrustState::Unknown,
        ),
        (
            "denied",
            vec![("/p/repo", None, TrustDecision::Deny)],
            "/p/repo",
            None,
            &none,
            TrustState::Denied,
        ),
        (
            "deeper deny wins",
            vec![
                ("/p", None, TrustDecision::Trust),
                ("/p/repo", None, TrustDecision::Deny),
            ],
            "/p/repo/a",
            None,
            &none,
            TrustState::Denied,
        ),
        (
            "once",
            vec![("/p/repo", None, TrustDecision::Once)],
            "/p/repo",
            None,
            &none,
            TrustState::Once,
        ),
        (
            "revoke",
            vec![
                ("/p/repo", None, TrustDecision::Trust),
                ("/p/repo", None, TrustDecision::Revoke),
            ],
            "/p/repo",
            None,
            &none,
            TrustState::Unknown,
        ),
        (
            "policy never",
            vec![("/p/repo", None, TrustDecision::Trust)],
            "/p/repo",
            None,
            &never,
            TrustState::Denied,
        ),
        (
            "policy allowed root",
            vec![],
            "/ci/work/job1",
            None,
            &roots,
            TrustState::Trusted,
        ),
    ];
    for (name, setup, path, id, policy, want) in cases {
        let dir = tempfile::tempdir().unwrap();
        let mut s = store(&dir);
        for (p, i, d) in setup {
            s.set(p, i, d, &TrustPolicy::default(), NOW).unwrap();
        }
        assert_eq!(s.state(path, id, policy), want, "{name}");
    }
}

#[test]
fn persistence_and_refusals() {
    let dir = tempfile::tempdir().unwrap();
    let p = TrustPolicy::default();
    {
        let mut s = store(&dir);
        s.set("/p/repo", Some("id"), TrustDecision::Trust, &p, NOW)
            .unwrap();
        s.set("/p/tmp", None, TrustDecision::Once, &p, NOW).unwrap();
        for bad in ["/", "/home/me", "/usr", "/etc", "C:\\", "c:/Windows"] {
            assert!(
                matches!(
                    s.set(bad, None, TrustDecision::Trust, &p, NOW),
                    Err(TrustError::TooBroad(_))
                ),
                "{bad}"
            );
        }
        let never = TrustPolicy {
            never: true,
            allowed_roots: vec![],
        };
        assert!(matches!(
            s.set("/p/x", None, TrustDecision::Trust, &never, NOW),
            Err(TrustError::PolicyNever)
        ));
        // Denying a broad folder is fine.
        s.set("/home/me", None, TrustDecision::Deny, &p, NOW)
            .unwrap();
    }
    let s = store(&dir);
    assert_eq!(s.state("/p/repo", Some("id"), &p), TrustState::Trusted);
    assert_eq!(
        s.state("/p/tmp", None, &p),
        TrustState::Unknown,
        "once is not persisted"
    );
    let e = &s.entries()[0];
    assert_eq!(
        (e.canonical_path.as_str(), e.decided_at.as_str()),
        ("/p/repo", NOW)
    );
    let raw = std::fs::read_to_string(dir.path().join("trust.json")).unwrap();
    assert!(raw.contains("\"canonical_path\"") && raw.contains("\"approvals\""));
}

#[test]
fn headless() {
    let dir = tempfile::tempdir().unwrap();
    let mut s = store(&dir);
    let p = TrustPolicy::default();
    assert!(!s.headless_trusted("/p/repo", None, &p, false));
    assert!(s.headless_trusted("/p/repo", None, &p, true));
    s.set("/p/repo", None, TrustDecision::Trust, &p, NOW)
        .unwrap();
    assert!(s.headless_trusted("/p/repo", None, &p, false));
    let roots = TrustPolicy {
        never: true,
        allowed_roots: vec!["/ci".into()],
    };
    assert!(s.headless_trusted("/ci/x", None, &roots, false));
    assert!(
        !s.headless_trusted("/p/repo", None, &roots, true),
        "policy never beats --trust"
    );
}

#[test]
fn definition_hash_approvals() {
    let dir = tempfile::tempdir().unwrap();
    let mut s = store(&dir);
    let p = TrustPolicy::default();
    let mcp = |cmd: &str| Definition {
        kind: DefinitionKind::Mcp,
        name: "db".into(),
        value: json!({"command": cmd, "args": ["--port", "1"]}),
    };
    let hook = Definition {
        kind: DefinitionKind::Hook,
        name: "fmt".into(),
        value: json!({"run": ["fmt"]}),
    };

    // Untrusted folder: nothing to approve.
    assert!(matches!(
        s.pending("/p/repo", None, &[mcp("a")]),
        Err(TrustError::NotTrusted)
    ));

    s.set("/p/repo", None, TrustDecision::Trust, &p, NOW)
        .unwrap();
    let pending = s
        .pending("/p/repo", None, &[mcp("a"), hook.clone()])
        .unwrap();
    assert_eq!(pending.len(), 2);
    assert!(pending[0].diff.lines().all(|l| l.starts_with('+')));

    // Approving with a stale hash does nothing.
    assert!(
        !s.approve(
            "/p/repo",
            None,
            &mcp("a"),
            "sha256:00",
            ApproveDecision::Approve
        )
        .unwrap()
    );
    assert!(
        !s.approve(
            "/p/repo",
            None,
            &mcp("a"),
            &pending[0].hash,
            ApproveDecision::Skip
        )
        .unwrap()
    );
    assert!(
        s.approve(
            "/p/repo",
            None,
            &mcp("a"),
            &pending[0].hash,
            ApproveDecision::Approve
        )
        .unwrap()
    );
    assert!(s.is_approved("/p/repo/sub", None, &mcp("a")));
    assert_eq!(s.pending("/p/repo", None, &[mcp("a")]).unwrap(), vec![]);

    // Key order doesn't change the hash.
    assert_eq!(
        definition_hash(&json!({"a": 1, "b": [1, {"y": 2, "x": 1}]})),
        definition_hash(&json!({"b": [1, {"x": 1, "y": 2}], "a": 1}))
    );

    // Changed after `git pull` → pending again, with a diff against the approved one.
    let changed = s.pending("/p/repo", None, &[mcp("b")]).unwrap();
    assert_eq!(changed.len(), 1);
    assert!(changed[0].diff.contains("-  \"command\": \"a\""));
    assert!(changed[0].diff.contains("+  \"command\": \"b\""));
    assert!(!s.is_approved("/p/repo", None, &mcp("b")));

    // Persisted; removing the entry drops its approval.
    let s2 = store(&dir);
    assert!(s2.is_approved("/p/repo", None, &mcp("a")));
    assert_eq!(s.prune("/p/repo", None, &[hook]).unwrap(), 1);
    assert!(!store(&dir).is_approved("/p/repo", None, &mcp("a")));

    // Re-trusting with a new identity clears approvals.
    s.approve(
        "/p/repo",
        None,
        &mcp("a"),
        &definition_hash(&mcp("a").value),
        ApproveDecision::Approve,
    )
    .unwrap();
    s.set("/p/repo", Some("new"), TrustDecision::Trust, &p, NOW)
        .unwrap();
    assert!(!s.is_approved("/p/repo", Some("new"), &mcp("a")));
}
