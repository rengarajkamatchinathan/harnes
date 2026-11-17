//! All configured servers: one supervised server per name, shared by every session in the
//! daemon (06 §7), plus the cross-server tool index with collision-free model names.

use std::collections::BTreeMap;
use std::sync::RwLock;
use std::time::Duration;

use genie_protocol::api::mcp::{McpServerInfo, McpStatus};
use serde_json::Value;
use tokio::sync::broadcast;

use crate::config::ScopedServer;
use crate::error::McpError;
use crate::naming;
use crate::result::ToolResult;
use crate::server::{CallOpts, McpEvent, McpServer, McpTool, Runtime, ServerOptions};

/// 06 §7: wait at most this long for pending servers before the first model call.
pub const FIRST_CALL_GRACE: Duration = Duration::from_secs(2);

pub struct McpManager {
    rt: Runtime,
    servers: RwLock<BTreeMap<String, McpServer>>,
    events: broadcast::Sender<McpEvent>,
}

impl McpManager {
    pub fn new(rt: Runtime) -> McpManager {
        let (events, _) = broadcast::channel(256);
        McpManager {
            rt,
            servers: RwLock::new(BTreeMap::new()),
            events,
        }
    }

    pub fn subscribe(&self) -> broadcast::Receiver<McpEvent> {
        self.events.subscribe()
    }

    /// Reconcile with the merged config: start new servers, stop removed ones, restart
    /// servers whose definition changed (by definition hash). Unchanged servers keep running.
    pub fn apply(
        &self,
        defs: Vec<ScopedServer>,
        mut opts: impl FnMut(&ScopedServer) -> ServerOptions,
    ) {
        let Ok(mut g) = self.servers.write() else {
            return;
        };
        let wanted: BTreeMap<String, ScopedServer> =
            defs.into_iter().map(|d| (d.name.clone(), d)).collect();
        g.retain(|name, s| {
            let keep = wanted.get(name).is_some_and(|d| {
                d.scope == s.scope()
                    && match (&d.config, s.config()) {
                        (Ok(a), Ok(b)) => a.definition_hash() == b.definition_hash(),
                        (Err(a), Err(b)) => a == b,
                        _ => false,
                    }
            });
            if !keep {
                s.stop();
            }
            keep
        });
        for (name, d) in wanted {
            if g.contains_key(&name) {
                continue;
            }
            let o = opts(&d);
            let s = McpServer::start(
                &name,
                d.scope,
                d.config,
                o,
                self.rt.clone(),
                self.events.clone(),
            );
            g.insert(name, s);
        }
    }

    /// Add or replace one server.
    pub fn insert(&self, def: ScopedServer, opts: ServerOptions) -> McpServer {
        let s = McpServer::start(
            &def.name,
            def.scope,
            def.config,
            opts,
            self.rt.clone(),
            self.events.clone(),
        );
        if let Ok(mut g) = self.servers.write()
            && let Some(old) = g.insert(def.name, s.clone())
        {
            old.stop();
        }
        s
    }

    pub fn remove(&self, name: &str) -> bool {
        let old = self.servers.write().ok().and_then(|mut g| g.remove(name));
        if let Some(s) = &old {
            s.stop();
        }
        old.is_some()
    }

    pub fn get(&self, name: &str) -> Result<McpServer, McpError> {
        self.servers
            .read()
            .ok()
            .and_then(|g| g.get(name).cloned())
            .ok_or_else(|| McpError::Unknown {
                kind: "server",
                name: name.to_string(),
            })
    }

    pub fn servers(&self) -> Vec<McpServer> {
        self.servers
            .read()
            .map(|g| g.values().cloned().collect())
            .unwrap_or_default()
    }

    /// `mcp.list`.
    pub fn list(&self) -> Vec<McpServerInfo> {
        self.servers().iter().map(McpServer::info).collect()
    }

    /// Wait up to `grace` for servers still `pending` (06 §7: 2 s before the first model
    /// call, rather than blocking on slow servers).
    pub async fn wait_ready(&self, grace: Duration) {
        let servers = self.servers();
        let waits = servers
            .iter()
            .map(|s| s.wait_status(grace, |st| st != McpStatus::Pending));
        futures::future::join_all(waits).await;
    }

    /// Every connected server's tools with collision-free `mcp__<server>__<tool>` names.
    pub fn tools(&self) -> Vec<McpTool> {
        let mut tools: Vec<McpTool> = self
            .servers()
            .iter()
            .filter(|s| s.status() == McpStatus::Connected)
            .flat_map(|s| s.tools())
            .collect();
        let pairs: Vec<(String, String)> = tools
            .iter()
            .map(|t| (t.server.clone(), t.name.clone()))
            .collect();
        for (t, n) in tools.iter_mut().zip(naming::assign(&pairs)) {
            t.model_name = n;
        }
        tools
    }

    /// Model-facing name → (server, original tool name).
    pub fn resolve(&self, model_name: &str) -> Result<(McpServer, String), McpError> {
        let t = self
            .tools()
            .into_iter()
            .find(|t| t.model_name == model_name)
            .ok_or_else(|| McpError::Unknown {
                kind: "tool",
                name: model_name.to_string(),
            })?;
        Ok((self.get(&t.server)?, t.name))
    }

    /// Call a tool by its model-facing name.
    pub async fn call(
        &self,
        model_name: &str,
        args: Value,
        opts: CallOpts,
    ) -> Result<ToolResult, McpError> {
        let (server, tool) = self.resolve(model_name)?;
        server.call_tool(&tool, args, opts).await
    }

    /// Stop every server (daemon shutdown).
    pub fn shutdown(&self) {
        for s in self.servers() {
            s.stop();
        }
    }
}
