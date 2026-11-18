//! Markdown slash commands (06 §9 "Slash commands"): `~/.genie/commands/` and (trusted
//! workspaces only) `.genie/commands/`.
//!
//! Compatible with the CLI's loader (`apps/cli/src/tui/commands.rs`): `*.md` files,
//! subfolders become `dir:name`, later folders win by name, `description` from frontmatter
//! or else the first line without leading `#`, and arguments are appended when the body has
//! no placeholder. This module adds the remaining frontmatter (`model`, `agent`,
//! `argument-hint`, `allowed-tools`), `$1..$N` and `@file` / `` !`cmd` `` extraction.
//!
//! Expansion never reads files or runs commands. It returns the `@file` references and the
//! `` !`cmd` `` snippets; the engine reads files through its normal tools and runs snippets
//! through the shell permission path, only in trusted workspaces, then calls
//! [`Expanded::fill_shell`].

use std::fs;
use std::path::{Path, PathBuf};

use serde_json::Value;

use crate::frontmatter;
use crate::scan::{Diagnostic, Severity};

/// Which session kinds a built-in appears in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BuiltinModes {
    Both,
    CodeOnly,
}

/// One engine built-in (06 §9.1 table).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Builtin {
    pub name: &'static str,
    pub aliases: &'static [&'static str],
    pub description: &'static str,
    pub args_hint: Option<&'static str>,
    pub modes: BuiltinModes,
    /// `prompt` route (sent as a message); otherwise `client` (no model turn).
    pub prompt: bool,
    /// Runs as soon as it is picked, even during a turn.
    pub immediate: bool,
}

const fn b(
    name: &'static str,
    aliases: &'static [&'static str],
    description: &'static str,
    args_hint: Option<&'static str>,
    modes: BuiltinModes,
    immediate: bool,
) -> Builtin {
    Builtin {
        name,
        aliases,
        description,
        args_hint,
        modes,
        prompt: false,
        immediate,
    }
}

use BuiltinModes::{Both, CodeOnly};

/// Engine built-ins in menu order (06 §9.1, D58). User commands and skills with these
/// names (or aliases) are skipped with a diagnostic.
pub const BUILTINS: &[Builtin] = &[
    b(
        "help",
        &[],
        "Show commands and keyboard shortcuts",
        None,
        Both,
        false,
    ),
    b(
        "model",
        &[],
        "Switch the model",
        Some("[model]"),
        Both,
        false,
    ),
    b(
        "effort",
        &[],
        "Change reasoning effort",
        Some("[level]"),
        Both,
        false,
    ),
    b(
        "mcp",
        &["connectors"],
        "Manage connectors (MCP servers)",
        Some("[enable|disable <server>]"),
        Both,
        true,
    ),
    b("skills", &[], "Browse and manage skills", None, Both, true),
    b("memory", &[], "View and edit memory", None, Both, true),
    b(
        "permissions",
        &[],
        "Capabilities and permission rules",
        None,
        Both,
        true,
    ),
    b(
        "cost",
        &["usage"],
        "Show usage and cost for this session",
        None,
        Both,
        false,
    ),
    b(
        "context",
        &[],
        "Show context window usage",
        None,
        Both,
        false,
    ),
    b(
        "resume",
        &[],
        "Open another chat or session",
        None,
        Both,
        false,
    ),
    b(
        "export",
        &[],
        "Export this conversation",
        Some("[markdown|json]"),
        Both,
        false,
    ),
    b(
        "clear",
        &[],
        "Start a new chat or session",
        None,
        Both,
        false,
    ),
    b(
        "mode",
        &[],
        "Change the permission mode",
        Some("[default|accept-edits|plan|bypass]"),
        CodeOnly,
        false,
    ),
    b(
        "plan",
        &[],
        "Switch to plan mode",
        Some("[<description>]"),
        CodeOnly,
        false,
    ),
    b(
        "compact",
        &[],
        "Summarise the conversation to free context",
        Some("[focus]"),
        CodeOnly,
        false,
    ),
    b(
        "rewind",
        &[],
        "Rewind files or conversation to a checkpoint",
        None,
        CodeOnly,
        false,
    ),
    b(
        "worktree",
        &[],
        "Start a new session in a worktree",
        None,
        CodeOnly,
        false,
    ),
    b("agents", &[], "Pick an agent", None, CodeOnly, false),
    Builtin {
        name: "init",
        aliases: &[],
        description: "Create GENIE.md for this project",
        args_hint: None,
        modes: CodeOnly,
        prompt: true,
        immediate: false,
    },
];

/// The built-in named `name` (or one of its aliases).
pub fn builtin(name: &str) -> Option<&'static Builtin> {
    BUILTINS
        .iter()
        .find(|b| b.name == name || b.aliases.contains(&name))
}

pub fn is_builtin(name: &str) -> bool {
    builtin(name).is_some()
}

/// The `client` built-in a prompt would send to the model, if any (06 §9.1 guard, SA-48):
/// a first text part that is exactly `/name` or `/name args`, or a `command` part naming
/// one. Returns the built-in's canonical name.
pub fn client_builtin_in_prompt(
    first_text: Option<&str>,
    command: Option<&str>,
) -> Option<&'static str> {
    let name = match (command, first_text) {
        (Some(c), _) => c,
        (None, Some(t)) => {
            let line = t.trim_start().strip_prefix('/')?;
            line.split(char::is_whitespace).next().unwrap_or("")
        }
        (None, None) => return None,
    };
    builtin(name).filter(|b| !b.prompt).map(|b| b.name)
}

/// Genie's prompt for `/init` (a `prompt`-route built-in).
pub const INIT_PROMPT: &str = "Create a GENIE.md file at the root of this project. Read the \
repository first (README, build files, the main source folders) and write concise \
instructions for a coding agent: what the project is, how to build, test and lint it, \
the code layout, and conventions worth following. If GENIE.md, AGENTS.md or CLAUDE.md \
already exists, improve it instead of starting over. Keep it short and specific.";

/// Largest command file read (bytes).
pub const MAX_COMMAND_FILE: u64 = 256 * 1024;
const MAX_DEPTH: usize = 4;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CommandScope {
    User,
    Project,
}

#[derive(Debug, Clone, PartialEq)]
pub struct CommandFile {
    /// Slash name without `/`; subfolders joined with `:` (`git/pr.md` → `git:pr`).
    pub name: String,
    pub description: String,
    pub model: Option<String>,
    pub agent: Option<String>,
    pub argument_hint: Option<String>,
    /// Tool rules the command expects (metadata; it never widens permissions).
    pub allowed_tools: Vec<String>,
    pub body: String,
    pub path: PathBuf,
    pub scope: CommandScope,
}

/// Command folders for a workspace; project folders only when trusted (06 §14).
pub fn command_dirs(
    genie_home: &Path,
    workspace: Option<&Path>,
    trusted: bool,
) -> Vec<(PathBuf, CommandScope)> {
    let mut v = vec![(genie_home.join("commands"), CommandScope::User)];
    if let (Some(ws), true) = (workspace, trusted) {
        v.push((ws.join(".genie/commands"), CommandScope::Project));
    }
    v
}

/// Loads commands from `dirs` (later dirs win by name), sorted by name.
pub fn load_commands(dirs: &[(PathBuf, CommandScope)]) -> (Vec<CommandFile>, Vec<Diagnostic>) {
    let mut out: Vec<CommandFile> = vec![];
    let mut diags = vec![];
    for (dir, scope) in dirs {
        let mut stack = vec![(dir.clone(), String::new(), 0usize)];
        while let Some((d, prefix, depth)) = stack.pop() {
            let Ok(rd) = fs::read_dir(&d) else {
                continue;
            };
            for e in rd.flatten() {
                let p = e.path();
                let fname = e.file_name().to_string_lossy().into_owned();
                if fname.starts_with('.') {
                    continue;
                }
                let stem = p
                    .file_stem()
                    .map(|s| s.to_string_lossy().into_owned())
                    .unwrap_or_default();
                if p.is_dir() {
                    if depth + 1 < MAX_DEPTH {
                        stack.push((p, format!("{prefix}{stem}:"), depth + 1));
                    }
                    continue;
                }
                if p.extension().is_none_or(|x| x != "md") {
                    continue;
                }
                let name = format!("{prefix}{stem}");
                match read_command(&p, name.clone(), *scope) {
                    Ok(c) if is_builtin(&c.name) => diags.push(Diagnostic {
                        path: p,
                        severity: Severity::Warning,
                        message: format!("`/{name}` is a built-in command; this file is ignored"),
                    }),
                    Ok(c) => {
                        out.retain(|x| x.name != c.name);
                        out.push(c);
                    }
                    Err(message) => diags.push(Diagnostic {
                        path: p,
                        severity: Severity::Error,
                        message,
                    }),
                }
            }
        }
    }
    out.sort_by(|a, b| a.name.cmp(&b.name));
    (out, diags)
}

fn read_command(p: &Path, name: String, scope: CommandScope) -> Result<CommandFile, String> {
    let len = fs::metadata(p).map(|m| m.len()).unwrap_or(0);
    if len > MAX_COMMAND_FILE {
        return Err(format!(
            "command file is larger than {} KiB",
            MAX_COMMAND_FILE / 1024
        ));
    }
    let text = fs::read_to_string(p).map_err(|e| e.to_string())?;
    parse_command(&text, name, p.to_path_buf(), scope)
}

/// Parses a command file's text.
pub fn parse_command(
    text: &str,
    name: String,
    path: PathBuf,
    scope: CommandScope,
) -> Result<CommandFile, String> {
    let (fm, body) = match frontmatter::split(text).map_err(|e| e.to_string())? {
        (Some(fm), body) => (
            frontmatter::parse(fm).map_err(|e| e.to_string())?,
            body.trim_start_matches(['\r', '\n']).to_string(),
        ),
        // No frontmatter: the whole text is the body (as in the CLI).
        (None, _) => (Default::default(), text.to_string()),
    };
    let s = |k: &str| -> Result<Option<String>, String> {
        match fm.get(k) {
            None | Some(Value::Null) => Ok(None),
            Some(Value::String(s)) => Ok(Some(s.trim().to_string())),
            Some(Value::Number(n)) => Ok(Some(n.to_string())),
            Some(Value::Bool(b)) => Ok(Some(b.to_string())),
            Some(_) => Err(format!("`{k}` must be a string")),
        }
    };
    let description = s("description")?.unwrap_or_else(|| {
        body.lines()
            .next()
            .unwrap_or_default()
            .trim_start_matches('#')
            .trim()
            .to_string()
    });
    Ok(CommandFile {
        name,
        description,
        model: s("model")?,
        agent: s("agent")?,
        argument_hint: s("argument-hint")?,
        allowed_tools: frontmatter::string_list(fm.get("allowed-tools"))
            .map_err(|e| format!("`allowed-tools`: {e}"))?,
        body,
        path,
        scope,
    })
}

/// A `` !`cmd` `` snippet found in an expanded command.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShellSnippet {
    pub command: String,
}

/// The result of expanding a command with arguments.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Expanded {
    /// The prompt, with `` !`cmd` `` snippets still in place.
    pub text: String,
    /// `@path` references, in order, deduplicated (the engine attaches them as `file_ref`s).
    pub file_refs: Vec<String>,
    /// `` !`cmd` `` snippets in order of appearance.
    pub shell: Vec<ShellSnippet>,
}

impl Expanded {
    /// Replaces each snippet, in order, with the matching output (missing outputs → empty).
    pub fn fill_shell(&self, outputs: &[String]) -> String {
        let mut out = String::new();
        let mut rest = self.text.as_str();
        let mut k = 0;
        while let Some((start, end)) = next_snippet(rest) {
            out.push_str(&rest[..start]);
            out.push_str(outputs.get(k).map(String::as_str).unwrap_or(""));
            k += 1;
            rest = &rest[end..];
        }
        out.push_str(rest);
        out
    }

    /// Used when snippets may not run (untrusted workspace, denied): each is replaced by a note.
    pub fn without_shell(&self, note: &str) -> String {
        let notes = vec![note.to_string(); self.shell.len()];
        self.fill_shell(&notes)
    }
}

/// `(start, end)` of the next `` !`…` `` in `s` (end is past the closing backtick).
fn next_snippet(s: &str) -> Option<(usize, usize)> {
    let start = s.find("!`")?;
    let close = s[start + 2..].find('`')?;
    Some((start, start + 2 + close + 1))
}

/// Shell-like split: whitespace separates, `"…"` and `'…'` group, `\` escapes in `"…"`.
pub fn split_args(args: &str) -> Vec<String> {
    let mut out = vec![];
    let mut cur = String::new();
    let mut has = false;
    let mut chars = args.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '"' => {
                has = true;
                while let Some(c) = chars.next() {
                    match c {
                        '"' => break,
                        '\\' => {
                            if let Some(n) = chars.next() {
                                cur.push(n);
                            }
                        }
                        c => cur.push(c),
                    }
                }
            }
            '\'' => {
                has = true;
                for c in chars.by_ref() {
                    if c == '\'' {
                        break;
                    }
                    cur.push(c);
                }
            }
            c if c.is_whitespace() => {
                if has {
                    out.push(std::mem::take(&mut cur));
                    has = false;
                }
            }
            c => {
                has = true;
                cur.push(c);
            }
        }
    }
    if has {
        out.push(cur);
    }
    out
}

impl CommandFile {
    /// Substitutes `$ARGUMENTS` (the raw argument string) and `$1..$N` (the highest
    /// placeholder used takes the rest of the arguments). Without any placeholder the
    /// arguments are appended after a blank line (CLI behaviour).
    pub fn expand(&self, args: &str) -> Expanded {
        let args = args.trim();
        let argv = split_args(args);
        let max_n = positional_max(&self.body);
        let has_all = self.body.contains("$ARGUMENTS");
        let text = if !has_all && max_n == 0 {
            if args.is_empty() {
                self.body.clone()
            } else {
                format!("{}\n\n{args}", self.body.trim_end())
            }
        } else {
            substitute(&self.body, args, &argv, max_n)
        };
        let shell = {
            let mut v = vec![];
            let mut rest = text.as_str();
            while let Some((s, e)) = next_snippet(rest) {
                v.push(ShellSnippet {
                    command: rest[s + 2..e - 1].to_string(),
                });
                rest = &rest[e..];
            }
            v
        };
        Expanded {
            file_refs: file_refs(&text),
            shell,
            text,
        }
    }
}

fn positional_max(body: &str) -> usize {
    let b = body.as_bytes();
    let mut max = 0;
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'$' {
            let mut j = i + 1;
            while j < b.len() && b[j].is_ascii_digit() {
                j += 1;
            }
            if j > i + 1
                && let Ok(n) = body[i + 1..j].parse::<usize>()
            {
                max = max.max(n);
            }
            i = j.max(i + 1);
        } else {
            i += 1;
        }
    }
    max
}

fn substitute(body: &str, all: &str, argv: &[String], max_n: usize) -> String {
    let mut out = String::with_capacity(body.len());
    let mut rest = body;
    while let Some(pos) = rest.find('$') {
        out.push_str(&rest[..pos]);
        let after = &rest[pos + 1..];
        if let Some(r) = after.strip_prefix("ARGUMENTS") {
            out.push_str(all);
            rest = r;
            continue;
        }
        let digits = after.bytes().take_while(u8::is_ascii_digit).count();
        match after[..digits].parse::<usize>() {
            Ok(n) if n >= 1 => {
                let v = if n == max_n {
                    argv.get(n - 1..).map(|r| r.join(" ")).unwrap_or_default()
                } else {
                    argv.get(n - 1).cloned().unwrap_or_default()
                };
                out.push_str(&v);
                rest = &after[digits..];
            }
            _ => {
                out.push('$');
                rest = after;
            }
        }
    }
    out.push_str(rest);
    out
}

/// `@path` tokens at the start of a word. `@server:resource` (MCP, D30), e-mail addresses
/// and URLs are not file references.
fn file_refs(text: &str) -> Vec<String> {
    let mut out: Vec<String> = vec![];
    let mut prev: Option<char> = None;
    for (i, c) in text.char_indices() {
        if c == '@' && prev.is_none_or(|p| p.is_whitespace() || p == '(') {
            let tok: String = text[i + 1..]
                .chars()
                .take_while(|c| !c.is_whitespace() && *c != '`')
                .collect();
            let tok = tok.trim_end_matches(['.', ',', ';', ')', '!', '?', '"', '\'']);
            let is_resource = tok
                .split_once(':')
                .is_some_and(|(a, _)| a.len() > 1 && !a.contains('/'));
            if !tok.is_empty()
                && !is_resource
                && !tok.contains("://")
                && !out.iter().any(|x| x == tok)
            {
                out.push(tok.to_string());
            }
        }
        prev = Some(c);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cmd(body: &str) -> CommandFile {
        parse_command(body, "t".into(), PathBuf::from("t.md"), CommandScope::User).unwrap()
    }

    #[test]
    fn frontmatter_fields() {
        let c = cmd(
            "---\ndescription: Review code\nmodel: fast\nagent: reviewer\nargument-hint: \"[file]\"\nallowed-tools: Read, Bash(git diff:*)\n---\nReview $ARGUMENTS",
        );
        assert_eq!(c.description, "Review code");
        assert_eq!(c.model.as_deref(), Some("fast"));
        assert_eq!(c.agent.as_deref(), Some("reviewer"));
        assert_eq!(c.argument_hint.as_deref(), Some("[file]"));
        assert_eq!(c.allowed_tools, vec!["Read", "Bash(git diff:*)"]);
        assert_eq!(c.body, "Review $ARGUMENTS");
        let c = cmd("# Open a PR\nDo it");
        assert_eq!(c.description, "Open a PR");
        assert_eq!(c.body, "# Open a PR\nDo it");
        assert!(
            parse_command(
                "---\nmodel: [a]\n---\n",
                "x".into(),
                PathBuf::new(),
                CommandScope::User
            )
            .is_err()
        );
    }

    #[test]
    fn arguments() {
        assert_eq!(
            cmd("Review $ARGUMENTS now").expand(" a b ").text,
            "Review a b now"
        );
        assert_eq!(cmd("Plain").expand("").text, "Plain");
        assert_eq!(cmd("Plain\n").expand("x y").text, "Plain\n\nx y");
        let c = cmd("from $1 to $2; all: $ARGUMENTS; cost $ 5 $0");
        assert_eq!(
            c.expand("\"New York\" Paris Rome").text,
            "from New York to Paris Rome; all: \"New York\" Paris Rome; cost $ 5 $0"
        );
        assert_eq!(cmd("$1 and $3").expand("a").text, "a and ");
        assert_eq!(
            split_args(r#"a "b \" c" 'd e' """#),
            vec!["a", "b \" c", "d e", ""]
        );
    }

    #[test]
    fn refs_and_shell() {
        let e = cmd("Look at @src/main.rs, @docs/a.md and @src/main.rs. Mail a@b.com, @github:repo://x @https://x.y\nStatus: !`git status` and !`git diff $1`").expand("HEAD");
        assert_eq!(e.file_refs, vec!["src/main.rs", "docs/a.md"]);
        assert_eq!(
            e.shell,
            vec![
                ShellSnippet {
                    command: "git status".into()
                },
                ShellSnippet {
                    command: "git diff HEAD".into()
                }
            ]
        );
        assert!(
            e.fill_shell(&["clean".into(), "diff".into()])
                .ends_with("Status: clean and diff")
        );
        assert!(
            e.without_shell("[not run]")
                .ends_with("Status: [not run] and [not run]")
        );
    }

    #[test]
    fn builtins() {
        assert!(is_builtin("compact") && is_builtin("help") && !is_builtin("review"));
        assert_eq!(builtin("connectors").map(|b| b.name), Some("mcp"));
        assert_eq!(builtin("usage").map(|b| b.name), Some("cost"));
        assert!(builtin("init").is_some_and(|b| b.prompt));
        assert_eq!(
            client_builtin_in_prompt(Some("/compact now"), None),
            Some("compact")
        );
        assert_eq!(client_builtin_in_prompt(Some("/usage"), None), Some("cost"));
        assert_eq!(client_builtin_in_prompt(Some("/init"), None), None);
        assert_eq!(
            client_builtin_in_prompt(Some("/usr/bin is here"), None),
            None
        );
        assert_eq!(client_builtin_in_prompt(Some("hi /model"), None), None);
        assert_eq!(client_builtin_in_prompt(None, Some("mcp")), Some("mcp"));
        assert_eq!(client_builtin_in_prompt(None, Some("review")), None);
    }
}
