//! `genie`: the CLI (spec/07-cli.md). The binary in `main.rs` is a thin shell over this
//! library so the TUI model, headless runner and importer are testable in-process.
//!
//! * [`tui`]: inline TUI (§1): state + update + view, separate from the terminal driver.
//! * [`headless`]: `genie -p` (§4): output formats, exit codes, asks, lifecycle.
//! * [`import`]: one-time importer (§5): scan → preview → apply.
//! * [`cmd`]: subcommands (§2.2) and `genie chat` (§3).

pub mod args;
pub mod cmd;
pub mod connect;
pub mod errors;
pub mod headless;
pub mod hook_adapter;
pub mod import;
pub mod render;
pub mod session;
pub mod tui;
