//! `genie-browser`: the engine side of the agent browser (spec/06-agent-runtime.md §10,
//! spec/08 §2.1, research/browser.md option B, D21/D56).
//!
//! - [`backend`]: `BrowserBackend` + `SystemChromeBackend` (the user's Chrome / Edge /
//!   Chromium, separate Genie profile, `--remote-debugging-pipe`, launched in its own
//!   kill-tree via `genie-process`); [`discover`] finds installed browsers (≥ 120, SA-36).
//! - [`cdp`]: our own thin CDP client (request/response/event routing, timeouts, lost
//!   results) over [`transport`] (the debugging pipe).
//! - [`snapshot`] / [`read`]: AX tree + DOMSnapshot → compact outline with refs,
//!   hidden-content stripping, diffs; Markdown for `browser_read`.
//! - [`site`] / [`sensitive`]: per-site `blocked | ask | read | act`, categories, URL
//!   exfiltration guard; sensitive fields and commit verbs (D21).
//! - [`session`]: the [`Browser`] service, per-session tabs/refs/control/taint/step caps,
//!   downloads, `browser.action` audit; [`tools`]: the 18 `browser_*` tools.
//! - [`screencast`]: `browser.screencast.*` with ack flow control and `browser.input`.
//! - [`testing`]: an in-process fake CDP endpoint that replays recorded messages.
//!
//! Dependency rule (01 §2.1 rule 3): `genie-protocol` plus the leaf exceptions
//! `genie-process` and `genie-tools` (`xtask/src/deps.rs`). Everything else the browser
//! needs from the daemon is the [`host::BrowserHost`] port.

pub mod backend;
pub mod cdp;
pub mod discover;
pub mod host;
pub mod input;
pub mod read;
pub mod screencast;
pub mod sensitive;
pub mod session;
pub mod site;
pub mod snapshot;
pub mod testing;
pub mod time;
pub mod tools;
pub mod transport;

pub use backend::{
    BrowserBackend, BrowserError, ProfileKind, SystemChromeBackend, SystemChromeConfig,
};
pub use cdp::{CdpClient, CdpError, CdpEvent, Wire};
pub use host::{BrowserHost, ConfirmDecision, ConfirmRequest, Control, FrameSink, NoHost};
pub use session::{Browser, BrowserConfig, Outcome, Status};
pub use site::SitePolicy;
pub use tools::tools;
