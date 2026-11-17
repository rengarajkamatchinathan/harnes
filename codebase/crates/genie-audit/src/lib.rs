//! `genie-audit`: observability (spec/01-architecture.md §8).
//!
//! - Logs: `tracing` to rotating files `<data dir>/logs/<name>.log` (10 × 10 MiB). Spans
//!   carry ids, never prompt content.
//! - [`audit`]: the org audit stream (08 §6) — versioned records, content exclusion, the
//!   local rotating JSONL sink; [`otlp`]: the OTLP/HTTP JSON exporter queue.
//! - [`telemetry`]: crash reports and usage analytics (08 §7, D39) with a local queue and an
//!   exporter port; [`scrub`]: on-device scrubbing (crash reports, diagnostics, logs).
//!   All of these stay separate from logs.

pub mod audit;
pub mod otlp;
pub mod rotating;
pub mod scrub;
pub mod sys;
pub mod telemetry;

use std::io;
use std::path::Path;

use tracing_subscriber::EnvFilter;
use tracing_subscriber::fmt::writer::MakeWriterExt;

pub use audit::{Attrs, AuditContext, AuditEvent, AuditRecord, AuditSink, Auditor, JsonlSink};
pub use otlp::{OtlpSink, OtlpTransport};
pub use rotating::RotatingLog;
pub use scrub::Scrubber;
pub use telemetry::{Telemetry, TelemetryConsent, UsageEvent, UsageName, UsageProp};

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
