//! The one command registry (07 §1.3, §2.3): slash commands and keybindings come from
//! the same table, so `/help` lists both and a key is just another way to run a command.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Cmd {
    Help,
    Model,
    Effort,
    Mode,
    CycleMode,
    Compact,
    Rewind,
    Worktree,
    Init,
    Memory,
    Skills,
    Agents,
    Mcp,
    Permissions,
    Cost,
    Context,
    Resume,
    Export,
    Clear,
    Exit,
    Interrupt,
    TakeOver,
}

/// A key chord bound to a command.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Chord {
    pub code: KeyCode,
    pub mods: KeyModifiers,
}

impl Chord {
    pub const fn new(code: KeyCode, mods: KeyModifiers) -> Chord {
        Chord { code, mods }
    }

    pub fn matches(&self, k: &KeyEvent) -> bool {
        // Shift on BackTab is implied; compare the rest exactly.
        let mods = if k.code == KeyCode::BackTab {
            k.modifiers - KeyModifiers::SHIFT
        } else {
            k.modifiers
        };
        k.code == self.code && mods == self.mods
    }

    pub fn label(&self) -> String {
        let mut s = String::new();
        if self.mods.contains(KeyModifiers::CONTROL) {
            s.push_str("Ctrl-");
        }
        if self.mods.contains(KeyModifiers::ALT) {
            s.push_str("Alt-");
        }
        match self.code {
            KeyCode::Char(c) => s.push(c.to_ascii_uppercase()),
            KeyCode::Esc => s.push_str("Esc"),
            KeyCode::BackTab => s.push_str("Shift-Tab"),
            other => s.push_str(&format!("{other:?}")),
        }
        s
    }
}

pub struct Spec {
    pub cmd: Cmd,
    /// Slash name without `/`; `None` for key-only commands.
    pub slash: Option<&'static str>,
    pub args: &'static str,
    pub help: &'static str,
    pub key: Option<Chord>,
}

const NONE: KeyModifiers = KeyModifiers::NONE;

pub const REGISTRY: &[Spec] = &[
    Spec {
        cmd: Cmd::Help,
        slash: Some("help"),
        args: "",
        help: "commands and keys",
        key: None,
    },
    Spec {
        cmd: Cmd::Model,
        slash: Some("model"),
        args: "[id]",
        help: "show or switch the model",
        key: None,
    },
    Spec {
        cmd: Cmd::Effort,
        slash: Some("effort"),
        args: "[level]",
        help: "reasoning effort",
        key: None,
    },
    Spec {
        cmd: Cmd::Mode,
        slash: Some("mode"),
        args: "[default|accept-edits|plan|bypass]",
        help: "permission mode",
        key: None,
    },
    Spec {
        cmd: Cmd::CycleMode,
        slash: None,
        args: "",
        help: "cycle mode default → accept-edits → plan",
        key: Some(Chord::new(KeyCode::BackTab, NONE)),
    },
    Spec {
        cmd: Cmd::Compact,
        slash: Some("compact"),
        args: "[focus]",
        help: "compact the conversation",
        key: None,
    },
    Spec {
        cmd: Cmd::Rewind,
        slash: Some("rewind"),
        args: "",
        help: "rewind to a checkpoint (Esc Esc on an empty prompt)",
        key: None,
    },
    Spec {
        cmd: Cmd::Worktree,
        slash: Some("worktree"),
        args: "[merge|pr|keep|discard]",
        help: "finish this session's worktree",
        key: None,
    },
    Spec {
        cmd: Cmd::Init,
        slash: Some("init"),
        args: "",
        help: "create GENIE.md for this project",
        key: None,
    },
    Spec {
        cmd: Cmd::Memory,
        slash: Some("memory"),
        args: "",
        help: "show memories",
        key: None,
    },
    Spec {
        cmd: Cmd::Skills,
        slash: Some("skills"),
        args: "",
        help: "list skills",
        key: None,
    },
    Spec {
        cmd: Cmd::Agents,
        slash: Some("agents"),
        args: "",
        help: "list agents",
        key: None,
    },
    Spec {
        cmd: Cmd::Mcp,
        slash: Some("mcp"),
        args: "",
        help: "MCP servers",
        key: None,
    },
    Spec {
        cmd: Cmd::Permissions,
        slash: Some("permissions"),
        args: "",
        help: "permission rules",
        key: None,
    },
    Spec {
        cmd: Cmd::Cost,
        slash: Some("cost"),
        args: "",
        help: "session cost and usage",
        key: None,
    },
    Spec {
        cmd: Cmd::Context,
        slash: Some("context"),
        args: "",
        help: "context window use",
        key: None,
    },
    Spec {
        cmd: Cmd::Resume,
        slash: Some("resume"),
        args: "[id]",
        help: "switch to another session",
        key: None,
    },
    Spec {
        cmd: Cmd::Export,
        slash: Some("export"),
        args: "[md|json]",
        help: "export this session",
        key: None,
    },
    Spec {
        cmd: Cmd::Clear,
        slash: Some("clear"),
        args: "",
        help: "start a new session",
        key: None,
    },
    Spec {
        cmd: Cmd::Exit,
        slash: Some("exit"),
        args: "",
        help: "quit (Ctrl-C twice)",
        key: None,
    },
    Spec {
        cmd: Cmd::Interrupt,
        slash: None,
        args: "",
        help: "interrupt the running turn",
        key: Some(Chord::new(KeyCode::Esc, NONE)),
    },
    Spec {
        cmd: Cmd::TakeOver,
        slash: Some("takeover"),
        args: "",
        help: "take over a session driven elsewhere",
        key: Some(Chord::new(KeyCode::Char('t'), KeyModifiers::CONTROL)),
    },
];

pub fn by_key(k: &KeyEvent) -> Option<Cmd> {
    REGISTRY
        .iter()
        .find(|s| s.key.is_some_and(|c| c.matches(k)))
        .map(|s| s.cmd)
}

pub fn by_slash(name: &str) -> Option<Cmd> {
    REGISTRY
        .iter()
        .find(|s| s.slash == Some(name) || (name == "quit" && s.cmd == Cmd::Exit))
        .map(|s| s.cmd)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    #[test]
    fn keys_and_slashes_resolve_from_one_table() {
        assert_eq!(
            by_key(&KeyEvent::new(KeyCode::Esc, NONE)),
            Some(Cmd::Interrupt)
        );
        assert_eq!(
            by_key(&KeyEvent::new(KeyCode::BackTab, KeyModifiers::SHIFT)),
            Some(Cmd::CycleMode)
        );
        assert_eq!(by_slash("compact"), Some(Cmd::Compact));
        assert_eq!(by_slash("quit"), Some(Cmd::Exit));
        assert_eq!(by_slash("nope"), None);
        // Every built-in from 07 §2.3 is registered.
        for n in [
            "help",
            "model",
            "effort",
            "mode",
            "compact",
            "rewind",
            "worktree",
            "init",
            "memory",
            "skills",
            "agents",
            "mcp",
            "permissions",
            "cost",
            "context",
            "resume",
            "export",
            "clear",
        ] {
            assert!(by_slash(n).is_some(), "/{n}");
        }
    }
}
