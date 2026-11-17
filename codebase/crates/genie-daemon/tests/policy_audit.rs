//! 08 §5–7 end to end against a loopback HTTP server (no network): signed remote policy
//! fetched through the `PolicyFetcher` port, the OTLP audit exporter, `doctor.run` and
//! `diagnostics.collect`.

#![allow(clippy::unwrap_used, clippy::expect_used)] // test code

use std::sync::{Arc, Mutex};
use std::time::Duration;

use base64::Engine;
use ed25519_dalek::{Signer, SigningKey};
use genie_audit::{Attrs, AuditContext, AuditEvent, Auditor, OtlpSink, OtlpTransport};
use genie_client::Endpoint;
use genie_client::connection::{ClientInfo, Connection};
use genie_config::policy::{PolicyManager, PolicySourceKind, StaticPolicySource, parse_policy};
use genie_core::config::ConfigHub;
use genie_core::{EngineBuilder, TaskScope};
use genie_daemon::{
    DaemonConfig, DaemonState, HttpOtlp, HttpPolicyFetcher, Registry, Services, serve_stream,
};
use genie_protocol::Channel;
use genie_protocol::api::daemon::{CheckStatus, DiagnosticsParams};
use genie_protocol::common::Empty;
use genie_protocol::handshake::{ClientCapabilities, ClientKind};
use genie_protocol::methods as m;
use serde_json::json;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

const TOKEN: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

#[derive(Default)]
struct Seen {
    requests: Mutex<Vec<(String, String)>>,
}

/// Minimal HTTP/1.1 server: `GET /policy.json`, `GET /policy.json.sig`, `POST /v1/logs`.
async fn serve(policy: String, sig: String, seen: Arc<Seen>) -> u16 {
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = l.local_addr().unwrap().port();
    tokio::spawn(async move {
        loop {
            let Ok((mut s, _)) = l.accept().await else {
                return;
            };
            let (policy, sig, seen) = (policy.clone(), sig.clone(), seen.clone());
            tokio::spawn(async move {
                let mut buf = Vec::new();
                let mut chunk = [0u8; 8192];
                let (head, body) = loop {
                    let n = s.read(&mut chunk).await.unwrap_or(0);
                    if n == 0 {
                        return;
                    }
                    buf.extend_from_slice(&chunk[..n]);
                    if let Some(i) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
                        let head = String::from_utf8_lossy(&buf[..i]).to_string();
                        let len = head
                            .lines()
                            .find_map(|l| {
                                let (k, v) = l.split_once(':')?;
                                k.eq_ignore_ascii_case("content-length")
                                    .then(|| v.trim().parse::<usize>().ok())?
                            })
                            .unwrap_or(0);
                        while buf.len() < i + 4 + len {
                            let n = s.read(&mut chunk).await.unwrap_or(0);
                            if n == 0 {
                                break;
                            }
                            buf.extend_from_slice(&chunk[..n]);
                        }
                        break (head, String::from_utf8_lossy(&buf[i + 4..]).to_string());
                    }
                };
                let path = head.split_whitespace().nth(1).unwrap_or("").to_string();
                seen.requests
                    .lock()
                    .unwrap()
                    .push((path.clone(), format!("{head}\n\n{body}")));
                let (status, out) = match path.as_str() {
                    "/policy.json" => ("200 OK", policy),
                    "/policy.json.sig" => ("200 OK", sig),
                    "/v1/logs" => ("200 OK", "{}".to_string()),
                    _ => ("404 Not Found", String::new()),
                };
                let resp = format!(
                    "HTTP/1.1 {status}\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{out}",
                    out.len()
                );
                let _ = s.write_all(resp.as_bytes()).await;
            });
        }
    });
    port
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn remote_policy_otlp_doctor_and_diagnostics() {
    let key = SigningKey::from_bytes(&[7u8; 32]);
    let policy = r#"{"org":{"name":"RemoteOrg"},"permissions":{"deny":["WebSearch"]}}"#.to_string();
    let sig =
        base64::engine::general_purpose::STANDARD.encode(key.sign(policy.as_bytes()).to_bytes());
    let seen = Arc::new(Seen::default());
    let port = serve(policy, sig, seen.clone()).await;
    let base = format!("http://127.0.0.1:{port}");

    let dir = tempfile::tempdir().unwrap();
    let mut cfg = DaemonConfig::new(Endpoint::in_dir(dir.path(), Channel::Dev).unwrap());
    cfg.engine_version = "1.0.0".into();
    cfg.write_logs = false;
    let hub = Arc::new(
        ConfigHub::new(
            dir.path().join("home/.genie"),
            dir.path().join("home/.genie/trust.json"),
            Some(dir.path().join("home").display().to_string()),
        )
        .unwrap(),
    );
    let mdm = StaticPolicySource {
        kind: PolicySourceKind::Mdm,
        values: Some(
            parse_policy(
                json!({
                    "policyUrl": format!("{base}/policy.json"),
                    "policyKeys": [hex::encode(key.verifying_key().to_bytes())],
                    "audit": {"otlp": {"endpoint": base}},
                })
                .to_string()
                .as_bytes(),
            )
            .unwrap(),
        ),
    };
    let client = reqwest::Client::new();
    let mut pm = PolicyManager::new(vec![Box::new(mdm)])
        .with_remote(Arc::new(HttpPolicyFetcher {
            client: client.clone(),
            handle: tokio::runtime::Handle::current(),
        }))
        .with_remote_cache(dir.path().join("policy-remote.json"));
    pm.refresh_local();
    hub.set_policy(pm.effective());

    let sink = Arc::new(OtlpSink::default());
    let transport: Arc<dyn OtlpTransport> = Arc::new(HttpOtlp::new(
        client,
        &base,
        vec![("x-collector-key".into(), "k1".into())],
    ));
    let audit = Arc::new(
        Auditor::new(AuditContext {
            install_id: "inst".into(),
            user: "alice".into(),
            org: None,
            include_content: false,
        })
        .with_sink(sink.clone()),
    );
    let root = TaskScope::root();
    let services = Services {
        hub: Some(hub.clone()),
        policy: Some(Mutex::new(pm)),
        audit: Some(audit.clone()),
        otlp: Some((sink.clone(), transport)),
        ..Services::default()
    };
    let engine = EngineBuilder::new().build(&root);
    let state = Arc::new(DaemonState::with_services(
        cfg,
        engine,
        TOKEN.into(),
        root,
        Arc::new(Registry::default()),
        None,
        Some(Arc::new(services)),
    ));
    state.start_background();

    // Remote policy fetched at start, verified, merged.
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    while hub.policy().org_name() != Some("RemoteOrg") {
        assert!(
            tokio::time::Instant::now() < deadline,
            "remote policy not applied"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert_eq!(hub.policy().strings("permissions.deny"), vec!["WebSearch"]);
    assert_eq!(hub.policy().source("org.name"), Some("remote"));
    assert!(
        dir.path().join("policy-remote.json").exists(),
        "last good copy cached"
    );

    // OTLP export of an audit record, with the keychain headers.
    audit.record(
        Some("s1"),
        AuditEvent::ToolCalled,
        Attrs::new().with("tool", "shell").with("outcome", "ok"),
    );
    let deadline = tokio::time::Instant::now() + Duration::from_secs(15);
    let logs = loop {
        let got = seen
            .requests
            .lock()
            .unwrap()
            .iter()
            .find(|(p, _)| p == "/v1/logs")
            .map(|(_, r)| r.clone());
        if let Some(r) = got {
            break r;
        }
        assert!(tokio::time::Instant::now() < deadline, "no OTLP export");
        tokio::time::sleep(Duration::from_millis(100)).await;
    };
    assert!(logs.contains("x-collector-key: k1"), "{logs}");
    assert!(logs.contains("\"eventName\":\"tool.called\""), "{logs}");
    assert!(sink.is_empty());

    // doctor.run + diagnostics.collect over the protocol.
    let (c, s) = genie_testkit::memory_pair();
    let st = state.clone();
    tokio::spawn(async move { serve_stream(st, Box::new(s), TaskScope::root()).await });
    let conn = Connection::handshake(
        Box::new(c),
        &ClientInfo {
            kind: ClientKind::Cli,
            version: "1.0.0".into(),
            instance_id: "cli".into(),
            capabilities: ClientCapabilities::default(),
        },
        TOKEN.into(),
    )
    .await
    .unwrap();
    let doc = conn.call::<m::DoctorRun>(Empty {}).await.unwrap();
    let ids: Vec<&str> = doc.checks.iter().map(|c| c.id.as_str()).collect();
    for want in [
        "daemon", "versions", "database", "browser", "ripgrep", "git", "disk", "policy",
        "settings", "audit",
    ] {
        assert!(ids.contains(&want), "missing check {want}: {ids:?}");
    }
    let db = doc.checks.iter().find(|c| c.id == "database").unwrap();
    assert_eq!(db.status, CheckStatus::Fail, "no store in this test");
    let pol = doc.checks.iter().find(|c| c.id == "policy").unwrap();
    assert!(pol.detail.contains("RemoteOrg"), "{}", pol.detail);

    let diag = conn
        .call::<m::DiagnosticsCollect>(DiagnosticsParams { include_logs: true })
        .await
        .unwrap();
    assert!(diag.text.starts_with("Genie diagnostics"), "{}", diag.text);
    assert!(diag.text.contains("org.name ← remote"), "{}", diag.text);
    assert!(
        !diag.text.contains("WebSearch"),
        "policy values are not included"
    );
    assert!(diag.text.contains("doctor:"));
}
