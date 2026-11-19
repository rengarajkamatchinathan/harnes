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
