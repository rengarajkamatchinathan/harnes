//! Daemon methods backed by the engine subsystems (02 §6.4, §6.8): config, policy,
//! trust, `tools.list`, the workspace-feed notifications, and the "can't render this
//! ask" notification rule (02 §7 step 2). In-memory transport, real core.

#![allow(clippy::unwrap_used, clippy::expect_used)] // test code

use std::sync::Arc;
use std::time::Duration;

use futures::future::BoxFuture;
use genie_client::Endpoint;
use genie_client::connection::{ClientError, ClientInfo, Connection, Incoming};
use genie_core::config::ConfigHub;
use genie_core::runtime::{NoModels, RuntimeBuilder};
use genie_core::{EngineBuilder, TaskScope, TurnCtx, TurnError, TurnRunner};
use genie_daemon::{DaemonConfig, DaemonState, Registry, Services, serve_stream};
use genie_protocol::api::config::{
    ConfigGetParams, ConfigSetParams, PathParams, TrustDecision, TrustSetParams, TrustState,
};
use genie_protocol::api::session::{
    CreateParams, PromptParams, SessionIdParams, SessionSettingsOverlay, SubscribeParams,
};
use genie_protocol::ask::{Ask, AskKind};
use genie_protocol::common::{ConfigScope, Empty, SessionKind};
use genie_protocol::handshake::{ClientCapabilities, ClientKind};
use genie_protocol::message::InputPart;
use genie_protocol::methods as m;
use genie_protocol::methods::Method;
use genie_protocol::session::Delivery;
use genie_protocol::{Channel, ErrorKind, Notification};
use serde_json::json;
use tokio::sync::mpsc::UnboundedReceiver;

const TOKEN: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
const WAIT: Duration = Duration::from_secs(5);

struct Runner;

impl TurnRunner for Runner {
    fn run(&self, ctx: TurnCtx) -> BoxFuture<'static, Result<(), TurnError>> {
        Box::pin(async move {
            let mut a = Ask::new("", "", AskKind::Permission, "");
            a.tool = Some("shell".into());
            ctx.ask_with(a).await?;
            Ok(())
        })
    }
}

struct Mem {
    state: Arc<DaemonState>,
    dir: tempfile::TempDir,
}

fn daemon() -> Mem {
    let dir = tempfile::tempdir().unwrap();
    let mut cfg = DaemonConfig::new(Endpoint::in_dir(dir.path(), Channel::Dev).unwrap());
    cfg.engine_version = "1.0.0".into();
    cfg.write_logs = false;
    let root = TaskScope::root();
    let registry = Arc::new(Registry::default());
    let hub = Arc::new(
        ConfigHub::new(
            dir.path().join("home/.genie"),
            dir.path().join("home/.genie/trust.json"),
            Some(dir.path().join("home").display().to_string()),
        )
        .unwrap(),
    );
    let runtime = RuntimeBuilder::new(registry.clone(), Arc::new(NoModels)).build();
    let engine = EngineBuilder::new()
        .turn_runner(Arc::new(Runner))
        .build(&root);
    let services = Services {
        hub: Some(hub),
        runtime: Some(runtime.runtime().clone()),
        ..Services::default()
    };
    let state = Arc::new(DaemonState::with_services(
        cfg,
        engine,
        TOKEN.into(),
        root,
        registry,
        None,
        Some(Arc::new(services)),
    ));
    Mem { state, dir }
}

struct Client {
    conn: Connection,
    rx: UnboundedReceiver<Incoming>,
}

async fn connect(m: &Mem, instance: &str, kind: ClientKind, asks: Vec<AskKind>) -> Client {
    let (client, server) = genie_testkit::memory_pair();
    let st = m.state.clone();
    tokio::spawn(async move { serve_stream(st, Box::new(server), TaskScope::root()).await });
    let info = ClientInfo {
        kind,
        version: "1.0.0".into(),
        instance_id: instance.into(),
        capabilities: ClientCapabilities {
            asks,
            notifications: true,
            ..Default::default()
        },
    };
    let conn = Connection::handshake(Box::new(client), &info, TOKEN.into())
        .await
        .unwrap();
    let rx = conn.take_incoming().unwrap();
    Client { conn, rx }
}

impl Client {
    async fn note(&mut self, method: &str) -> Notification {
        loop {
            let i = tokio::time::timeout(WAIT, self.rx.recv())
                .await
                .expect("timed out")
                .expect("closed");
            if let Incoming::Notification(n) = i
                && n.method == method
            {
                return n;
            }
        }
    }
}

#[tokio::test]
async fn config_policy_and_trust_methods() {
    let mem = daemon();
    let mut c = connect(&mem, "desk", ClientKind::Desktop, vec![AskKind::Permission]).await;
    c.conn
        .call::<m::WorkspaceSubscribe>(Empty {})
        .await
        .unwrap();
    c.conn
        .call::<m::ConfigSet>(ConfigSetParams {
            key: "model".into(),
            value: json!("anthropic/claude-x"),
            scope: ConfigScope::User,
            cwd: None,
        })
        .await
        .unwrap();
    let n = c.note(m::ConfigChanged::NAME).await;
    assert_eq!(n.params["keys"], json!(["model"]));
    assert_eq!(n.params["scope"], "user");
    let got = c
        .conn
        .call::<m::ConfigGet>(ConfigGetParams::default())
        .await
        .unwrap();
    assert_eq!(got.values["model"], json!("anthropic/claude-x"));
    assert!(
        !c.conn
            .call::<m::ConfigSchema>(Empty {})
            .await
            .unwrap()
            .schema
            .is_null()
    );
    assert!(
        c.conn
            .call::<m::PolicyEffective>(Empty {})
            .await
            .unwrap()
            .values
            .is_empty()
    );
    // A bad value is refused with invalid_params.
    let e = c
        .conn
        .call::<m::ConfigSet>(ConfigSetParams {
            key: "theme".into(),
            value: json!(42),
            scope: ConfigScope::User,
            cwd: None,
        })
        .await
        .unwrap_err();
    assert!(matches!(e, ClientError::Rpc(ref r) if r.code == ErrorKind::InvalidParams.code()));

    let proj = mem.dir.path().join("work/proj");
    std::fs::create_dir_all(&proj).unwrap();
    let path = proj.display().to_string();
    let st = c
        .conn
        .call::<m::TrustGet>(PathParams { path: path.clone() })
        .await
        .unwrap();
    assert_eq!(st.state, TrustState::Unknown);
    c.conn
        .call::<m::TrustSet>(TrustSetParams {
            path: path.clone(),
            decision: TrustDecision::Trust,
        })
        .await
        .unwrap();
    let st = c
        .conn
        .call::<m::TrustGet>(PathParams { path: path.clone() })
        .await
        .unwrap();
    assert_eq!(st.state, TrustState::Trusted);
    let pend = c
        .conn
        .call::<m::TrustPendingDefinitions>(PathParams { path })
        .await
        .unwrap();
    assert!(pend.items.is_empty());
    // MCP is not wired in this daemon: calm not_implemented.
    let e = c
        .conn
        .call::<m::McpList>(Default::default())
        .await
        .unwrap_err();
    assert!(matches!(e, ClientError::Rpc(ref r) if r.code == ErrorKind::MethodNotFound.code()));
}

#[tokio::test]
async fn tools_list_and_session_only_settings() {
    let mem = daemon();
    let c = connect(&mem, "cli", ClientKind::Cli, vec![AskKind::Permission]).await;
    let sid = c
        .conn
        .call::<m::SessionCreate>(CreateParams {
            kind: SessionKind::Chat,
            model: "m".into(),
            settings: Some(SessionSettingsOverlay {
                allow: vec!["Bash(git status)".into()],
                ..Default::default()
            }),
            ..Default::default()
        })
        .await
        .unwrap()
        .session_id;
    let tools = c
        .conn
        .call::<m::ToolsList>(SessionIdParams {
            session_id: sid.clone(),
        })
        .await
        .unwrap()
        .tools;
    let names: Vec<&str> = tools.iter().map(|t| t.name.as_str()).collect();
    assert!(names.contains(&"tool_output_read"), "{names:?}");
    // A malformed session-only rule is refused at create time.
    let e = c
        .conn
        .call::<m::SessionCreate>(CreateParams {
            kind: SessionKind::Chat,
            model: "m".into(),
            settings: Some(SessionSettingsOverlay {
                deny: vec!["Bash(".into()],
                ..Default::default()
            }),
            ..Default::default()
        })
        .await
        .unwrap_err();
    assert!(matches!(e, ClientError::Rpc(ref r) if r.code == ErrorKind::InvalidParams.code()));
}

#[tokio::test]
async fn ask_the_attached_client_cannot_render_raises_a_notification() {
    let mem = daemon();
    let mut desk = connect(&mem, "desk", ClientKind::Desktop, vec![AskKind::Permission]).await;
    desk.conn
        .call::<m::WorkspaceSubscribe>(Empty {})
        .await
        .unwrap();
    // A headless client renders no asks; it subscribes and drives the session.
    let head = connect(&mem, "head", ClientKind::Headless, vec![]).await;
    let sid = head
        .conn
        .call::<m::SessionCreate>(CreateParams {
            kind: SessionKind::Chat,
            model: "m".into(),
            ..Default::default()
        })
        .await
        .unwrap()
        .session_id;
    head.conn
        .call::<m::SessionSubscribe>(SubscribeParams {
            session_id: sid.clone(),
            after_seq: None,
        })
        .await
        .unwrap();
    head.conn
        .call::<m::SessionPrompt>(PromptParams {
            session_id: sid.clone(),
            parts: vec![InputPart::text("go")],
            delivery: Delivery::Steer,
            parent_id: None,
            client_msg_id: "c1".into(),
        })
        .await
        .unwrap();
    let n = desk.note(m::UserNotification::NAME).await;
    assert_eq!(n.params["session_id"], json!(sid));
}
