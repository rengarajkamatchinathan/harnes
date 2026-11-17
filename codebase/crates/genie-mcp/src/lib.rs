//! `genie-mcp`: the MCP client (spec/06-agent-runtime.md §7, D12, D30).
//!
//! * [`config`]: Claude-compatible `mcpServers` entries, scope merge, definition hash.
//! * [`transport`]: stdio (pipe mode via `genie-process`, kill-tree, minimal env) and
//!   Streamable HTTP (behind the [`transport::http::HttpClient`] port).
//! * [`client`]: JSON-RPC session: initialize (MCP 2025-06-18), requests with timeouts,
//!   progress, cancellation (`notifications/cancelled`), server → client requests.
//! * [`server`]: status machine `pending | connected | disabled | failed | needs_auth`,
//!   startup retries, reconnect with backoff, debounced `list_changed`, log ring, tools /
//!   resources / prompts.
//! * [`manager`]: one server per name shared by sessions; collision-free tool names.
//! * [`handler`]: elicitation and sampling as ports (+ ask builders, sampling budget).
//! * [`auth`]: OAuth as a port (`needs_auth`).
//! * [`result`]: results → parts, with their [`result::TaintSource`] (06 §5.5, 08 §2.2).
//!
//! Dependency rule (01 §2.1 rule 3): this crate depends on `genie-protocol`, plus the leaf
//! exceptions `genie-process` and `genie-secrets` (`xtask/src/deps.rs`). Every task is
//! spawned on the caller's `TaskTracker` ([`server::Runtime::tasks`]).

pub mod auth;
pub mod client;
pub mod config;
pub mod error;
pub mod handler;
pub mod logs;
pub mod manager;
pub mod naming;
pub mod result;
pub mod schema;
pub mod server;
pub mod transport;

pub use client::{LATEST_PROTOCOL_VERSION, McpClient, Progress, SUPPORTED_PROTOCOL_VERSIONS};
pub use config::{McpScope, ScopedServer, ServerConfig, merge_scopes};
pub use error::McpError;
pub use genie_protocol::api::mcp::{McpServerInfo, McpStatus};
pub use manager::McpManager;
pub use result::{Part, TaintSource, ToolResult};
pub use server::{CallOpts, McpEvent, McpServer, McpTool, Runtime, ServerOptions};
