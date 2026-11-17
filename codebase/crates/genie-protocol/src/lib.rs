//! `genie-protocol`: the Genie wire types (spec/01-architecture.md §2, spec/02-protocol.md).
//!
//! This crate is the single source of truth for the client ↔ daemon protocol. TypeScript
//! types are generated from it (02 §11, `cargo xtask schema`) and must never be edited
//! by hand.
//!
//! * [`jsonrpc`]: the JSON-RPC 2.0 envelope, one object per NDJSON line (§1).
//! * [`handshake`]: `initialize` (§2).
//! * [`event`]: session events and the stream notifications (§3, §4).
//! * [`ask`]: the shared ask shape (§7).
//! * [`api`] + [`methods`]: params/results and the method table (§6).
//! * [`error`]: error kinds and codes (§8). [`version`]: semver and skew (§10).
//! * [`schema`]: JSON Schema export for codegen (§11).
//!
//! It depends on nothing internal (01 §2.1 rule 2).

pub mod api;
pub mod ask;
pub mod channel;
pub mod common;
pub mod error;
pub mod event;
pub mod handshake;
pub mod jsonrpc;
pub mod message;
pub mod methods;
pub mod schema;
pub mod session;
pub mod version;
pub mod workspace;

pub use ask::{Ask, AskAnswer, Decision};
pub use channel::Channel;
pub use common::{ClientRef, Empty};
pub use error::{ErrorKind, RpcError};
pub use event::{SessionEvent, SessionEventEnvelope};
pub use handshake::{AskKind, ClientCapabilities, ClientKind, InitializeParams, InitializeResult};
pub use jsonrpc::{Id, Message, Notification, Outcome, Request, Response};
pub use methods::Method;
pub use version::{PROTOCOL_VERSION, SkewDecision};

/// Max NDJSON line size, in bytes (02 §1).
pub const MAX_LINE_BYTES: usize = 16 * 1024 * 1024;

/// Format Unix milliseconds as RFC 3339 UTC (`2026-11-17T09:30:00.123Z`), the `ts`
/// format of envelopes and asks. No date crate needed: civil-from-days (H. Hinnant).
pub fn rfc3339_ms(unix_ms: u64) -> String {
    let secs = unix_ms / 1000;
    let ms = unix_ms % 1000;
    let days = i64::try_from(secs / 86_400).unwrap_or(0);
    let rem = secs % 86_400;
    let (h, m, s) = (rem / 3600, (rem % 3600) / 60, rem % 60);
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let mo = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(mo <= 2);
    format!("{y:04}-{mo:02}-{d:02}T{h:02}:{m:02}:{s:02}.{ms:03}Z")
}

#[cfg(test)]
mod tests {
    #[test]
    fn rfc3339() {
        assert_eq!(super::rfc3339_ms(0), "1970-01-01T00:00:00.000Z");
        assert_eq!(
            super::rfc3339_ms(1_795_000_000_123),
            "2026-11-18T11:06:40.123Z"
        );
    }
}
