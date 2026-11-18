//! Evaluation (06 §5.2–5.5): policy deny → deny → ask → allow → mode default, with modes,
//! the policy mode floor and taint.
//!
//! The evaluator is pure: it never touches the filesystem or the clock. The engine builds
//! one per session from the folded config layers ([`RuleSet`]) and the session state
//! ([`Context`]), then calls [`Evaluator::evaluate`] for each tool action.

use std::sync::Arc;

use genie_protocol::ask::SaveSuggestion;
use genie_protocol::common::{ConfigScope, Mode, RuleScope, SessionKind};

use crate::path::{NormPath, glob_match, is_absolute, normalize, wildcard};
use crate::rule::{BrowserSpec, Matcher, Rule, RuleError, domain_matches, normalize_command};
use crate::shell::{
    BuiltinReadOnly, ReadOnlyClassifier, ShellDialect, is_cd, path_operands, suggest_rule,
};

/// Rule class. Evaluation is by class, never by file order (06 §5.2).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RuleClass {
    Allow,
    Ask,
    Deny,
}

/// Where a rule (or a decision) came from. Ordered from strongest to weakest layer.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Origin {
    /// Managed policy (08 §5).
    Policy,
    /// `--settings` / CLI flags (D23 layer 2).
    Cli,
    /// `<project>/.genie/settings.local.json` (incl. saved "always").
    Local,
    /// `<project>/.genie/settings.json` (only after trust).
    Project,
    /// `~/.genie/settings.json`.
    User,
    /// "Always" saved for this session only.
    Session,
    /// Engine-enforced limits (plan mode). Not a rule layer.
    Engine,
}

impl Origin {
    pub fn as_str(self) -> &'static str {
        match self {
            Origin::Policy => "policy",
            Origin::Cli => "cli",
            Origin::Local => "local",
            Origin::Project => "project",
            Origin::User => "user",
            Origin::Session => "session",
            Origin::Engine => "engine",
        }
    }
}

#[derive(Debug, Clone)]
struct Entry {
    class: RuleClass,
    origin: Origin,
    rule: Rule,
}

/// Every rule from every layer, tagged with its class and origin.
#[derive(Debug, Clone, Default)]
pub struct RuleSet {
    entries: Vec<Entry>,
}

impl RuleSet {
    pub fn new() -> RuleSet {
        RuleSet::default()
    }

    pub fn add(&mut self, class: RuleClass, origin: Origin, rule: Rule) {
        let dup = self
            .entries
            .iter()
            .any(|e| e.class == class && e.origin == origin && e.rule == rule);
        if !dup {
            self.entries.push(Entry {
                class,
                origin,
                rule,
            });
        }
    }

    /// Parse and add a list from one settings layer. Bad rules are skipped and returned
    /// so the caller can raise per-key diagnostics (D23).
    pub fn add_list<S: AsRef<str>>(
        &mut self,
        class: RuleClass,
        origin: Origin,
        texts: &[S],
    ) -> Vec<RuleError> {
        let mut errors = Vec::new();
        for t in texts {
            match Rule::parse(t.as_ref()) {
                Ok(r) => self.add(class, origin, r),
                Err(e) => errors.push(e),
            }
        }
        errors
    }

    /// Revoke a rule (Settings → Permissions).
    pub fn remove(&mut self, class: RuleClass, origin: Origin, text: &str) -> bool {
        let before = self.entries.len();
        self.entries.retain(|e| {
            !(e.class == class && e.origin == origin && e.rule.as_str() == text.trim())
        });
        before != self.entries.len()
    }

    pub fn iter(&self) -> impl Iterator<Item = (RuleClass, Origin, &Rule)> {
        self.entries.iter().map(|e| (e.class, e.origin, &e.rule))
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

/// Session facts the evaluator needs.
#[derive(Debug, Clone)]
pub struct Context {
    pub session_kind: SessionKind,
    pub mode: Mode,
    /// Workspace roots (project root, worktree). Empty for chat.
    pub workspace_roots: Vec<String>,
    /// `permissions.additionalDirectories`.
    pub additional_dirs: Vec<String>,
    /// Session cwd; relative rule paths resolve against it (default: first root).
    pub cwd: Option<String>,
    pub home: Option<String>,
    /// Windows: case-insensitive paths (06 §5.1).
    pub case_insensitive_paths: bool,
}

impl Context {
    pub fn chat() -> Context {
        Context {
            session_kind: SessionKind::Chat,
            mode: Mode::Default,
            workspace_roots: Vec::new(),
            additional_dirs: Vec::new(),
            cwd: None,
            home: None,
            case_insensitive_paths: cfg!(windows),
        }
    }

    pub fn code(root: impl Into<String>, mode: Mode) -> Context {
        Context {
            session_kind: SessionKind::Code,
            mode,
            workspace_roots: vec![root.into()],
            ..Context::chat()
        }
    }
}

/// Policy limits on modes (08 §5.2: `disableBypassMode`, `defaultMode` floor).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ModePolicy {
    pub disable_bypass: bool,
    /// The most permissive mode a session may run in.
    pub floor: Option<Mode>,
}

/// What a browser tool is about to do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BrowserAction {
    Navigate,
    Read,
    /// `click`, `type`, `submit`, …; matched by `Browser(action:<name>)`.
    Act(String),
}

impl BrowserAction {
    fn name(&self) -> &str {
        match self {
            BrowserAction::Navigate => "navigate",
            BrowserAction::Read => "read",
            BrowserAction::Act(a) => a,
        }
    }
}

/// The resource a tool wants to touch, already resolved by the tool.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    /// `parts`: the scanner's sub-commands; `None` = unparseable (→ ask).
    Shell {
        dialect: ShellDialect,
        command: String,
        parts: Option<Vec<String>>,
        /// Directory the command runs in.
        cwd: Option<String>,
    },
    /// `read`, `grep`, `glob`, `list` (absolute path).
    Read {
        path: String,
    },
    /// `write`, `edit`, `multi_edit`, `patch` (absolute path).
    Edit {
        path: String,
    },
    /// `pasted`: the user pasted this URL in the chat. `seen`: already fetched this turn.
    WebFetch {
        domain: String,
        pasted: bool,
        seen: bool,
    },
    WebSearch,
    /// `read_only_hint` is informational: it never skips the ask (t13), it only narrows
    /// the taint downgrade (SA-29).
    Mcp {
        server: String,
        tool: String,
        read_only_hint: bool,
    },
    /// `site`: eTLD+1 of the page.
    Browser {
        site: String,
        action: BrowserAction,
    },
    /// Anything else; `read_only` from the tool's `ToolKind` (classification hook).
    Other {
        tool: String,
        specifier: Option<String>,
        read_only: bool,
    },
}

/// Untrusted content seen earlier in the turn (SA-29).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Taint {
    /// Shown on the ask card: "This step follows content from <source>".
    pub source: String,
    /// Sites the agent read (browser `act` on another origin is downgraded).
    pub read_origins: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Request {
    pub action: Action,
    pub taint: Option<Taint>,
}

impl Request {
    pub fn new(action: Action) -> Request {
        Request {
            action,
            taint: None,
        }
    }

    pub fn tainted(mut self, taint: Taint) -> Request {
        self.taint = Some(taint);
        self
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AllowedBy {
    Rule {
        rule: String,
        origin: Origin,
    },
    /// Mode default for read-only actions.
    ReadOnly,
    AcceptEdits,
    Bypass,
    /// URL pasted by the user (D35).
    Pasted,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AskReason {
    /// An ask rule matched.
    Rule { rule: String, origin: Origin },
    /// Nothing allowed it; the mode default asks.
    NoRule,
    /// The scanner could not split the command (06 §13.3).
    Unparseable,
    /// The command would run outside the workspace + additional directories.
    CwdOutside,
    /// Auto-allow downgraded after untrusted content (SA-29).
    Tainted { source: String },
}

#[derive(Debug, Clone, PartialEq)]
pub enum Verdict {
    Allow {
        by: AllowedBy,
    },
    Deny {
        rule: String,
        source: Origin,
    },
    Ask {
        reason: AskReason,
        save_suggestions: Vec<SaveSuggestion>,
    },
}

impl Verdict {
    pub fn is_allow(&self) -> bool {
        matches!(self, Verdict::Allow { .. })
    }

    /// The user-facing text for a deny (08 §5.3): "Blocked by <org> policy" with the rule.
    pub fn blocked_message(&self, org: Option<&str>) -> Option<String> {
        match self {
            Verdict::Deny {
                rule,
                source: Origin::Policy,
            } => Some(format!(
                "Blocked by {} policy ({rule})",
                org.unwrap_or("your organization's")
            )),
            Verdict::Deny {
                rule,
                source: Origin::Engine,
            } => Some(format!("Not allowed: {rule}")),
            Verdict::Deny { rule, source } => Some(format!(
                "Blocked by rule {rule} ({} settings)",
                source.as_str()
            )),
            _ => None,
        }
    }
}

/// Where a saved "always" must be persisted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SaveTarget {
    /// In memory for this session only.
    Session,
    /// A settings file (`permissions.allow`) in this scope (06 §5.4).
    Settings(ConfigScope),
}

/// The default scope for "always" (06 §5.4): Code → local project settings, Chat → user.
pub fn default_save_scope(kind: SessionKind) -> RuleScope {
    match kind {
        SessionKind::Code => RuleScope::Local,
        _ => RuleScope::User,
    }
}

/// Mode permissiveness: plan < default < accept-edits < bypass.
fn rank(m: Mode) -> u8 {
    match m {
        Mode::Plan => 0,
        Mode::AcceptEdits => 2,
        Mode::Bypass => 3,
        Mode::Default | Mode::Unknown => 1,
    }
}

/// The mode a session actually runs in after policy (08 §5.2) and session kind (06 §5.3).
pub fn effective_mode(kind: SessionKind, requested: Mode, policy: ModePolicy) -> Mode {
    if kind != SessionKind::Code {
        return Mode::Default;
    }
    let mut m = match requested {
        Mode::Unknown => Mode::Default,
        m => m,
    };
    if m == Mode::Bypass && policy.disable_bypass {
        m = Mode::Default;
    }
    if let Some(floor) = policy.floor
        && rank(m) > rank(floor)
    {
        m = if floor == Mode::Bypass && policy.disable_bypass {
            Mode::Default
        } else {
            floor
        };
    }
    m
}

enum Target<'a> {
    /// A sub-command: dialect, normalised text (for rules), raw text (for operands) and the
    /// directory it runs in (after any earlier `cd` in the same command).
    Cmd(ShellDialect, String, &'a str, Option<NormPath>),
    Path {
        edit: bool,
        path: NormPath,
    },
    Fetch(&'a str),
    Search,
    Mcp(&'a str, &'a str),
    Browser(&'a str, &'a BrowserAction),
    Other(&'a str, Option<&'a str>, bool),
}

/// The permission evaluator for one session.
#[derive(Clone)]
pub struct Evaluator {
    rules: RuleSet,
    ctx: Context,
    policy: ModePolicy,
    classifier: Arc<dyn ReadOnlyClassifier>,
}

impl std::fmt::Debug for Evaluator {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Evaluator")
            .field("rules", &self.rules)
            .field("ctx", &self.ctx)
            .field("policy", &self.policy)
            .finish_non_exhaustive()
    }
}

impl Evaluator {
    pub fn new(rules: RuleSet, ctx: Context) -> Evaluator {
        Evaluator {
            rules,
            ctx,
            policy: ModePolicy::default(),
            classifier: Arc::new(BuiltinReadOnly),
        }
    }

    pub fn with_policy(mut self, policy: ModePolicy) -> Evaluator {
        self.policy = policy;
        self
    }

    /// Replace the read-only shell classifier (hook for settings-driven lists).
    pub fn with_classifier(mut self, c: Arc<dyn ReadOnlyClassifier>) -> Evaluator {
        self.classifier = c;
        self
    }

    pub fn rules(&self) -> &RuleSet {
        &self.rules
    }

    pub fn rules_mut(&mut self) -> &mut RuleSet {
        &mut self.rules
    }

    pub fn context(&self) -> &Context {
        &self.ctx
    }

    /// Mode changes are a session property set over the protocol (06 §5.3).
    pub fn set_mode(&mut self, mode: Mode) {
        self.ctx.mode = mode;
    }

    pub fn mode(&self) -> Mode {
        effective_mode(self.ctx.session_kind, self.ctx.mode, self.policy)
    }

    /// Record an "always" answer: the rule takes effect immediately in memory, and the
    /// result says where the caller must persist it (06 §5.4). A local scope in a session
    /// without a project falls back to the user scope.
    pub fn remember(&mut self, s: &SaveSuggestion) -> Result<SaveTarget, RuleError> {
        let rule = Rule::parse(&s.rule)?;
        let (origin, target) = match s.scope {
            RuleScope::Session => (Origin::Session, SaveTarget::Session),
            RuleScope::Local if !self.ctx.workspace_roots.is_empty() => {
                (Origin::Local, SaveTarget::Settings(ConfigScope::Local))
            }
            _ => (Origin::User, SaveTarget::Settings(ConfigScope::User)),
        };
        self.rules.add(RuleClass::Allow, origin, rule);
        Ok(target)
    }

    /// After an "always", re-evaluate the other pending asks (t13) and return those that
    /// are now allowed and can be released automatically.
    pub fn released<'a, K: Clone + 'a>(
        &self,
        pending: impl IntoIterator<Item = (&'a K, &'a Request)>,
    ) -> Vec<K> {
        pending
            .into_iter()
            .filter(|(_, r)| self.evaluate(r).is_allow())
            .map(|(k, _)| k.clone())
            .collect()
    }

    pub fn evaluate(&self, req: &Request) -> Verdict {
        let mode = self.mode();
        let targets = self.targets(&req.action);

        // 1. Managed policy deny. 2. Deny rules from every layer.
        for policy_pass in [true, false] {
            for t in &targets {
                if let Some(e) =
                    self.find(RuleClass::Deny, t, |o| (o == Origin::Policy) == policy_pass)
                {
                    return Verdict::Deny {
                        rule: e.rule.to_string(),
                        source: e.origin,
                    };
                }
            }
        }

        // Plan mode is engine-enforced read-only (06 §5.3).
        if mode == Mode::Plan
            && let Some(why) = self.plan_block(&req.action, &targets)
        {
            return Verdict::Deny {
                rule: why.to_string(),
                source: Origin::Engine,
            };
        }

        // 3. Ask rules. Policy asks hold even in bypass (policy still applies there).
        for t in &targets {
            if let Some(e) = self.find(RuleClass::Ask, t, |o| o == Origin::Policy) {
                return ask_rule(e);
            }
        }
        if mode == Mode::Bypass {
            return Verdict::Allow {
                by: AllowedBy::Bypass,
            };
        }
        for t in &targets {
            if let Some(e) = self.find(RuleClass::Ask, t, |o| o != Origin::Policy) {
                return ask_rule(e);
            }
        }

        if let Action::Shell { parts, cwd, .. } = &req.action {
            if parts.is_none() {
                return Verdict::Ask {
                    reason: AskReason::Unparseable,
                    save_suggestions: Vec::new(),
                };
            }
            if let Some(cwd) = cwd
                && !self.in_workspace(&self.norm(cwd))
            {
                return Verdict::Ask {
                    reason: AskReason::CwdOutside,
                    save_suggestions: Vec::new(),
                };
            }
        }

        // 4. Allow rules, 5. mode default — every part must pass.
        let mut allowed = Vec::new();
        let mut missing = Vec::new();
        for t in &targets {
            match self.allowed_by(t, &req.action, mode) {
                Some(by) => allowed.push((t, by)),
                None => missing.push(t),
            }
        }
        if !missing.is_empty() {
            let scope = default_save_scope(self.ctx.session_kind);
            let mut save_suggestions: Vec<SaveSuggestion> = Vec::new();
            for t in missing {
                for rule in self.suggest(t) {
                    if !save_suggestions.iter().any(|s| s.rule == rule) {
                        save_suggestions.push(SaveSuggestion { rule, scope });
                    }
                }
            }
            return Verdict::Ask {
                reason: AskReason::NoRule,
                save_suggestions,
            };
        }

        // Taint (SA-29): auto-allows by rule or accept-edits become asks.
        if let Some(taint) = &req.taint {
            for (t, by) in &allowed {
                if matches!(by, AllowedBy::Rule { .. } | AllowedBy::AcceptEdits)
                    && self.taint_applies(t, &req.action, taint)
                {
                    return Verdict::Ask {
                        reason: AskReason::Tainted {
                            source: taint.source.clone(),
                        },
                        save_suggestions: Vec::new(),
                    };
                }
            }
        }

        let by = allowed
            .into_iter()
            .map(|(_, b)| b)
            .find(|b| *b != AllowedBy::ReadOnly)
            .unwrap_or(AllowedBy::ReadOnly);
        Verdict::Allow { by }
    }

    fn norm(&self, p: &str) -> NormPath {
        NormPath::new(p, self.ctx.case_insensitive_paths)
    }

    fn targets<'a>(&self, action: &'a Action) -> Vec<Target<'a>> {
        match action {
            Action::Shell {
                dialect,
                command,
                parts,
                cwd,
            } => {
                let mut cur = cwd
                    .as_deref()
                    .or(self.ctx.workspace_roots.first().map(String::as_str))
                    .map(|c| self.norm(c));
                match parts {
                    Some(p) if !p.is_empty() => p
                        .iter()
                        .map(|c| {
                            let t = Target::Cmd(*dialect, normalize_command(c), c, cur.clone());
                            // Later parts run where a `cd` went (`cd x && cat y`).
                            if is_cd(*dialect, c)
                                && let Some(dest) = path_operands(*dialect, c).first()
                            {
                                cur = Some(self.resolve_operand(dest, cur.as_ref()));
                            }
                            t
                        })
                        .collect(),
                    // Unparseable: deny/ask rules still see the raw text.
                    _ => vec![Target::Cmd(
                        *dialect,
                        normalize_command(command),
                        command,
                        cur,
                    )],
                }
            }
            Action::Read { path } => vec![Target::Path {
                edit: false,
                path: self.norm(path),
            }],
            Action::Edit { path } => vec![Target::Path {
                edit: true,
                path: self.norm(path),
            }],
            Action::WebFetch { domain, .. } => vec![Target::Fetch(domain)],
            Action::WebSearch => vec![Target::Search],
            Action::Mcp { server, tool, .. } => vec![Target::Mcp(server, tool)],
            Action::Browser { site, action } => vec![Target::Browser(site, action)],
            Action::Other {
                tool,
                specifier,
                read_only,
            } => vec![Target::Other(tool, specifier.as_deref(), *read_only)],
        }
    }

    fn find(
        &self,
        class: RuleClass,
        t: &Target<'_>,
        origin: impl Fn(Origin) -> bool,
    ) -> Option<&Entry> {
        self.rules
            .entries
            .iter()
            .find(|e| e.class == class && origin(e.origin) && self.matches(&e.rule, t))
    }

    fn matches(&self, rule: &Rule, t: &Target<'_>) -> bool {
        match (&rule.matcher, t) {
            (Matcher::Shell { posix, pattern }, Target::Cmd(d, cmd, ..)) => {
                *posix == (*d == ShellDialect::Posix)
                    && pattern
                        .as_ref()
                        .is_none_or(|p| p.matches(cmd, *d == ShellDialect::PowerShell))
            }
            (Matcher::Read(spec), Target::Path { edit: false, path })
            | (Matcher::Edit(spec), Target::Path { edit: true, path }) => spec
                .as_ref()
                .is_none_or(|s| glob_match(&self.resolve_pattern(s), path.as_str())),
            (Matcher::WebFetch(d), Target::Fetch(host)) => {
                d.as_ref().is_none_or(|d| domain_matches(d, host))
            }
            (Matcher::WebSearch, Target::Search) => true,
            (Matcher::Browser(spec), Target::Browser(site, act)) => match spec {
                None => true,
                Some(BrowserSpec::Site(p)) => domain_matches(p, site),
                Some(BrowserSpec::Action(a)) => a.eq_ignore_ascii_case(act.name()),
            },
            (Matcher::Mcp { server, tool }, Target::Mcp(s, t)) => {
                server == s && tool.as_ref().is_none_or(|x| x == t)
            }
            (Matcher::Other { tool, spec }, Target::Other(t, s, _)) => {
                tool == t
                    && spec
                        .as_ref()
                        .is_none_or(|p| s.is_some_and(|s| wildcard(p, s)))
            }
            _ => false,
        }
    }

    /// Resolve a `Read`/`Edit` specifier to an absolute normalised glob (06 §5.1):
    /// `//abs`, `~/home`, `/project-root-relative`, `./cwd-relative`, bare names match at
    /// any depth below the cwd (gitignore style).
    fn resolve_pattern(&self, spec: &str) -> String {
        let ci = self.ctx.case_insensitive_paths;
        let root = self.ctx.workspace_roots.first();
        let cwd = self.ctx.cwd.as_ref().or(root);
        let join = |base: Option<&String>, rel: &str| match base {
            Some(b) => NormPath::new(b, ci).join(rel, ci).as_str().to_string(),
            None => normalize(&format!("/{rel}"), ci),
        };
        let spec = spec.replace('\\', "/");
        if let Some(rest) = spec.strip_prefix("//") {
            if is_absolute(rest) {
                normalize(rest, ci)
            } else {
                normalize(&format!("/{rest}"), ci)
            }
        } else if spec == "~" || spec.starts_with("~/") {
            join(self.ctx.home.as_ref(), spec.trim_start_matches('~'))
        } else if let Some(rest) = spec.strip_prefix('/') {
            join(root, rest)
        } else if is_absolute(&spec) {
            normalize(&spec, ci)
        } else if let Some(rest) = spec.strip_prefix("./") {
            join(cwd, rest)
        } else if !spec.trim_end_matches('/').contains('/') {
            join(cwd, &format!("**/{spec}"))
        } else {
            join(cwd, &spec)
        }
    }

    /// A command operand as an absolute path: `~` is the home directory, relative paths
    /// resolve against the directory the command runs in. Anything that can't be resolved
    /// (`~user`, no home, no cwd) becomes `/`, which is outside every workspace.
    fn resolve_operand(&self, op: &str, cwd: Option<&NormPath>) -> NormPath {
        let ci = self.ctx.case_insensitive_paths;
        let outside = || NormPath::new("/", ci);
        if let Some(rest) = op.strip_prefix('~') {
            if !(rest.is_empty() || rest.starts_with('/') || rest.starts_with('\\')) {
                return outside();
            }
            return match &self.ctx.home {
                Some(h) => NormPath::new(h, ci).join(rest.trim_start_matches(['/', '\\']), ci),
                None => outside(),
            };
        }
        if is_absolute(op) {
            return NormPath::new(op, ci);
        }
        match cwd {
            Some(c) => c.join(op, ci),
            None => outside(),
        }
    }

    /// Path operands of a read-only command that keep it from running without a prompt:
    /// outside the workspace + additional directories (and not covered by a `Read` allow
    /// rule), or matched by a `Read` deny rule. `true` = blocked by a deny rule.
    fn operand_problems(
        &self,
        d: ShellDialect,
        raw: &str,
        cwd: Option<&NormPath>,
    ) -> Vec<(NormPath, bool)> {
        path_operands(d, raw)
            .iter()
            .filter_map(|op| {
                let path = self.resolve_operand(op, cwd);
                let t = Target::Path {
                    edit: false,
                    path: path.clone(),
                };
                if self.find(RuleClass::Deny, &t, |_| true).is_some() {
                    return Some((path, true));
                }
                let ok = self.in_workspace(&path)
                    || self
                        .find(RuleClass::Allow, &t, |o| o != Origin::Policy)
                        .is_some();
                (!ok).then_some((path, false))
            })
            .collect()
    }

    fn in_workspace(&self, p: &NormPath) -> bool {
        self.ctx
            .workspace_roots
            .iter()
            .chain(&self.ctx.additional_dirs)
            .any(|r| p.starts_with(&self.norm(r)))
    }

    fn plan_block(&self, action: &Action, targets: &[Target<'_>]) -> Option<&'static str> {
        match action {
            Action::Shell { .. } => Some("plan mode: no shell"),
            Action::Edit { .. } => Some("plan mode: no file changes"),
            Action::Mcp { .. } => {
                let ok = targets
                    .iter()
                    .all(|t| self.find(RuleClass::Allow, t, |_| true).is_some());
                (!ok).then_some("plan mode: MCP tools need an allow rule")
            }
            Action::Browser {
                action: BrowserAction::Act(_),
                ..
            } => Some("plan mode: browser navigate/read only"),
            Action::Other {
                read_only: false, ..
            } => Some("plan mode: read-only tools only"),
            _ => None,
        }
    }

    fn allowed_by(&self, t: &Target<'_>, action: &Action, mode: Mode) -> Option<AllowedBy> {
        if let Some(e) = self.find(RuleClass::Allow, t, |o| o != Origin::Policy) {
            return Some(AllowedBy::Rule {
                rule: e.rule.to_string(),
                origin: e.origin,
            });
        }
        match t {
            Target::Cmd(d, c, raw, cwd) => (self.classifier.is_read_only(*d, c)
                && self.operand_problems(*d, raw, cwd.as_ref()).is_empty())
            .then_some(AllowedBy::ReadOnly),
            Target::Path { edit: false, path } => {
                self.in_workspace(path).then_some(AllowedBy::ReadOnly)
            }
            Target::Path { edit: true, path } => (mode == Mode::AcceptEdits
                && self.in_workspace(path))
            .then_some(AllowedBy::AcceptEdits),
            Target::Fetch(_) => match action {
                Action::WebFetch { pasted: true, .. } => Some(AllowedBy::Pasted),
                _ => None,
            },
            Target::Search => Some(AllowedBy::ReadOnly),
            // MCP tools always ask on first use; annotations never skip it (t13).
            Target::Mcp(..) => None,
            // First visit to a site asks (D21).
            Target::Browser(..) => None,
            Target::Other(_, _, ro) => ro.then_some(AllowedBy::ReadOnly),
        }
    }

    fn taint_applies(&self, t: &Target<'_>, action: &Action, taint: &Taint) -> bool {
        match t {
            Target::Cmd(d, c, ..) => !self.classifier.is_read_only(*d, c),
            Target::Path { edit: true, path } => !self.in_workspace(path),
            Target::Path { edit: false, .. } | Target::Search | Target::Other(..) => false,
            Target::Mcp(..) => !matches!(
                action,
                Action::Mcp {
                    read_only_hint: true,
                    ..
                }
            ),
            Target::Fetch(_) => matches!(
                action,
                Action::WebFetch {
                    seen: false,
                    pasted: false,
                    ..
                }
            ),
            Target::Browser(site, BrowserAction::Act(_)) => !taint
                .read_origins
                .iter()
                .any(|o| o.eq_ignore_ascii_case(site)),
            Target::Browser(..) => false,
        }
    }

    /// The rules an "always" saves so that `t` passes next time. A read-only command that
    /// only asked because of a path outside the workspace gets `Read(//dir/**)` for that
    /// directory instead of a blanket `Bash(cd:*)` / `Bash(cat:*)`.
    fn suggest(&self, t: &Target<'_>) -> Vec<String> {
        match t {
            Target::Cmd(d, c, raw, cwd) => {
                if self.classifier.is_read_only(*d, c) {
                    let problems = self.operand_problems(*d, raw, cwd.as_ref());
                    if !problems.is_empty() {
                        let dir_operands = is_cd(*d, raw) || lists_dirs(*d, c);
                        return problems
                            .into_iter()
                            // Never offer a rule for a path a deny rule covers, or for `/`.
                            .filter(|(_, denied)| !denied)
                            .filter_map(|(p, _)| {
                                let dir = if dir_operands { p } else { p.parent() };
                                let abs = dir.as_str().trim_matches('/');
                                (!abs.is_empty()).then(|| format!("Read(//{abs}/**)"))
                            })
                            .collect();
                    }
                }
                suggest_rule(*d, c).into_iter().collect()
            }
            Target::Path { edit, path } => {
                let tool = if *edit { "Edit" } else { "Read" };
                let dir = path.parent();
                let root = self
                    .ctx
                    .workspace_roots
                    .iter()
                    .map(|r| self.norm(r))
                    .find(|r| dir.starts_with(r));
                vec![match root {
                    Some(r) => {
                        let rel = dir.as_str()[r.as_str().trim_end_matches('/').len()..]
                            .trim_start_matches('/');
                        if rel.is_empty() {
                            format!("{tool}(/**)")
                        } else {
                            format!("{tool}(/{rel}/**)")
                        }
                    }
                    None => {
                        let abs = dir.as_str().trim_start_matches('/').trim_end_matches('/');
                        if abs.is_empty() {
                            format!("{tool}(//**)")
                        } else {
                            format!("{tool}(//{abs}/**)")
                        }
                    }
                }]
            }
            Target::Fetch(host) => vec![format!("WebFetch(domain:{})", host.to_ascii_lowercase())],
            Target::Search => vec!["WebSearch".to_string()],
            Target::Mcp(s, t) => vec![format!("mcp__{s}__{t}")],
            Target::Browser(site, _) => {
                vec![format!("Browser(site:{})", site.to_ascii_lowercase())]
            }
            Target::Other(t, _, _) => vec![(*t).to_string()],
        }
    }
}

/// Commands whose operands are usually directories (`ls dir`, `find dir`).
fn lists_dirs(d: ShellDialect, cmd: &str) -> bool {
    let first = cmd.split(' ').next().unwrap_or("").to_ascii_lowercase();
    match d {
        ShellDialect::Posix => matches!(first.as_str(), "ls" | "tree" | "du" | "df" | "find"),
        ShellDialect::PowerShell => {
            matches!(first.as_str(), "get-childitem" | "gci" | "dir" | "ls")
        }
    }
}

fn ask_rule(e: &Entry) -> Verdict {
    Verdict::Ask {
        reason: AskReason::Rule {
            rule: e.rule.to_string(),
            origin: e.origin,
        },
        save_suggestions: Vec::new(),
    }
}
