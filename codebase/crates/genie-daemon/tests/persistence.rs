//! Data survives a daemon restart (04 §2, §3.2): the session list comes back from the
//! store, incognito sessions don't, and a missing DB key is reported to clients and
//! fixed only on request (`storage.start_fresh`), never silently.

#![allow(clippy::unwrap_used, clippy::expect_used)] // test code

use std::sync::Arc;
use std::time::Duration;

use genie_client::connection::{ClientInfo, Connection};
use genie_client::{Endpoint, EnsureOptions, EnsureOutcome, ensure};
use genie_daemon::{DaemonConfig, ShutdownReason};
use genie_protocol::handshake::{ClientCapabilities, ClientKind};
use genie_protocol::{Channel, methods};
use genie_secrets::{MemoryBackend, SecretBackend};
use serde_json::{Value, json};

fn cfg(dir: &std::path::Path, keys: Arc<dyn SecretBackend>) -> DaemonConfig {
    let mut cfg = DaemonConfig::new(Endpoint::in_dir(dir, Channel::Dev).unwrap());
    cfg.idle_timeout = Duration::from_secs(60);
    cfg.write_logs = false;
    // Never the real OS keychain in automated tests.
    cfg.secrets = genie_daemon::SecretsChoice::Backend(keys);
    cfg
}

async fn connect(ep: &Endpoint) -> Connection {
    let info = ClientInfo {
        kind: ClientKind::Cli,
        version: env!("CARGO_PKG_VERSION").into(),
        instance_id: "cli_test".into(),
        capabilities: ClientCapabilities::default(),
    };
    let opts = EnsureOptions::new(ep.clone(), info, None, vec![]);
    match ensure(&opts, |_| {}).await.unwrap() {
        EnsureOutcome::Ready(c) => c,
        _ => panic!("expected Ready"),
    }
}

/// Start a daemon, hand `f` a connection, then restart it and wait for the exit.
async fn with_daemon<F, Fut>(cfg: &DaemonConfig, f: F)
where
    F: FnOnce(Connection) -> Fut,
    Fut: std::future::Future<Output = Connection>,
{
    let ep = cfg.endpoint.clone();
    let daemon = tokio::spawn(genie_daemon::run(cfg.clone()));
    let conn = f(connect(&ep).await).await;
    let _: Result<Value, _> = conn
        .request(methods::DAEMON_RESTART, json!({"stop_running": true}))
        .await;
    conn.close().await;
    let reason = tokio::time::timeout(Duration::from_secs(10), daemon)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert_eq!(reason, ShutdownReason::Restart);
}

async fn create(conn: &Connection, incognito: bool) -> String {
    let r: Value = conn
        .request(
            "session.create",
            json!({"kind": "chat", "model": "test/model", "incognito": incognito}),
        )
        .await
        .unwrap();
    r["session_id"].as_str().unwrap().to_string()
}

async fn list(conn: &Connection) -> Vec<Value> {
    let r: Value = conn.request("session.list", json!({})).await.unwrap();
    r["items"].as_array().unwrap().clone()
}

#[tokio::test]
async fn sessions_survive_a_restart() {
    let dir = tempfile::tempdir().unwrap();
    let keys: Arc<dyn SecretBackend> = Arc::new(MemoryBackend::new());
    let cfg = cfg(dir.path(), keys);

    let ids = Arc::new(std::sync::Mutex::new((String::new(), String::new())));
    let ids2 = ids.clone();
    with_daemon(&cfg, |conn| async move {
        assert!(conn.init.storage.is_none(), "store must open");
        let kept = create(&conn, false).await;
        let _: Value = conn
            .request(
                "session.update",
                json!({"session_id": kept, "title": "Kept across restarts", "pinned": true}),
            )
            .await
            .unwrap();
        let hidden = create(&conn, true).await;
        *ids2.lock().unwrap() = (kept, hidden);
        conn
    })
    .await;

    let (kept, hidden) = ids.lock().unwrap().clone();
    with_daemon(&cfg, |conn| async move {
        assert!(conn.init.storage.is_none());
        let items = list(&conn).await;
        assert_eq!(
            items.len(),
            1,
            "only the normal session comes back: {items:?}"
        );
        assert_eq!(items[0]["session_id"], kept.as_str());
        assert_eq!(items[0]["title"], "Kept across restarts");
        assert_eq!(items[0]["pinned"], true);
        let got: Value = conn
            .request("session.get", json!({"session_id": kept}))
            .await
            .unwrap();
        assert_eq!(got["session"]["model"], "test/model");
        // The incognito session is gone for good.
        let e: Result<Value, _> = conn
            .request("session.get", json!({"session_id": hidden}))
            .await;
        assert!(e.is_err());
        // Deleting is a real delete: it does not come back either.
        let _: Value = conn
            .request("session.delete", json!({"session_id": kept}))
            .await
            .unwrap();
        conn
    })
    .await;

    with_daemon(&cfg, |conn| async move {
        assert!(list(&conn).await.is_empty());
        conn
    })
    .await;
}

#[tokio::test]
async fn missing_key_is_reported_and_start_fresh_sets_data_aside() {
    let dir = tempfile::tempdir().unwrap();
    // A DB whose key lives in a key store this daemon will not see. (Written directly:
    // an in-process daemon keeps its store open after `run` returns, which Windows
    // won't let us rename.)
    {
        let opts = genie_store::StoreOptions::new(dir.path(), "genie-dev.db");
        let lost = genie_store::MemoryKeyStore::new();
        let store = genie_store::Store::open_with_keystore(&opts, &lost).unwrap();
        store
            .write(|tx| genie_store::sessions::ensure(tx, "ses_old"))
            .unwrap();
    }

    // Same data dir, but a key store without the DB key (e.g. another build's keychain).
    let other: Arc<dyn SecretBackend> = Arc::new(MemoryBackend::new());
    let second = cfg(dir.path(), other.clone());
    with_daemon(&second, |conn| async move {
        let p = conn.init.storage.clone().expect("storage problem reported");
        assert_eq!(p.reason, "key_missing");
        assert!(!p.recovery_available);
        // A wrong recovery key changes nothing.
        let e: Result<Value, _> = conn
            .request("storage.recover", json!({"recovery_key": "not-a-key"}))
            .await;
        assert!(e.is_err());
        conn
    })
    .await;
    // Still there: nothing is moved without the user asking.
    assert!(dir.path().join("genie-dev.db").exists());

    let ep = second.endpoint.clone();
    let daemon = tokio::spawn(genie_daemon::run(second.clone()));
    let conn = connect(&ep).await;
    let _: Value = conn
        .request("storage.start_fresh", json!({}))
        .await
        .unwrap();
    conn.close().await;
    let reason = tokio::time::timeout(Duration::from_secs(10), daemon)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert_eq!(reason, ShutdownReason::Restart);
    let set_aside = std::fs::read_dir(dir.path()).unwrap().flatten().any(|e| {
        e.file_name()
            .to_string_lossy()
            .starts_with("genie-dev.db.unreadable-")
    });
    assert!(set_aside, "old DB renamed, not deleted");

    with_daemon(&second, |conn| async move {
        assert!(conn.init.storage.is_none(), "a new DB opens");
        assert!(list(&conn).await.is_empty());
        conn
    })
    .await;
}
