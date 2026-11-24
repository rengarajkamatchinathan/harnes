//! Browser (02 §6.6) and terminal (§6.7).

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum BrowserKind {
    Chrome,
    Edge,
    Chromium,
    #[serde(other)]
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct InstalledBrowser {
    pub kind: BrowserKind,
    pub version: String,
    pub path: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct BrowserStatus {
    pub installed: Vec<InstalledBrowser>,
    pub running: bool,
    pub profile_path: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ScreencastStartParams {
    pub session_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tab_id: Option<String>,
    pub max_width: u32,
    pub max_height: u32,
    /// JPEG quality 1–100.
    pub quality: u8,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct StreamIdResult {
    pub stream_id: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ScreencastAckParams {
    pub stream_id: String,
    pub frame_seq: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct StreamIdParams {
    pub stream_id: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum InputEvent {
    Mouse {
        /// `pressed` | `released` | `moved`.
        action: String,
        /// CSS pixels of the viewport (`0..BrowserFrame.w`).
        x: f64,
        /// CSS pixels of the viewport (`0..BrowserFrame.h`).
        y: f64,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        button: Option<String>,
        #[serde(default)]
        click_count: u32,
        #[serde(default)]
        modifiers: u32,
    },
    Key {
        /// `down` | `up` | `char`.
        action: String,
        key: String,
        code: String,
        #[serde(default)]
        modifiers: u32,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        text: Option<String>,
    },
    Wheel {
        x: f64,
        y: f64,
        delta_x: f64,
        delta_y: f64,
    },
    Ime {
        /// Composition text; empty ends the composition.
        text: String,
        selection_start: u32,
        selection_end: u32,
    },
    InsertText {
        text: String,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct BrowserInputParams {
    pub session_id: String,
    pub tab_id: String,
    pub event: InputEvent,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct TabInfo {
    pub tab_id: String,
    pub url: String,
    pub title: String,
    pub active: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct TabsResult {
    pub tabs: Vec<TabInfo>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct TabActivateParams {
    pub session_id: String,
    pub tab_id: String,
}

/// `browser.navigate`: the user's own navigation in the Browser tab. It never prompts
/// (research §4.2) and launches the browser and opens a tab when none exists.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct NavigateParams {
    pub session_id: String,
    /// Defaults to the active tab (a new one when the session has none).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tab_id: Option<String>,
    /// What the user typed: a URL, `host[:port]` (https, http for localhost) or `about:blank`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    /// Instead of `url`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub action: Option<NavAction>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum NavAction {
    Back,
    Forward,
    Reload,
}

/// `browser.tab.new`: a tab the user opened (`about:blank` without `url`); it becomes active.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct TabNewParams {
    pub session_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct TabIdResult {
    pub tab_id: String,
}

/// `browser.copy`, `browser.tab.close`: one tab of a session.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct BrowserTabParams {
    pub session_id: String,
    pub tab_id: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct CopyResult {
    /// The page's selected text (empty when nothing is selected).
    pub text: String,
}

/// `browser.viewport`: the size the Browser tab shows pages at, so the page fills the
/// panel (`Emulation.setDeviceMetricsOverride` on the session's tabs while headless).
/// Zoom is folded in by the client: a smaller CSS viewport at a larger `device_scale`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct ViewportParams {
    pub session_id: String,
    /// CSS pixels.
    pub width: u32,
    pub height: u32,
    /// Device pixels per CSS pixel (the page's `devicePixelRatio`).
    pub device_scale: f64,
}

/// `browser.find`: find in page (the next match is selected and scrolled into view).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct FindParams {
    pub session_id: String,
    pub tab_id: String,
    pub query: String,
    #[serde(default)]
    pub backwards: bool,
    /// Start from the top of the page (a new query) instead of after the current match.
    #[serde(default)]
    pub restart: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct FindResult {
    /// Case-insensitive occurrences in the page's visible text.
    pub matches: u32,
    pub found: bool,
}

/// `browser.hit`: what is under a screencast point (cursor, link, right-click menu).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct HitParams {
    pub session_id: String,
    pub tab_id: String,
    pub x: f64,
    pub y: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct HitResult {
    /// The CSS cursor there (`auto` resolved to `text` over text and editable fields).
    pub cursor: String,
    /// The link's absolute URL when the point is on one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub link: Option<String>,
    pub editable: bool,
    /// The page handled the last right-click itself (its own menu): show none of ours.
    pub page_menu: bool,
}

/// `browser.file_chooser.answer`: the files the user picked; empty cancels.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct FileChooserAnswerParams {
    pub session_id: String,
    pub chooser_id: String,
    #[serde(default)]
    pub files: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum SiteState {
    Blocked,
    Ask,
    Read,
    Act,
    #[serde(other)]
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct SitePermission {
    pub origin: String,
    pub state: SiteState,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct SitesListResult {
    pub sites: Vec<SitePermission>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ClearWhat {
    Cookies,
    All,
    #[serde(other)]
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct DataClearParams {
    pub what: ClearWhat,
}

// ---------------------------------------------------------------- terminal (§6.7)

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct TerminalOpenParams {
    pub session_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub shell: Option<String>,
    pub cols: u16,
    pub rows: u16,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct TerminalIdParams {
    pub terminal_id: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct TerminalAttachParams {
    pub terminal_id: String,
    pub offset: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct TerminalAttachResult {
    pub truncated: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub checkpoint_b64: Option<String>,
    pub replay_b64: String,
    pub end_offset: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct TerminalInputParams {
    pub terminal_id: String,
    pub data_b64: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct TerminalResizeParams {
    pub terminal_id: String,
    pub cols: u16,
    pub rows: u16,
}

// ---------------------------------------------------------------- dev servers (ticket 47)

/// Where a dev server is in its life.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum PreviewState {
    /// Spawned; waiting for its port to accept connections.
    Starting,
    /// Its port accepts connections (or, for a `url`-only entry, the URL answered).
    Running,
    /// The process ended or was stopped.
    Exited,
    /// It never came up (spawn error, port taken, exited or timed out while starting).
    Failed,
    #[serde(other)]
    Unknown,
}

/// One dev server of a session (`.genie/launch.json`, ticket 47 phase 2).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct PreviewServer {
    /// The `name` of its `launch.json` entry.
    pub name: String,
    pub state: PreviewState,
    /// Where to open it (`url`, or `http://localhost:<port>`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub port: Option<u16>,
    /// The command line, for display; `None` for a `url`-only entry.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pid: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exit_code: Option<i32>,
    /// Why it failed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// `preview.list`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct PreviewListResult {
    /// Servers started in this session (running or ended), in start order.
    pub servers: Vec<PreviewServer>,
    /// Entry names in the session's `.genie/launch.json`.
    pub configs: Vec<String>,
    /// The project's `autoVerify` (default on).
    pub auto_verify: bool,
    /// `launch.json` exists but couldn't be read.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub config_error: Option<String>,
}

/// `preview.start`: start a `launch.json` entry (no prompt: the user asked).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct PreviewStartParams {
    pub session_id: String,
    pub name: String,
}

/// `preview.stop`: one server, or all of the session's without `name`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct PreviewStopParams {
    pub session_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
}

/// `preview.logs`: the last `lines` of a server's output (default 200).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct PreviewLogsParams {
    pub session_id: String,
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lines: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct PreviewLogsResult {
    pub text: String,
}

/// `preview.auto_verify`: the ⋮ menu's **Auto-verify changes** for the session's project
/// (`autoVerify` in `.genie/launch.json`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct PreviewAutoVerifyParams {
    pub session_id: String,
    pub enabled: bool,
}

/// `browser.open_file`: preview a file of the session's folder (HTML, PDF, video, SVG) in a
/// new Browser tab; `path` is relative to the folder or absolute inside it (ticket 47).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct OpenFileParams {
    pub session_id: String,
    pub path: String,
}

/// `browser.inspect`: the element at a point of the screencast (Element select, ticket 47).
/// `x`/`y` are viewport CSS pixels like `browser.input`. `capture` adds a JPEG crop.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct InspectParams {
    pub session_id: String,
    pub tab_id: String,
    pub x: f64,
    pub y: f64,
    #[serde(default)]
    pub capture: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct InspectResult {
    /// A CSS selector for the element (unique when an id allows it).
    pub selector: String,
    pub tag: String,
    /// Visible text, at most 500 characters.
    pub text: String,
    /// `outerHTML`, at most 1500 characters.
    pub html: String,
    /// The element's box in viewport CSS pixels.
    pub x: f64,
    pub y: f64,
    pub w: f64,
    pub h: f64,
    pub url: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub image_b64: Option<String>,
}
