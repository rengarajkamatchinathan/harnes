//! `genie-protocol`: the Genie wire types (spec/01-architecture.md §2, spec/02-protocol.md).
//!
//! This crate is the single source of truth for the client ↔ daemon protocol. TypeScript
//! types are generated from it (02 §11) and must never be edited by hand.
//!
//! It depends on nothing internal (01 §2.1 rule 2).

pub mod channel;
pub mod error;
pub mod handshake;
pub mod jsonrpc;
pub mod methods;
pub mod session;
pub mod version;

pub use channel::Channel;
pub use error::{ErrorKind, RpcError};
pub use handshake::{AskKind, ClientCapabilities, ClientKind, InitializeParams, InitializeResult};
pub use jsonrpc::{Id, Message, Notification, Outcome, Request, Response};
pub use version::{PROTOCOL_VERSION, SkewDecision};

/// Max NDJSON line size, in bytes (02 §1).
pub const MAX_LINE_BYTES: usize = 16 * 1024 * 1024;
