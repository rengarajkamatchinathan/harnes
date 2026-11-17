//! Server definitions: Claude-compatible `mcpServers` entries (06 §7, D30).
//!
//! `{command, args?, env?, cwd?}` (stdio, `type` optional or `"stdio"`) or
//! `{type: "http", url, headers?}` (Streamable HTTP). `type: "sse"` is the legacy transport,
//! recognised but not implemented (used only if a server reports it; see README gaps).
//! Reading the settings files and trust/definition-hash approval belong to `genie-config`;
//! this module parses one entry and merges scopes (later scope wins per name).

use std::collections::BTreeMap;

use genie_protocol::common::ConfigScope;
use serde_json::{Map, Value};
use sha2::{Digest, Sha256};

use crate::error::McpError;

/// Where a server definition came from. Order is precedence: later wins per name.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum McpScope {
    User,
    Project,
    Local,
    Managed,
}

impl McpScope {
    /// The wire scope (`McpServerInfo.scope`). `ConfigScope` has no `managed` (policy is a
    /// source, never a write target), so managed servers report `unknown`.
    pub fn to_config_scope(self) -> ConfigScope {
        match self {
            McpScope::User => ConfigScope::User,
            McpScope::Project => ConfigScope::Project,
            McpScope::Local => ConfigScope::Local,
            McpScope::Managed => ConfigScope::Unknown,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ServerConfig {
    Stdio {
        command: String,
        args: Vec<String>,
        /// Values may be keychain references; resolved at spawn by a [`SecretResolver`].
        env: BTreeMap<String, String>,
        cwd: Option<String>,
    },
    Http {
        url: String,
        /// Values may be keychain references; resolved per request by a [`SecretResolver`].
        headers: BTreeMap<String, String>,
    },
}

impl ServerConfig {
    pub fn from_value(v: &Value) -> Result<ServerConfig, McpError> {
        let obj = v
            .as_object()
            .ok_or_else(|| McpError::Config("server definition must be an object".into()))?;
        let ty = obj.get("type").and_then(Value::as_str);
        match ty {
            None | Some("stdio") => {
                let command = str_field(obj, "command")?
                    .ok_or_else(|| McpError::Config("stdio server needs `command`".into()))?;
                Ok(ServerConfig::Stdio {
                    command,
                    args: str_list(obj, "args")?,
                    env: str_map(obj, "env")?,
                    cwd: str_field(obj, "cwd")?,
                })
            }
            Some("http") | Some("streamable-http") | Some("streamableHttp") => {
                let url = str_field(obj, "url")?
                    .ok_or_else(|| McpError::Config("http server needs `url`".into()))?;
                if !(url.starts_with("https://") || url.starts_with("http://")) {
                    return Err(McpError::Config(format!("unsupported url `{url}`")));
                }
                Ok(ServerConfig::Http {
                    url,
                    headers: str_map(obj, "headers")?,
                })
            }
            Some("sse") => Err(McpError::Config(
                "legacy SSE transport is not supported yet".into(),
            )),
            Some(other) => Err(McpError::Config(format!("unknown server type `{other}`"))),
        }
    }

    pub fn to_value(&self) -> Value {
        match self {
            ServerConfig::Stdio {
                command,
                args,
                env,
                cwd,
            } => {
                let mut m = Map::new();
                m.insert("command".into(), command.clone().into());
                m.insert("args".into(), args.clone().into());
                m.insert(
                    "env".into(),
                    Value::Object(
                        env.iter()
                            .map(|(k, v)| (k.clone(), v.clone().into()))
                            .collect(),
                    ),
                );
                if let Some(c) = cwd {
                    m.insert("cwd".into(), c.clone().into());
                }
                Value::Object(m)
            }
            ServerConfig::Http { url, headers } => serde_json::json!({
                "type": "http",
                "url": url,
                "headers": headers,
            }),
        }
    }

    /// SHA-256 over the canonical definition. Used for `.mcp.json` per-server approval
    /// (D43) and to share one process per identical config across sessions.
    pub fn definition_hash(&self) -> String {
        // serde_json maps are BTreeMap-backed (no `preserve_order`), so this is canonical.
        let canon = serde_json::to_vec(&self.to_value()).unwrap_or_default();
        hex::encode(Sha256::digest(&canon))
    }

    pub fn is_remote(&self) -> bool {
        matches!(self, ServerConfig::Http { .. })
    }
}

fn str_field(obj: &Map<String, Value>, k: &str) -> Result<Option<String>, McpError> {
    match obj.get(k) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(s)) => Ok(Some(s.clone())),
        Some(_) => Err(McpError::Config(format!("`{k}` must be a string"))),
    }
}

fn str_list(obj: &Map<String, Value>, k: &str) -> Result<Vec<String>, McpError> {
    match obj.get(k) {
        None | Some(Value::Null) => Ok(Vec::new()),
        Some(Value::Array(a)) => a
            .iter()
            .map(|v| {
                v.as_str()
                    .map(String::from)
                    .ok_or_else(|| McpError::Config(format!("`{k}` must be strings")))
            })
            .collect(),
        Some(_) => Err(McpError::Config(format!("`{k}` must be an array"))),
    }
}

fn str_map(obj: &Map<String, Value>, k: &str) -> Result<BTreeMap<String, String>, McpError> {
    match obj.get(k) {
        None | Some(Value::Null) => Ok(BTreeMap::new()),
        Some(Value::Object(m)) => m
            .iter()
            .map(|(name, v)| {
                v.as_str()
                    .map(|s| (name.clone(), s.to_string()))
                    .ok_or_else(|| McpError::Config(format!("`{k}.{name}` must be a string")))
            })
            .collect(),
        Some(_) => Err(McpError::Config(format!("`{k}` must be an object"))),
    }
}

/// A server definition after scope merging.
#[derive(Debug, Clone, PartialEq)]
pub struct ScopedServer {
    pub name: String,
    pub scope: McpScope,
    pub config: Result<ServerConfig, String>,
}

/// Merge `mcpServers` maps from several scopes; later scope wins per server name (06 §7).
/// Layers may come in any order; they are applied by [`McpScope`] precedence.
pub fn merge_scopes(
    layers: impl IntoIterator<Item = (McpScope, Map<String, Value>)>,
) -> Vec<ScopedServer> {
    let mut layers: Vec<_> = layers.into_iter().collect();
    layers.sort_by_key(|(s, _)| *s);
    let mut out: BTreeMap<String, ScopedServer> = BTreeMap::new();
    for (scope, servers) in layers {
        for (name, def) in servers {
            let config = ServerConfig::from_value(&def).map_err(|e| e.to_string());
            out.insert(
                name.clone(),
                ScopedServer {
                    name,
                    scope,
                    config,
                },
            );
        }
    }
    out.into_values().collect()
}

/// Resolves keychain references in `env`/`headers` values (D26). Returning the value
/// unchanged is fine for literals; an error fails the connection with that message.
pub trait SecretResolver: Send + Sync {
    fn resolve(&self, value: &str) -> Result<genie_secrets::SecretString, String>;
}

/// Treats every value as a literal.
pub struct LiteralSecrets;

impl SecretResolver for LiteralSecrets {
    fn resolve(&self, value: &str) -> Result<genie_secrets::SecretString, String> {
        Ok(genie_secrets::SecretString::from(value.to_string()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn parses_claude_shapes() {
        let s = ServerConfig::from_value(
            &json!({"command": "npx", "args": ["-y", "x"], "env": {"K": "v"}}),
        )
        .unwrap();
        assert!(matches!(s, ServerConfig::Stdio { ref command, .. } if command == "npx"));
        let h = ServerConfig::from_value(
            &json!({"type": "http", "url": "https://x/mcp", "headers": {"A": "b"}}),
        )
        .unwrap();
        assert!(h.is_remote());
        assert!(ServerConfig::from_value(&json!({"type": "sse", "url": "https://x"})).is_err());
        assert!(ServerConfig::from_value(&json!({"args": []})).is_err());
        assert!(ServerConfig::from_value(&json!({"type": "http", "url": "file:///x"})).is_err());
        assert!(ServerConfig::from_value(&json!({"command": "x", "env": {"K": 1}})).is_err());
    }

    #[test]
    fn hash_is_stable_and_sensitive() {
        let a = ServerConfig::from_value(&json!({"command": "a", "env": {"B": "1", "A": "2"}}))
            .unwrap();
        let b = ServerConfig::from_value(&json!({"env": {"A": "2", "B": "1"}, "command": "a"}))
            .unwrap();
        let c = ServerConfig::from_value(&json!({"command": "a", "env": {"A": "3", "B": "1"}}))
            .unwrap();
        assert_eq!(a.definition_hash(), b.definition_hash());
        assert_ne!(a.definition_hash(), c.definition_hash());
    }

    #[test]
    fn later_scope_wins() {
        let user = json!({"x": {"command": "user"}, "y": {"command": "y"}});
        let project = json!({"x": {"command": "project"}});
        let merged = merge_scopes([
            (McpScope::Project, project.as_object().unwrap().clone()),
            (McpScope::User, user.as_object().unwrap().clone()),
        ]);
        assert_eq!(merged.len(), 2);
        let x = merged.iter().find(|s| s.name == "x").unwrap();
        assert_eq!(x.scope, McpScope::Project);
        assert!(
            matches!(x.config, Ok(ServerConfig::Stdio { ref command, .. }) if command == "project")
        );
    }
}
