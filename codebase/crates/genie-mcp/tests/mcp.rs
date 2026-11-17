//! MCP client against fake servers: in-process (duplex), real stdio child process
//! (`genie-mcp-fake-server` via genie-process), and Streamable HTTP over a fake `HttpClient`.

#![allow(clippy::unwrap_used, clippy::expect_used)]

mod support;

use std::collections::BTreeMap;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::Duration;

use async_trait::async_trait;
use genie_mcp::auth::{AuthKey, AuthProvider};
use genie_mcp::config::{ScopedServer, ServerConfig};
use genie_mcp::transport::DefaultConnector;
use genie_mcp::{
    CallOpts, McpError, McpEvent, McpManager, McpScope, McpStatus, Part, ServerOptions,
};
use genie_secrets::{ExposeSecret, SecretString};
use serde_json::{Value, json};
use support::fake_http::FakeHttp;
use support::fake_server::Options;
use support::*;
use tokio_util::sync::CancellationToken;

fn structured(r: &genie_mcp::ToolResult) -> Value {
    match &r.parts[0] {
        Part::Json(v) => v.clone(),
        p => panic!("expected JSON part, got {p:?}"),
    }
}

#[tokio::test]
async fn handshake_tools_and_naming() {
    let conn = DuplexConnector::new(Options::default());
    let (s, _rx) = start("fake srv", stdio_cfg(), fast_opts(), runtime(conn.clone()));
    connected(&s).await;
    assert_eq!(s.instructions().as_deref(), Some("Use echo to echo."));

    // Both pages of tools/list.
    let tools = s.tools();
    assert_eq!(tools.len(), 10);
    let echo = tools.iter().find(|t| t.name == "echo").unwrap();
    assert_eq!(echo.model_name, "mcp__fake_srv__echo");
    let bad = tools.iter().find(|t| t.name == "bad.name").unwrap();
    assert_eq!(bad.model_name, "mcp__fake_srv__bad_name");
    assert_eq!(bad.input_schema["type"], "object");
    assert!(!bad.schema_warnings.is_empty());
    assert_eq!(bad.annotations.as_ref().unwrap().read_only_hint, Some(true));
    assert!(s.logs(100).iter().any(|l| l.contains("[schema]")));

    let r = s
        .call_tool("echo", json!({"text": "hi"}), CallOpts::default())
        .await
        .unwrap();
    // Structured content first, duplicate text dropped, untrusted by default.
    assert_eq!(r.parts, vec![Part::Json(json!({"text": "hi"}))]);
    assert!(r.taint.taints());
    assert!(!r.is_error);

    let r = s
        .call_tool("nope", json!({}), CallOpts::default())
        .await
        .unwrap();
    assert!(r.is_error);

    let st = s
        .call_tool("stats", Value::Null, CallOpts::default())
        .await
        .unwrap();
    assert_eq!(structured(&st)["initialized"], true);

    let info = s.info();
    assert_eq!(info.status, McpStatus::Connected);
    assert_eq!(info.tools, 10);
}

#[tokio::test]
async fn trusted_server_does_not_taint() {
    let conn = DuplexConnector::new(Options::default());
    let opts = ServerOptions {
        trusted: true,
        ..fast_opts()
    };
    let (s, _rx) = start("t", stdio_cfg(), opts, runtime(conn));
    connected(&s).await;
    let r = s
        .call_tool("echo", json!({}), CallOpts::default())
        .await
        .unwrap();
    assert!(!r.taint.taints());
}

#[tokio::test]
async fn progress_and_cancellation() {
    let conn = DuplexConnector::new(Options::default());
    let (s, _rx) = start("p", stdio_cfg(), fast_opts(), runtime(conn));
    connected(&s).await;

    let (ptx, mut prx) = tokio::sync::mpsc::unbounded_channel();
    let r = s
        .call_tool(
            "slow",
            json!({"steps": 3}),
            CallOpts {
                progress: Some(ptx),
                ..CallOpts::default()
            },
        )
        .await
        .unwrap();
    assert_eq!(r.parts, vec![Part::Text("slow done".into())]);
    let mut seen = vec![];
    while let Ok(p) = prx.try_recv() {
        seen.push(p);
    }
    assert_eq!(seen.len(), 3);
    assert_eq!(seen[2].progress, 3.0);
    assert_eq!(seen[2].total, Some(3.0));
    assert_eq!(seen[0].message.as_deref(), Some("step 1"));

    // Cancel: the call returns at once and the server gets notifications/cancelled.
    let cancel = CancellationToken::new();
    let c2 = cancel.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(100)).await;
        c2.cancel();
    });
    let err = s
        .call_tool(
            "hang",
            json!({}),
            CallOpts {
                cancel: Some(cancel),
                ..CallOpts::default()
            },
        )
        .await
        .unwrap_err();
    assert!(matches!(err, McpError::Cancelled));

    // Timeout also cancels server-side.
    let err = s
        .call_tool(
            "hang",
            json!({}),
            CallOpts {
                timeout: Some(Duration::from_millis(100)),
                ..CallOpts::default()
            },
        )
        .await
        .unwrap_err();
    assert!(matches!(err, McpError::Timeout { .. }), "{err:?}");

    tokio::time::sleep(Duration::from_millis(100)).await;
    let st = s
        .call_tool("stats", json!({}), CallOpts::default())
        .await
        .unwrap();
    assert_eq!(structured(&st)["cancelled"], 2);
}

#[tokio::test]
async fn resources_and_prompts() {
    let conn = DuplexConnector::new(Options::default());
    let (s, mut rx) = start("r", stdio_cfg(), fast_opts(), runtime(conn));
    connected(&s).await;

    let res = s.list_resources().await.unwrap();
    assert_eq!(res.len(), 2);
    assert_eq!(s.list_resource_templates().await.unwrap().len(), 1);
    let a = s.read_resource("mem://a").await.unwrap();
    assert!(a.taint.taints());
    assert!(matches!(&a.parts[0], Part::Resource { text, .. } if text == "hello from a"));
    let b = s.read_resource("mem://b").await.unwrap();
    assert!(matches!(&b.parts[0], Part::Binary { mime, .. } if mime == "image/png"));
    let err = s.read_resource("mem://zzz").await.unwrap_err();
    assert!(matches!(err, McpError::Rpc(ref e) if e.code == -32002));

    assert!(s.subscribe_resource("mem://a").await.unwrap());
    let ev = event(&mut rx, |e| matches!(e, McpEvent::ResourceUpdated { .. })).await;
    assert_eq!(
        ev,
        McpEvent::ResourceUpdated {
            server: "r".into(),
            uri: "mem://a".into()
        }
    );

    let prompts = s.list_prompts().await.unwrap();
    assert_eq!(prompts[0]["name"], "greet");
    let p = s
        .get_prompt(
            "greet",
            BTreeMap::from([("who".to_string(), "Ada".to_string())]),
        )
        .await
        .unwrap();
    assert_eq!(
        p.messages,
        vec![("user".to_string(), Part::Text("Hello, Ada".into()))]
    );
    assert!(p.taint.taints());
}

#[tokio::test]
async fn list_changed_reloads_tools() {
    let conn = DuplexConnector::new(Options::default());
    let (s, mut rx) = start("l", stdio_cfg(), fast_opts(), runtime(conn));
    connected(&s).await;
    assert!(!s.tools().iter().any(|t| t.name == "extra"));
    // Drain the initial ToolsChanged.
    event(&mut rx, |e| matches!(e, McpEvent::ToolsChanged { .. })).await;
    s.call_tool("add_tool", json!({}), CallOpts::default())
        .await
        .unwrap();
    event(&mut rx, |e| matches!(e, McpEvent::ToolsChanged { .. })).await;
    assert!(s.tools().iter().any(|t| t.name == "extra"));
}

#[tokio::test]
async fn elicitation_and_sampling_ports() {
    let conn = DuplexConnector::new(Options::default());
    let opts = ServerOptions {
        sampling_budget_tokens: Some(100),
        ..fast_opts()
    };
    let (s, _rx) = start("e", stdio_cfg(), opts, runtime(conn.clone()));
    connected(&s).await;

    let r = s
        .call_tool("ask", json!({}), CallOpts::default())
        .await
        .unwrap();
    assert_eq!(
        structured(&r),
        json!({"action": "accept", "content": {"name": "bob"}})
    );

    let r = s
        .call_tool("sample", json!({"max": 50}), CallOpts::default())
        .await
        .unwrap();
    let v = structured(&r);
    assert_eq!(v["result"]["content"]["text"], "summary");
    assert_eq!(v["result"]["model"], "test-model");
    assert_eq!(s.sampling_budget().used(), 10);

    // 90 left; asking for 95 exceeds the per-server budget.
    let r = s
        .call_tool("sample", json!({"max": 95}), CallOpts::default())
        .await
        .unwrap();
    let v = structured(&r);
    assert_eq!(v["error"]["code"], -1);
    assert!(v["error"]["message"].as_str().unwrap().contains("budget"));

    // Policy ban.
    let opts = ServerOptions {
        sampling_allowed: false,
        ..fast_opts()
    };
    let (s2, _rx) = start("e2", stdio_cfg(), opts, runtime(conn));
    connected(&s2).await;
    let r = s2
        .call_tool("sample", json!({}), CallOpts::default())
        .await
        .unwrap();
    assert!(
        structured(&r)["error"]["message"]
            .as_str()
            .unwrap()
            .contains("disabled by policy")
    );
}

#[tokio::test]
async fn crash_fails_then_reconnects() {
    let conn = DuplexConnector::new(Options::default());
    let (s, mut rx) = start("c", stdio_cfg(), fast_opts(), runtime(conn.clone()));
    connected(&s).await;
    let err = s
        .call_tool("crash", json!({}), CallOpts::default())
        .await
        .unwrap_err();
    assert!(matches!(err, McpError::Closed(_)), "{err:?}");
    event(&mut rx, |e| {
        matches!(
            e,
            McpEvent::Status {
                status: McpStatus::Failed,
                ..
            }
        )
    })
    .await;
    event(&mut rx, |e| {
        matches!(
            e,
            McpEvent::Status {
                status: McpStatus::Connected,
                ..
            }
        )
    })
    .await;
    assert_eq!(conn.connects.load(Ordering::SeqCst), 2);
    s.call_tool("echo", json!({}), CallOpts::default())
        .await
        .unwrap();
    assert!(s.logs(100).iter().any(|l| l.contains("failed")));
}

#[tokio::test]
async fn transient_connect_failures_retry_within_startup() {
    let conn = DuplexConnector::new(Options::default());
    *conn.fail_first.lock().await = 2;
    let (s, _rx) = start("t", stdio_cfg(), fast_opts(), runtime(conn.clone()));
    connected(&s).await;
    assert_eq!(conn.connects.load(Ordering::SeqCst), 3);
}

#[tokio::test]
async fn gives_up_after_max_attempts_until_restart() {
    let conn = DuplexConnector::new(Options::default());
    *conn.fail_first.lock().await = 1000;
    let mut opts = fast_opts();
    opts.timeouts.startup = Duration::from_millis(100);
    let (s, _rx) = start("g", stdio_cfg(), opts, runtime(conn.clone()));
    let st = s
        .wait_status(Duration::from_secs(5), |st| st == McpStatus::Failed)
        .await;
    assert_eq!(st, McpStatus::Failed);
    tokio::time::sleep(Duration::from_millis(1500)).await;
    let n = conn.connects.load(Ordering::SeqCst);
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert_eq!(
        n,
        conn.connects.load(Ordering::SeqCst),
        "parked after max attempts"
    );
    *conn.fail_first.lock().await = 0;
    s.restart();
    connected(&s).await;
}

#[tokio::test]
async fn version_mismatch_names_supported_versions() {
    let conn = DuplexConnector::new(Options {
        version: Some("1999-01-01".into()),
        ..Options::default()
    });
    let (s, _rx) = start("v", stdio_cfg(), fast_opts(), runtime(conn));
    let st = s
        .wait_status(Duration::from_secs(5), |st| st == McpStatus::Failed)
        .await;
    assert_eq!(st, McpStatus::Failed);
    let e = s.error().unwrap();
    assert!(e.contains("1999-01-01") && e.contains("2025-06-18"), "{e}");
}

#[tokio::test]
async fn disable_and_enable() {
    let conn = DuplexConnector::new(Options::default());
    let opts = ServerOptions {
        enabled: false,
        ..fast_opts()
    };
    let (s, _rx) = start("d", stdio_cfg(), opts, runtime(conn.clone()));
    let st = s
        .wait_status(Duration::from_secs(5), |st| st == McpStatus::Disabled)
        .await;
    assert_eq!(st, McpStatus::Disabled);
    assert_eq!(conn.connects.load(Ordering::SeqCst), 0);
    s.set_enabled(true);
    connected(&s).await;
    s.set_enabled(false);
    let st = s
        .wait_status(Duration::from_secs(5), |st| st == McpStatus::Disabled)
        .await;
    assert_eq!(st, McpStatus::Disabled);
    assert!(matches!(
        s.call_tool("echo", json!({}), CallOpts::default()).await,
        Err(McpError::NotConnected { .. })
    ));
}

#[tokio::test]
async fn manager_names_resolve_and_reconcile() {
    let conn = DuplexConnector::new(Options::default());
    let mgr = McpManager::new(runtime(conn.clone()));
    let def = |name: &str| ScopedServer {
        name: name.into(),
        scope: McpScope::User,
        config: Ok(stdio_cfg()),
    };
    // "a.b" and "a_b" sanitise to the same server part → hashed suffixes.
    mgr.apply(vec![def("a.b"), def("a_b"), def("bad")], |_| fast_opts());
    mgr.wait_ready(Duration::from_secs(5)).await;
    let list = mgr.list();
    assert_eq!(list.len(), 3);
    assert!(list.iter().all(|i| i.status == McpStatus::Connected));
    let tools = mgr.tools();
    let echoes: Vec<_> = tools.iter().filter(|t| t.name == "echo").collect();
    assert_eq!(echoes.len(), 3);
    let names: std::collections::HashSet<_> = tools.iter().map(|t| t.model_name.clone()).collect();
    assert_eq!(names.len(), tools.len(), "collision-free");
    let ab = echoes.iter().find(|t| t.server == "a.b").unwrap();
    assert!(ab.model_name.starts_with("mcp__a_b__echo_"));
    let r = mgr
        .call(&ab.model_name, json!({"x": 1}), CallOpts::default())
        .await
        .unwrap();
    assert_eq!(r.taint.server, "a.b");
    assert!(
        mgr.call("mcp__nope__x", json!({}), CallOpts::default())
            .await
            .is_err()
    );

    // Reconcile: drop one, keep the others running (no reconnect).
    let before = conn.connects.load(Ordering::SeqCst);
    mgr.apply(vec![def("a.b"), def("bad")], |_| fast_opts());
    assert_eq!(mgr.list().len(), 2);
    assert_eq!(conn.connects.load(Ordering::SeqCst), before);
    assert!(mgr.get("a_b").is_err());
    mgr.shutdown();
}

#[tokio::test]
async fn config_error_is_failed() {
    let conn = DuplexConnector::new(Options::default());
    let mgr = McpManager::new(runtime(conn));
    let defs = genie_mcp::merge_scopes([(
        McpScope::Project,
        json!({"x": {"type": "sse", "url": "https://x"}})
            .as_object()
            .unwrap()
            .clone(),
    )]);
    mgr.apply(defs, |_| fast_opts());
    let s = mgr.get("x").unwrap();
    let st = s
        .wait_status(Duration::from_secs(5), |st| st == McpStatus::Failed)
        .await;
    assert_eq!(st, McpStatus::Failed);
    assert!(s.error().unwrap().contains("SSE"));
}

// --- real child process over stdio (genie-process pipe mode) ---

fn real_stdio(extra: &[&str]) -> ServerConfig {
    let mut args = vec!["--stderr".to_string()];
    args.extend(extra.iter().map(|s| s.to_string()));
    ServerConfig::Stdio {
        command: env!("CARGO_BIN_EXE_genie-mcp-fake-server").into(),
        args,
        env: BTreeMap::from([("FAKE_ENV".to_string(), "1".to_string())]),
        cwd: None,
    }
}

#[tokio::test]
async fn stdio_process_handshake_logs_crash_reconnect() {
    let connector = Arc::new(DefaultConnector::new(FakeHttp::new(
        Options::default(),
        false,
    )));
    let (s, mut rx) = start("proc", real_stdio(&[]), fast_opts(), runtime(connector));
    connected(&s).await;
    let r = s
        .call_tool("echo", json!({"a": 1}), CallOpts::default())
        .await
        .unwrap();
    assert_eq!(structured(&r), json!({"a": 1}));
    s.call_tool("log", json!({}), CallOpts::default())
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(200)).await;
    let logs = s.logs(100);
    assert!(
        logs.iter()
            .any(|l| l.contains("[stderr] fake server starting")),
        "{logs:#?}"
    );
    assert!(
        logs.iter().any(|l| l.contains("[server:warning] careful")),
        "{logs:#?}"
    );
    assert!(logs.iter().any(|l| l.contains("started")));

    // Crash: the process exits with 3 → failed (with the exit status) → reconnect.
    let _ = s.call_tool("crash", json!({}), CallOpts::default()).await;
    let ev = event(&mut rx, |e| {
        matches!(
            e,
            McpEvent::Status {
                status: McpStatus::Failed,
                ..
            }
        )
    })
    .await;
    let McpEvent::Status { error, .. } = ev else {
        unreachable!()
    };
    assert!(error.unwrap().contains("exited"));
    connected(&s).await;
    s.call_tool("echo", json!({}), CallOpts::default())
        .await
        .unwrap();
    s.stop();
    let st = s
        .wait_status(Duration::from_secs(5), |st| st == McpStatus::Disabled)
        .await;
    assert_eq!(st, McpStatus::Disabled);
}

#[tokio::test]
async fn stdio_spawn_failure_is_failed() {
    let connector = Arc::new(DefaultConnector::new(FakeHttp::new(
        Options::default(),
        false,
    )));
    let cfg = ServerConfig::Stdio {
        command: "genie-no-such-mcp-server".into(),
        args: vec![],
        env: BTreeMap::new(),
        cwd: None,
    };
    let mut opts = fast_opts();
    opts.timeouts.startup = Duration::from_millis(200);
    let (s, _rx) = start("missing", cfg, opts, runtime(connector));
    let st = s
        .wait_status(Duration::from_secs(5), |st| st == McpStatus::Failed)
        .await;
    assert_eq!(st, McpStatus::Failed);
    assert!(s.error().unwrap().contains("couldn't start"));
}

// --- Streamable HTTP over the fake HttpClient ---

fn http_cfg() -> ServerConfig {
    ServerConfig::Http {
        url: "https://mcp.example.test/mcp".into(),
        headers: BTreeMap::from([("X-Static".to_string(), "v".to_string())]),
    }
}

async fn http_roundtrip(json_mode: bool) {
    let http = FakeHttp::new(Options::default(), json_mode);
    let connector = Arc::new(DefaultConnector::new(http.clone()));
    let (s, mut rx) = start("remote", http_cfg(), fast_opts(), runtime(connector));
    connected(&s).await;
    assert_eq!(s.tools().len(), 10);
    assert_eq!(
        http.seen_protocol_header.lock().unwrap().as_deref(),
        Some("2025-06-18")
    );
    let r = s
        .call_tool("echo", json!({"h": 1}), CallOpts::default())
        .await
        .unwrap();
    assert_eq!(structured(&r), json!({"h": 1}));
    if !json_mode {
        let (ptx, mut prx) = tokio::sync::mpsc::unbounded_channel();
        s.call_tool(
            "slow",
            json!({"steps": 2}),
            CallOpts {
                progress: Some(ptx),
                ..CallOpts::default()
            },
        )
        .await
        .unwrap();
        assert!(prx.try_recv().is_ok());
        // Server → client request on the POST's SSE stream; answer POSTed back.
        let r = s
            .call_tool("ask", json!({}), CallOpts::default())
            .await
            .unwrap();
        assert_eq!(structured(&r)["action"], "accept");
    }

    // Session expired (404) → reconnect with a new session.
    http.expire.store(true, Ordering::SeqCst);
    let _ = s.call_tool("echo", json!({}), CallOpts::default()).await;
    event(&mut rx, |e| {
        matches!(
            e,
            McpEvent::Status {
                status: McpStatus::Pending,
                ..
            }
        )
    })
    .await;
    connected(&s).await;
    assert_eq!(http.sessions.load(Ordering::SeqCst), 2);
    s.call_tool("echo", json!({}), CallOpts::default())
        .await
        .unwrap();
}

#[tokio::test]
async fn http_sse_mode() {
    http_roundtrip(false).await;
}

#[tokio::test]
async fn http_json_mode() {
    http_roundtrip(true).await;
}

#[tokio::test]
async fn http_401_is_needs_auth_then_restart_after_auth() {
    let http = FakeHttp::new(Options::default(), false);
    *http.token.lock().unwrap() = Some("good".into());
    let auth = Arc::new(TestAuth {
        token: std::sync::Mutex::new(None),
        refresh_to: None,
    });
    let connector = Arc::new(DefaultConnector::new(http.clone()));
    let mut rt = runtime(connector);
    rt.auth = auth.clone();
    let (s, _rx) = start("oauth", http_cfg(), fast_opts(), rt);
    let st = s
        .wait_status(Duration::from_secs(5), |st| st == McpStatus::NeedsAuth)
        .await;
    assert_eq!(st, McpStatus::NeedsAuth);
    assert_eq!(
        s.authorize().await.unwrap(),
        "https://auth.example.test/authorize"
    );
    // The user completes OAuth; the engine restarts the server.
    *auth.token.lock().unwrap() = Some("good".into());
    s.restart();
    connected(&s).await;
}

#[tokio::test]
async fn http_401_refreshes_once() {
    let http = FakeHttp::new(Options::default(), false);
    *http.token.lock().unwrap() = Some("fresh".into());
    let auth = Arc::new(TestAuth {
        token: std::sync::Mutex::new(Some("stale".into())),
        refresh_to: Some("fresh".into()),
    });
    let connector = Arc::new(DefaultConnector::new(http));
    let mut rt = runtime(connector);
    rt.auth = auth;
    let (s, _rx) = start("refresh", http_cfg(), fast_opts(), rt);
    connected(&s).await;
}

struct TestAuth {
    token: std::sync::Mutex<Option<String>>,
    refresh_to: Option<String>,
}

#[async_trait]
impl AuthProvider for TestAuth {
    async fn token(&self, key: &AuthKey) -> Option<SecretString> {
        assert_eq!(key.url, "https://mcp.example.test/mcp");
        self.token.lock().unwrap().clone().map(SecretString::from)
    }

    async fn refresh(
        &self,
        _key: &AuthKey,
        rejected: Option<&SecretString>,
        challenge: Option<&str>,
    ) -> Option<SecretString> {
        assert!(challenge.unwrap().contains("resource_metadata"));
        // Refresh only if the stored token is the rejected one.
        let stored = self.token.lock().unwrap().clone();
        if stored.as_deref() != rejected.map(|r| r.expose_secret()) {
            return stored.map(SecretString::from);
        }
        self.refresh_to.clone().map(SecretString::from)
    }

    async fn begin(&self, _key: &AuthKey, _c: Option<&str>) -> Result<String, String> {
        Ok("https://auth.example.test/authorize".into())
    }
}
