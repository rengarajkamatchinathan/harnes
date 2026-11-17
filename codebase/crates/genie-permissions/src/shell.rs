//! The rule side of the shell scanner (06 §13.3): the built-in read-only command list and
//! the arity table that turns a command into an "always" prefix.
//!
//! The scanner that splits compound commands lives with the shell tool; the evaluator
//! receives the already-split sub-commands (`None` = unparseable → ask).

use crate::rule::normalize_command;

/// Which rule family a shell command is checked against.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShellDialect {
    /// bash / zsh / Git Bash: `Bash(...)` rules.
    Posix,
    /// pwsh / Windows PowerShell: `PowerShell(...)` rules, case-insensitive.
    PowerShell,
}

impl ShellDialect {
    pub fn rule_tool(self) -> &'static str {
        match self {
            ShellDialect::Posix => "Bash",
            ShellDialect::PowerShell => "PowerShell",
        }
    }
}

/// Classifies sub-commands as read-only. Injectable so the engine can extend the list
/// (e.g. from settings) without touching the evaluator.
pub trait ReadOnlyClassifier: Send + Sync {
    fn is_read_only(&self, dialect: ShellDialect, command: &str) -> bool;
}

/// The built-in list (06 §13.3): `ls`, `cat`, `git status`, `git diff`, `Get-ChildItem`, …
#[derive(Debug, Default, Clone, Copy)]
pub struct BuiltinReadOnly;

/// Whole-command prefixes that never mutate anything, with arguments that disqualify them.
const POSIX_READ_ONLY: &[(&str, &[&str])] = &[
    ("ls", &[]),
    ("cat", &[]),
    ("head", &[]),
    ("tail", &["-f", "--follow"]),
    ("wc", &[]),
    ("pwd", &[]),
    ("echo", &[]),
    ("which", &[]),
    ("type", &[]),
    ("file", &[]),
    ("stat", &[]),
    ("du", &[]),
    ("df", &[]),
    ("tree", &[]),
    ("grep", &[]),
    ("rg", &["--pre"]),
    (
        "find",
        &[
            "-exec", "-execdir", "-delete", "-ok", "-okdir", "-fprint", "-fprintf", "-fls",
        ],
    ),
    ("sort", &["-o", "--output"]),
    ("uniq", &[]),
    ("cut", &[]),
    ("diff", &[]),
    ("date", &["-s", "--set"]),
    ("whoami", &[]),
    ("uname", &[]),
    ("basename", &[]),
    ("dirname", &[]),
    ("realpath", &[]),
    ("git status", &[]),
    ("git diff", &["--output"]),
    ("git log", &["--output"]),
    ("git show", &["--output"]),
    ("git blame", &[]),
    ("git rev-parse", &[]),
    ("git ls-files", &[]),
    ("git remote -v", &[]),
    ("git branch --show-current", &[]),
    ("git branch --list", &[]),
];

const POWERSHELL_READ_ONLY: &[(&str, &[&str])] = &[
    ("get-childitem", &[]),
    ("gci", &[]),
    ("dir", &[]),
    ("ls", &[]),
    ("get-content", &[]),
    ("gc", &[]),
    ("cat", &[]),
    ("get-location", &[]),
    ("pwd", &[]),
    ("get-item", &[]),
    ("select-string", &[]),
    ("test-path", &[]),
    ("resolve-path", &[]),
    ("get-command", &[]),
    ("measure-object", &[]),
    ("write-output", &[]),
    ("git status", &[]),
    ("git diff", &["--output"]),
    ("git log", &["--output"]),
    ("git show", &["--output"]),
];

/// Characters that mean the scanner left a redirection or substitution in the part; such a
/// command is never read-only.
const UNSAFE_CHARS: &[char] = &['>', '<', '`', '$', '|', ';', '&', '(', ')'];

impl ReadOnlyClassifier for BuiltinReadOnly {
    fn is_read_only(&self, dialect: ShellDialect, command: &str) -> bool {
        let cmd = normalize_command(command);
        if cmd.is_empty() || cmd.contains(UNSAFE_CHARS) {
            return false;
        }
        let (table, cmd) = match dialect {
            ShellDialect::Posix => (POSIX_READ_ONLY, cmd),
            ShellDialect::PowerShell => (POWERSHELL_READ_ONLY, cmd.to_lowercase()),
        };
        let words: Vec<&str> = cmd.split(' ').collect();
        table.iter().any(|(prefix, banned)| {
            let pw: Vec<&str> = prefix.split(' ').collect();
            words.len() >= pw.len()
                && words[..pw.len()] == pw[..]
                && !words[pw.len()..].iter().any(|w| {
                    banned
                        .iter()
                        .any(|b| w == b || w.starts_with(&format!("{b}=")))
                })
        })
    }
}

/// Number of leading words that name the command (`git commit`, `npm run test`), per
/// program; default 1. Port of the idea behind OpenCode's arity table.
const ARITY: &[(&str, usize)] = &[
    ("git", 2),
    ("npm", 2),
    ("npm run", 3),
    ("pnpm", 2),
    ("pnpm run", 3),
    ("yarn", 2),
    ("yarn run", 3),
    ("bun", 2),
    ("bun run", 3),
    ("npx", 2),
    ("cargo", 2),
    ("go", 2),
    ("docker", 2),
    ("docker compose", 3),
    ("kubectl", 2),
    ("helm", 2),
    ("terraform", 2),
    ("pip", 2),
    ("pip3", 2),
    ("uv", 2),
    ("poetry", 2),
    ("python -m", 3),
    ("python3 -m", 3),
    ("dotnet", 2),
    ("mvn", 2),
    ("gradle", 2),
    ("make", 2),
    ("brew", 2),
    ("apt", 2),
    ("gh", 3),
    ("az", 3),
    ("aws", 3),
    ("gcloud", 3),
];

/// The prefix an "always" saves for `command` (06 §13.3): `git commit -m x` →
/// `git commit`, `npm run test -- --watch` → `npm run test`. Flags end the prefix.
pub fn arity_prefix(command: &str) -> String {
    let cmd = normalize_command(command);
    let words: Vec<&str> = cmd.split(' ').filter(|w| !w.is_empty()).collect();
    // The longest matching table key wins.
    let mut best: Option<(usize, usize)> = None;
    for (prefix, n) in ARITY {
        let pw: Vec<&str> = prefix.split(' ').collect();
        if words.len() >= pw.len()
            && words[..pw.len()] == pw[..]
            && best.is_none_or(|(len, _)| pw.len() > len)
        {
            best = Some((pw.len(), *n));
        }
    }
    let (key_len, arity) = best.unwrap_or((0, 1));
    let mut out = Vec::new();
    for (i, w) in words.iter().take(arity).enumerate() {
        // Flags end the prefix, except inside the table key itself (`python -m`).
        if i >= key_len.max(1) && w.starts_with('-') {
            break;
        }
        out.push(*w);
    }
    out.join(" ")
}

/// The suggested rule for an "always" on `command`.
pub fn suggest_rule(dialect: ShellDialect, command: &str) -> Option<String> {
    let prefix = arity_prefix(command);
    if prefix.is_empty() {
        return None;
    }
    Some(format!("{}({prefix}:*)", dialect.rule_tool()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn read_only_table() {
        let c = BuiltinReadOnly;
        let cases = [
            (ShellDialect::Posix, "ls -la", true),
            (ShellDialect::Posix, "git status", true),
            (ShellDialect::Posix, "git  diff HEAD~1", true),
            (ShellDialect::Posix, "git commit -m x", false),
            (ShellDialect::Posix, "git branch -D main", false),
            (ShellDialect::Posix, "find . -name x", true),
            (ShellDialect::Posix, "find . -delete", false),
            (ShellDialect::Posix, "cat a > b", false),
            (ShellDialect::Posix, "echo $(rm -rf /)", false),
            (ShellDialect::Posix, "rm -rf x", false),
            (ShellDialect::Posix, "lsof", false),
            (ShellDialect::Posix, "sort -o out.txt in.txt", false),
            (ShellDialect::Posix, "git diff --output=x", false),
            (ShellDialect::PowerShell, "Get-ChildItem -Recurse", true),
            (ShellDialect::PowerShell, "get-content a.txt", true),
            (ShellDialect::PowerShell, "Remove-Item x", false),
            (ShellDialect::Posix, "", false),
        ];
        for (d, cmd, want) in cases {
            assert_eq!(c.is_read_only(d, cmd), want, "{cmd:?}");
        }
    }

    #[test]
    fn arity_table() {
        let cases = [
            ("git commit -m 'x'", "git commit"),
            ("git", "git"),
            ("npm run test -- --watch", "npm run test"),
            ("npm install left-pad", "npm install"),
            ("docker compose up -d", "docker compose up"),
            ("rm -rf build", "rm"),
            ("cargo test -p x", "cargo test"),
            ("python -m pytest -q", "python -m pytest"),
            ("gh pr create --fill", "gh pr create"),
            ("ls", "ls"),
        ];
        for (cmd, want) in cases {
            assert_eq!(arity_prefix(cmd), want, "{cmd}");
        }
        assert_eq!(
            suggest_rule(ShellDialect::Posix, "git commit -m x").as_deref(),
            Some("Bash(git commit:*)")
        );
        assert_eq!(
            suggest_rule(ShellDialect::PowerShell, "Get-Process x").as_deref(),
            Some("PowerShell(Get-Process:*)")
        );
    }
}
