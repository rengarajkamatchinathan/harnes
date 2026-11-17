//! The `genie-permissions` evaluator behind the [`ToolAuthorizer`] port (06 §5, 08 §3).
//!
//! Per call: fold the rules of every loaded settings layer
//! ([`ConfigHub::permission_layers`]), managed policy `permissions.deny/ask`, the
//! session-only CLI layer and the session's remembered "always" rules into a
//! [`RuleSet`]; build the [`ModePolicy`] from `disableBypassMode` / the `defaultMode`
//! floor; map the tool call to an [`Action`]; evaluate. `Verdict`s become allow, deny
//! (`policy_blocked` for policy) or an ask with save suggestions.
//!
//! "Always" (06 §5.4): the rule takes effect at once in memory, is persisted with
//! [`ConfigHub::add_permission_rule`] in the chosen scope (local → `settings.local.json`,
//! user → `~/.genie/settings.json`; session → memory only), and the session's other
//! pending asks are re-evaluated and released when they now pass (02 §7 step 6).
//!
//! Audit (08 §6): `permission.asked`, `permission.decided` (rule allow/deny and user
//! answers) and `policy.blocked`. Commands and arguments are content (dropped unless
//! policy sets `audit.includeContent`).

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use genie_audit::{Attrs, AuditEvent, Auditor};
use genie_config::LayerKind;
use genie_permissions::{
    Action, AllowedBy, AskReason, BrowserAction, Context, Evaluator, ModePolicy, Origin, Request,
    Rule, RuleClass, RuleSet, SaveTarget, ShellDialect, Taint, Verdict,
};
use genie_protocol::ask::{Ask, AskAnswer, AskKind, Decision, Preview};
use genie_protocol::common::{ConfigScope, SessionKind};
use genie_tools::ToolKind;

use crate::config::{ConfigHub, project_root};
use crate::error::BlockSource;
use crate::runtime::ports::{
    AuthzDecision, AuthzRequest, SessionSettings, SettingsSource, ToolAuthorizer, saved_rule,
};

#[derive(Default)]
struct SessState {
    /// "Always" rules remembered for this session (and as a fallback when persisting
    /// failed), with their origin.
    remembered: Vec<(Origin, Rule)>,
    /// Open asks: id → the request (for re-evaluation) and the ask (save suggestions).
    pending: HashMap<String, (Request, Ask)>,
}

/// See the module docs.
pub struct PermissionAuthorizer {
    hub: Arc<ConfigHub>,
    settings: Arc<dyn SettingsSource>,
    audit: Option<Arc<Auditor>>,
    home: Option<String>,
    sessions: Mutex<HashMap<String, SessState>>,
}

impl PermissionAuthorizer {
    pub fn new(hub: Arc<ConfigHub>, settings: Arc<dyn SettingsSource>) -> PermissionAuthorizer {
        PermissionAuthorizer {
            hub,
            settings,
            audit: None,
            home: std::env::var("HOME")
                .or_else(|_| std::env::var("USERPROFILE"))
                .ok(),
            sessions: Mutex::new(HashMap::new()),
        }
    }

    pub fn with_audit(mut self, audit: Arc<Auditor>) -> Self {
        self.audit = Some(audit);
        self
    }

    pub fn with_home(mut self, home: Option<String>) -> Self {
        self.home = home;
        self
    }

    fn record(&self, session: &str, ev: AuditEvent, attrs: Attrs) {
        if let Some(a) = &self.audit {
            a.record(Some(session), ev, attrs);
        }
    }

    fn remembered(&self, session: &str) -> Vec<(Origin, Rule)> {
        self.sessions
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(session)
            .map(|s| s.remembered.clone())
            .unwrap_or_default()
    }

    /// The project root whose settings apply (Code sessions).
    fn root(s: &SessionSettings) -> Option<PathBuf> {
        if s.kind != SessionKind::Code {
            return None;
        }
        s.workspace.as_deref().map(project_root)
    }

    /// The evaluator for `session` in `mode`.
    fn evaluator(
        &self,
        session: &str,
        s: &SessionSettings,
        req_mode: genie_protocol::common::Mode,
    ) -> Evaluator {
        let root = Self::root(s);
        let mut rules = RuleSet::new();
        let mut add_dirs: Vec<String> = Vec::new();
        for (layer, p) in self.hub.permission_layers(root.as_deref()) {
            let origin = match layer {
                LayerKind::Policy => Origin::Policy,
                LayerKind::Cli => Origin::Cli,
                LayerKind::Local => Origin::Local,
                LayerKind::Project => Origin::Project,
                LayerKind::User | LayerKind::Default => Origin::User,
            };
            for (class, list) in [
                (RuleClass::Allow, &p.allow),
                (RuleClass::Ask, &p.ask),
                (RuleClass::Deny, &p.deny),
            ] {
                if let Some(l) = list {
                    log_bad(rules.add_list(class, origin, l));
                }
            }
            add_dirs.extend(p.additional_directories.clone().unwrap_or_default());
        }
        let policy = self.hub.policy();
        log_bad(rules.add_list(
            RuleClass::Deny,
            Origin::Policy,
            &policy.strings("permissions.deny"),
        ));
        log_bad(rules.add_list(
            RuleClass::Ask,
            Origin::Policy,
            &policy.strings("permissions.ask"),
        ));
        if let Some(o) = &s.overlay {
            log_bad(rules.add_list(RuleClass::Allow, Origin::Cli, &o.allow));
            log_bad(rules.add_list(RuleClass::Ask, Origin::Cli, &o.ask));
            log_bad(rules.add_list(RuleClass::Deny, Origin::Cli, &o.deny));
            add_dirs.extend(o.add_dirs.iter().cloned());
        }
        for (origin, rule) in self.remembered(session) {
            rules.add(RuleClass::Allow, origin, rule);
        }
        let mut ctx = match (&s.kind, &s.workspace) {
            (SessionKind::Code, Some(w)) => Context::code(w.display().to_string(), req_mode),
            _ => Context::chat(),
        };
        ctx.mode = if s.kind == SessionKind::Code {
            req_mode
        } else {
            ctx.mode
        };
        ctx.additional_dirs = add_dirs
            .into_iter()
            .map(|d| resolve_dir(&d, s.workspace.as_deref(), self.home.as_deref()))
            .collect();
        ctx.home.clone_from(&self.home);
        Evaluator::new(rules, ctx).with_policy(ModePolicy {
            disable_bypass: policy.disable_bypass(),
            floor: policy.mode_floor(),
        })
    }

    fn request(req: &AuthzRequest, s: &SessionSettings) -> Request {
        let action = action_for(req, s.workspace.as_deref());
        let r = Request::new(action);
        match &req.taint {
            Some(src) => r.tainted(Taint {
                source: src.clone(),
                read_origins: Vec::new(),
            }),
            None => r,
        }
    }
}

fn log_bad(errors: Vec<genie_permissions::RuleError>) {
    for e in errors {
        tracing::warn!(error = %e, "skipping an invalid permission rule");
    }
}

fn resolve_dir(d: &str, workspace: Option<&Path>, home: Option<&str>) -> String {
    if let Some(rest) = d.strip_prefix("~/")
        && let Some(h) = home
    {
        return Path::new(h).join(rest).display().to_string();
    }
    let p = Path::new(d);
    match (p.is_absolute(), workspace) {
        (false, Some(w)) => w.join(p).display().to_string(),
        _ => d.to_string(),
    }
}

/// Split a shell command into its sub-commands at `&&`, `||`, `;`, `|` and newlines,
/// outside quotes. Command substitution (`$(…)`, backticks) or unbalanced quotes make it
/// unparseable (`None` → the evaluator asks, 06 §13.3). The full scanner lives with the
/// shell tool; this is the engine-side fallback.
pub fn split_shell(cmd: &str) -> Option<Vec<String>> {
    let mut parts = Vec::new();
    let mut cur = String::new();
    let mut quote: Option<char> = None;
    let mut chars = cmd.chars().peekable();
    while let Some(c) = chars.next() {
        match quote {
            Some('\'') => {
                cur.push(c);
                if c == '\'' {
                    quote = None;
                }
            }
            Some(q) => {
                if c == '`' || (c == '$' && chars.peek() == Some(&'(')) {
                    return None;
                }
                cur.push(c);
                if c == '\\' {
                    if let Some(n) = chars.next() {
                        cur.push(n);
                    }
                } else if c == q {
                    quote = None;
                }
            }
            None => match c {
                '\'' | '"' => {
                    quote = Some(c);
                    cur.push(c);
                }
                '`' => return None,
                '$' if chars.peek() == Some(&'(') => return None,
                '\\' => {
                    cur.push(c);
                    if let Some(n) = chars.next() {
                        cur.push(n);
                    }
                }
                ';' | '\n' => parts.push(std::mem::take(&mut cur)),
                '&' if chars.peek() == Some(&'&') => {
                    chars.next();
                    parts.push(std::mem::take(&mut cur));
                }
                '|' => {
                    if chars.peek() == Some(&'|') {
                        chars.next();
                    }
                    parts.push(std::mem::take(&mut cur));
                }
                _ => cur.push(c),
            },
        }
    }
    if quote.is_some() {
        return None;
    }
    parts.push(cur);
    let parts: Vec<String> = parts
        .into_iter()
        .map(|p| p.trim().to_string())
        .filter(|p| !p.is_empty())
        .collect();
    (!parts.is_empty()).then_some(parts)
}

/// Host of a URL or a bare domain.
fn host_of(s: &str) -> String {
    let rest = s.split_once("://").map_or(s, |(_, r)| r);
    let hostport = rest.split(['/', '?', '#']).next().unwrap_or(rest);
    let hostport = hostport.rsplit_once('@').map_or(hostport, |(_, h)| h);
    let host = if hostport.starts_with('[') {
        hostport
            .split(']')
            .next()
            .map(|h| h.trim_start_matches('['))
            .unwrap_or(hostport)
    } else {
        hostport.split(':').next().unwrap_or(hostport)
    };
    host.to_ascii_lowercase()
}

/// The tool call as a `genie-permissions` [`Action`]. Tools report what they touch through
/// `resource` and `preview` (06 §4); names follow the v1 tool set.
pub fn action_for(req: &AuthzRequest, workspace: Option<&Path>) -> Action {
    let name = req.tool.as_str();
    let resource = req.resource.clone();
    let abs = |p: String| -> String {
        let path = Path::new(&p);
        match (path.is_absolute(), workspace) {
            (false, Some(w)) => w.join(path).display().to_string(),
            _ => p,
        }
    };
    match name {
        "shell" | "bash" | "powershell" => {
            let (command, cwd) = match &req.preview {
                Some(Preview::Command { command, cwd }) => (command.clone(), cwd.clone()),
                _ => (resource.unwrap_or_default(), None),
            };
            let dialect = if name == "powershell" {
                ShellDialect::PowerShell
            } else {
                ShellDialect::Posix
            };
            Action::Shell {
                parts: split_shell(&command),
                dialect,
                command,
                cwd: cwd.map(abs),
            }
        }
        "read" | "grep" | "glob" | "list" | "ls" => Action::Read {
            path: abs(resource.unwrap_or_default()),
        },
        "write" | "edit" | "multi_edit" | "patch" | "apply_patch" => Action::Edit {
            path: abs(resource
                .or_else(|| match &req.preview {
                    Some(Preview::Diff { path, .. }) => Some(path.clone()),
                    _ => None,
                })
                .unwrap_or_default()),
        },
        "web_fetch" | "fetch" => {
            let src = match &req.preview {
                Some(Preview::Url { url }) => url.clone(),
                _ => resource.unwrap_or_default(),
            };
            Action::WebFetch {
                domain: host_of(&src),
                pasted: false,
                seen: false,
            }
        }
        "web_search" => Action::WebSearch,
        // The `skill.use` check (06 §9): `Skill(<name>)` rules.
        "skill" | "skill_read" => Action::Other {
            tool: "Skill".into(),
            specifier: resource,
            read_only: true,
        },
        n if n.starts_with("mcp__") => {
            let (server, tool) = match &req.preview {
                Some(Preview::McpArgs { server, tool, .. }) => (server.clone(), tool.clone()),
                _ => {
                    let rest = &n["mcp__".len()..];
                    match rest.split_once("__") {
                        Some((s, t)) => (s.to_string(), t.to_string()),
                        None => (rest.to_string(), String::new()),
                    }
                }
            };
            Action::Mcp {
                server,
                tool,
                read_only_hint: false,
            }
        }
        n if n.starts_with("browser") => {
            let act = n.trim_start_matches("browser").trim_start_matches('_');
            Action::Browser {
                site: host_of(&resource.unwrap_or_default()),
                action: match act {
                    "navigate" | "open" => BrowserAction::Navigate,
                    "read" | "snapshot" | "screenshot" | "" => BrowserAction::Read,
                    other => BrowserAction::Act(other.to_string()),
                },
            }
        }
        other => Action::Other {
            tool: other.to_string(),
            specifier: resource,
            read_only: req.kind == ToolKind::ReadOnly,
        },
    }
}

fn origin_str(by: &AllowedBy) -> String {
    match by {
        AllowedBy::Rule { rule, origin } => format!("rule:{rule} ({})", origin.as_str()),
        AllowedBy::ReadOnly => "read_only".into(),
        AllowedBy::AcceptEdits => "accept_edits".into(),
        AllowedBy::Bypass => "bypass".into(),
        AllowedBy::Pasted => "pasted".into(),
    }
}

fn content_of(req: &AuthzRequest) -> Option<String> {
    match &req.preview {
        Some(Preview::Command { command, .. }) => Some(command.clone()),
        Some(Preview::Url { url }) => Some(url.clone()),
        Some(Preview::McpArgs { args, .. }) => Some(args.to_string()),
        _ => None,
    }
}

impl ToolAuthorizer for PermissionAuthorizer {
    fn authorize(&self, req: &AuthzRequest) -> AuthzDecision {
        let Some(s) = self.settings.settings(&req.session_id) else {
            return AuthzDecision::Deny {
                rule: "unknown session".into(),
                source: BlockSource::Rule,
                message: "Not allowed: unknown session".into(),
            };
        };
        let ev = self.evaluator(&req.session_id, &s, req.mode);
        let r = Self::request(req, &s);
        let verdict = ev.evaluate(&r);
        let mut attrs = Attrs::new().with("tool", req.tool.clone());
        if let Some(res) = &req.resource {
            attrs = attrs.with("resource", res.clone());
        }
        if let Some(c) = content_of(req) {
            attrs = attrs.content("input", c);
        }
        match verdict {
            Verdict::Allow { by } => {
                if matches!(by, AllowedBy::Rule { .. } | AllowedBy::Bypass) {
                    self.record(
                        &req.session_id,
                        AuditEvent::PermissionDecided,
                        attrs.with("decision", "allow").with("by", origin_str(&by)),
                    );
                }
                AuthzDecision::Allow
            }
            Verdict::Deny { ref rule, source } => {
                let policy = self.hub.policy();
                let message = verdict
                    .blocked_message(policy.org_name())
                    .unwrap_or_else(|| format!("Blocked by rule {rule}"));
                let attrs = attrs
                    .with("rule", rule.clone())
                    .with("source", source.as_str());
                if source == Origin::Policy {
                    self.record(&req.session_id, AuditEvent::PolicyBlocked, attrs);
                } else {
                    self.record(
                        &req.session_id,
                        AuditEvent::PermissionDecided,
                        attrs.with("decision", "deny"),
                    );
                }
                AuthzDecision::Deny {
                    rule: rule.clone(),
                    source: if source == Origin::Policy {
                        BlockSource::Policy
                    } else {
                        BlockSource::Rule
                    },
                    message,
                }
            }
            Verdict::Ask {
                reason,
                save_suggestions,
            } => {
                let mut ask = Ask::new("", req.session_id.clone(), AskKind::Permission, "");
                ask.tool = Some(req.tool.clone());
                ask.resource.clone_from(&req.resource);
                ask.preview.clone_from(&req.preview);
                ask.save_suggestions =
                    if save_suggestions.is_empty() && matches!(reason, AskReason::NoRule) {
                        req.save_suggestions.clone()
                    } else {
                        save_suggestions
                    };
                if let AskReason::Tainted { source } = &reason
                    && ask.preview.is_none()
                {
                    ask.preview = Some(Preview::Text {
                        text: format!("This step follows content from {source}"),
                    });
                }
                AuthzDecision::Ask(Box::new(ask))
            }
        }
    }

    fn asked(&self, req: &AuthzRequest, ask_id: &str, ask: &Ask) {
        let Some(s) = self.settings.settings(&req.session_id) else {
            return;
        };
        let r = Self::request(req, &s);
        self.sessions
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .entry(req.session_id.clone())
            .or_default()
            .pending
            .insert(ask_id.to_string(), (r, ask.clone()));
        let mut attrs = Attrs::new()
            .with("tool", req.tool.clone())
            .with("ask_id", ask_id.to_string());
        if let Some(res) = &req.resource {
            attrs = attrs.with("resource", res.clone());
        }
        if let Some(c) = content_of(req) {
            attrs = attrs.content("input", c);
        }
        self.record(&req.session_id, AuditEvent::PermissionAsked, attrs);
    }

    fn answered(&self, req: &AuthzRequest, ask_id: &str, answer: &AskAnswer) -> Vec<String> {
        let pending = self
            .sessions
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get_mut(&req.session_id)
            .and_then(|s| s.pending.remove(ask_id));
        let decision = serde_json::to_value(answer.decision)
            .ok()
            .and_then(|v| v.as_str().map(str::to_string))
            .unwrap_or_default();
        let mut attrs = Attrs::new()
            .with("tool", req.tool.clone())
            .with("ask_id", ask_id.to_string())
            .with("decision", decision)
            .with("by", "user");
        if let Some(m) = &answer.message {
            attrs = attrs.content("message", m.clone());
        }
        if answer.decision != Decision::Always {
            self.record(&req.session_id, AuditEvent::PermissionDecided, attrs);
            return Vec::new();
        }
        let (Some((_, ask)), Some(s)) = (pending, self.settings.settings(&req.session_id)) else {
            self.record(&req.session_id, AuditEvent::PermissionDecided, attrs);
            return Vec::new();
        };
        let Some(sugg) = saved_rule(&ask, answer) else {
            self.record(&req.session_id, AuditEvent::PermissionDecided, attrs);
            return Vec::new();
        };
        let mut ev = self.evaluator(&req.session_id, &s, req.mode);
        let target = match ev.remember(&sugg) {
            Ok(t) => t,
            Err(e) => {
                tracing::warn!(error = %e, rule = %sugg.rule, "cannot save an invalid rule");
                self.record(&req.session_id, AuditEvent::PermissionDecided, attrs);
                return Vec::new();
            }
        };
        let Ok(rule) = Rule::parse(&sugg.rule) else {
            return Vec::new();
        };
        let origin = match target {
            SaveTarget::Session => Origin::Session,
            SaveTarget::Settings(scope) => {
                let root = Self::root(&s);
                match self
                    .hub
                    .add_permission_rule(root.as_deref(), scope, "allow", &sugg.rule)
                {
                    Ok(_) => match scope {
                        ConfigScope::Local => Origin::Local,
                        _ => Origin::User,
                    },
                    Err(e) => {
                        tracing::warn!(error = %e, rule = %sugg.rule, "saving the rule failed; kept for this session");
                        Origin::Session
                    }
                }
            }
        };
        self.record(
            &req.session_id,
            AuditEvent::PermissionDecided,
            attrs.with("saved_rule", sugg.rule.clone()),
        );
        let mut g = self.sessions.lock().unwrap_or_else(|e| e.into_inner());
        let st = g.entry(req.session_id.clone()).or_default();
        st.remembered.push((origin, rule));
        let others: Vec<(String, Request)> = st
            .pending
            .iter()
            .map(|(k, (r, _))| (k.clone(), r.clone()))
            .collect();
        drop(g);
        // Re-evaluate the other open asks with the new rule (02 §7 step 6).
        let ev = self.evaluator(&req.session_id, &s, req.mode);
        ev.released(others.iter().map(|(k, r)| (k, r)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shell_split() {
        assert_eq!(
            split_shell("git status && ls -la | wc -l; echo 'a;b'"),
            Some(vec![
                "git status".into(),
                "ls -la".into(),
                "wc -l".into(),
                "echo 'a;b'".into()
            ])
        );
        assert_eq!(split_shell("echo $(rm -rf /)"), None);
        assert_eq!(split_shell("echo \"unclosed"), None);
        assert_eq!(split_shell("echo '$(x)'"), Some(vec!["echo '$(x)'".into()]));
    }

    #[test]
    fn hosts() {
        assert_eq!(host_of("https://user@Docs.Rs:443/a?b"), "docs.rs");
        assert_eq!(host_of("example.com"), "example.com");
    }
}
