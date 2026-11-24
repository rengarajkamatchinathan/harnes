//! The settings schema (D23): Rust types → JSON Schema (`schemars`) → editors, TS types and
//! the Settings UI metadata (D34).
//!
//! Only keys whose owning spec is implemented, or that permissions/policy need, are typed
//! here; subsystem blocks that are specified elsewhere (`mcpServers`, `hooks`, `providers`)
//! are kept as opaque JSON until their crates land.

use std::collections::BTreeMap;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};

use genie_protocol::api::config::ConfigSchemaResult;
use genie_protocol::common::{Diagnostic, Mode, Severity};

/// `settings.json` at any scope (strict JSON, D23).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Settings {
    /// Editor schema reference; ignored by Genie.
    #[serde(rename = "$schema", default, skip_serializing_if = "Option::is_none")]
    pub schema: Option<String>,
    /// Permission rules and mode (06 §5).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub permissions: Option<PermissionsSettings>,
    /// Default model id for new sessions.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// Model used for titles and compaction.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub small_model: Option<String>,
    /// Default reasoning effort.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effort: Option<Effort>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub theme: Option<Theme>,
    /// Shell used by the `shell` tool (06 §13.2).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub shell: Option<ShellChoice>,
    /// Run the project formatter after edits (06 §4.2).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub formatter: Option<bool>,
    /// Extra instruction files (06 §6).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub instructions: Option<Vec<String>>,
    /// MCP servers (06 §7); validated by `genie-mcp`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mcp_servers: Option<BTreeMap<String, Value>>,
    /// Hooks (06 §12); validated by the hooks dispatcher.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hooks: Option<Vec<Value>>,
    /// Provider routes (03); `${env:VAR}` only allowed here (D23).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub providers: Option<BTreeMap<String, Value>>,
    /// User model overrides `models.<id>` (03 §8.1 step 5): merge patches over catalog
    /// entries (context window, price, capabilities); validated by the catalog.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub models: Option<BTreeMap<String, Value>>,
    /// Model ids left out of the model pickers (Settings → Models). The models stay in the
    /// catalog, so they can be turned back on; unlike policy-hidden models (D38) they are not removed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hidden_models: Option<Vec<String>>,
    /// `secrets.backend: "file"` forces `secrets.enc` (03 §10.1); read at daemon start.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub secrets: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub memory: Option<Toggle>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub chat_search: Option<Toggle>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub web_search: Option<Toggle>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub code_execution: Option<Toggle>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub artifacts: Option<Toggle>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub browser: Option<BrowserSettings>,
    /// The `image_generate` tool.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub image_generation: Option<ImageGenerationSettings>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub telemetry: Option<TelemetrySettings>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub updates: Option<UpdateSettings>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subagents: Option<SubagentSettings>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct PermissionsSettings {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub allow: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ask: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub deny: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_mode: Option<ModeSetting>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub additional_directories: Option<Vec<String>>,
}

/// `permissions.defaultMode`. Accepts Genie's snake_case and Claude's spellings
/// (`acceptEdits`, `bypassPermissions`) so imported settings load unchanged.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ModeSetting {
    Default,
    #[serde(alias = "acceptEdits")]
    AcceptEdits,
    Plan,
    #[serde(alias = "bypassPermissions")]
    Bypass,
}

impl ModeSetting {
    pub fn to_mode(self) -> Mode {
        match self {
            ModeSetting::Default => Mode::Default,
            ModeSetting::AcceptEdits => Mode::AcceptEdits,
            ModeSetting::Plan => Mode::Plan,
            ModeSetting::Bypass => Mode::Bypass,
        }
    }

    /// Parse a mode string in either spelling (also used for the policy floor).
    pub fn parse(s: &str) -> Option<ModeSetting> {
        serde_json::from_value(Value::String(s.to_string())).ok()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Effort {
    Minimal,
    Low,
    Medium,
    High,
    Max,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum Theme {
    System,
    Light,
    Dark,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ShellChoice {
    Default,
    Bash,
    Pwsh,
    Powershell,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Toggle {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub enabled: Option<bool>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum BrowserProfile {
    Persistent,
    Ephemeral,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct BrowserSettings {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub enabled: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub profile: Option<BrowserProfile>,
    /// Per-site states set in Settings → Browser (`browser.sites.set`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sites: Option<Vec<genie_protocol::api::browser::SitePermission>>,
    /// The Browser tab's ⋮ **Keep cookies** (ticket 47); default on. Off: cookies are
    /// cleared when the daemon exits.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub keep_cookies: Option<bool>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ImageGenerationSettings {
    /// On by default; the tool still needs its provider set up.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub enabled: Option<bool>,
    /// `<provider>/<model>`, e.g. `openai/gpt-image-1` or `azure/<deployment>`
    /// (default `openai/gpt-image-1`). The provider must speak the OpenAI images API.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TelemetrySettings {
    /// Crash reports (on by default, D39).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub crash_reports: Option<bool>,
    /// Usage analytics (opt-in, D39).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage: Option<bool>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct UpdateSettings {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub channel: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auto: Option<bool>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SubagentSettings {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_depth: Option<u8>,
}

/// Settings UI metadata per key (D34): which scopes the scope picker offers, restart
/// banners, the advanced section.
#[derive(Debug, Clone, Copy)]
pub struct KeyMeta {
    pub key: &'static str,
    pub title: &'static str,
    /// `user`, `project`, `local`.
    pub scopes: &'static [&'static str],
    pub restart_required: bool,
    pub advanced: bool,
}

const ALL: &[&str] = &["user", "project", "local"];
const USER: &[&str] = &["user"];

pub const UI_META: &[KeyMeta] = &[
    KeyMeta {
        key: "model",
        title: "Default model",
        scopes: ALL,
        restart_required: false,
        advanced: false,
    },
    KeyMeta {
        key: "smallModel",
        title: "Small model",
        scopes: ALL,
        restart_required: false,
        advanced: true,
    },
    KeyMeta {
        key: "effort",
        title: "Reasoning effort",
        scopes: ALL,
        restart_required: false,
        advanced: false,
    },
    KeyMeta {
        key: "hiddenModels",
        title: "Hidden models",
        scopes: USER,
        restart_required: false,
        advanced: false,
    },
    KeyMeta {
        key: "theme",
        title: "Theme",
        scopes: USER,
        restart_required: false,
        advanced: false,
    },
    KeyMeta {
        key: "shell",
        title: "Shell",
        scopes: ALL,
        restart_required: false,
        advanced: false,
    },
    KeyMeta {
        key: "formatter",
        title: "Format after edits",
        scopes: ALL,
        restart_required: false,
        advanced: false,
    },
    KeyMeta {
        key: "instructions",
        title: "Instruction files",
        scopes: ALL,
        restart_required: false,
        advanced: true,
    },
    KeyMeta {
        key: "permissions.allow",
        title: "Allowed actions",
        scopes: ALL,
        restart_required: false,
        advanced: false,
    },
    KeyMeta {
        key: "permissions.ask",
        title: "Always ask for",
        scopes: ALL,
        restart_required: false,
        advanced: false,
    },
    KeyMeta {
        key: "permissions.deny",
        title: "Blocked actions",
        scopes: ALL,
        restart_required: false,
        advanced: false,
    },
    KeyMeta {
        key: "permissions.defaultMode",
        title: "Default mode",
        scopes: ALL,
        restart_required: false,
        advanced: false,
    },
    KeyMeta {
        key: "permissions.additionalDirectories",
        title: "Additional directories",
        scopes: ALL,
        restart_required: false,
        advanced: true,
    },
    KeyMeta {
        key: "mcpServers",
        title: "MCP servers",
        scopes: ALL,
        restart_required: false,
        advanced: false,
    },
    KeyMeta {
        key: "hooks",
        title: "Hooks",
        scopes: ALL,
        restart_required: false,
        advanced: true,
    },
    KeyMeta {
        key: "providers",
        title: "Providers",
        scopes: USER,
        restart_required: false,
        advanced: false,
    },
    KeyMeta {
        key: "memory.enabled",
        title: "Memory",
        scopes: USER,
        restart_required: false,
        advanced: false,
    },
    KeyMeta {
        key: "chatSearch.enabled",
        title: "Search past chats",
        scopes: USER,
        restart_required: false,
        advanced: false,
    },
    KeyMeta {
        key: "webSearch.enabled",
        title: "Web search",
        scopes: ALL,
        restart_required: false,
        advanced: false,
    },
    KeyMeta {
        key: "codeExecution.enabled",
        title: "Code execution",
        scopes: USER,
        restart_required: false,
        advanced: false,
    },
    KeyMeta {
        key: "artifacts.enabled",
        title: "Artifacts",
        scopes: USER,
        restart_required: false,
        advanced: false,
    },
    KeyMeta {
        key: "browser.enabled",
        title: "Browser",
        scopes: USER,
        restart_required: false,
        advanced: false,
    },
    KeyMeta {
        key: "browser.keepCookies",
        title: "Keep browser cookies",
        scopes: USER,
        restart_required: false,
        advanced: false,
    },
    KeyMeta {
        key: "imageGeneration.enabled",
        title: "Image generation",
        scopes: ALL,
        restart_required: false,
        advanced: false,
    },
    KeyMeta {
        key: "imageGeneration.model",
        title: "Image model",
        scopes: ALL,
        restart_required: false,
        advanced: true,
    },
    KeyMeta {
        key: "browser.profile",
        title: "Browser profile",
        scopes: USER,
        restart_required: true,
        advanced: true,
    },
    KeyMeta {
        key: "telemetry.crashReports",
        title: "Crash reports",
        scopes: USER,
        restart_required: true,
        advanced: false,
    },
    KeyMeta {
        key: "telemetry.usage",
        title: "Usage analytics",
        scopes: USER,
        restart_required: false,
        advanced: false,
    },
    KeyMeta {
        key: "updates.channel",
        title: "Update channel",
        scopes: USER,
        restart_required: true,
        advanced: true,
    },
    KeyMeta {
        key: "updates.auto",
        title: "Automatic updates",
        scopes: USER,
        restart_required: false,
        advanced: false,
    },
    KeyMeta {
        key: "subagents.maxDepth",
        title: "Subagent depth",
        scopes: ALL,
        restart_required: false,
        advanced: true,
    },
];

pub fn key_meta(key: &str) -> Option<&'static KeyMeta> {
    UI_META.iter().find(|m| m.key == key)
}

/// `config.schema` (02 §6.4): JSON Schema 2020-12 of [`Settings`] plus UI metadata.
pub fn schema() -> ConfigSchemaResult {
    let schema = serde_json::to_value(schemars::schema_for!(Settings)).unwrap_or(Value::Null);
    let mut keys = Map::new();
    for m in UI_META {
        keys.insert(
            m.key.to_string(),
            json!({
                "title": m.title,
                "scopes_allowed": m.scopes,
                "restart_required": m.restart_required,
                "advanced": m.advanced,
            }),
        );
    }
    ConfigSchemaResult {
        schema,
        ui: json!({ "keys": keys }),
    }
}

fn diag(severity: Severity, source: &str, key: &str, message: String) -> Diagnostic {
    Diagnostic {
        severity,
        message,
        source: Some(source.to_string()),
        key: Some(key.to_string()),
    }
}

fn check(key: &str, v: &Value) -> Result<(), String> {
    let mut m = Map::new();
    m.insert(key.to_string(), v.clone());
    serde_json::from_value::<Settings>(Value::Object(m))
        .map(|_| ())
        .map_err(|e| e.to_string())
}

/// Per-key validation (D23): invalid keys are dropped with a diagnostic and the rest of the
/// file still loads. Objects are checked one level deeper so one bad sub-key does not drop
/// its siblings.
pub fn validate(raw: &Map<String, Value>, source: &str) -> (Map<String, Value>, Vec<Diagnostic>) {
    let mut out = Map::new();
    let mut diags = Vec::new();
    for (k, v) in raw {
        if check(k, v).is_ok() {
            out.insert(k.clone(), v.clone());
            continue;
        }
        let known = check(k, &Value::Null).is_ok();
        if !known {
            diags.push(diag(
                Severity::Warning,
                source,
                k,
                format!("unknown key `{k}`"),
            ));
            continue;
        }
        match v {
            Value::Object(sub) => {
                let mut kept = Map::new();
                for (sk, sv) in sub {
                    let mut one = Map::new();
                    one.insert(sk.clone(), sv.clone());
                    match check(k, &Value::Object(one)) {
                        Ok(()) => {
                            kept.insert(sk.clone(), sv.clone());
                        }
                        Err(e) => diags.push(diag(
                            Severity::Error,
                            source,
                            &format!("{k}.{sk}"),
                            format!("invalid `{k}.{sk}`: {e}"),
                        )),
                    }
                }
                if !kept.is_empty() {
                    out.insert(k.clone(), Value::Object(kept));
                }
            }
            _ => {
                let e = check(k, v).err().unwrap_or_default();
                diags.push(diag(
                    Severity::Error,
                    source,
                    k,
                    format!("invalid `{k}`: {e}"),
                ));
            }
        }
    }
    (out, diags)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validation_is_per_key() {
        let raw = json!({
            "model": "gpt-x",
            "theme": "neon",
            "bogus": 1,
            "permissions": {"allow": ["Bash(ls:*)"], "defaultMode": "sideways", "nope": true},
            "memory": {"enabled": false}
        });
        let (ok, diags) = validate(raw.as_object().unwrap(), "user");
        assert_eq!(ok["model"], "gpt-x");
        assert!(!ok.contains_key("theme"));
        assert!(!ok.contains_key("bogus"));
        assert_eq!(ok["permissions"], json!({"allow": ["Bash(ls:*)"]}));
        assert_eq!(ok["memory"], json!({"enabled": false}));
        let keys: Vec<_> = diags.iter().filter_map(|d| d.key.clone()).collect();
        assert!(keys.contains(&"theme".to_string()));
        assert!(keys.contains(&"bogus".to_string()));
        assert!(keys.contains(&"permissions.defaultMode".to_string()));
        assert!(keys.contains(&"permissions.nope".to_string()));
        let bogus = diags
            .iter()
            .find(|d| d.key.as_deref() == Some("bogus"))
            .unwrap();
        assert_eq!(bogus.severity, Severity::Warning);
    }

    #[test]
    fn schema_has_ui_metadata() {
        let s = schema();
        assert!(s.schema["properties"]["permissions"].is_object());
        assert_eq!(s.ui["keys"]["theme"]["scopes_allowed"], json!(["user"]));
        assert!(key_meta("permissions.allow").is_some());
    }

    #[test]
    fn hidden_models_is_a_user_list_of_ids() {
        let s = schema();
        assert_eq!(
            s.ui["keys"]["hiddenModels"]["scopes_allowed"],
            json!(["user"])
        );
        // An optional array of strings (schemars writes `type: ["array", "null"]`).
        let hm = &s.schema["properties"]["hiddenModels"];
        assert!(hm["type"].to_string().contains("array"), "{hm}");
        assert_eq!(hm["items"]["type"], "string", "{hm}");
        let raw = json!({"hiddenModels": ["openai/gpt-x", "mock/genie-test"]});
        let (ok, diags) = validate(raw.as_object().unwrap(), "user");
        assert_eq!(
            ok["hiddenModels"],
            json!(["openai/gpt-x", "mock/genie-test"])
        );
        assert!(diags.is_empty());
        let bad = json!({"hiddenModels": "openai/gpt-x"});
        let (ok, diags) = validate(bad.as_object().unwrap(), "user");
        assert!(!ok.contains_key("hiddenModels"));
        assert_eq!(diags[0].key.as_deref(), Some("hiddenModels"));
    }
}
