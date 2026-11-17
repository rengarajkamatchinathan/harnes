//! `genie-client`: the Rust client used by the CLI and the Tauri side (01 §1.3, §4.1).
//!
//! It also owns the pieces of the transport both ends share (endpoint naming, NDJSON
//! framing, Windows SID helpers), so `genie-daemon` depends on this crate rather than the
//! other way round: the Tauri side must never pull in the engine.

pub mod connection;
pub mod endpoint;
pub mod ensure;
pub mod framing;
pub mod stream;
#[cfg(windows)]
pub mod win;

pub use connection::{ClientError, ClientInfo, Connection, Incoming};
pub use endpoint::{Address, Endpoint};
pub use ensure::{EnsureOptions, EnsureOutcome, EnsureState, ensure};
pub use stream::{Action, SessionTracker};
