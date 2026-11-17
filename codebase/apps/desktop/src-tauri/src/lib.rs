//! Genie desktop shell (spec 05; 01 §1.3).
//!
//! - [`relay`]: the `genie-client` connection, the typed IPC bridge (requests, a stream of
//!   notifications batched every ~8 ms, daemon → client requests), resume on reconnect.
//! - [`batch`]: delta coalescing (D57).
//! - [`artifact`]: the `genie-artifact:` scheme content and CSP (05 §8).
//! - [`host`]: channel, bundled daemon binary, instance id, `state.json`, OS open, exports.
//! - `shell` (with `cfg(webview)`): the Tauri 2 app. On Linux it needs
//!   `--features linux-webview` (WebKitGTK dev packages), so workspace builds work without them.

pub mod artifact;
pub mod batch;
pub mod host;
pub mod relay;
#[cfg(webview)]
pub mod shell;

/// Log to stderr; `GENIE_LOG` sets the filter (same variable as the CLI and daemon).
pub fn init_tracing() {
    let filter = tracing_subscriber::EnvFilter::try_from_env("GENIE_LOG")
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info"));
    let _ = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .try_init();
}
