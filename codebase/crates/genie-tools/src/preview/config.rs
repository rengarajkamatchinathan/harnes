//! `.genie/launch.json` (ticket 47 phase 2): the same fields as Claude Code Desktop's
//! `.claude/launch.json`, so a project can carry one file for both.
//!
//! ```json
//! {
//!   "version": "0.0.1",
//!   "autoVerify": true,
//!   "configurations": [
//!     { "name": "web", "runtimeExecutable": "npm", "runtimeArgs": ["run", "dev"], "port": 5173 }
//!   ]
//! }
//! ```
//!
//! Keys Genie doesn't know are kept when the file is rewritten.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

/// The file, relative to the session folder.
pub const LAUNCH_FILE: &str = ".genie/launch.json";

/// One server entry.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct ServerConfig {
    /// Unique name of the server (e.g. `web`, `api`).
    pub name: String,
    /// The program that starts the server (`npm`, `bun`, `node`, `python3` …).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub runtime_executable: Option<String>,
    /// Arguments to `runtimeExecutable` (e.g. `["run", "dev"]`).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub runtime_args: Vec<String>,
    /// A script to run (after `runtimeArgs`); without `runtimeExecutable`, `.js/.mjs/.cjs/.ts`
    /// run with `node` and `.py` with `python3`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub program: Option<String>,
    /// Arguments after `program`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub args: Vec<String>,
    /// The port the server listens on. It is also passed as `PORT`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub port: Option<u16>,
    /// `true`: pick a free port when `port` is taken or missing. `false`: fail if taken.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auto_port: Option<bool>,
    /// Working directory, relative to the project folder.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    /// Extra environment variables.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub env: BTreeMap<String, String>,
    /// The URL to open. Alone (no command), attaches to a server that is already running.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub url: Option<String>,
    /// Per-server override of the project's `autoVerify`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auto_verify: Option<bool>,
    #[serde(flatten)]
    #[schemars(skip)]
    pub extra: Map<String, Value>,
}

impl ServerConfig {
    /// Whether the entry starts a process (`false`: attach to `url`).
    pub fn has_command(&self) -> bool {
        self.runtime_executable.is_some() || self.program.is_some()
    }

    /// `argv` of the server: `runtimeExecutable runtimeArgs… program args…`.
    pub fn argv(&self) -> Option<Vec<String>> {
        let mut v = Vec::new();
        match (&self.runtime_executable, &self.program) {
            (Some(rt), _) => {
                v.push(rt.clone());
                v.extend(self.runtime_args.iter().cloned());
            }
            (None, Some(p)) => {
                let lower = p.to_ascii_lowercase();
                if [".js", ".mjs", ".cjs", ".ts"]
                    .iter()
                    .any(|e| lower.ends_with(e))
                {
                    v.push("node".into());
                } else if lower.ends_with(".py") {
                    v.push(if cfg!(windows) { "python" } else { "python3" }.into());
                }
                v.extend(self.runtime_args.iter().cloned());
            }
            (None, None) => return None,
        }
        if let Some(p) = &self.program {
            v.push(p.clone());
        }
        v.extend(self.args.iter().cloned());
        Some(v)
    }

    /// The command line for display and permission rules (POSIX quoting).
    pub fn command_line(&self) -> Option<String> {
        self.argv().map(|v| {
            v.iter()
                .map(|a| display_quote(a))
                .collect::<Vec<_>>()
                .join(" ")
        })
    }
}

fn display_quote(a: &str) -> String {
    let plain = !a.is_empty()
        && a.chars()
            .all(|c| c.is_ascii_alphanumeric() || "-_./:=@%+,".contains(c));
    if plain {
        a.to_string()
    } else {
        format!("'{}'", a.replace('\'', r"'\''"))
    }
}

/// The whole file.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LaunchFile {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    /// Project-wide auto-verify (ticket 47 phase 3); default on.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auto_verify: Option<bool>,
    #[serde(default)]
    pub configurations: Vec<ServerConfig>,
    #[serde(flatten)]
    pub extra: Map<String, Value>,
}

impl LaunchFile {
    pub fn path(root: &Path) -> PathBuf {
        root.join(LAUNCH_FILE)
    }

    /// Read the file; a missing file is an empty one.
    pub fn load(root: &Path) -> Result<LaunchFile, String> {
        let path = LaunchFile::path(root);
        match std::fs::read_to_string(&path) {
            Ok(s) => parse(&s).map_err(|e| format!("{LAUNCH_FILE}: {e}")),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(LaunchFile::default()),
            Err(e) => Err(format!("{LAUNCH_FILE}: {e}")),
        }
    }

    pub fn get(&self, name: &str) -> Option<&ServerConfig> {
        self.configurations.iter().find(|c| c.name == name)
    }

    /// Add or replace the entry with `cfg.name`.
    pub fn upsert(&mut self, cfg: ServerConfig) {
        match self.configurations.iter_mut().find(|c| c.name == cfg.name) {
            Some(slot) => *slot = cfg,
            None => self.configurations.push(cfg),
        }
    }

    pub fn save(&self, root: &Path) -> Result<(), String> {
        let path = LaunchFile::path(root);
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
        }
        let mut f = self.clone();
        if f.version.is_none() {
            f.version = Some("0.0.1".into());
        }
        let mut s = serde_json::to_string_pretty(&f).map_err(|e| e.to_string())?;
        s.push('\n');
        std::fs::write(&path, s).map_err(|e| format!("{}: {e}", path.display()))
    }

    /// Project `autoVerify`, default on.
    pub fn auto_verify(&self) -> bool {
        self.auto_verify != Some(false)
    }
}

/// JSON with `//` and `/* */` comments and trailing commas (VS Code style).
fn parse(s: &str) -> Result<LaunchFile, serde_json::Error> {
    serde_json::from_str(&strip_jsonc(s))
}

fn strip_jsonc(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = String::with_capacity(s.len());
    let mut i = 0;
    let mut in_str = false;
    let mut start = 0;
    while i < b.len() {
        let c = b[i];
        if in_str {
            if c == b'\\' {
                i += 2;
                continue;
            }
            if c == b'"' {
                in_str = false;
            }
            i += 1;
            continue;
        }
        match c {
            b'"' => {
                in_str = true;
                i += 1;
            }
            b'/' if b.get(i + 1) == Some(&b'/') => {
                out.push_str(&s[start..i]);
                while i < b.len() && b[i] != b'\n' {
                    i += 1;
                }
                start = i;
            }
            b'/' if b.get(i + 1) == Some(&b'*') => {
                out.push_str(&s[start..i]);
                i += 2;
                while i + 1 < b.len() && !(b[i] == b'*' && b[i + 1] == b'/') {
                    i += 1;
                }
                i = (i + 2).min(b.len());
                start = i;
            }
            _ => i += 1,
        }
    }
    out.push_str(&s[start.min(s.len())..]);
    strip_trailing_commas(&out)
}

fn strip_trailing_commas(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = String::with_capacity(s.len());
    let mut in_str = false;
    let mut i = 0;
    let mut start = 0;
    while i < b.len() {
        let c = b[i];
        if in_str {
            if c == b'\\' {
                i += 2;
                continue;
            }
            if c == b'"' {
                in_str = false;
            }
        } else if c == b'"' {
            in_str = true;
        } else if c == b',' {
            let mut j = i + 1;
            while j < b.len() && b[j].is_ascii_whitespace() {
                j += 1;
            }
            if j < b.len() && (b[j] == b'}' || b[j] == b']') {
                out.push_str(&s[start..i]);
                start = i + 1;
            }
        }
        i += 1;
    }
    out.push_str(&s[start.min(s.len())..]);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_claude_style_files_with_comments_and_keeps_unknown_keys() {
        let f = parse(
            r#"{
              // Claude's format
              "version": "0.0.1",
              "configurations": [
                { "name": "web", "runtimeExecutable": "npm", "runtimeArgs": ["run", "dev"],
                  "port": 3000, "type": "node", /* kept */ },
                { "name": "api", "program": "server.js", "args": ["--x"], "env": {"A": "b"} },
              ],
            }"#,
        )
        .unwrap();
        assert!(f.auto_verify());
        let web = f.get("web").unwrap();
        assert_eq!(web.argv().unwrap(), ["npm", "run", "dev"]);
        assert_eq!(web.extra.get("type"), Some(&Value::from("node")));
        let api = f.get("api").unwrap();
        assert_eq!(api.argv().unwrap(), ["node", "server.js", "--x"]);
        assert_eq!(api.command_line().unwrap(), "node server.js --x");
        let back = serde_json::to_value(&f).unwrap();
        assert_eq!(back["configurations"][0]["type"], "node");
        assert_eq!(back["configurations"][0]["runtimeArgs"][1], "dev");
    }

    #[test]
    fn strings_with_comment_markers_survive() {
        let f = parse(r#"{"configurations":[{"name":"a","url":"http://x//y/*z*/"}]}"#).unwrap();
        assert_eq!(f.get("a").unwrap().url.as_deref(), Some("http://x//y/*z*/"));
        assert!(!f.get("a").unwrap().has_command());
    }

    #[test]
    fn round_trips_through_disk() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(LaunchFile::load(dir.path()).unwrap(), LaunchFile::default());
        let mut f = LaunchFile::default();
        f.upsert(ServerConfig {
            name: "web".into(),
            runtime_executable: Some("bun".into()),
            runtime_args: vec!["dev".into()],
            ..Default::default()
        });
        f.save(dir.path()).unwrap();
        let g = LaunchFile::load(dir.path()).unwrap();
        assert_eq!(g.version.as_deref(), Some("0.0.1"));
        assert_eq!(g.get("web").unwrap().argv().unwrap(), ["bun", "dev"]);
    }
}
