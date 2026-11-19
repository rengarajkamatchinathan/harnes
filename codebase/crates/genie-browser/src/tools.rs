//! The 18 `browser_*` tools (06 §10, research §5). Every action returns a status
//! (`ok | blocked | needs_confirmation | user_in_control | unknown`), the URL and title,
//! and a compact snapshot diff framed as untrusted web content.

use std::borrow::Cow;
use std::sync::Arc;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use genie_tools::{
    Availability, Tool, ToolCtx, ToolEnv, ToolError, ToolKind, ToolOutput, parse_input, schema_of,
};
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::{Value, json};

use crate::input::{self, KeyDef};
use crate::read::markdown;
use crate::sensitive::commit_verb;
use crate::session::{
    Browser, Op, Outcome, Status, Stop, TabHandle, cdp_stop, conclude_early, decode_b64,
};
use crate::site::Need;
use crate::snapshot::{Mode, READ_CHARS, SNAPSHOT_CHARS};

/// Every browser tool, sharing one [`Browser`].
pub fn tools(browser: &Arc<Browser>) -> Vec<Arc<dyn Tool>> {
    Which::ALL
        .iter()
        .map(|w| {
            Arc::new(BrowserTool {
                b: browser.clone(),
                which: *w,
                schema: w.schema(),
            }) as Arc<dyn Tool>
        })
        .collect()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Which {
    Open,
    Navigate,
    Snapshot,
    Read,
    Find,
    Screenshot,
    Click,
    Type,
    FillForm,
    Select,
    PressKey,
    Hover,
    Scroll,
    Wait,
    Tabs,
    Upload,
    Downloads,
    Handoff,
}

impl Which {
    pub const ALL: [Which; 18] = [
        Which::Open,
        Which::Navigate,
        Which::Snapshot,
        Which::Read,
        Which::Find,
        Which::Screenshot,
        Which::Click,
        Which::Type,
        Which::FillForm,
        Which::Select,
        Which::PressKey,
        Which::Hover,
        Which::Scroll,
        Which::Wait,
        Which::Tabs,
        Which::Upload,
        Which::Downloads,
        Which::Handoff,
    ];

    pub fn name(self) -> &'static str {
        match self {
            Which::Open => "browser_open",
            Which::Navigate => "browser_navigate",
            Which::Snapshot => "browser_snapshot",
            Which::Read => "browser_read",
            Which::Find => "browser_find",
            Which::Screenshot => "browser_screenshot",
            Which::Click => "browser_click",
            Which::Type => "browser_type",
            Which::FillForm => "browser_fill_form",
            Which::Select => "browser_select",
            Which::PressKey => "browser_press_key",
            Which::Hover => "browser_hover",
            Which::Scroll => "browser_scroll",
            Which::Wait => "browser_wait",
            Which::Tabs => "browser_tabs",
            Which::Upload => "browser_upload",
            Which::Downloads => "browser_downloads",
            Which::Handoff => "browser_handoff",
        }
    }

    fn description(self) -> &'static str {
        match self {
            Which::Open => {
                "Open the browser for this chat (or focus it) and optionally go to a URL. Returns the tab id."
            }
            Which::Navigate => {
                "Go to a URL in a tab, or `back` / `forward` / `reload`. New sites may ask the user for permission."
            }
            Which::Snapshot => {
                "Accessibility snapshot of the page with element refs (`[ref=e12]`). `mode`: interactive (default) or full; `scope`: a ref to show only its subtree."
            }
            Which::Read => "Main readable text of the page (or a ref subtree) as Markdown.",
            Which::Find => {
                "Find elements by text, label or role; returns matching refs with context."
            }
            Which::Screenshot => {
                "JPEG of the viewport, the full page or one ref. Use only when the snapshot is not enough (canvas, maps, visual checks)."
            }
            Which::Click => {
                "Click an element by ref. Purchases, sends, deletes and similar actions ask the user first."
            }
            Which::Type => {
                "Type text into an element by ref (clears it first unless `clear` is false; `submit` presses Enter). Password, one-time-code and card fields are refused: use browser_handoff."
            }
            Which::FillForm => {
                "Set several fields at once: text, checkbox/radio (true/false) and select (option value or label)."
            }
            Which::Select => "Choose option(s) of a select element by value or label.",
            Which::PressKey => {
                "Press a key or chord on the focused element, e.g. `Enter`, `Escape`, `Control+A`."
            }
            Which::Hover => "Move the pointer over an element (menus, tooltips).",
            Which::Scroll => {
                "Scroll the page or an element (`direction` up/down/left/right, `amount` in pixels), or scroll a ref into view."
            }
            Which::Wait => {
                "Wait for text or a ref to appear (or disappear with `gone`), or for the page to load. Timeout ≤ 30 s."
            }
            Which::Tabs => {
                "List, open (`new`, optional URL), switch to or close tabs of this chat."
            }
            Which::Upload => {
                "Attach files the user gave in this chat (attachment ids) to a file input. Always asks the user."
            }
            Which::Downloads => {
                "List downloads started in this chat and their status (files stay in quarantine)."
            }
            Which::Handoff => {
                "Ask the user to take over the browser (login, 2FA, CAPTCHA, a choice only they can make), with a reason."
            }
        }
    }

    fn schema(self) -> Value {
        match self {
            Which::Open => schema_of::<OpenInput>(),
            Which::Navigate => schema_of::<NavigateInput>(),
            Which::Snapshot => schema_of::<SnapshotInput>(),
            Which::Read => schema_of::<ReadInput>(),
            Which::Find => schema_of::<FindInput>(),
            Which::Screenshot => schema_of::<ScreenshotInput>(),
            Which::Click => schema_of::<ClickInput>(),
            Which::Type => schema_of::<TypeInput>(),
            Which::FillForm => schema_of::<FillFormInput>(),
            Which::Select => schema_of::<SelectInput>(),
            Which::PressKey => schema_of::<PressKeyInput>(),
            Which::Hover => schema_of::<HoverInput>(),
            Which::Scroll => schema_of::<ScrollInput>(),
            Which::Wait => schema_of::<WaitInput>(),
            Which::Tabs => schema_of::<TabsInput>(),
            Which::Upload => schema_of::<UploadInput>(),
            Which::Downloads => schema_of::<DownloadsInput>(),
            Which::Handoff => schema_of::<HandoffInput>(),
        }
    }
}

// ---------------------------------------------------------------- inputs

#[derive(Debug, Deserialize, JsonSchema)]
struct OpenInput {
    /// Optional URL to open.
    #[serde(default)]
    url: Option<String>,
}

#[derive(Debug, Clone, Copy, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
enum NavAction {
    Back,
    Forward,
    Reload,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct NavigateInput {
    /// Absolute http(s) URL. Omit when using `action`.
    #[serde(default)]
    url: Option<String>,
    #[serde(default)]
    action: Option<NavAction>,
    #[serde(default)]
    tab_id: Option<String>,
}

#[derive(Debug, Clone, Copy, Default, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
enum SnapMode {
    #[default]
    Interactive,
    Full,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct SnapshotInput {
    #[serde(default)]
    tab_id: Option<String>,
    #[serde(default)]
    mode: Option<SnapMode>,
    /// A ref: show only this element's subtree.
    #[serde(default)]
    scope: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct ReadInput {
    #[serde(default)]
    tab_id: Option<String>,
    #[serde(default)]
    scope: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct FindInput {
    /// Text to look for in names, labels and values.
    query: String,
    /// Only this role (button, link, textbox, …).
    #[serde(default)]
    role: Option<String>,
    #[serde(default)]
    tab_id: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct ScreenshotInput {
    #[serde(default)]
    tab_id: Option<String>,
    #[serde(default, rename = "ref")]
    element: Option<String>,
    #[serde(default)]
    full_page: Option<bool>,
}

#[derive(Debug, Clone, Copy, Default, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
enum Button {
    #[default]
    Left,
    Right,
    Middle,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct ClickInput {
    #[serde(rename = "ref")]
    element: String,
    #[serde(default)]
    tab_id: Option<String>,
    #[serde(default)]
    button: Option<Button>,
    #[serde(default)]
    double: Option<bool>,
    /// Alt, Control, Meta, Shift.
    #[serde(default)]
    modifiers: Option<Vec<String>>,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct TypeInput {
    #[serde(rename = "ref")]
    element: String,
    text: String,
    #[serde(default)]
    tab_id: Option<String>,
    /// Clear the field first (default true).
    #[serde(default)]
    clear: Option<bool>,
    /// Press Enter afterwards.
    #[serde(default)]
    submit: Option<bool>,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct FieldValue {
    #[serde(rename = "ref")]
    element: String,
    /// Text, true/false for checkboxes and radios, or an option for selects.
    value: Value,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct FillFormInput {
    fields: Vec<FieldValue>,
    #[serde(default)]
    tab_id: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct SelectInput {
    #[serde(rename = "ref")]
    element: String,
    /// Option values or labels.
    values: Vec<String>,
    #[serde(default)]
    tab_id: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct PressKeyInput {
    /// A key or chord: `Enter`, `Tab`, `Escape`, `ArrowDown`, `Control+A`, `a`.
    key: String,
    #[serde(default)]
    tab_id: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct HoverInput {
    #[serde(rename = "ref")]
    element: String,
    #[serde(default)]
    tab_id: Option<String>,
}

#[derive(Debug, Clone, Copy, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
enum Direction {
    Up,
    Down,
    Left,
    Right,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct ScrollInput {
    #[serde(default)]
    tab_id: Option<String>,
    /// Scroll inside this element, or (without `direction`) scroll it into view.
    #[serde(default, rename = "ref")]
    element: Option<String>,
    #[serde(default)]
    direction: Option<Direction>,
    /// Pixels (default 600).
    #[serde(default)]
    amount: Option<u32>,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct WaitInput {
    #[serde(default)]
    tab_id: Option<String>,
    #[serde(default)]
    text: Option<String>,
    #[serde(default, rename = "ref")]
    element: Option<String>,
    /// Wait for the text/ref to disappear instead.
    #[serde(default)]
    gone: Option<bool>,
    /// Wait for the page to finish loading.
    #[serde(default)]
    load: Option<bool>,
    /// Default 10000, max 30000.
    #[serde(default)]
    timeout_ms: Option<u64>,
}

#[derive(Debug, Clone, Copy, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
enum TabsAction {
    List,
    New,
    Switch,
    Close,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct TabsInput {
    action: TabsAction,
    #[serde(default)]
    tab_id: Option<String>,
    /// For `new`.
    #[serde(default)]
    url: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct UploadInput {
    #[serde(rename = "ref")]
    element: String,
    /// Ids of files the user attached in this chat.
    attachment_ids: Vec<String>,
    #[serde(default)]
    tab_id: Option<String>,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct DownloadsInput {}

#[derive(Debug, Deserialize, JsonSchema)]
struct HandoffInput {
    /// What the user should do (log in, solve a CAPTCHA, choose …).
    reason: String,
}

// ---------------------------------------------------------------- the tool

pub struct BrowserTool {
    b: Arc<Browser>,
    which: Which,
    schema: Value,
}

macro_rules! with_op {
    ($self:ident, $ctx:ident, $input:ident, $T:ty, $f:path) => {{
        let i: $T = parse_input($input, &$self.schema)?;
        let name = $self.which.name();
        let mut op = match Op::begin(&$self.b, $ctx, name).await {
            Ok(o) => o,
            Err(s) => return conclude_early($ctx, name, s),
        };
        let r = $f(&mut op, i).await;
        op.conclude(r)
    }};
}

impl BrowserTool {
    async fn run(&self, input: Value, ctx: &ToolCtx) -> Result<ToolOutput, ToolError> {
        match self.which {
            Which::Open => with_op!(self, ctx, input, OpenInput, open),
            Which::Navigate => with_op!(self, ctx, input, NavigateInput, navigate),
            Which::Snapshot => with_op!(self, ctx, input, SnapshotInput, snapshot),
            Which::Read => with_op!(self, ctx, input, ReadInput, read),
            Which::Find => with_op!(self, ctx, input, FindInput, find),
            Which::Screenshot => with_op!(self, ctx, input, ScreenshotInput, screenshot),
            Which::Click => with_op!(self, ctx, input, ClickInput, click),
            Which::Type => with_op!(self, ctx, input, TypeInput, type_text),
            Which::FillForm => with_op!(self, ctx, input, FillFormInput, fill_form),
            Which::Select => with_op!(self, ctx, input, SelectInput, select),
            Which::PressKey => with_op!(self, ctx, input, PressKeyInput, press_key),
            Which::Hover => with_op!(self, ctx, input, HoverInput, hover),
            Which::Scroll => with_op!(self, ctx, input, ScrollInput, scroll),
            Which::Wait => with_op!(self, ctx, input, WaitInput, wait),
            Which::Tabs => with_op!(self, ctx, input, TabsInput, tabs),
            Which::Upload => with_op!(self, ctx, input, UploadInput, upload),
            Which::Downloads => with_op!(self, ctx, input, DownloadsInput, downloads),
            Which::Handoff => with_op!(self, ctx, input, HandoffInput, handoff),
        }
    }
}

#[async_trait]
impl Tool for BrowserTool {
    fn name(&self) -> &str {
        self.which.name()
    }

    fn description(&self, _env: &ToolEnv) -> Cow<'_, str> {
        Cow::Borrowed(self.which.description())
    }

    fn input_schema(&self) -> &Value {
        &self.schema
    }

    /// Browser tools reach the network; site asks go through `ctx.authorize`.
    fn kind(&self) -> ToolKind {
        ToolKind::External
    }

    fn availability(&self, env: &ToolEnv) -> Availability {
        if self.which == Which::Screenshot && !self.b.host.vision(&env.session_id) {
            return Availability::Unavailable(Cow::Borrowed("the model has no vision"));
        }
        Availability::Available
    }

    async fn execute(&self, input: Value, ctx: ToolCtx) -> Result<ToolOutput, ToolError> {
        tokio::select! {
            r = self.run(input, &ctx) => r,
            () = ctx.cancel.cancelled() => Err(ToolError::Cancelled),
        }
    }
}

// ---------------------------------------------------------------- tool bodies

type R = Result<Outcome, Stop>;

fn bad(msg: impl Into<String>) -> Stop {
    Stop::Err(ToolError::invalid_input(vec![msg.into()]))
}

async fn go(op: &mut Op<'_>, t: &TabHandle, url: &str) -> R {
    op.gate_navigation(t, url).await?;
    let r =
        op.c.call_in(Some(&t.cdp_session), "Page.navigate", json!({"url": url}))
            .await
            .map_err(cdp_stop)?;
    if let Some(e) = r["errorText"].as_str().filter(|e| !e.is_empty()) {
        op.set_note(format!("navigation failed: {e}"));
    } else if !op.wait_load(t).await {
        op.set_note("the page is still loading");
    }
    op.finish(t).await
}

async fn open(op: &mut Op<'_>, i: OpenInput) -> R {
    let t = op.tab(None).await?;
    op.activate(&t);
    if let Some(u) = i.url {
        return go(op, &t, &u).await;
    }
    let (url, title) = op.page_info(&t).await?;
    let ids: Vec<String> = op.s.tabs().into_iter().map(|t| t.tab_id).collect();
    Ok(Outcome {
        status: Status::Ok,
        tab_id: Some(t.tab_id.clone()),
        url,
        title,
        note: Some(format!("tabs: {}", ids.join(", "))),
        body: None,
        origin: String::new(),
    })
}

async fn navigate(op: &mut Op<'_>, i: NavigateInput) -> R {
    let t = op.tab(i.tab_id.as_deref()).await?;
    match (i.url, i.action) {
        (Some(u), None) => go(op, &t, &u).await,
        (None, Some(a)) => {
            let sid = Some(t.cdp_session.as_str());
            match a {
                NavAction::Reload => {
                    let (url, _) = op.page_info(&t).await?;
                    op.gate(&url, Need::Read).await?;
                    op.c.call_in(sid, "Page.reload", json!({}))
                        .await
                        .map_err(cdp_stop)?;
                    op.wait_load(&t).await;
                }
                NavAction::Back | NavAction::Forward => {
                    let h =
                        op.c.call_in(sid, "Page.getNavigationHistory", json!({}))
                            .await
                            .map_err(cdp_stop)?;
                    let cur = h["currentIndex"].as_i64().unwrap_or(0);
                    let idx = if matches!(a, NavAction::Back) {
                        cur - 1
                    } else {
                        cur + 1
                    };
                    let entry = usize::try_from(idx)
                        .ok()
                        .and_then(|i| h["entries"].get(i))
                        .ok_or_else(|| {
                            Stop::Err(ToolError::failed(
                                "there is no page to go to in that direction",
                            ))
                        })?;
                    let url = entry["url"].as_str().unwrap_or("").to_string();
                    op.gate(&url, Need::Read).await?;
                    op.c.call_in(
                        sid,
                        "Page.navigateToHistoryEntry",
                        json!({"entryId": entry["id"]}),
                    )
                    .await
                    .map_err(cdp_stop)?;
                }
            }
            op.finish(&t).await
        }
        _ => Err(bad("give either `url` or `action`")),
    }
}

/// Read-gate the tab's current page and capture it.
async fn read_page(op: &mut Op<'_>, t: &TabHandle) -> Result<crate::snapshot::Snapshot, Stop> {
    let (url, _) = op.page_info(t).await?;
    op.gate(&url, Need::Read).await?;
    op.capture(t).await
}

async fn snapshot(op: &mut Op<'_>, i: SnapshotInput) -> R {
    let t = op.tab(i.tab_id.as_deref()).await?;
    let snap = read_page(op, &t).await?;
    let mode = match i.mode.unwrap_or_default() {
        SnapMode::Interactive => Mode::Interactive,
        SnapMode::Full => Mode::Full,
    };
    let body = snap
        .render(mode, i.scope.as_deref(), SNAPSHOT_CHARS)
        .map_err(bad)?;
    let out = op.outcome(&t, &snap, Some(body));
    op.store(&t, snap);
    Ok(out)
}

async fn read(op: &mut Op<'_>, i: ReadInput) -> R {
    let t = op.tab(i.tab_id.as_deref()).await?;
    let snap = read_page(op, &t).await?;
    let body = markdown(&snap, i.scope.as_deref(), READ_CHARS).map_err(bad)?;
    let out = op.outcome(&t, &snap, Some(body));
    op.store(&t, snap);
    Ok(out)
}

async fn find(op: &mut Op<'_>, i: FindInput) -> R {
    let t = op.tab(i.tab_id.as_deref()).await?;
    let snap = read_page(op, &t).await?;
    let hits = snap.find(&i.query, i.role.as_deref(), 30);
    let body = if hits.is_empty() {
        "(no matches)".to_string()
    } else {
        hits.join("\n")
    };
    let out = op.outcome(&t, &snap, Some(body));
    op.store(&t, snap);
    Ok(out)
}

async fn screenshot(op: &mut Op<'_>, i: ScreenshotInput) -> R {
    let t = op.tab(i.tab_id.as_deref()).await?;
    let (url, title) = op.page_info(&t).await?;
    op.gate(&url, Need::Read).await?;
    let sid = Some(t.cdp_session.as_str());
    let mut p = json!({"format": "jpeg", "quality": 70});
    let metrics =
        op.c.call_in(sid, "Page.getLayoutMetrics", json!({}))
            .await
            .map_err(cdp_stop)?;
    let vp = &metrics["cssLayoutViewport"];
    let (px, py) = (
        vp["pageX"].as_f64().unwrap_or(0.0),
        vp["pageY"].as_f64().unwrap_or(0.0),
    );
    if let Some(r) = &i.element {
        let info = op.ref_info(&t, r)?;
        let (_, _) = op.center(&t, info.backend).await?;
        let q =
            op.c.call_in(
                sid,
                "DOM.getBoxModel",
                json!({"backendNodeId": info.backend}),
            )
            .await
            .map_err(cdp_stop)?;
        let b: Vec<f64> = q["model"]["border"]
            .as_array()
            .map(|a| a.iter().filter_map(Value::as_f64).collect())
            .unwrap_or_default();
        if b.len() == 8 {
            let xs = [b[0], b[2], b[4], b[6]];
            let ys = [b[1], b[3], b[5], b[7]];
            let x0 = xs.iter().copied().fold(f64::MAX, f64::min);
            let y0 = ys.iter().copied().fold(f64::MAX, f64::min);
            let x1 = xs.iter().copied().fold(f64::MIN, f64::max);
            let y1 = ys.iter().copied().fold(f64::MIN, f64::max);
            p["clip"] = json!({"x": x0 + px, "y": y0 + py, "width": x1 - x0, "height": y1 - y0, "scale": 1});
        }
    } else if i.full_page == Some(true) {
        let cs = &metrics["cssContentSize"];
        p["captureBeyondViewport"] = json!(true);
        p["clip"] = json!({
            "x": 0, "y": 0,
            "width": cs["width"].as_f64().unwrap_or(1280.0),
            "height": cs["height"].as_f64().unwrap_or(800.0).min(8000.0),
            "scale": 1,
        });
    }
    let r =
        op.c.call_in(sid, "Page.captureScreenshot", p)
            .await
            .map_err(cdp_stop)?;
    let bytes = r["data"]
        .as_str()
        .and_then(decode_b64)
        .ok_or_else(|| Stop::Err(ToolError::failed("the browser returned no image")))?;
    let Some(image) =
        op.b.host
            .store_image(&op.ctx.env.session_id, &bytes, "image/jpeg")
    else {
        return Err(Stop::Err(ToolError::failed(
            "screenshots need image storage",
        )));
    };
    let origin = crate::snapshot::origin_of(&url);
    op.mark_read_text(&origin, "");
    Ok(Outcome {
        status: Status::Ok,
        tab_id: Some(t.tab_id.clone()),
        url,
        title,
        note: Some(format!("image: {image} (jpeg, {} bytes)", bytes.len())),
        body: None,
        origin,
    })
}

fn modifier_bits(m: &[String]) -> Result<u32, Stop> {
    let mut bits = 0;
    for x in m {
        bits |= match x.to_ascii_lowercase().as_str() {
            "alt" | "option" => input::ALT,
            "control" | "ctrl" => input::CTRL,
            "meta" | "cmd" | "command" => input::META,
            "shift" => input::SHIFT,
            other => return Err(bad(format!("unknown modifier `{other}`"))),
        };
    }
    Ok(bits)
}

/// Confirm a click that submits a sensitive form or names a commit verb (D21).
async fn sensitive_click(
    op: &mut Op<'_>,
    t: &TabHandle,
    info: &crate::snapshot::RefInfo,
) -> Result<(), Stop> {
    let (sensitive_form, payment) = op
        .last(t)
        .map(|s| {
            (
                info.form.is_some_and(|f| s.sensitive_forms.contains(&f)),
                s.payment_frame,
            )
        })
        .unwrap_or_default();
    let host = crate::site::host_of(&op.current_url(t)).unwrap_or_default();
    if info.is_submit() && (sensitive_form || payment) {
        let what = if payment {
            "payment"
        } else {
            "password or payment"
        };
        return op
            .confirm(
                "submit",
                format!(
                    "Submit a form with {what} fields on {host} (\"{}\")",
                    info.name
                ),
                true,
            )
            .await;
    }
    if let Some(v) = commit_verb(&info.name) {
        return op
            .confirm(
                "click",
                format!("Click \"{}\" on {host} (looks like \"{v}\")", info.name),
                true,
            )
            .await;
    }
    Ok(())
}

async fn click_at(
    op: &Op<'_>,
    t: &TabHandle,
    backend: i64,
    button: &str,
    count: u32,
    mods: u32,
) -> Result<(), Stop> {
    let (x, y) = op.center(t, backend).await?;
    op.mouse(
        t,
        json!({"type": "mouseMoved", "x": x, "y": y, "modifiers": mods}),
    )
    .await?;
    for n in 1..=count {
        op.mouse(
            t,
            json!({"type": "mousePressed", "x": x, "y": y, "button": button, "clickCount": n, "modifiers": mods}),
        )
        .await?;
        op.mouse(
            t,
            json!({"type": "mouseReleased", "x": x, "y": y, "button": button, "clickCount": n, "modifiers": mods}),
        )
        .await?;
    }
    Ok(())
}

async fn click(op: &mut Op<'_>, i: ClickInput) -> R {
    let t = op.tab(i.tab_id.as_deref()).await?;
    let info = op.ref_info(&t, &i.element)?;
    let url = op.current_url(&t);
    op.gate(&url, Need::Act).await?;
    sensitive_click(op, &t, &info).await?;
    let mods = modifier_bits(&i.modifiers.unwrap_or_default())?;
    let button = match i.button.unwrap_or_default() {
        Button::Left => "left",
        Button::Right => "right",
        Button::Middle => "middle",
    };
    let count = if i.double == Some(true) { 2 } else { 1 };
    click_at(op, &t, info.backend, button, count, mods).await?;
    op.finish(&t).await
}

const CLEAR_FN: &str = "function(){ if (this.isContentEditable) { this.textContent = ''; } \
    else if ('value' in this) { this.value = ''; this.dispatchEvent(new Event('input', {bubbles: true})); } }";

const SELECT_FN: &str = "function(vals){ if (this.tagName !== 'SELECT') return null; const out = []; \
    for (const o of this.options) { const m = vals.includes(o.value) || vals.includes(o.label) || vals.includes(o.text.trim()); \
    if (m && !this.multiple && out.length) continue; if (this.multiple || m) o.selected = m; if (m) out.push(o.label || o.text); } \
    this.dispatchEvent(new Event('input', {bubbles: true})); this.dispatchEvent(new Event('change', {bubbles: true})); return out; }";

const CHECKED_FN: &str = "function(){ return !!this.checked; }";

fn refuse_sensitive(info: &crate::snapshot::RefInfo) -> Result<(), Stop> {
    match info.sensitive {
        Some(kind) => Err(Stop::boxed(Outcome::status(
            Status::Blocked,
            format!(
                "typing into {kind} fields is not allowed; call browser_handoff so the user can enter it"
            ),
        ))),
        None => Ok(()),
    }
}

async fn insert(
    op: &Op<'_>,
    t: &TabHandle,
    backend: i64,
    text: &str,
    clear: bool,
) -> Result<(), Stop> {
    let sid = Some(t.cdp_session.as_str());
    op.c.call_in(sid, "DOM.focus", json!({"backendNodeId": backend}))
        .await
        .map_err(cdp_stop)?;
    if clear {
        op.call_fn(t, backend, CLEAR_FN, vec![]).await?;
    }
    op.c.call_in(sid, "Input.insertText", json!({"text": text}))
        .await
        .map_err(cdp_stop)?;
    Ok(())
}

async fn press(op: &Op<'_>, t: &TabHandle, mods: u32, def: &KeyDef) -> Result<(), Stop> {
    for p in input::key_press(mods, def) {
        op.c.call_in(Some(&t.cdp_session), "Input.dispatchKeyEvent", p)
            .await
            .map_err(cdp_stop)?;
    }
    Ok(())
}

async fn confirm_enter(op: &mut Op<'_>, t: &TabHandle, form: Option<i64>) -> Result<(), Stop> {
    let (sensitive, payment) = op
        .last(t)
        .map(|s| {
            (
                form.is_some_and(|f| s.sensitive_forms.contains(&f)),
                s.payment_frame,
            )
        })
        .unwrap_or_default();
    if sensitive || (payment && form.is_some()) {
        let host = crate::site::host_of(&op.current_url(t)).unwrap_or_default();
        op.confirm(
            "submit",
            format!("Submit a form with password or payment fields on {host}"),
            true,
        )
        .await?;
    }
    Ok(())
}

async fn type_text(op: &mut Op<'_>, i: TypeInput) -> R {
    let t = op.tab(i.tab_id.as_deref()).await?;
    let info = op.ref_info(&t, &i.element)?;
    let url = op.current_url(&t);
    op.gate(&url, Need::Act).await?;
    refuse_sensitive(&info)?;
    let submit = i.submit == Some(true);
    if submit {
        confirm_enter(op, &t, info.form).await?;
    }
    insert(op, &t, info.backend, &i.text, i.clear != Some(false)).await?;
    if submit && let Some(def) = input::key_def("Enter") {
        press(op, &t, 0, &def).await?;
    }
    op.finish(&t).await
}

async fn fill_form(op: &mut Op<'_>, i: FillFormInput) -> R {
    if i.fields.is_empty() || i.fields.len() > 50 {
        return Err(bad("give 1–50 fields"));
    }
    let t = op.tab(i.tab_id.as_deref()).await?;
    let mut infos = Vec::new();
    for f in &i.fields {
        let info = op.ref_info(&t, &f.element)?;
        refuse_sensitive(&info)?;
        infos.push(info);
    }
    let url = op.current_url(&t);
    op.gate(&url, Need::Act).await?;
    for (f, info) in i.fields.iter().zip(&infos) {
        match (info.role.as_str(), &f.value) {
            ("checkbox" | "radio" | "switch" | "menuitemcheckbox", v) => {
                let want = match v {
                    Value::Bool(b) => *b,
                    Value::String(s) => matches!(s.as_str(), "true" | "on" | "yes" | "checked"),
                    _ => return Err(bad(format!("`{}` needs true or false", f.element))),
                };
                let now = op.call_fn(&t, info.backend, CHECKED_FN, vec![]).await?;
                if now.as_bool() != Some(want) {
                    click_at(op, &t, info.backend, "left", 1, 0).await?;
                }
            }
            (_, v) if info.tag == "SELECT" => {
                let vals: Vec<Value> = match v {
                    Value::Array(a) => a.clone(),
                    other => vec![Value::String(value_text(other))],
                };
                let r = op
                    .call_fn(&t, info.backend, SELECT_FN, vec![Value::Array(vals)])
                    .await?;
                if r.as_array().is_none_or(Vec::is_empty) {
                    return Err(Stop::Err(ToolError::Failed {
                        message: format!("no option of `{}` matched", f.element),
                        may_have_run: true,
                    }));
                }
            }
            (_, v) => insert(op, &t, info.backend, &value_text(v), true).await?,
        }
    }
    op.finish(&t).await
}

fn value_text(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Null => String::new(),
        other => other.to_string(),
    }
}

async fn select(op: &mut Op<'_>, i: SelectInput) -> R {
    let t = op.tab(i.tab_id.as_deref()).await?;
    let info = op.ref_info(&t, &i.element)?;
    let url = op.current_url(&t);
    op.gate(&url, Need::Act).await?;
    let vals: Vec<Value> = i.values.into_iter().map(Value::String).collect();
    let r = op
        .call_fn(&t, info.backend, SELECT_FN, vec![Value::Array(vals)])
        .await?;
    match r.as_array() {
        None => Err(bad(format!("`{}` is not a select element", i.element))),
        Some(a) if a.is_empty() => Err(Stop::Err(ToolError::failed("no option matched"))),
        Some(_) => op.finish(&t).await,
    }
}

async fn press_key(op: &mut Op<'_>, i: PressKeyInput) -> R {
    let (mods, def) =
        input::parse_chord(&i.key).ok_or_else(|| bad(format!("unknown key `{}`", i.key)))?;
    let t = op.tab(i.tab_id.as_deref()).await?;
    let url = op.current_url(&t);
    op.gate(&url, Need::Act).await?;
    if def.0 == "Enter" {
        let focused_form = op.last(&t).and_then(|s| {
            s.nodes
                .iter()
                .find(|n| n.flags.iter().any(|f| f == "focused"))
                .and_then(|n| n.r#ref.as_ref())
                .and_then(|r| s.refs.get(r))
                .and_then(|info| info.form)
        });
        confirm_enter(op, &t, focused_form).await?;
    }
    press(op, &t, mods, &def).await?;
    op.finish(&t).await
}

async fn hover(op: &mut Op<'_>, i: HoverInput) -> R {
    let t = op.tab(i.tab_id.as_deref()).await?;
    let info = op.ref_info(&t, &i.element)?;
    let url = op.current_url(&t);
    op.gate(&url, Need::Act).await?;
    let (x, y) = op.center(&t, info.backend).await?;
    op.mouse(&t, json!({"type": "mouseMoved", "x": x, "y": y}))
        .await?;
    op.finish(&t).await
}

async fn scroll(op: &mut Op<'_>, i: ScrollInput) -> R {
    let t = op.tab(i.tab_id.as_deref()).await?;
    let (url, _) = op.page_info(&t).await?;
    op.gate(&url, Need::Read).await?;
    let sid = Some(t.cdp_session.as_str());
    let backend = match &i.element {
        Some(r) => Some(op.ref_info(&t, r)?.backend),
        None => None,
    };
    match (backend, i.direction) {
        (Some(b), None) => {
            op.c.call_in(
                sid,
                "DOM.scrollIntoViewIfNeeded",
                json!({"backendNodeId": b}),
            )
            .await
            .map_err(cdp_stop)?;
        }
        (b, dir) => {
            let amount = f64::from(i.amount.unwrap_or(600).min(20_000));
            let (dx, dy) = match dir.unwrap_or(Direction::Down) {
                Direction::Up => (0.0, -amount),
                Direction::Down => (0.0, amount),
                Direction::Left => (-amount, 0.0),
                Direction::Right => (amount, 0.0),
            };
            let (x, y) = match b {
                Some(b) => op.center(&t, b).await?,
                None => {
                    let m =
                        op.c.call_in(sid, "Page.getLayoutMetrics", json!({}))
                            .await
                            .map_err(cdp_stop)?;
                    let v = &m["cssLayoutViewport"];
                    (
                        v["clientWidth"].as_f64().unwrap_or(1280.0) / 2.0,
                        v["clientHeight"].as_f64().unwrap_or(800.0) / 2.0,
                    )
                }
            };
            op.mouse(
                &t,
                json!({"type": "mouseWheel", "x": x, "y": y, "deltaX": dx, "deltaY": dy}),
            )
            .await?;
        }
    }
    op.finish(&t).await
}

async fn wait(op: &mut Op<'_>, i: WaitInput) -> R {
    let t = op.tab(i.tab_id.as_deref()).await?;
    let timeout = Duration::from_millis(i.timeout_ms.unwrap_or(10_000).min(30_000));
    let deadline = Instant::now() + timeout;
    let gone = i.gone == Some(true);
    if i.load == Some(true) {
        if !op.wait_load(&t).await {
            op.set_note("timed out waiting for the page to load");
        }
    } else if i.text.is_some() || i.element.is_some() {
        let mut met = false;
        loop {
            let (url, _) = op.page_info(&t).await?;
            op.gate(&url, Need::Read).await?;
            let snap = op.capture(&t).await?;
            let found = match (&i.text, &i.element) {
                (Some(x), _) => snap.text.to_lowercase().contains(&x.to_lowercase()),
                (None, Some(r)) => snap.refs.contains_key(r),
                _ => false,
            };
            if found != gone {
                met = true;
                break;
            }
            if Instant::now() >= deadline {
                break;
            }
            tokio::time::sleep(Duration::from_millis(400)).await;
        }
        if !met {
            op.set_note(format!(
                "condition not met after {} ms",
                timeout.as_millis()
            ));
        }
    } else {
        tokio::time::sleep(timeout.min(Duration::from_secs(10))).await;
    }
    op.finish(&t).await
}

async fn tabs(op: &mut Op<'_>, i: TabsInput) -> R {
    match i.action {
        TabsAction::List => {
            op.adopt_popups().await?;
            let active = op.s.active_tab().map(|t| t.tab_id);
            let mut lines = Vec::new();
            for t in op.s.tabs() {
                let (url, title) = op.page_info(&t).await?;
                let mark = if active.as_deref() == Some(t.tab_id.as_str()) {
                    " (active)"
                } else {
                    ""
                };
                lines.push(format!("- {}{mark}: {title} — {url}", t.tab_id));
            }
            Ok(Outcome {
                status: Status::Ok,
                tab_id: active,
                url: String::new(),
                title: String::new(),
                note: None,
                body: Some(if lines.is_empty() {
                    "(no tabs)".into()
                } else {
                    lines.join("\n")
                }),
                origin: "tabs".into(),
            })
        }
        TabsAction::New => {
            let t = op.new_tab().await?;
            match i.url {
                Some(u) => go(op, &t, &u).await,
                None => Ok(Outcome {
                    status: Status::Ok,
                    tab_id: Some(t.tab_id.clone()),
                    url: "about:blank".into(),
                    title: String::new(),
                    note: None,
                    body: None,
                    origin: String::new(),
                }),
            }
        }
        TabsAction::Switch => {
            let id = i.tab_id.ok_or_else(|| bad("`switch` needs `tab_id`"))?;
            let t = op.tab(Some(&id)).await?;
            op.activate(&t);
            let (url, title) = op.page_info(&t).await?;
            Ok(Outcome {
                status: Status::Ok,
                tab_id: Some(t.tab_id.clone()),
                url,
                title,
                note: Some("take a snapshot to see the page".into()),
                body: None,
                origin: String::new(),
            })
        }
        TabsAction::Close => {
            let id = i.tab_id.ok_or_else(|| bad("`close` needs `tab_id`"))?;
            let t = op.tab(Some(&id)).await?;
            op.close_tab(&t).await?;
            Ok(Outcome::status(Status::Ok, format!("closed {id}")))
        }
    }
}

async fn upload(op: &mut Op<'_>, i: UploadInput) -> R {
    if i.attachment_ids.is_empty() {
        return Err(bad("give at least one attachment id"));
    }
    let t = op.tab(i.tab_id.as_deref()).await?;
    let info = op.ref_info(&t, &i.element)?;
    let url = op.current_url(&t);
    op.gate(&url, Need::Act).await?;
    let mut files = Vec::new();
    for id in &i.attachment_ids {
        let p =
            op.b.host
                .attachment_path(&op.ctx.env.session_id, id)
                .filter(|p| p.is_file())
                .ok_or_else(|| bad(format!("`{id}` is not a file attached in this chat")))?;
        files.push(p);
    }
    let names: Vec<String> = files
        .iter()
        .filter_map(|p| p.file_name().map(|n| n.to_string_lossy().into_owned()))
        .collect();
    let host = crate::site::host_of(&url).unwrap_or_default();
    op.confirm(
        "upload",
        format!("Upload {} to {host}", names.join(", ")),
        true,
    )
    .await?;
    let paths: Vec<String> = files
        .iter()
        .map(|p| p.to_string_lossy().into_owned())
        .collect();
    op.c.call_in(
        Some(&t.cdp_session),
        "DOM.setFileInputFiles",
        json!({"files": paths, "backendNodeId": info.backend}),
    )
    .await
    .map_err(cdp_stop)?;
    op.finish(&t).await
}

async fn downloads(op: &mut Op<'_>, _i: DownloadsInput) -> R {
    let list = op.b.downloads(&op.ctx.env.session_id);
    let lines: Vec<String> = list
        .iter()
        .map(|d| {
            format!(
                "- {} ({}): {} {}/{} bytes from {}",
                if d.filename.is_empty() {
                    &d.guid
                } else {
                    &d.filename
                },
                d.guid,
                d.state,
                d.received,
                d.total,
                crate::snapshot::origin_of(&d.url)
            )
        })
        .collect();
    Ok(Outcome {
        status: Status::Ok,
        tab_id: None,
        url: String::new(),
        title: String::new(),
        note: Some(
            "downloads stay in the quarantine folder; the user decides where they go".into(),
        ),
        body: Some(if lines.is_empty() {
            "(no downloads)".into()
        } else {
            lines.join("\n")
        }),
        origin: "downloads".into(),
    })
}

async fn handoff(op: &mut Op<'_>, i: HandoffInput) -> R {
    let sid = op.ctx.env.session_id.clone();
    op.s.set_control(crate::host::Control::User);
    // One banner event carrying the reason ("handoff" is a user-in-control state); a
    // following `control_changed(User)` would replace it and lose the reason.
    op.b.host.handoff(&sid, &i.reason);
    Ok(Outcome::status(
        Status::UserInControl,
        format!(
            "asked the user to take over ({}). Stop using the browser until they hand back; you'll get a fresh snapshot.",
            i.reason
        ),
    ))
}
