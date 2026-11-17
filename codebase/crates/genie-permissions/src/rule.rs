//! Rule grammar (06 §5.1): `Tool` or `Tool(specifier)`, Claude-compatible.
//!
//! ```text
//! rule      = tool [ "(" specifier ")" ]
//! tool      = 1*( ALPHA / DIGIT / "_" / "-" ) [ "*" ]      ; "*" only after `mcp__server__`
//! specifier = 1*CHAR                                       ; up to the last ")"
//! ```
//!
//! Specifiers by tool:
//! - `Bash(cmd)` / `PowerShell(cmd)`: exact command, `prefix:*` (word-boundary prefix), or a
//!   pattern with `*` wildcards.
//! - `Read(path)` / `Edit(path)` / `Write(path)`: gitignore-style glob. `//abs`, `~/home`,
//!   `/project-root-relative`, `./cwd-relative` or bare relative (cwd).
//! - `WebFetch(domain:host)`, `Browser(site:host)` with `*.` wildcard, `Browser(action:name)`.
//! - `mcp__server` / `mcp__server__*` (whole server) and `mcp__server__tool`.
//! - Anything else: the tool name, and an optional specifier compared with `*` wildcards.
//!
//! `Tool(*)` is the same as `Tool`.

use std::fmt;
use std::str::FromStr;

/// What a rule applies to, after parsing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Matcher {
    /// `Bash` / `PowerShell` (`posix` false for PowerShell).
    Shell {
        posix: bool,
        pattern: Option<CommandPattern>,
    },
    /// `Read(...)` rules.
    Read(Option<String>),
    /// `Edit(...)` / `Write(...)` rules (every file-mutating tool).
    Edit(Option<String>),
    WebFetch(Option<String>),
    WebSearch,
    Browser(Option<BrowserSpec>),
    Mcp {
        server: String,
        tool: Option<String>,
    },
    Other {
        tool: String,
        spec: Option<String>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CommandPattern {
    Exact(String),
    /// `git commit:*`: the command is the prefix or the prefix followed by a space.
    Prefix(String),
    /// Contains `*` wildcards.
    Glob(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BrowserSpec {
    Site(String),
    Action(String),
}

/// A parsed permission rule. Displays as the original text.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Rule {
    raw: String,
    pub matcher: Matcher,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum RuleError {
    #[error("empty rule")]
    Empty,
    #[error("`{0}`: tool name may only contain letters, digits, `_` and `-`")]
    BadTool(String),
    #[error("`{0}`: missing closing `)`")]
    Unclosed(String),
    #[error("`{0}`: empty specifier, use the bare tool name")]
    EmptySpecifier(String),
    #[error("`{0}`: {1}")]
    BadSpecifier(String, &'static str),
}

impl Rule {
    pub fn parse(text: &str) -> Result<Rule, RuleError> {
        let raw = text.trim();
        if raw.is_empty() {
            return Err(RuleError::Empty);
        }
        let (tool, spec) = match raw.find('(') {
            None => (raw, None),
            Some(i) => {
                if !raw.ends_with(')') {
                    return Err(RuleError::Unclosed(raw.into()));
                }
                let s = raw[i + 1..raw.len() - 1].trim();
                if s.is_empty() {
                    return Err(RuleError::EmptySpecifier(raw.into()));
                }
                (raw[..i].trim(), if s == "*" { None } else { Some(s) })
            }
        };
        let matcher = matcher(raw, tool, spec)?;
        Ok(Rule {
            raw: raw.to_string(),
            matcher,
        })
    }

    pub fn as_str(&self) -> &str {
        &self.raw
    }
}

impl fmt::Display for Rule {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.raw)
    }
}

impl FromStr for Rule {
    type Err = RuleError;
    fn from_str(s: &str) -> Result<Rule, RuleError> {
        Rule::parse(s)
    }
}

fn valid_ident(s: &str) -> bool {
    !s.is_empty()
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
}

fn matcher(raw: &str, tool: &str, spec: Option<&str>) -> Result<Matcher, RuleError> {
    if let Some(rest) = tool.strip_prefix("mcp__") {
        if spec.is_some() {
            return Err(RuleError::BadSpecifier(
                raw.into(),
                "MCP rules take no specifier (use mcp__server__tool)",
            ));
        }
        let (server, t) = match rest.split_once("__") {
            None => (rest, None),
            Some((s, "*")) => (s, None),
            Some((s, t)) => (s, Some(t)),
        };
        if !valid_ident(server) || t.is_some_and(|t| !valid_ident(t)) {
            return Err(RuleError::BadTool(raw.into()));
        }
        return Ok(Matcher::Mcp {
            server: server.to_string(),
            tool: t.map(str::to_string),
        });
    }
    if !valid_ident(tool) {
        return Err(RuleError::BadTool(raw.into()));
    }
    let spec_s = spec.map(str::to_string);
    Ok(match tool {
        "Bash" | "PowerShell" => Matcher::Shell {
            posix: tool == "Bash",
            pattern: spec.map(command_pattern),
        },
        "Read" => Matcher::Read(spec_s),
        "Edit" | "Write" => Matcher::Edit(spec_s),
        "WebFetch" => match spec {
            None => Matcher::WebFetch(None),
            Some(s) => match s.strip_prefix("domain:") {
                Some(d) if !d.trim().is_empty() => {
                    Matcher::WebFetch(Some(d.trim().to_ascii_lowercase()))
                }
                _ => {
                    return Err(RuleError::BadSpecifier(
                        raw.into(),
                        "expected `domain:<host>`",
                    ));
                }
            },
        },
        "WebSearch" => {
            if spec.is_some() {
                return Err(RuleError::BadSpecifier(
                    raw.into(),
                    "WebSearch takes no specifier",
                ));
            }
            Matcher::WebSearch
        }
        "Browser" => match spec {
            None => Matcher::Browser(None),
            Some(s) => {
                if let Some(site) = s.strip_prefix("site:") {
                    Matcher::Browser(Some(BrowserSpec::Site(site.trim().to_ascii_lowercase())))
                } else if let Some(a) = s.strip_prefix("action:") {
                    Matcher::Browser(Some(BrowserSpec::Action(a.trim().to_ascii_lowercase())))
                } else {
                    return Err(RuleError::BadSpecifier(
                        raw.into(),
                        "expected `site:<host>` or `action:<name>`",
                    ));
                }
            }
        },
        _ => Matcher::Other {
            tool: tool.to_string(),
            spec: spec_s,
        },
    })
}

/// Collapse runs of whitespace so `git  commit` matches `git commit`.
pub fn normalize_command(cmd: &str) -> String {
    cmd.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn command_pattern(spec: &str) -> CommandPattern {
    let s = normalize_command(spec);
    if let Some(p) = s.strip_suffix(":*") {
        CommandPattern::Prefix(p.trim_end().to_string())
    } else if s.contains('*') {
        CommandPattern::Glob(s)
    } else {
        CommandPattern::Exact(s)
    }
}

impl CommandPattern {
    /// `cmd` must already be normalised (see [`normalize_command`]).
    pub fn matches(&self, cmd: &str, case_insensitive: bool) -> bool {
        let (pat, cmd) = if case_insensitive {
            (self.text().to_lowercase(), cmd.to_lowercase())
        } else {
            (self.text().to_string(), cmd.to_string())
        };
        match self {
            CommandPattern::Exact(_) => cmd == pat,
            CommandPattern::Prefix(_) => {
                cmd == pat || (cmd.starts_with(&pat) && cmd[pat.len()..].starts_with(' '))
            }
            CommandPattern::Glob(_) => crate::path::wildcard(&pat, &cmd),
        }
    }

    fn text(&self) -> &str {
        match self {
            CommandPattern::Exact(s) | CommandPattern::Prefix(s) | CommandPattern::Glob(s) => s,
        }
    }
}

/// `example.com` matches only that host; `*.example.com` matches the apex and every
/// subdomain; `*` matches everything. Case-insensitive.
pub fn domain_matches(pattern: &str, host: &str) -> bool {
    let host = host.trim_end_matches('.').to_ascii_lowercase();
    let pattern = pattern.to_ascii_lowercase();
    if pattern == "*" {
        return true;
    }
    match pattern.strip_prefix("*.") {
        Some(apex) => host == apex || host.ends_with(&format!(".{apex}")),
        None => host == pattern,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_table() {
        let ok = [
            (
                "Bash",
                Matcher::Shell {
                    posix: true,
                    pattern: None,
                },
            ),
            (
                "Bash(*)",
                Matcher::Shell {
                    posix: true,
                    pattern: None,
                },
            ),
            (
                "Bash(git  commit:*)",
                Matcher::Shell {
                    posix: true,
                    pattern: Some(CommandPattern::Prefix("git commit".into())),
                },
            ),
            (
                "PowerShell(Get-ChildItem:*)",
                Matcher::Shell {
                    posix: false,
                    pattern: Some(CommandPattern::Prefix("Get-ChildItem".into())),
                },
            ),
            (
                "Bash(npm run build)",
                Matcher::Shell {
                    posix: true,
                    pattern: Some(CommandPattern::Exact("npm run build".into())),
                },
            ),
            ("Read(./src/**)", Matcher::Read(Some("./src/**".into()))),
            ("Edit(/docs/**)", Matcher::Edit(Some("/docs/**".into()))),
            ("Write", Matcher::Edit(None)),
            (
                "WebFetch(domain:Example.com)",
                Matcher::WebFetch(Some("example.com".into())),
            ),
            ("WebSearch", Matcher::WebSearch),
            (
                "Browser(site:*.example.com)",
                Matcher::Browser(Some(BrowserSpec::Site("*.example.com".into()))),
            ),
            (
                "Browser(action:submit)",
                Matcher::Browser(Some(BrowserSpec::Action("submit".into()))),
            ),
            (
                "Artifact(network)",
                Matcher::Other {
                    tool: "Artifact".into(),
                    spec: Some("network".into()),
                },
            ),
            (
                "mcp__github",
                Matcher::Mcp {
                    server: "github".into(),
                    tool: None,
                },
            ),
            (
                "mcp__github__*",
                Matcher::Mcp {
                    server: "github".into(),
                    tool: None,
                },
            ),
            (
                "mcp__github__create_issue",
                Matcher::Mcp {
                    server: "github".into(),
                    tool: Some("create_issue".into()),
                },
            ),
        ];
        for (text, want) in ok {
            let r = Rule::parse(text).unwrap_or_else(|e| panic!("{text}: {e}"));
            assert_eq!(r.matcher, want, "{text}");
        }
        let bad = [
            "",
            "Bash(",
            "Bash()",
            "Ba sh",
            "WebFetch(example.com)",
            "Browser(x)",
            "mcp__a(b)",
            "WebSearch(x)",
        ];
        for text in bad {
            assert!(Rule::parse(text).is_err(), "{text:?} should fail");
        }
    }

    #[test]
    fn command_patterns() {
        let p = command_pattern("git commit:*");
        assert!(p.matches("git commit", false));
        assert!(p.matches("git commit -m x", false));
        assert!(!p.matches("git commits", false));
        assert!(!p.matches("git", false));
        let g = command_pattern("npm run *");
        assert!(g.matches("npm run test", false));
        assert!(!g.matches("npm install", false));
        let ci = command_pattern("Get-ChildItem:*");
        assert!(ci.matches("get-childitem -Recurse", true));
    }

    #[test]
    fn domains() {
        assert!(domain_matches("example.com", "EXAMPLE.com"));
        assert!(!domain_matches("example.com", "api.example.com"));
        assert!(domain_matches("*.example.com", "api.example.com"));
        assert!(domain_matches("*.example.com", "example.com"));
        assert!(!domain_matches("*.example.com", "badexample.com"));
        assert!(domain_matches("*", "x.y"));
    }
}
