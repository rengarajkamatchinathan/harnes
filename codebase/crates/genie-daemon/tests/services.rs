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
    let skills = genie_core::runtime::skills::SkillsHub::new(
        dir.path().join("home/.genie"),
        hub.clone(),
        registry.clone(),
    );
    let tasks = tokio_util::task::TaskTracker::new();
    let (_fake, wire) = genie_browser::testing::FakeCdp::new(&tasks);
    let client =
        genie_browser::CdpClient::connect_with_timeout(wire, &tasks, Duration::from_secs(5));
    let browser = genie_browser::Browser::new(
        Arc::new(genie_browser::testing::FakeBackend::new(client)),
        Arc::new(genie_browser::NoHost),
        Arc::new(genie_browser::SitePolicy::new()),
        genie_browser::BrowserConfig::default(),
        tasks,
    );
    let runtime = RuntimeBuilder::new(registry.clone(), Arc::new(NoModels))
        .skills(skills.clone())
        .browser(&browser)
        .build();
    let engine = EngineBuilder::new()
        .turn_runner(Arc::new(Runner))
        .build(&root);
    let services = Services {
        runtime: Some(runtime.runtime().clone()),
        skills: Some(skills),
        hub: Some(hub.clone()),
        browser: Some(browser),
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
    assert!(names.contains(&"skill"), "{names:?}");
    let sk = c.conn.call::<m::SkillList>(Empty {}).await.unwrap();
    assert!(sk.skills.iter().any(|s| s.id == "write-spec" && s.enabled));
    c.conn
        .call::<m::SkillSetEnabled>(genie_protocol::api::mcp::SkillSetEnabledParams {
            id: "write-spec".into(),
            enabled: false,
        })
        .await
        .unwrap();
    let sk = c.conn.call::<m::SkillList>(Empty {}).await.unwrap();
    assert!(sk.skills.iter().any(|s| s.id == "write-spec" && !s.enabled));
    // The skills directory feed is not available yet: a clear error, not a hang.
    let e = c
        .conn
        .call::<m::DirectoryList>(genie_protocol::api::mcp::DirectoryListParams {
            kind: genie_protocol::api::mcp::DirectoryKind::Skills,
            query: None,
        })
        .await
        .unwrap_err();
    assert!(matches!(e, ClientError::Rpc(ref r) if r.code == ErrorKind::InvalidState.code()));
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

async fn names(conn: &Connection, sid: String) -> Vec<String> {
    conn.call::<m::ToolsList>(SessionIdParams { session_id: sid })
        .await
        .unwrap()
        .tools
        .into_iter()
        .map(|t| t.name)
        .collect()
}

async fn create(conn: &Connection, overlay: Option<SessionSettingsOverlay>) -> String {
    conn.call::<m::SessionCreate>(CreateParams {
        kind: SessionKind::Chat,
        model: "m".into(),
        settings: overlay,
        ..Default::default()
    })
    .await
    .unwrap()
    .session_id
}

#[tokio::test]
async fn browser_tools_are_off_in_headless_sessions_and_sites_persist() {
    let mem = daemon();
    let head = connect(&mem, "head", ClientKind::Headless, vec![]).await;
    let sid = create(&head.conn, None).await;
    assert!(
        !names(&head.conn, sid)
            .await
            .iter()
            .any(|n| n == "browser_open")
    );
    let sid = create(
        &head.conn,
        Some(SessionSettingsOverlay {
            enable_browser: true,
            ..Default::default()
        }),
    )
    .await;
    assert!(
        names(&head.conn, sid)
            .await
            .iter()
            .any(|n| n == "browser_open")
    );
    let cli = connect(&mem, "cli", ClientKind::Cli, vec![AskKind::Permission]).await;
    let sid = create(&cli.conn, None).await;
    assert!(
        names(&cli.conn, sid)
            .await
            .iter()
            .any(|n| n == "browser_open")
    );

    cli.conn
        .call::<m::BrowserSitesSet>(genie_protocol::api::browser::SitePermission {
            origin: "shop.example".into(),
            state: genie_protocol::api::browser::SiteState::Read,
        })
        .await
        .unwrap();
    let list = cli
        .conn
        .call::<m::BrowserSitesList>(Empty {})
        .await
        .unwrap();
    assert!(list.sites.iter().any(|s| s.origin == "shop.example"));
    let got = cli
        .conn
        .call::<m::ConfigGet>(ConfigGetParams {
            keys: Some(vec!["browser.sites".into()]),
            ..Default::default()
        })
        .await
        .unwrap();
    assert!(
        got.values["browser.sites"]
            .to_string()
            .contains("shop.example")
    );
    let st = cli
        .conn
        .call::<m::BrowserStatusMethod>(Empty {})
        .await
        .unwrap();
    assert!(st.running);
}
