//! Errors of the MCP client.

use genie_protocol::RpcError;

#[derive(Debug, Clone, thiserror::Error)]
pub enum McpError {
    /// The server answered with a JSON-RPC error.
    #[error("MCP server error {}: {}", .0.code, .0.message)]
    Rpc(RpcError),
    /// The request did not finish in time; `notifications/cancelled` was sent.
    #[error("MCP request `{method}` timed out after {secs} s")]
    Timeout { method: String, secs: u64 },
    /// The caller cancelled; `notifications/cancelled` was sent.
    #[error("MCP request cancelled")]
    Cancelled,
    /// The connection closed (server crashed, stream ended).
    #[error("MCP connection closed: {0}")]
    Closed(String),
    /// The server is not connected (status pending/failed/disabled/needs_auth).
    #[error("MCP server `{server}` is {status}")]
    NotConnected { server: String, status: String },
    /// The server needs (re)authorisation (OAuth, 401).
    #[error("MCP server needs authorisation")]
    NeedsAuth { www_authenticate: Option<String> },
    /// Protocol-version mismatch: names the versions both sides support (t17).
    #[error(
        "MCP protocol version mismatch: server offered `{server}`, Genie supports {}",
        .supported.join(", ")
    )]
    Version {
        server: String,
        supported: Vec<String>,
    },
    /// Transport-level failure (spawn, HTTP, framing).
    #[error("MCP transport: {0}")]
    Transport(String),
    /// The peer sent something we could not understand.
    #[error("MCP protocol: {0}")]
    Protocol(String),
    /// Bad server definition.
    #[error("MCP config: {0}")]
    Config(String),
    /// Unknown server or tool name.
    #[error("unknown MCP {kind} `{name}`")]
    Unknown { kind: &'static str, name: String },
}

impl McpError {
    /// Worth retrying the connection (within the startup deadline, or by reconnecting)?
    pub fn is_transient(&self) -> bool {
        matches!(
            self,
            McpError::Timeout { .. } | McpError::Closed(_) | McpError::Transport(_)
        )
    }
}
