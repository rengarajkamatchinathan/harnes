//! `genie-audit`: observability (spec/01-architecture.md §8).
//!
//! - Logs: `tracing` to rotating files `<data dir>/logs/<name>.log` (10 × 10 MiB). Spans
//!   carry ids, never prompt content.
//! - [`audit`]: the org audit stream (08 §6) — versioned records, content exclusion, the
//!   local rotating JSONL sink. The OTLP exporter, crash reports and analytics are later
//!   work and stay separate from logs.

pub mod audit;
pub mod rotating;

use std::io;
use std::path::Path;

use tracing_subscriber::EnvFilter;
use tracing_subscriber::fmt::writer::MakeWriterExt;

pub use audit::{Attrs, AuditContext, AuditEvent, AuditRecord, AuditSink, Auditor, JsonlSink};
pub use rotating::RotatingLog;

/// Number of files kept (current + rotated).
pub const LOG_FILES: usize = 10;
/// Size at which the current file rotates.
pub const LOG_FILE_BYTES: u64 = 10 * 1024 * 1024;

/// Install the global subscriber: rotating file `<logs_dir>/<name>.log`, plus stderr when
/// `also_stderr`. Filter from `GENIE_LOG` (default `info`). Safe to call once per process;
/// later calls are ignored.
pub fn init_logging(logs_dir: &Path, name: &str, also_stderr: bool) -> io::Result<()> {
    let file = RotatingLog::open(logs_dir, name, LOG_FILES, LOG_FILE_BYTES)?;
    let filter = EnvFilter::try_from_env("GENIE_LOG").unwrap_or_else(|_| EnvFilter::new("info"));
    let builder = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_ansi(false);
    let res = if also_stderr {
        builder.with_writer(file.and(io::stderr)).try_init()
    } else {
        builder.with_writer(file).try_init()
    };
    if res.is_err() {
        tracing::debug!("logging already initialised");
    }
    Ok(())
}
