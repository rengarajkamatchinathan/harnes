//! `genie-testkit`: test support (01 §2, t34).
//!
//! This milestone ships the in-memory transport: a duplex byte pipe that carries the
//! same NDJSON lines as the socket/pipe, so contract tests run without an OS endpoint
//! (02 §1). Cassettes, fixtures and fake providers land with specs 03 and 06.

use tokio::io::DuplexStream;

/// Buffer per direction for in-memory connections.
pub const MEMORY_BUFFER: usize = 1 << 20;

/// A connected (client, daemon) pair of in-memory streams.
pub fn memory_pair() -> (DuplexStream, DuplexStream) {
    tokio::io::duplex(MEMORY_BUFFER)
}
