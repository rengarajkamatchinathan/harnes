//! Daemon contract tests: handshake and skew over the in-memory transport, lifecycle over
//! the real socket / named pipe.

#![allow(clippy::unwrap_used, clippy::expect_used)] // test code

use std::sync::Arc;
use std::time::Duration;

use genie_client::connection::{ClientError, ClientInfo, Connection, Incoming};
use genie_client::{Endpoint, EnsureOptions, EnsureOutcome, ensure};
use genie_core::{EngineBuilder, TaskScope};
use genie_daemon::{DaemonConfig, DaemonError, DaemonState, ShutdownReason, serve_stream};
use genie_protocol::handshake::{ClientCapabilities, ClientKind};
use genie_protocol::methods::{self, DaemonStatus};
use genie_protocol::{Channel, ErrorKind};
use serde_json::{Value, json};

fn info(version: &str) -> ClientInfo {
    ClientInfo {
        kind: ClientKind::Cli,
        version: version.into(),
        instance_id: "cli_test".into(),
        capabilities: ClientCapabilities::default(),
    }
}

struct Mem {
    state: Arc<DaemonState>,
    _dir: tempfile::TempDir,
}

const TOKEN: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

fn mem_daemon() -> Mem {
    let dir = tempfile::tempdir().unwrap();
    let mut cfg = DaemonConfig::new(Endpoint::in_dir(dir.path(), Channel::Dev).unwrap());
    cfg.engine_version = "1.0.0".into();
    cfg.write_logs = false;
    let root = TaskScope::root();
    let engine = EngineBuilder::new().build(&root);
    Mem {
        state: Arc::new(DaemonState::new(cfg, engine, TOKEN.into(), root.clone())),
        _dir: dir,
    }
}

async fn mem_connect(m: &Mem, version: &str, token: &str) -> Result<Connection, ClientError> {
    let (client, server) = genie_testkit::memory_pair();
    let st = m.state.clone();
    tokio::spawn(async move { serve_stream(st, Box::new(server), TaskScope::root()).await });
    Connection::handshake(Box::new(client), &info(version), token.into()).await
}

fn kind(e: ClientError) -> ErrorKind {
    match e {
        ClientError::Rpc(e) => e.kind(),
        other => panic!("expected rpc error, got {other:?}"),
    }
}

#[tokio::test]
async fn handshake_status_and_unknown_method() {
    let m = mem_daemon();
    let conn = mem_connect(&m, "1.0.0", TOKEN).await.unwrap();
    assert_eq!(conn.init.engine_version, "1.0.0");
    assert_eq!(conn.init.restart_pending, None);
    let st: DaemonStatus = conn
        .request(methods::DAEMON_STATUS, Value::Null)
        .await
        .unwrap();
    assert_eq!(st.clients.len(), 1);
    assert_eq!(st.clients[0].client_instance_id, "cli_test");
    let _: Value = conn.request(methods::PING, Value::Null).await.unwrap();
    let e = conn
        .request::<_, Value>("nope.nothing", Value::Null)
        .await
        .unwrap_err();
    assert_eq!(kind(e), ErrorKind::MethodNotFound);
    let e = conn
        .request::<_, Value>(methods::INITIALIZE, json!({}))
        .await
        .unwrap_err();
    assert_eq!(kind(e), ErrorKind::InvalidRequest);
    conn.close().await;
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert_eq!(m.state.client_count(), 0);
}

#[tokio::test]
async fn bad_token_is_refused() {
    let m = mem_daemon();
    let e = mem_connect(&m, "1.0.0", "wrong").await.unwrap_err();
    assert_eq!(kind(e), ErrorKind::Unauthorized);
    let e = mem_connect(&m, "1.0.0", &TOKEN[..63]).await.unwrap_err();
    assert_eq!(kind(e), ErrorKind::Unauthorized);
}

#[tokio::test]
async fn first_request_must_be_initialize() {
    let m = mem_daemon();
    let (client, server) = genie_testkit::memory_pair();
    let st = m.state.clone();
    tokio::spawn(async move { serve_stream(st, Box::new(server), TaskScope::root()).await });
    let (mut r, mut w) = genie_client::framing::split(client);
    w.send_raw_line(r#"{"jsonrpc":"2.0","id":5,"method":"daemon.status"}"#)
        .await
        .unwrap();
    let Some(genie_protocol::Message::Response(resp)) = r.next().await.unwrap() else {
        panic!()
    };
    let genie_protocol::Outcome::Error(e) = resp.outcome else {
        panic!()
    };
    assert_eq!(e.kind(), ErrorKind::InvalidRequest);
    assert!(r.next().await.unwrap().is_none(), "connection closed");
}

#[tokio::test]
async fn newer_client_restarts_idle_daemon() {
    let m = mem_daemon();
    let conn = mem_connect(&m, "1.0.1", TOKEN).await.unwrap();
    assert_eq!(conn.init.restart_pending, Some(true));
    let reason = tokio::time::timeout(Duration::from_secs(1), m.state.wait_shutdown())
        .await
        .unwrap();
    assert_eq!(reason, ShutdownReason::Restart);
}

#[tokio::test]
async fn older_client_same_major_proceeds() {
    let m = mem_daemon();
    let conn = mem_connect(&m, "0.9.0", TOKEN).await.unwrap();
    assert_eq!(conn.init.restart_pending, None);
}

struct Blocking;

impl genie_core::TurnRunner for Blocking {
    fn run(
        &self,
        t: genie_core::TurnCtx,
    ) -> futures::future::BoxFuture<'static, Result<(), genie_core::TurnError>> {
        Box::pin(async move {
            t.token().cancelled().await;
            Err(genie_core::TurnError::Cancelled)
        })
    }
}

#[tokio::test]
async fn restart_is_refused_while_busy_unless_stop_running() {
    let dir = tempfile::tempdir().unwrap();
    let mut cfg = DaemonConfig::new(Endpoint::in_dir(dir.path(), Channel::Dev).unwrap());
    cfg.engine_version = "1.0.0".into();
    let root = TaskScope::root();
    let engine = EngineBuilder::new()
        .turn_runner(Arc::new(Blocking))
        .build(&root);
    let m = Mem {
        state: Arc::new(DaemonState::new(cfg, engine, TOKEN.into(), root)),
        _dir: dir,
    };
    let conn = mem_connect(&m, "1.0.0", TOKEN).await.unwrap();

    let s = m.state.engine().create_session();
    s.prompt("work", genie_protocol::session::Delivery::Steer)
        .unwrap();
    let mut st = s.watch_status();
    st.wait_for(|s| *s == genie_protocol::session::LiveStatus::Running)
        .await
        .unwrap();

    // A newer client while busy is served degraded rather than restarting the engine.
    let degraded = mem_connect(&m, "1.0.1", TOKEN).await.unwrap();
    assert_eq!(degraded.init.restart_pending, None);
    let status: DaemonStatus = conn
        .request(methods::DAEMON_STATUS, Value::Null)
        .await
        .unwrap();
    assert_eq!(status.sessions_running, 1);
    assert!(status.clients.iter().any(|c| c.degraded));

    let e = conn
        .request::<_, Value>(methods::DAEMON_RESTART, json!({}))
        .await
        .unwrap_err();
    assert_eq!(kind(e), ErrorKind::Busy);
    let _: Value = conn
        .request(methods::DAEMON_RESTART, json!({"stop_running": true}))
        .await
        .unwrap();
    assert_eq!(m.state.wait_shutdown().await, ShutdownReason::Restart);
}

// ---- Real endpoint (Unix socket / named pipe) --------------------------------------

fn real_cfg(dir: &std::path::Path, idle: Duration) -> DaemonConfig {
    let mut cfg = DaemonConfig::new(Endpoint::in_dir(dir, Channel::Dev).unwrap());
    cfg.idle_timeout = idle;
    cfg.write_logs = false;
    cfg
}

async fn ensure_ready(ep: &Endpoint) -> Connection {
    let opts = EnsureOptions::new(ep.clone(), info(env!("CARGO_PKG_VERSION")), None, vec![]);
    match ensure(&opts, |_| {}).await.unwrap() {
        EnsureOutcome::Ready(c) => c,
        _ => panic!("expected Ready"),
    }
}

#[tokio::test]
async fn lifecycle_over_real_endpoint() {
    let dir = tempfile::tempdir().unwrap();
    let cfg = real_cfg(dir.path(), Duration::from_millis(400));
    let ep = cfg.endpoint.clone();
    let daemon = tokio::spawn(genie_daemon::run(cfg.clone()));

    // ensure without spawning waits for the daemon to come up (WaitReady → Connect).
    let conn = ensure_ready(&ep).await;
    let st: DaemonStatus = conn
        .request(methods::DAEMON_STATUS, Value::Null)
        .await
        .unwrap();
    assert_eq!(st.channel, Channel::Dev);
    assert_eq!(st.pid, std::process::id());
    assert!(ep.lock_held());

    // A second daemon on the same endpoint loses the lock race.
    match genie_daemon::run(cfg.clone()).await {
        Err(DaemonError::AlreadyRunning) => {}
        other => panic!("expected AlreadyRunning, got {other:?}"),
    }

    // A client with a wrong token is refused.
    let raw = genie_client::framing::connect(&ep.address).await.unwrap();
    let e = Connection::handshake(raw, &info(env!("CARGO_PKG_VERSION")), "bad".into())
        .await
        .unwrap_err();
    assert_eq!(kind(e), ErrorKind::Unauthorized);

    // While a client is attached the daemon stays up past the idle timeout.
    tokio::time::sleep(Duration::from_millis(900)).await;
    assert!(!daemon.is_finished());

    // Disconnect → idle exit, endpoint files cleaned up.
    conn.close().await;
    let reason = tokio::time::timeout(Duration::from_secs(5), daemon)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert_eq!(reason, ShutdownReason::Idle);
    assert!(!ep.token_path().exists());
    assert!(!ep.lock_held());
    assert!(genie_client::framing::connect(&ep.address).await.is_err());
}

#[tokio::test]
async fn restart_notifies_clients_then_exits() {
    let dir = tempfile::tempdir().unwrap();
    let cfg = real_cfg(dir.path(), Duration::from_secs(60));
    let ep = cfg.endpoint.clone();
    let daemon = tokio::spawn(genie_daemon::run(cfg));
    let watcher = ensure_ready(&ep).await;
    let mut incoming = watcher.take_incoming().unwrap();
    let admin = ensure_ready(&ep).await;

    let _: Value = admin
        .request(methods::DAEMON_RESTART, json!({"stop_running": false}))
        .await
        .unwrap();
    let n = tokio::time::timeout(Duration::from_secs(5), incoming.recv())
        .await
        .unwrap()
        .unwrap();
    match n {
        Incoming::Notification(n) => {
            assert_eq!(n.method, methods::DAEMON_RESTARTING);
            assert_eq!(n.params["reason"], "restart");
        }
        other => panic!("unexpected {other:?}"),
    }
    tokio::time::timeout(Duration::from_secs(5), watcher.closed())
        .await
        .expect("EOF seen");
    let reason = tokio::time::timeout(Duration::from_secs(10), daemon)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert_eq!(reason, ShutdownReason::Restart);
}

#[tokio::test]
async fn ensure_times_out_when_nothing_starts() {
    let dir = tempfile::tempdir().unwrap();
    let ep = Endpoint::in_dir(dir.path(), Channel::Dev).unwrap();
    let mut opts = EnsureOptions::new(ep, info("1.0.0"), None, vec![]);
    opts.ready_timeout = Duration::from_millis(300);
    let started = std::time::Instant::now();
    let r = ensure(&opts, |_| {}).await;
    assert!(matches!(r, Err(ClientError::Timeout(_))));
    // One automatic retry (SA-03): two windows.
    assert!(started.elapsed() >= Duration::from_millis(600));
}
