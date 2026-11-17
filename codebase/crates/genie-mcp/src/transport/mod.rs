//! Transports: each one turns a server definition into a pair of message channels driven
//! by tasks on the caller's `TaskTracker`.

pub mod http;
pub mod sse;
pub mod stdio;

use std::path::PathBuf;
use std::sync::Arc;

use async_trait::async_trait;
use genie_process::pipe::SpawnHook;
use genie_protocol::Message;
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;
use tokio_util::task::TaskTracker;

use crate::auth::{AuthKey, AuthProvider};
use crate::config::{SecretResolver, ServerConfig};
use crate::error::McpError;
use crate::logs::LogRing;

/// Why a transport stopped.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CloseReason {
    /// The stream ended (stdout EOF / process exit).
    Eof(String),
    /// I/O or HTTP failure.
    Error(String),
    /// HTTP 401 and the auth port had no fresh token: status `needs_auth`.
    Unauthorized { www_authenticate: Option<String> },
    /// HTTP 404 for our `Mcp-Session-Id`: the server forgot the session; reconnect.
    SessionExpired,
    /// We closed it.
    Local,
}

impl std::fmt::Display for CloseReason {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CloseReason::Eof(s) | CloseReason::Error(s) => f.write_str(s),
            CloseReason::Unauthorized { .. } => f.write_str("unauthorized (401)"),
            CloseReason::SessionExpired => f.write_str("session expired"),
            CloseReason::Local => f.write_str("closed"),
        }
    }
}

/// What a transport delivers to the client.
#[derive(Debug)]
pub enum Inbound {
    Message(Message),
    Closed(CloseReason),
}

/// The two ends of a running transport. Cancelling `close` stops its tasks (and kills a
/// stdio server's process tree).
pub struct TransportPair {
    pub tx: mpsc::Sender<Message>,
    pub rx: mpsc::Receiver<Inbound>,
    pub close: CancellationToken,
}

/// Everything a connector may need.
#[derive(Clone)]
pub struct ConnectEnv {
    pub server: String,
    pub tasks: TaskTracker,
    pub logs: LogRing,
    pub secrets: Arc<dyn SecretResolver>,
    pub auth: Arc<dyn AuthProvider>,
    /// Cancelled when the server is stopped; transports derive their `close` from it.
    pub parent: CancellationToken,
}

impl ConnectEnv {
    pub fn auth_key(&self, url: &str) -> AuthKey {
        AuthKey::new(&self.server, url)
    }
}

/// Opens a transport for a server definition. A port so tests (and the daemon) can swap
/// in other transports.
#[async_trait]
pub trait Connector: Send + Sync {
    async fn connect(
        &self,
        cfg: &ServerConfig,
        env: &ConnectEnv,
    ) -> Result<TransportPair, McpError>;
}

/// stdio via `genie-process`, Streamable HTTP via an [`http::HttpClient`].
pub struct DefaultConnector {
    pub http: Arc<dyn http::HttpClient>,
    pub spawn_hook: Option<Arc<dyn SpawnHook>>,
    /// Working directory for stdio servers without `cwd`.
    pub default_cwd: Option<PathBuf>,
}

impl DefaultConnector {
    pub fn new(http: Arc<dyn http::HttpClient>) -> DefaultConnector {
        DefaultConnector {
            http,
            spawn_hook: None,
            default_cwd: None,
        }
    }
}

#[async_trait]
impl Connector for DefaultConnector {
    async fn connect(
        &self,
        cfg: &ServerConfig,
        env: &ConnectEnv,
    ) -> Result<TransportPair, McpError> {
        match cfg {
            ServerConfig::Stdio { .. } => stdio::spawn_process(
                cfg,
                env,
                self.spawn_hook.as_deref(),
                self.default_cwd.as_deref(),
            ),
            ServerConfig::Http { url, headers } => {
                Ok(http::connect(self.http.clone(), url, headers, env))
            }
        }
    }
}

/// Channel capacity for both directions.
pub(crate) const CHANNEL: usize = 256;
