//! What the browser needs from the daemon/engine (01 §2.1 rule 3: a port, injected).

use std::path::PathBuf;

use async_trait::async_trait;
use genie_protocol::ask::BrowserConfirm;
use genie_protocol::event::BrowserFrame;

/// The answer to a sensitive-action confirmation card (D21): Approve / Take over / Deny.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConfirmDecision {
    Approve,
    TakeOver,
    Deny,
    /// No client can answer (headless without asks): the tool returns
    /// `needs_confirmation`.
    Unavailable,
}

/// A `browser_confirm` ask (`AskKind::BrowserConfirm`, 02 §7).
#[derive(Debug, Clone, PartialEq)]
pub struct ConfirmRequest {
    pub session_id: String,
    pub turn_id: String,
    pub call_id: String,
    pub confirm: BrowserConfirm,
}

/// Who drives the browser (05 §9 banner pill).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Control {
    /// "Genie is browsing".
    Agent,
    /// "Paused – you're in control".
    User,
    /// "Waiting for your approval".
    WaitingForApproval,
}

/// Implemented by the daemon (asks, blobs, taint, attachments, banner state).
#[async_trait]
pub trait BrowserHost: Send + Sync {
    /// Raise a durable `browser_confirm` ask and wait for the answer. D21 confirmations
    /// apply in every mode, bypass included.
    async fn confirm(&self, req: ConfirmRequest) -> ConfirmDecision;

    /// Untrusted web content entered this turn (SA-29). The engine feeds it to the
    /// permission evaluator's `Taint { source, read_origins }`.
    fn taint(&self, _session_id: &str, _turn_id: &str, _origin: &str) {}

    /// The session's model accepts images (`browser_screenshot` is vision-only).
    fn vision(&self, _session_id: &str) -> bool {
        false
    }

    /// Store an image blob (screenshot, audit thumbnail, confirmation card) and return its
    /// reference.
    fn store_image(&self, _session_id: &str, _bytes: &[u8], _mime: &str) -> Option<String> {
        None
    }

    /// A chat attachment's local file (uploads take attachment ids only, never paths).
    fn attachment_path(&self, _session_id: &str, _attachment_id: &str) -> Option<PathBuf> {
        None
    }

    /// The user typed this URL in the chat (not model-built; exfiltration guard).
    fn user_supplied_url(&self, _session_id: &str, _url: &str) -> bool {
        false
    }

    /// The banner state changed.
    fn control_changed(&self, _session_id: &str, _control: Control) {}

    /// The agent asked the user to take over (`browser_handoff`).
    fn handoff(&self, _session_id: &str, _reason: &str) {}
}

/// Receives screencast frames for one stream (02 §6.6 `browser.frame`, ephemeral).
pub trait FrameSink: Send + Sync {
    fn frame(&self, frame: BrowserFrame);
}

/// Answers every confirmation with `Unavailable` (tests, headless without asks).
pub struct NoHost;

#[async_trait]
impl BrowserHost for NoHost {
    async fn confirm(&self, _req: ConfirmRequest) -> ConfirmDecision {
        ConfirmDecision::Unavailable
    }
}
