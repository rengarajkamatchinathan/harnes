//! TUI state and update (07 §1). Pure: messages in, effects out, no terminal or socket.
//!
//! The runner ([`super::run`]) turns terminal input and daemon traffic into [`Msg`]s,
//! executes the returned [`Effect`]s against the protocol, inserts
//! [`App::take_scrollback`] lines above the inline viewport and draws the footer with
//! [`super::view`].

use std::collections::{BTreeSet, VecDeque};
use std::path::{Path, PathBuf};

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use genie_protocol::Id;
use genie_protocol::ask::{Ask, AskAnswer, AskKind, Decision};
use genie_protocol::common::{Mode, RuleScope, SessionKind};
use genie_protocol::event::{
    InboxItemView, SessionEvent, SessionEventEnvelope, ToolOutcome, TurnStatus,
};
use genie_protocol::message::{InputPart, Part, PartKind, Role as MsgRole};
use genie_protocol::session::{Delivery, LiveStatus};
use ratatui::text::{Line, Span};
use serde_json::{Value, json};

use super::commands::{self, Cmd, UserCommand};
use super::markdown;
use super::theme::{Role, Theme};
use crate::errors;
use crate::render;

/// What the runner should do.
#[derive(Debug, Clone, PartialEq)]
pub enum Effect {
    Prompt {
        parts: Vec<InputPart>,
        delivery: Delivery,
    },
    Interrupt,
    Update {
        model: Option<String>,
        effort: Option<String>,
        mode: Option<Mode>,
    },
    /// Answer an ask: reply to the daemon's request when we have one, else
    /// `permission.respond`.
    Answer {
        ask_id: String,
        request: Option<Id>,
        answer: AskAnswer,
    },
    /// A thin wrapper over a protocol method; the result comes back as
    /// [`Msg::CallResult`] with the same label.
    Call {
        label: String,
        method: &'static str,
        params: Value,
    },
    TakeOver,
    NewSession,
    SwitchSession(String),
    /// Ask the runner to list agent definitions from disk (no protocol method yet).
    ListAgents,
    Exit,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Msg {
    Key(KeyEvent),
    Paste(String),
    Event(Box<SessionEventEnvelope>),
    /// `permission.ask` request from the daemon.
    AskRequest {
        id: Id,
        ask: Box<Ask>,
    },
    /// `$/cancelRequest`: the ask was answered elsewhere or the lease moved.
    AskWithdrawn(Id),
    LeaseLost {
        by: Option<String>,
    },
    CallResult {
        label: String,
        result: Result<Value, String>,
    },
    Notice(String),
    Error(String),
    Files(Vec<String>),
    UserCommands(Vec<UserCommand>),
    Width(u16),
    Connection(ConnState),
    /// The session this view follows changed (new / resumed).
    Session {
        id: String,
        kind: SessionKind,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConnState {
    Connected,
    Reconnecting,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Menu {
    Slash {
        items: Vec<MenuItem>,
        selected: usize,
    },
    Files {
        items: Vec<String>,
        selected: usize,
        start: usize,
    },
}

#[derive(Debug, Clone, PartialEq)]
pub struct MenuItem {
    pub name: String,
    pub hint: String,
}

/// An open ask in the footer.
#[derive(Debug, Clone, PartialEq)]
pub struct AskState {
    pub ask: Ask,
    pub request: Option<Id>,
    pub selected: usize,
    /// Typing a deny reason.
    pub reason: Option<String>,
    pub form: Option<FormState>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct FormState {
    pub fields: Vec<(String, bool)>, // (name, sensitive)
    pub index: usize,
    pub values: Vec<String>,
}

/// One streamed part (07 §1.1): settled blocks go to scrollback, the tail stays here.
#[derive(Debug, Clone, PartialEq)]
pub struct StreamPart {
    pub part_id: String,
    pub kind: PartKind,
    pub text: String,
    pub committed: usize,
}

pub const ASK_OPTIONS: [&str; 3] = ["Allow once", "Always", "Deny"];

#[derive(Debug)]
pub struct App {
    pub theme: Theme,
    pub width: u16,
    pub instance_id: String,
    pub session_id: Option<String>,
    pub kind: SessionKind,
    pub model: String,
    pub effort: Option<String>,
    pub mode: Mode,
    /// Started with `--dangerously-skip-permissions` (and not disabled by policy).
    pub bypass_allowed: bool,
    pub status: LiveStatus,
    pub context_pct: Option<u8>,
    pub cost_usd: f64,
    pub subagents: BTreeSet<String>,
    pub input: String,
    pub cursor: usize,
    pub history: Vec<String>,
    history_pos: Option<usize>,
    pub menu: Option<Menu>,
    pub asks: VecDeque<AskState>,
    pub stream: Vec<StreamPart>,
    pub inbox: Vec<InboxItemView>,
    pub attachments: Vec<PathBuf>,
    pub files: Vec<String>,
    pub user_commands: Vec<UserCommand>,
    pub notice: Option<String>,
    pub driver: Option<String>,
    pub read_only: bool,
    pub reconnecting: bool,
    ctrl_c_armed: bool,
    esc_armed: bool,
    scrollback: Vec<Line<'static>>,
    pub exit: bool,
}

impl App {
    pub fn new(theme: Theme, instance_id: impl Into<String>) -> App {
        App {
            theme,
            width: 80,
            instance_id: instance_id.into(),
            session_id: None,
            kind: SessionKind::Code,
            model: String::new(),
            effort: None,
            mode: Mode::Default,
            bypass_allowed: false,
            status: LiveStatus::Idle,
            context_pct: None,
            cost_usd: 0.0,
            subagents: BTreeSet::new(),
            input: String::new(),
            cursor: 0,
            history: vec![],
            history_pos: None,
            menu: None,
            asks: VecDeque::new(),
            stream: vec![],
            inbox: vec![],
            attachments: vec![],
            files: vec![],
            user_commands: vec![],
            notice: None,
            driver: None,
            read_only: false,
            reconnecting: false,
            ctrl_c_armed: false,
            esc_armed: false,
            scrollback: vec![],
            exit: false,
        }
    }

    pub fn running(&self) -> bool {
        matches!(
            self.status,
            LiveStatus::Running
                | LiveStatus::Compacting
                | LiveStatus::Interrupting
                | LiveStatus::AwaitingAsk
                | LiveStatus::Paused
        )
    }

    /// Lines to insert into the terminal scrollback (wrapped to the width).
    pub fn take_scrollback(&mut self) -> Vec<Line<'static>> {
        markdown::wrap(std::mem::take(&mut self.scrollback), self.width)
    }

    pub fn push_line(&mut self, line: Line<'static>) {
        self.scrollback.push(line);
    }

    fn say(&mut self, role: Role, text: impl Into<String>) {
        let st = self.theme.style(role);
        for l in text.into().lines() {
            self.scrollback
                .push(Line::from(Span::styled(l.to_string(), st)));
        }
    }

    // ------------------------------------------------------------------ update

    pub fn update(&mut self, msg: Msg) -> Vec<Effect> {
        match msg {
            Msg::Key(k) => self.on_key(k),
            Msg::Paste(s) => {
                self.on_paste(&s);
                vec![]
            }
            Msg::Event(env) => {
                self.on_event(&env);
                vec![]
            }
            Msg::AskRequest { id, ask } => {
                match self.asks.iter_mut().find(|a| a.ask.id == ask.id) {
                    Some(a) => a.request = Some(id),
                    None => self.push_ask(*ask, Some(id)),
                }
                vec![]
            }
            Msg::AskWithdrawn(id) => {
                self.asks.retain(|a| a.request.as_ref() != Some(&id));
                vec![]
            }
            Msg::LeaseLost { by } => {
                self.read_only = true;
                self.notice = Some(format!(
                    "{} took over this session · Ctrl-T to take it back",
                    by.unwrap_or_else(|| "Another client".into())
                ));
                vec![]
            }
            Msg::CallResult { label, result } => {
                self.on_call_result(&label, result);
                vec![]
            }
            Msg::Notice(s) => {
                self.notice = Some(s);
                vec![]
            }
            Msg::Error(s) => {
                self.say(Role::Error, format!("✗ {s}"));
                vec![]
            }
            Msg::Files(f) => {
                self.files = f;
                vec![]
            }
            Msg::UserCommands(c) => {
                self.user_commands = c;
                vec![]
            }
            Msg::Width(w) => {
                self.width = w;
                vec![]
            }
            Msg::Connection(c) => {
                self.reconnecting = c == ConnState::Reconnecting;
                vec![]
            }
            Msg::Session { id, kind } => {
                self.session_id = Some(id);
                self.kind = kind;
                self.stream.clear();
                self.asks.clear();
                self.inbox.clear();
                self.subagents.clear();
                self.cost_usd = 0.0;
                self.context_pct = None;
                self.read_only = false;
                self.status = LiveStatus::Idle;
                vec![]
            }
        }
    }

    fn on_key(&mut self, k: KeyEvent) -> Vec<Effect> {
        let ctrl = k.modifiers.contains(KeyModifiers::CONTROL);
        let alt = k.modifiers.contains(KeyModifiers::ALT);
        let shift = k.modifiers.contains(KeyModifiers::SHIFT);

        // Ctrl-C twice exits; the first press clears the prompt.
        if ctrl && k.code == KeyCode::Char('c') {
            if self.ctrl_c_armed {
                self.exit = true;
                return vec![Effect::Exit];
            }
            self.ctrl_c_armed = true;
            self.set_input(String::new());
            self.attachments.clear();
            self.menu = None;
            self.notice = Some("Press Ctrl-C again to exit".into());
            return vec![];
        }
        self.ctrl_c_armed = false;
        if self.notice.as_deref() == Some("Press Ctrl-C again to exit") {
            self.notice = None;
        }
        let esc_was_armed = std::mem::take(&mut self.esc_armed);

        if !self.asks.is_empty() {
            return self.on_ask_key(k);
        }

        // Menu navigation.
        if self.menu.is_some() {
            match k.code {
                KeyCode::Up => {
                    self.menu_move(-1);
                    return vec![];
                }
                KeyCode::Down => {
                    self.menu_move(1);
                    return vec![];
                }
                KeyCode::Tab => {
                    self.menu_accept();
                    return vec![];
                }
                KeyCode::Esc => {
                    self.menu = None;
                    return vec![];
                }
                KeyCode::Enter if !alt && !shift => {
                    let slash = matches!(self.menu, Some(Menu::Slash { .. }));
                    self.menu_accept();
                    if !slash {
                        return vec![];
                    }
                    // A slash command without arguments runs right away.
                    let name = self.input.trim().trim_start_matches('/').to_string();
                    let takes_args = commands::REGISTRY
                        .iter()
                        .find(|s| s.slash == Some(name.as_str()))
                        .is_some_and(|s| !s.args.is_empty());
                    if takes_args {
                        return vec![];
                    }
                    return self.submit(Delivery::Steer);
                }
                _ => {}
            }
        }

        match k.code {
            KeyCode::Enter if shift => {
                self.insert("\n");
                vec![]
            }
            KeyCode::Enter if alt => {
                if self.running() {
                    self.submit(Delivery::Queue)
                } else {
                    self.insert("\n");
                    vec![]
                }
            }
            KeyCode::Enter => self.submit(Delivery::Steer),
            KeyCode::Esc => {
                if self.running() {
                    return self.run_cmd(Cmd::Interrupt, "");
                }
                if self.input.is_empty() {
                    if esc_was_armed {
                        return self.run_cmd(Cmd::Rewind, "");
                    }
                    self.esc_armed = true;
                }
                vec![]
            }
            KeyCode::Char('j') if ctrl => {
                self.insert("\n");
                vec![]
            }
            KeyCode::Char('a') if ctrl => {
                self.cursor = self.input[..self.cursor].rfind('\n').map_or(0, |i| i + 1);
                vec![]
            }
            KeyCode::Char('e') if ctrl => {
                self.cursor = self.input[self.cursor..]
                    .find('\n')
                    .map_or(self.input.len(), |i| self.cursor + i);
                vec![]
            }
            KeyCode::Char('u') if ctrl => {
                self.set_input(String::new());
                vec![]
            }
            KeyCode::Char(c) if !ctrl => {
                let mut b = [0u8; 4];
                self.insert(c.encode_utf8(&mut b));
                vec![]
            }
            KeyCode::Backspace => {
                if let Some((i, _)) = self.input[..self.cursor].char_indices().next_back() {
                    self.input.replace_range(i..self.cursor, "");
                    self.cursor = i;
                } else if !self.attachments.is_empty() && self.input.is_empty() {
                    self.attachments.pop();
                }
                self.refresh_menu();
                vec![]
            }
            KeyCode::Delete => {
                if let Some(c) = self.input[self.cursor..].chars().next() {
                    self.input
                        .replace_range(self.cursor..self.cursor + c.len_utf8(), "");
                }
                self.refresh_menu();
                vec![]
            }
            KeyCode::Left => {
                if let Some((i, _)) = self.input[..self.cursor].char_indices().next_back() {
                    self.cursor = i;
                }
                vec![]
            }
            KeyCode::Right => {
                if let Some(c) = self.input[self.cursor..].chars().next() {
                    self.cursor += c.len_utf8();
                }
                vec![]
            }
            KeyCode::Home => {
                self.cursor = 0;
                vec![]
            }
            KeyCode::End => {
                self.cursor = self.input.len();
                vec![]
            }
            KeyCode::Up if !self.input.contains('\n') => {
                self.history_step(-1);
                vec![]
            }
            KeyCode::Down if !self.input.contains('\n') => {
                self.history_step(1);
                vec![]
            }
            _ => match commands::by_key(&k) {
                Some(cmd) => self.run_cmd(cmd, ""),
                None => vec![],
            },
        }
    }

    fn set_input(&mut self, s: String) {
        self.input = s;
        self.cursor = self.input.len();
        self.refresh_menu();
    }

    fn insert(&mut self, s: &str) {
        self.input.insert_str(self.cursor, s);
        self.cursor += s.len();
        self.history_pos = None;
        self.refresh_menu();
    }

    fn on_paste(&mut self, s: &str) {
        let t = s.trim().trim_matches(['\'', '"']);
        // A pasted path to an existing file (e.g. a dragged image) becomes an attachment.
        if !t.contains('\n') && !t.is_empty() && Path::new(t).is_file() {
            self.attachments.push(PathBuf::from(t));
            return;
        }
        let normalized = s.replace("\r\n", "\n").replace('\r', "\n");
        self.insert(&normalized);
    }

    fn history_step(&mut self, dir: i32) {
        if self.history.is_empty() {
            return;
        }
        let len = self.history.len();
        let next = match (self.history_pos, dir) {
            (None, -1) => Some(len - 1),
            (None, _) => None,
            (Some(0), -1) => Some(0),
            (Some(i), -1) => Some(i - 1),
            (Some(i), _) if i + 1 < len => Some(i + 1),
            (Some(_), _) => None,
        };
        self.history_pos = next;
        let text = next.map(|i| self.history[i].clone()).unwrap_or_default();
        self.input = text;
        self.cursor = self.input.len();
        self.menu = None;
    }

    // ------------------------------------------------------------------ menus

    fn refresh_menu(&mut self) {
        self.menu = None;
        if let Some(q) = self.input.strip_prefix('/')
            && !q.contains(char::is_whitespace)
        {
            let mut items: Vec<MenuItem> = commands::REGISTRY
                .iter()
                .filter_map(|s| s.slash.map(|n| (n, s)))
                .filter(|(n, _)| n.starts_with(q))
                .map(|(n, s)| MenuItem {
                    name: n.to_string(),
                    hint: format!("{} {}", s.args, s.help).trim().to_string(),
                })
                .collect();
            items.extend(
                self.user_commands
                    .iter()
                    .filter(|c| c.name.starts_with(q))
                    .map(|c| MenuItem {
                        name: c.name.clone(),
                        hint: c.description.clone(),
                    }),
            );
            if !items.is_empty() {
                self.menu = Some(Menu::Slash { items, selected: 0 });
            }
            return;
        }
        // `@` file completion on the word under the cursor.
        let before = &self.input[..self.cursor];
        let start = before.rfind(char::is_whitespace).map_or(0, |i| i + 1);
        if let Some(q) = before[start..].strip_prefix('@') {
            let ql = q.to_lowercase();
            let items: Vec<String> = self
                .files
                .iter()
                .filter(|f| f.to_lowercase().contains(&ql))
                .take(8)
                .cloned()
                .collect();
            if !items.is_empty() {
                self.menu = Some(Menu::Files {
                    items,
                    selected: 0,
                    start,
                });
            }
        }
    }

    fn menu_move(&mut self, d: i32) {
        let len = match &self.menu {
            Some(Menu::Slash { items, .. }) => items.len(),
            Some(Menu::Files { items, .. }) => items.len(),
            None => return,
        };
        if let Some(Menu::Slash { selected, .. } | Menu::Files { selected, .. }) = &mut self.menu {
            *selected = ((*selected as i32 + d).rem_euclid(len as i32)) as usize;
        }
    }

    fn menu_accept(&mut self) {
        match self.menu.take() {
            Some(Menu::Slash { items, selected }) => {
                if let Some(it) = items.get(selected) {
                    self.input = format!("/{} ", it.name);
                    self.cursor = self.input.len();
                }
            }
            Some(Menu::Files {
                items,
                selected,
                start,
            }) => {
                if let Some(f) = items.get(selected) {
                    let repl = format!("@{f} ");
                    self.input.replace_range(start..self.cursor, &repl);
                    self.cursor = start + repl.len();
                }
            }
            None => {}
        }
    }

    // ------------------------------------------------------------------ submit

    fn submit(&mut self, delivery: Delivery) -> Vec<Effect> {
        let text = self.input.trim().to_string();
        if text.is_empty() && self.attachments.is_empty() {
            return vec![];
        }
        self.menu = None;
        if !text.is_empty() && self.history.last() != Some(&text) {
            self.history.push(text.clone());
        }
        self.history_pos = None;
        self.set_input(String::new());
        if let Some(rest) = text.strip_prefix('/') {
            let (name, args) = rest.split_once(char::is_whitespace).unwrap_or((rest, ""));
            if let Some(cmd) = commands::by_slash(name) {
                return self.run_cmd(cmd, args.trim());
            }
            if let Some(uc) = self.user_commands.iter().find(|c| c.name == name).cloned() {
                return self.prompt(uc.expand(args.trim()), delivery);
            }
            // `/skill-name` and `/server:prompt` go to the engine as a skill reference.
            if !name.is_empty() {
                return vec![Effect::Prompt {
                    parts: vec![
                        InputPart::Skill {
                            id: name.to_string(),
                        },
                        InputPart::text(args.trim()),
                    ]
                    .into_iter()
                    .filter(|p| !matches!(p, InputPart::Text { text } if text.is_empty()))
                    .collect(),
                    delivery,
                }];
            }
        }
        self.prompt(text, delivery)
    }

    fn prompt(&mut self, text: String, delivery: Delivery) -> Vec<Effect> {
        let mut parts: Vec<InputPart> = std::mem::take(&mut self.attachments)
            .into_iter()
            .map(|p| InputPart::FileRef {
                path: p.to_string_lossy().into_owned(),
            })
            .collect();
        if !text.is_empty() {
            parts.push(InputPart::text(text));
        }
        if self.running() && delivery == Delivery::Queue {
            self.notice = Some("Queued; it runs after this turn".into());
        }
        vec![Effect::Prompt { parts, delivery }]
    }

    /// Run a registry command (from a slash or a key).
    pub fn run_cmd(&mut self, cmd: Cmd, args: &str) -> Vec<Effect> {
        let sid = self.session_id.clone().unwrap_or_default();
        let call = |label: &str, method: &'static str, params: Value| Effect::Call {
            label: label.into(),
            method,
            params,
        };
        match cmd {
            Cmd::Help => {
                self.say(Role::Heading, "Commands");
                let rows: Vec<String> = commands::REGISTRY
                    .iter()
                    .map(|s| {
                        let name = match (s.slash, s.key) {
                            (Some(n), Some(k)) => format!("/{n} {} ({})", s.args, k.label()),
                            (Some(n), None) => format!("/{n} {}", s.args),
                            (None, Some(k)) => k.label(),
                            (None, None) => String::new(),
                        };
                        format!("  {:<44} {}", name.trim_end(), s.help)
                    })
                    .collect();
                for r in rows {
                    self.say(Role::Text, r);
                }
                let uc: Vec<String> = self
                    .user_commands
                    .iter()
                    .map(|c| format!("  /{:<43} {}", c.name, c.description))
                    .collect();
                for r in uc {
                    self.say(Role::Muted, r);
                }
                self.say(
                    Role::Muted,
                    "  Enter send/steer · Alt-Enter queue · Shift-Enter newline · @ files · ↑↓ history",
                );
                vec![]
            }
            Cmd::Model => {
                if args.is_empty() {
                    vec![call("model.list", "model.list", json!({}))]
                } else {
                    vec![Effect::Update {
                        model: Some(args.to_string()),
                        effort: None,
                        mode: None,
                    }]
                }
            }
            Cmd::Effort => {
                if args.is_empty() {
                    self.notice = Some(format!(
                        "effort: {}",
                        self.effort.as_deref().unwrap_or("default")
                    ));
                    vec![]
                } else {
                    vec![Effect::Update {
                        model: None,
                        effort: Some(args.to_string()),
                        mode: None,
                    }]
                }
            }
            Cmd::Mode => match parse_mode(args) {
                Some(m) if m == Mode::Bypass && !self.bypass_allowed => {
                    self.notice =
                        Some("bypass needs --dangerously-skip-permissions at start".into());
                    vec![]
                }
                Some(m) => self.set_mode(m),
                None if args.is_empty() => self.run_cmd(Cmd::CycleMode, ""),
                None => {
                    self.notice = Some(format!("unknown mode `{args}`"));
                    vec![]
                }
            },
            Cmd::CycleMode => {
                if self.kind == SessionKind::Chat {
                    self.notice = Some("chats always use the default mode".into());
                    return vec![];
                }
                let next = match self.mode {
                    Mode::Default => Mode::AcceptEdits,
                    Mode::AcceptEdits => Mode::Plan,
                    Mode::Plan if self.bypass_allowed => Mode::Bypass,
                    _ => Mode::Default,
                };
                self.set_mode(next)
            }
            Cmd::Compact => vec![call(
                "compact",
                "session.compact",
                json!({"session_id": sid, "focus": (!args.is_empty()).then_some(args)}),
            )],
            Cmd::Rewind => vec![call(
                "rewind",
                "checkpoint.list",
                json!({"session_id": sid}),
            )],
            Cmd::Worktree => {
                if args.is_empty() {
                    self.notice = Some("/worktree merge | pr | keep | discard".into());
                    vec![]
                } else {
                    vec![call(
                        "worktree",
                        "worktree.finish",
                        json!({"session_id": sid, "action": args}),
                    )]
                }
            }
            Cmd::Init => self.prompt(
                "Create a GENIE.md file at the root of this project with the build, test and \
                 lint commands, the code layout and the conventions a new contributor needs. \
                 If GENIE.md already exists, improve it."
                    .into(),
                Delivery::Steer,
            ),
            Cmd::Memory => vec![call("memory", "memory.list", json!({}))],
            Cmd::Skills => vec![call("skills", "skill.list", json!({}))],
            Cmd::Agents => vec![Effect::ListAgents],
            Cmd::Mcp => vec![call("mcp", "mcp.list", json!({}))],
            Cmd::Permissions => vec![call(
                "permissions",
                "config.get",
                json!({"keys": ["permissions"]}),
            )],
            Cmd::Cost => {
                self.say(
                    Role::Muted,
                    format!("Session cost so far: ${:.4}", self.cost_usd),
                );
                vec![call(
                    "usage",
                    "usage.query",
                    json!({"from": "", "to": "", "group_by": ["model"]}),
                )]
            }
            Cmd::Context => {
                self.say(
                    Role::Muted,
                    match self.context_pct {
                        Some(p) => format!("Context window: {p}% used"),
                        None => "Context window: no usage reported yet".into(),
                    },
                );
                vec![]
            }
            Cmd::Resume => {
                if args.is_empty() {
                    vec![call(
                        "resume",
                        "session.list",
                        json!({"kind": match self.kind { SessionKind::Chat => "chat", _ => "code" }, "limit": 10}),
                    )]
                } else {
                    vec![Effect::SwitchSession(args.to_string())]
                }
            }
            Cmd::Export => {
                let format = if args == "json" { "json" } else { "markdown" };
                vec![call(
                    "export",
                    "session.export",
                    json!({"session_id": sid, "format": format}),
                )]
            }
            Cmd::Clear => vec![Effect::NewSession],
            Cmd::Exit => {
                self.exit = true;
                vec![Effect::Exit]
            }
            Cmd::Interrupt => {
                if self.running() {
                    self.notice = Some("Interrupting…".into());
                    vec![Effect::Interrupt]
                } else {
                    vec![]
                }
            }
            Cmd::TakeOver => {
                self.read_only = false;
                self.notice = None;
                vec![Effect::TakeOver]
            }
        }
    }

    fn set_mode(&mut self, m: Mode) -> Vec<Effect> {
        self.mode = m;
        vec![Effect::Update {
            model: None,
            effort: None,
            mode: Some(m),
        }]
    }

    fn on_call_result(&mut self, label: &str, result: Result<Value, String>) {
        let v = match result {
            Ok(v) => v,
            Err(e) => {
                self.say(Role::Warning, format!("/{label}: {e}"));
                return;
            }
        };
        match label {
            "resume" => {
                self.say(Role::Heading, "Recent sessions (/resume <id>)");
                let items = v["items"].as_array().cloned().unwrap_or_default();
                if items.is_empty() {
                    self.say(Role::Muted, "  none");
                }
                for it in items {
                    self.say(
                        Role::Text,
                        format!(
                            "  {}  {}  {}",
                            it["session_id"].as_str().unwrap_or_default(),
                            it["updated_at"].as_str().unwrap_or_default(),
                            it["title"].as_str().unwrap_or("(untitled)")
                        ),
                    );
                }
            }
            "export" => {
                let text = v["text"].as_str().map(str::to_string).unwrap_or_else(|| {
                    format!("exported to blob {}", v["blob_id"].as_str().unwrap_or("?"))
                });
                self.say(Role::Text, text);
            }
            "compact" => self.say(Role::Muted, "Compaction requested"),
            _ => {
                let pretty = serde_json::to_string_pretty(&v).unwrap_or_default();
                self.say(Role::Muted, format!("/{label}:\n{pretty}"));
            }
        }
    }

    // ------------------------------------------------------------------ asks

    fn push_ask(&mut self, ask: Ask, request: Option<Id>) {
        let form = match ask.kind {
            AskKind::Question | AskKind::Elicitation => {
                let mut fields: Vec<(String, bool)> = ask
                    .form
                    .as_ref()
                    .and_then(|f| f.get("properties"))
                    .and_then(Value::as_object)
                    .map(|p| {
                        p.keys()
                            .map(|k| (k.clone(), ask.sensitive_fields.contains(k)))
                            .collect()
                    })
                    .unwrap_or_default();
                if fields.is_empty() {
                    fields.push(("answer".into(), false));
                }
                let n = fields.len();
                Some(FormState {
                    fields,
                    index: 0,
                    values: vec![String::new(); n],
                })
            }
            _ => None,
        };
        self.asks.push_back(AskState {
            ask,
            request,
            selected: 0,
            reason: None,
            form,
        });
    }

    fn on_ask_key(&mut self, k: KeyEvent) -> Vec<Effect> {
        let Some(a) = self.asks.front_mut() else {
            return vec![];
        };
        // Forms (elicitation / question).
        if let Some(form) = &mut a.form {
            match k.code {
                KeyCode::Esc => return self.answer_front(Decision::Cancel, None, None),
                KeyCode::Enter => {
                    if form.index + 1 < form.fields.len() {
                        form.index += 1;
                        return vec![];
                    }
                    let data: serde_json::Map<String, Value> = form
                        .fields
                        .iter()
                        .zip(&form.values)
                        .map(|((n, _), v)| (n.clone(), Value::String(v.clone())))
                        .collect();
                    return self.answer_front(Decision::Submit, None, Some(Value::Object(data)));
                }
                KeyCode::Backspace => {
                    form.values[form.index].pop();
                }
                KeyCode::Char(c) => form.values[form.index].push(c),
                _ => {}
            }
            return vec![];
        }
        // Typing a deny reason.
        if let Some(reason) = &mut a.reason {
            match k.code {
                KeyCode::Esc => return self.answer_front(Decision::Reject, None, None),
                KeyCode::Enter => {
                    let msg = reason.trim().to_string();
                    return self.answer_front(
                        Decision::Reject,
                        (!msg.is_empty()).then_some(msg),
                        None,
                    );
                }
                KeyCode::Backspace => {
                    reason.pop();
                }
                KeyCode::Char(c) => reason.push(c),
                _ => {}
            }
            return vec![];
        }
        let choose = match k.code {
            KeyCode::Left | KeyCode::Up | KeyCode::BackTab => {
                a.selected = (a.selected + ASK_OPTIONS.len() - 1) % ASK_OPTIONS.len();
                None
            }
            KeyCode::Right | KeyCode::Down | KeyCode::Tab => {
                a.selected = (a.selected + 1) % ASK_OPTIONS.len();
                None
            }
            KeyCode::Char('1') | KeyCode::Char('y') => Some(0),
            KeyCode::Char('2') | KeyCode::Char('a') => Some(1),
            KeyCode::Char('3') | KeyCode::Char('n') => Some(2),
            KeyCode::Enter => Some(a.selected),
            KeyCode::Esc => Some(2),
            _ => None,
        };
        match choose {
            Some(0) => self.answer_front(Decision::Once, None, None),
            Some(1) => self.answer_front(Decision::Always, None, None),
            Some(_) => {
                if let Some(a) = self.asks.front_mut() {
                    a.selected = 2;
                    a.reason = Some(String::new());
                }
                vec![]
            }
            None => vec![],
        }
    }

    fn answer_front(
        &mut self,
        decision: Decision,
        message: Option<String>,
        data: Option<Value>,
    ) -> Vec<Effect> {
        let Some(a) = self.asks.pop_front() else {
            return vec![];
        };
        let scope = (decision == Decision::Always).then(|| {
            a.ask
                .save_suggestions
                .first()
                .map_or(RuleScope::Session, |s| s.scope)
        });
        vec![Effect::Answer {
            ask_id: a.ask.id,
            request: a.request,
            answer: AskAnswer {
                decision,
                scope,
                message,
                data,
            },
        }]
    }

    // ------------------------------------------------------------------ events

    fn on_event(&mut self, env: &SessionEventEnvelope) {
        if self
            .session_id
            .as_deref()
            .is_some_and(|s| s != env.session_id)
        {
            return;
        }
        let sid = env.session_id.clone();
        match &env.event {
            SessionEvent::SessionCreated(c) => {
                self.model.clone_from(&c.model);
                self.effort.clone_from(&c.effort);
                self.mode = c.mode;
                self.kind = c.kind;
            }
            SessionEvent::SessionUpdated(u) => {
                if let Some(m) = &u.model {
                    self.model.clone_from(m);
                }
                if u.effort.is_some() {
                    self.effort.clone_from(&u.effort);
                }
                if let Some(m) = u.mode {
                    self.mode = m;
                }
            }
            SessionEvent::MessageAdmitted(m) if m.role == MsgRole::User => {
                let text: Vec<String> = m
                    .parts
                    .iter()
                    .map(|p| match p {
                        InputPart::Text { text } => text.clone(),
                        InputPart::FileRef { path } => format!("[{path}]"),
                        InputPart::Attachment { name, .. } => format!("[{name}]"),
                        InputPart::Skill { id } => format!("/{id}"),
                        InputPart::Resource { server, uri } => format!("[{server}:{uri}]"),
                    })
                    .collect();
                let st = self.theme.style(Role::User);
                for (i, l) in text.join(" ").lines().enumerate() {
                    let prefix = if i == 0 { "› " } else { "  " };
                    self.scrollback
                        .push(Line::from(Span::styled(format!("{prefix}{l}"), st)));
                }
            }
            SessionEvent::InboxChanged(i) => self.inbox.clone_from(&i.items),
            SessionEvent::TurnStarted(_) => {
                self.status = LiveStatus::Running;
                self.stream.clear();
            }
            SessionEvent::PartStarted(p) => {
                if !self.stream.iter().any(|s| s.part_id == p.part_id) {
                    self.stream.push(StreamPart {
                        part_id: p.part_id.clone(),
                        kind: p.kind,
                        text: String::new(),
                        committed: 0,
                    });
                }
            }
            SessionEvent::PartDelta(d) => {
                let Some(t) = &d.text else { return };
                let idx = match self.stream.iter().position(|s| s.part_id == d.part_id) {
                    Some(i) => i,
                    None => {
                        self.stream.push(StreamPart {
                            part_id: d.part_id.clone(),
                            kind: PartKind::Text,
                            text: String::new(),
                            committed: 0,
                        });
                        self.stream.len() - 1
                    }
                };
                self.stream[idx].text.push_str(t);
                if self.stream[idx].kind == PartKind::Text {
                    self.commit_stable(idx);
                }
            }
            SessionEvent::PartEnded(p) => {
                let idx = self.stream.iter().position(|s| s.part_id == p.part_id);
                let committed = idx.map_or(0, |i| self.stream[i].committed);
                match &p.part {
                    Part::Text {
                        text, citations, ..
                    } => {
                        let (text, refs) = render::cite(text, citations, 1);
                        let from = if citations.is_empty()
                            && idx.is_some_and(|i| {
                                text.starts_with(&self.stream[i].text[..committed])
                            }) {
                            committed
                        } else if committed > 0 {
                            // Citations shift offsets; re-render only what was not shown.
                            text.len().min(committed)
                        } else {
                            0
                        };
                        let rest = text.get(from..).unwrap_or_default().to_string();
                        let lines = markdown::render(&rest, &self.theme);
                        self.scrollback.extend(lines);
                        for r in refs {
                            self.say(Role::Muted, format!("  {r}"));
                        }
                    }
                    Part::Reasoning { .. } => {
                        for l in render::part_lines(&sid, &p.part, &mut vec![]) {
                            self.say(Role::Thinking, l);
                        }
                    }
                    Part::ToolCall { .. } => {
                        for l in render::part_lines(&sid, &p.part, &mut vec![]) {
                            self.say(Role::Accent, l);
                        }
                    }
                    Part::ToolResult { is_error, .. } => {
                        let role = if *is_error { Role::Error } else { Role::Muted };
                        for l in render::part_lines(&sid, &p.part, &mut vec![]) {
                            self.say(role, l);
                        }
                    }
                    other => {
                        for l in render::part_lines(&sid, other, &mut vec![]) {
                            self.say(Role::Link, l);
                        }
                    }
                }
                if let Some(i) = idx {
                    self.stream.remove(i);
                }
            }
            SessionEvent::ToolEnded(t) => {
                let word = match t.outcome {
                    ToolOutcome::Declined => Some("declined"),
                    ToolOutcome::Blocked => Some("blocked"),
                    ToolOutcome::Cancelled => Some("cancelled"),
                    _ => None,
                };
                if let Some(w) = word {
                    self.say(Role::Warning, format!("  ⎿ {w}"));
                }
            }
            SessionEvent::PermissionAsked(a) => {
                if !self.asks.iter().any(|x| x.ask.id == a.ask.id) {
                    self.push_ask((*a.ask).clone(), None);
                }
            }
            SessionEvent::PermissionResolved(r) => {
                self.asks.retain(|a| a.ask.id != r.ask_id);
            }
            SessionEvent::StepEnded(s) => self.cost_usd += s.cost.usd,
            SessionEvent::UsageUpdated(u) => {
                self.cost_usd = u.cost.usd;
                if let Some(pct) = u
                    .context_tokens
                    .saturating_mul(100)
                    .checked_div(u.context_window)
                {
                    let pct = pct.min(100);
                    self.context_pct = u8::try_from(pct).ok();
                }
            }
            SessionEvent::TurnSettled(t) => {
                self.flush_stream(&sid);
                self.status = LiveStatus::Idle;
                if self.notice.as_deref() == Some("Interrupting…") {
                    self.notice = None;
                }
                match t.status {
                    TurnStatus::Cancelled => self.say(Role::Warning, "  ⎿ Interrupted"),
                    TurnStatus::Failed => {
                        let msg = t
                            .error
                            .as_ref()
                            .map(errors::turn_error)
                            .unwrap_or_else(|| "The turn failed.".into());
                        self.say(Role::Error, format!("✗ {msg}"));
                    }
                    _ => {}
                }
                self.scrollback.push(Line::default());
            }
            SessionEvent::TurnInterrupted(_) => {
                self.status = LiveStatus::Interrupted;
                self.say(
                    Role::Warning,
                    "The engine stopped during this turn. Send a message to continue.",
                );
            }
            SessionEvent::SessionCompacted(c) => self.say(
                Role::Muted,
                format!(
                    "── compacted: {} → {} tokens ──",
                    c.tokens_before, c.tokens_after
                ),
            ),
            SessionEvent::ArtifactVersioned(a) => self.say(
                Role::Link,
                format!(
                    "▣ {} v{} ({}) — open in desktop: {}",
                    a.title,
                    a.n,
                    a.artifact_type,
                    render::part_link(&sid, &a.artifact_id)
                ),
            ),
            SessionEvent::MemoryChanged(_) => self.say(Role::Muted, "◆ Memory updated"),
            SessionEvent::SubagentStarted(s) => {
                self.subagents.insert(s.child_session_id.clone());
                self.say(
                    Role::Accent,
                    format!("⏺ {}: {}", s.agent, render::truncate(&s.description, 80)),
                );
            }
            SessionEvent::SubagentEnded(s) => {
                self.subagents.remove(&s.child_session_id);
                if let Some(sum) = &s.summary {
                    self.say(Role::Muted, format!("  ⎿ {}", render::truncate(sum, 100)));
                }
            }
            SessionEvent::LeaseChanged(l) => {
                self.driver = l.driver.as_ref().and_then(|d| d.label.clone());
                self.read_only = l
                    .driver
                    .as_ref()
                    .is_some_and(|d| d.client_instance_id != self.instance_id);
            }
            SessionEvent::SessionError(e) => {
                self.say(Role::Error, format!("✗ {} ({})", e.message, e.code));
            }
            SessionEvent::RetryScheduled(r) => {
                self.notice = Some(format!(
                    "Retrying in {:.1}s ({}, attempt {})",
                    r.after_ms as f64 / 1000.0,
                    r.error_class,
                    r.attempt
                ));
            }
            SessionEvent::Status(s) => {
                self.status = s.state;
            }
            _ => {}
        }
    }

    /// Commit the settled blocks of a streaming text part to scrollback.
    fn commit_stable(&mut self, idx: usize) {
        let s = &self.stream[idx];
        let n = markdown::stable_prefix(&s.text[s.committed..]);
        if n == 0 {
            return;
        }
        let chunk = s.text[s.committed..s.committed + n].to_string();
        self.stream[idx].committed += n;
        let lines = markdown::render(&chunk, &self.theme);
        self.scrollback.extend(lines);
    }

    fn flush_stream(&mut self, _sid: &str) {
        for s in std::mem::take(&mut self.stream) {
            if s.kind == PartKind::Text && s.committed < s.text.len() {
                let lines = markdown::render(&s.text[s.committed..], &self.theme);
                self.scrollback.extend(lines);
            }
        }
    }

    /// The unstable tail(s) shown in the footer.
    pub fn live_lines(&self) -> Vec<Line<'static>> {
        let mut out = vec![];
        for s in &self.stream {
            match s.kind {
                PartKind::Text => out.extend(markdown::render(&s.text[s.committed..], &self.theme)),
                PartKind::Reasoning => out.push(Line::from(Span::styled(
                    format!(
                        "∴ {}",
                        render::truncate(
                            render::first_line(s.text.lines().last().unwrap_or_default()),
                            100
                        )
                    ),
                    self.theme.style(Role::Thinking),
                ))),
                _ => {}
            }
        }
        out
    }
}

pub fn parse_mode(s: &str) -> Option<Mode> {
    match s {
        "default" => Some(Mode::Default),
        "accept-edits" | "acceptEdits" | "accept_edits" => Some(Mode::AcceptEdits),
        "plan" => Some(Mode::Plan),
        "bypass" => Some(Mode::Bypass),
        _ => None,
    }
}

pub fn mode_label(m: Mode) -> &'static str {
    match m {
        Mode::Default => "default",
        Mode::AcceptEdits => "accept-edits",
        Mode::Plan => "plan",
        Mode::Bypass => "bypass",
        Mode::Unknown => "?",
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;
    use genie_protocol::ask::{Preview, SaveSuggestion};
    use genie_protocol::event::{
        ErrorInfo, MessageAdmitted, MessageSource, PartDelta, PartEnded, PartStarted,
        PermissionAsked, TurnSettled, TurnStarted,
    };

    fn key(c: KeyCode) -> Msg {
        Msg::Key(KeyEvent::new(c, KeyModifiers::NONE))
    }
    fn mkey(c: KeyCode, m: KeyModifiers) -> Msg {
        Msg::Key(KeyEvent::new(c, m))
    }
    fn typ(app: &mut App, s: &str) {
        for c in s.chars() {
            app.update(key(KeyCode::Char(c)));
        }
    }
    fn env(event: SessionEvent) -> Msg {
        Msg::Event(Box::new(SessionEventEnvelope {
            session_id: "ses_1".into(),
            seq: event.is_durable().then_some(1),
            ref_seq: None,
            ts: String::new(),
            turn_id: None,
            step: None,
            subagent_of: None,
            event,
        }))
    }
    fn app() -> App {
        let mut a = App::new(Theme::default(), "cli_me");
        a.session_id = Some("ses_1".into());
        a
    }
    fn text(lines: &[Line<'_>]) -> Vec<String> {
        lines
            .iter()
            .map(|l| l.spans.iter().map(|s| s.content.as_ref()).collect())
            .collect()
    }
    fn running(a: &mut App) {
        a.update(env(SessionEvent::TurnStarted(TurnStarted {
            turn_id: "t".into(),
            message_id: "m".into(),
            resume: false,
        })));
    }

    #[test]
    fn enter_sends_alt_enter_queues_while_running() {
        let mut a = app();
        typ(&mut a, "hello");
        let fx = a.update(key(KeyCode::Enter));
        assert_eq!(
            fx,
            [Effect::Prompt {
                parts: vec![InputPart::text("hello")],
                delivery: Delivery::Steer
            }]
        );
        assert!(a.input.is_empty());
        // Idle: Alt-Enter is a newline; Shift-Enter too.
        typ(&mut a, "a");
        assert!(a.update(mkey(KeyCode::Enter, KeyModifiers::ALT)).is_empty());
        a.update(mkey(KeyCode::Enter, KeyModifiers::SHIFT));
        assert_eq!(a.input, "a\n\n");
        a.update(mkey(KeyCode::Char('u'), KeyModifiers::CONTROL));
        running(&mut a);
        typ(&mut a, "later");
        let fx = a.update(mkey(KeyCode::Enter, KeyModifiers::ALT));
        assert!(matches!(
            &fx[0],
            Effect::Prompt {
                delivery: Delivery::Queue,
                ..
            }
        ));
        typ(&mut a, "now");
        let fx = a.update(key(KeyCode::Enter));
        assert!(matches!(
            &fx[0],
            Effect::Prompt {
                delivery: Delivery::Steer,
                ..
            }
        ));
    }

    #[test]
    fn esc_interrupts_and_esc_esc_rewinds() {
        let mut a = app();
        running(&mut a);
        assert_eq!(a.update(key(KeyCode::Esc)), [Effect::Interrupt]);
        a.status = LiveStatus::Idle;
        assert!(a.update(key(KeyCode::Esc)).is_empty());
        let fx = a.update(key(KeyCode::Esc));
        assert!(matches!(
            &fx[0],
            Effect::Call {
                method: "checkpoint.list",
                ..
            }
        ));
    }

    #[test]
    fn ctrl_c_twice_exits_first_clears() {
        let mut a = app();
        typ(&mut a, "draft");
        let cc = mkey(KeyCode::Char('c'), KeyModifiers::CONTROL);
        assert!(a.update(cc.clone()).is_empty());
        assert!(a.input.is_empty());
        assert_eq!(a.update(cc), [Effect::Exit]);
        assert!(a.exit);
    }

    #[test]
    fn shift_tab_cycles_modes_bypass_only_when_allowed() {
        let mut a = app();
        let st = mkey(KeyCode::BackTab, KeyModifiers::SHIFT);
        let modes: Vec<Mode> = (0..4)
            .map(|_| {
                a.update(st.clone());
                a.mode
            })
            .collect();
        assert_eq!(
            modes,
            [
                Mode::AcceptEdits,
                Mode::Plan,
                Mode::Default,
                Mode::AcceptEdits
            ]
        );
        a.bypass_allowed = true;
        a.update(st.clone());
        a.update(st);
        assert_eq!(a.mode, Mode::Bypass);
    }

    #[test]
    fn slash_menu_and_commands() {
        let mut a = app();
        typ(&mut a, "/co");
        match &a.menu {
            Some(Menu::Slash { items, .. }) => {
                let names: Vec<&str> = items.iter().map(|i| i.name.as_str()).collect();
                assert_eq!(names, ["compact", "cost", "context"]);
            }
            other => panic!("{other:?}"),
        }
        a.update(key(KeyCode::Down));
        a.update(key(KeyCode::Tab));
        assert_eq!(a.input, "/cost ");
        a.update(mkey(KeyCode::Char('u'), KeyModifiers::CONTROL));
        typ(&mut a, "/compact keep the API notes");
        let fx = a.update(key(KeyCode::Enter));
        match &fx[0] {
            Effect::Call { method, params, .. } => {
                assert_eq!(*method, "session.compact");
                assert_eq!(params["focus"], "keep the API notes");
            }
            other => panic!("{other:?}"),
        }
        typ(&mut a, "/model gpt-x");
        assert_eq!(
            a.update(key(KeyCode::Enter)),
            [Effect::Update {
                model: Some("gpt-x".into()),
                effort: None,
                mode: None
            }]
        );
        typ(&mut a, "/clear");
        a.menu = None;
        assert_eq!(a.update(key(KeyCode::Enter)), [Effect::NewSession]);
    }

    #[test]
    fn at_completes_files_and_history_recalls() {
        let mut a = app();
        a.update(Msg::Files(vec![
            "src/main.rs".into(),
            "src/lib.rs".into(),
            "README.md".into(),
        ]));
        typ(&mut a, "look at @lib");
        assert!(matches!(&a.menu, Some(Menu::Files { items, .. }) if items == &["src/lib.rs"]));
        a.update(key(KeyCode::Tab));
        assert_eq!(a.input, "look at @src/lib.rs ");
        a.update(key(KeyCode::Enter));
        typ(&mut a, "second");
        a.update(key(KeyCode::Enter));
        a.update(key(KeyCode::Up));
        assert_eq!(a.input, "second");
        a.update(key(KeyCode::Up));
        assert_eq!(a.input, "look at @src/lib.rs");
        a.update(key(KeyCode::Down));
        a.update(key(KeyCode::Down));
        assert_eq!(a.input, "");
    }

    #[test]
    fn pasted_file_path_is_an_attachment() {
        let d = tempfile::tempdir().unwrap();
        let f = d.path().join("shot.png");
        std::fs::write(&f, b"x").unwrap();
        let mut a = app();
        a.update(Msg::Paste(f.to_string_lossy().into_owned()));
        assert_eq!(a.attachments, std::slice::from_ref(&f));
        a.update(Msg::Paste("not a path".into()));
        let fx = a.update(key(KeyCode::Enter));
        assert_eq!(
            fx,
            [Effect::Prompt {
                parts: vec![
                    InputPart::FileRef {
                        path: f.to_string_lossy().into_owned()
                    },
                    InputPart::text("not a path")
                ],
                delivery: Delivery::Steer
            }]
        );
    }

    #[test]
    fn streaming_commits_settled_blocks_only() {
        let mut a = app();
        running(&mut a);
        a.update(env(SessionEvent::PartStarted(PartStarted {
            message_id: "m".into(),
            part_id: "p".into(),
            kind: PartKind::Text,
        })));
        let delta = |t: &str| {
            env(SessionEvent::PartDelta(PartDelta {
                part_id: "p".into(),
                text: Some(t.into()),
                json: None,
            }))
        };
        a.update(delta("First para"));
        assert!(a.take_scrollback().is_empty());
        assert_eq!(text(&a.live_lines()), ["First para"]);
        a.update(delta("graph.\n\nSecond"));
        assert_eq!(text(&a.take_scrollback()), ["First paragraph.", ""]);
        assert_eq!(text(&a.live_lines()), ["Second"]);
        a.update(env(SessionEvent::PartEnded(PartEnded {
            part_id: "p".into(),
            part: Part::Text {
                id: "p".into(),
                text: "First paragraph.\n\nSecond one.".into(),
                citations: vec![],
            },
        })));
        assert_eq!(text(&a.take_scrollback()), ["Second one."]);
        assert!(a.live_lines().is_empty());
    }

    #[test]
    fn permission_prompt_allow_always_deny_with_reason() {
        let mut a = app();
        let mut ask = Ask::new("ask_1", "ses_1", AskKind::Permission, "");
        ask.tool = Some("shell".into());
        ask.preview = Some(Preview::Command {
            command: "make".into(),
            cwd: None,
        });
        ask.save_suggestions = vec![SaveSuggestion {
            rule: "Bash(make:*)".into(),
            scope: RuleScope::Local,
        }];
        a.update(env(SessionEvent::PermissionAsked(PermissionAsked {
            ask: Box::new(ask.clone()),
        })));
        a.update(Msg::AskRequest {
            id: Id::Num(7),
            ask: Box::new(ask.clone()),
        });
        assert_eq!(a.asks.len(), 1);
        // Typing goes to the ask, not the prompt.
        let fx = a.update(key(KeyCode::Char('2')));
        assert_eq!(
            fx,
            [Effect::Answer {
                ask_id: "ask_1".into(),
                request: Some(Id::Num(7)),
                answer: AskAnswer {
                    decision: Decision::Always,
                    scope: Some(RuleScope::Local),
                    message: None,
                    data: None
                }
            }]
        );
        a.update(env(SessionEvent::PermissionAsked(PermissionAsked {
            ask: Box::new(ask),
        })));
        a.update(key(KeyCode::Char('n')));
        typ(&mut a, "use ninja");
        let fx = a.update(key(KeyCode::Enter));
        match &fx[0] {
            Effect::Answer {
                answer, request, ..
            } => {
                assert_eq!(answer.decision, Decision::Reject);
                assert_eq!(answer.message.as_deref(), Some("use ninja"));
                assert_eq!(*request, None);
            }
            other => panic!("{other:?}"),
        }
        assert!(a.input.is_empty());
    }

    #[test]
    fn question_form_submits_data() {
        let mut a = app();
        let mut ask = Ask::new("ask_q", "ses_1", AskKind::Question, "");
        ask.form = Some(json!({"properties": {"name": {"type": "string"}}}));
        a.update(Msg::AskRequest {
            id: Id::Num(1),
            ask: Box::new(ask),
        });
        typ(&mut a, "Ada");
        let fx = a.update(key(KeyCode::Enter));
        match &fx[0] {
            Effect::Answer { answer, .. } => {
                assert_eq!(answer.decision, Decision::Submit);
                assert_eq!(answer.data, Some(json!({"name": "Ada"})));
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn user_messages_errors_and_lease() {
        let mut a = app();
        a.update(env(SessionEvent::MessageAdmitted(MessageAdmitted {
            message_id: "m".into(),
            parent_id: None,
            role: MsgRole::User,
            parts: vec![InputPart::text("hi there")],
            delivery: Delivery::Steer,
            source: MessageSource::User,
        })));
        running(&mut a);
        a.update(env(SessionEvent::TurnSettled(TurnSettled {
            turn_id: "t".into(),
            status: TurnStatus::Failed,
            error: Some(ErrorInfo {
                code: "no_provider_configured".into(),
                message: "no_provider_configured".into(),
                link: None,
            }),
        })));
        let sb = text(&a.take_scrollback());
        assert_eq!(sb[0], "› hi there");
        assert!(sb[1].contains("genie login"), "{sb:?}");
        assert!(!a.running());
        a.update(Msg::LeaseLost {
            by: Some("Desktop".into()),
        });
        assert!(a.read_only);
        let fx = a.update(mkey(KeyCode::Char('t'), KeyModifiers::CONTROL));
        assert_eq!(fx, [Effect::TakeOver]);
    }

    #[test]
    fn help_lists_registry() {
        let mut a = app();
        typ(&mut a, "/help");
        a.update(key(KeyCode::Enter));
        let sb = text(&a.take_scrollback()).join("\n");
        assert!(sb.contains("/compact [focus]"));
        assert!(sb.contains("Shift-Tab"));
        assert!(sb.contains("Esc"));
    }
}
