#![allow(dead_code)]

pub mod fake_http;
pub mod fake_server;

use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;

use async_trait::async_trait;
use genie_mcp::config::{LiteralSecrets, ServerConfig};
use genie_mcp::handler::{
    ClientHandler, ElicitRequest, ElicitResponse, SamplingRefusal, SamplingRequest,
    SamplingResponse,
};
use genie_mcp::server::{Backoff, Timeouts};
use genie_mcp::transport::{ConnectEnv, Connector, TransportPair, stdio};
use genie_mcp::{McpError, McpServer, McpStatus, Runtime, ServerOptions};
use serde_json::json;
use tokio::sync::{Mutex, broadcast};
use tokio_util::sync::CancellationToken;
use tokio_util::task::TaskTracker;

pub use genie_mcp::McpEvent;

/// Connects to an in-process fake server over `tokio::io::duplex`.
pub struct DuplexConnector {
    pub opts: fake_server::Options,
    pub connects: AtomicU32,
    /// Refuse this many connection attempts first (transient failures).
    pub fail_first: Mutex<u32>,
    pub fakes: Mutex<Vec<Arc<fake_server::Fake>>>,
}

impl DuplexConnector {
    pub fn new(opts: fake_server::Options) -> Arc<DuplexConnector> {
        Arc::new(DuplexConnector {
            opts,
            connects: AtomicU32::new(0),
            fail_first: Mutex::new(0),
            fakes: Mutex::new(Vec::new()),
        })
    }
}

#[async_trait]
impl Connector for DuplexConnector {
    async fn connect(
        &self,
        _cfg: &ServerConfig,
        env: &ConnectEnv,
    ) -> Result<TransportPair, McpError> {
        self.connects.fetch_add(1, Ordering::SeqCst);
        let refuse = {
            let mut n = self.fail_first.lock().await;
            let r = *n > 0;
            *n = n.saturating_sub(1);
            r
        };
        if refuse {
            return Err(McpError::Transport("connection refused".into()));
        }
        let (client_side, server_side) = tokio::io::duplex(1 << 20);
        let (sr, sw) = tokio::io::split(server_side);
        let fake = fake_server::Fake::new(self.opts.clone());
        self.fakes.lock().await.push(fake.clone());
        env.tasks.spawn(async move {
            fake_server::serve_with(fake, sr, sw).await;
        });
        let (cr, cw) = tokio::io::split(client_side);
        Ok(stdio::from_streams(
            cr,
            cw,
            &env.tasks,
            env.logs.clone(),
            env.parent.child_token(),
            None,
        ))
    }
}

/// Accepts elicitations with `{name: "bob"}` and samples "summary" for 10 tokens.
pub struct TestHandler;

#[async_trait]
impl ClientHandler for TestHandler {
    async fn elicit(
        &self,
        _server: &str,
        req: ElicitRequest,
        _c: CancellationToken,
    ) -> ElicitResponse {
        assert_eq!(req.sensitive_fields, vec!["token".to_string()]);
        ElicitResponse::accept(json!({"name": "bob"}))
    }

    async fn sample(
        &self,
        _server: &str,
        _req: SamplingRequest,
        _left: u64,
        _c: CancellationToken,
    ) -> Result<SamplingResponse, SamplingRefusal> {
        Ok(SamplingResponse {
            model: "test-model".into(),
            content: json!({"type": "text", "text": "summary"}),
            stop_reason: Some("endTurn".into()),
            tokens_used: 10,
        })
    }
}

pub fn runtime(connector: Arc<dyn Connector>) -> Runtime {
    Runtime {
        tasks: TaskTracker::new(),
        cancel: CancellationToken::new(),
        connector,
        handler: Arc::new(TestHandler),
        auth: Arc::new(genie_mcp::auth::NoAuth),
        secrets: Arc::new(LiteralSecrets),
        client_name: "genie-test".into(),
        client_version: "0.0.0".into(),
    }
}

pub fn fast_opts() -> ServerOptions {
    ServerOptions {
        timeouts: Timeouts {
            startup: Duration::from_secs(5),
            list: Duration::from_secs(5),
            tool: Duration::from_secs(10),
            request: Duration::from_secs(5),
        },
        backoff: Backoff {
            initial: Duration::from_millis(50),
            max: Duration::from_millis(200),
            max_attempts: 4,
            stable_after: Duration::from_secs(60),
            connect_retry: Duration::from_millis(50),
            debounce: Duration::from_millis(50),
        },
        ..ServerOptions::default()
    }
}

pub fn stdio_cfg() -> ServerConfig {
    ServerConfig::Stdio {
        command: "unused".into(),
        args: vec![],
        env: Default::default(),
        cwd: None,
    }
}

pub fn start(
    name: &str,
    cfg: ServerConfig,
    opts: ServerOptions,
    rt: Runtime,
) -> (McpServer, broadcast::Receiver<McpEvent>) {
    let (tx, rx) = broadcast::channel(256);
    let s = McpServer::start(name, genie_mcp::McpScope::User, Ok(cfg), opts, rt, tx);
    (s, rx)
}

pub async fn connected(s: &McpServer) {
    let st = s
        .wait_status(Duration::from_secs(10), |st| st == McpStatus::Connected)
        .await;
    assert_eq!(
        st,
        McpStatus::Connected,
        "error: {:?}; logs: {:#?}",
        s.error(),
        s.logs(50)
    );
}

/// Wait for an event matching `f`.
pub async fn event(
    rx: &mut broadcast::Receiver<McpEvent>,
    f: impl Fn(&McpEvent) -> bool,
) -> McpEvent {
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            match rx.recv().await {
                Ok(e) if f(&e) => return e,
                Ok(_) | Err(broadcast::error::RecvError::Lagged(_)) => {}
                Err(e) => panic!("event channel: {e}"),
            }
        }
    })
    .await
    .expect("event in time")
}
