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

/// A user command from `~/.genie/commands` or `.genie/commands` (Markdown body with
/// `$ARGUMENTS`), sent as a prompt.
#[derive(Debug, Clone, PartialEq)]
pub struct UserCommand {
    pub name: String,
    pub description: String,
    pub body: String,
}

impl UserCommand {
    pub fn expand(&self, args: &str) -> String {
        if self.body.contains("$ARGUMENTS") {
            self.body.replace("$ARGUMENTS", args)
        } else if args.is_empty() {
            self.body.clone()
        } else {
            format!("{}\n\n{args}", self.body)
        }
    }
}

/// Load `*.md` user commands from the given directories (later dirs win by name).
/// Subdirectories become `dir:name`. Frontmatter `description:` is used when present.
pub fn load_user_commands(dirs: &[std::path::PathBuf]) -> Vec<UserCommand> {
    let mut out: Vec<UserCommand> = vec![];
    for dir in dirs {
        let mut stack = vec![(dir.clone(), String::new())];
        while let Some((d, prefix)) = stack.pop() {
            let Ok(rd) = std::fs::read_dir(&d) else {
                continue;
            };
            for e in rd.flatten() {
                let p = e.path();
                let stem = p
                    .file_stem()
                    .map(|s| s.to_string_lossy().into_owned())
                    .unwrap_or_default();
                if p.is_dir() {
                    stack.push((p, format!("{prefix}{stem}:")));
                } else if p.extension().is_some_and(|x| x == "md")
                    && let Ok(text) = std::fs::read_to_string(&p)
                {
                    let (description, body) = split_frontmatter(&text);
                    let name = format!("{prefix}{stem}");
                    out.retain(|c| c.name != name);
                    out.push(UserCommand {
                        name,
                        description,
                        body,
                    });
                }
            }
        }
    }
    out.sort_by(|a, b| a.name.cmp(&b.name));
    out
}

fn split_frontmatter(text: &str) -> (String, String) {
    if let Some(rest) = text.strip_prefix("---\n")
        && let Some(end) = rest.find("\n---")
    {
        let fm = &rest[..end];
        let body = rest[end + 4..].trim_start_matches('\n');
        let desc = fm
            .lines()
            .find_map(|l| l.strip_prefix("description:"))
            .map(|d| d.trim().trim_matches('"').to_string())
            .unwrap_or_default();
        return (desc, body.to_string());
    }
    let desc = text
        .lines()
        .next()
        .unwrap_or_default()
        .trim_start_matches('#')
        .trim()
        .to_string();
    (desc, text.to_string())
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

    #[test]
    fn user_commands_load_and_expand() {
        let d = tempfile::tempdir().unwrap();
        std::fs::write(
            d.path().join("review.md"),
            "---\ndescription: Review code\n---\nReview $ARGUMENTS",
        )
        .unwrap();
        std::fs::create_dir(d.path().join("git")).unwrap();
        std::fs::write(d.path().join("git/pr.md"), "# Open a PR\nDo it").unwrap();
        let c = load_user_commands(&[d.path().to_path_buf()]);
        assert_eq!(c.len(), 2);
        assert_eq!(c[0].name, "git:pr");
        assert_eq!(c[0].description, "Open a PR");
        assert_eq!(c[1].description, "Review code");
        assert_eq!(c[1].expand("src/"), "Review src/");
    }
}
