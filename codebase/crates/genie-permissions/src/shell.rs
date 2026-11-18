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
    // `cd` only changes the directory of the hidden shell; its target is checked against
    // the workspace like every other path operand (see [`path_operands`]).
    ("cd", &[]),
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
    ("egrep", &[]),
    ("fgrep", &[]),
    ("rg", &["--pre"]),
    ("ps", &[]),
    // Port and process lookups, e.g. before starting a dev server (ticket 45).
    ("lsof", &[]),
    ("netstat", &[]),
    ("ss", &["-K", "--kill"]),
    ("pgrep", &[]),
    ("printenv", &[]),
    ("nl", &[]),
    ("jq", &[]),
    ("id", &[]),
    ("true", &[]),
    ("false", &[]),
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
    ("git stash list", &[]),
];

const POWERSHELL_READ_ONLY: &[(&str, &[&str])] = &[
    ("set-location", &[]),
    ("sl", &[]),
    ("cd", &[]),
    ("get-process", &[]),
    ("gps", &[]),
    ("get-nettcpconnection", &[]),
    ("netstat", &[]),
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

/// Redirections that only discard or merge output (`2>&1`, `2>/dev/null`, `*>$null`). They
/// don't make a read-only command write anything, so they are dropped before the check.
const SAFE_REDIRECTS: &[&str] = &[
    "2>&1",
    "1>&2",
    ">&2",
    ">/dev/null",
    "1>/dev/null",
    "2>/dev/null",
    "&>/dev/null",
    ">$null",
    "2>$null",
    "*>$null",
];
const NULL_TARGET_OPS: &[&str] = &[">", "1>", "2>", "&>", "*>"];

/// `words` without the harmless redirections.
fn drop_safe_redirects<S: AsRef<str>>(words: &[S]) -> Vec<&str> {
    let mut out: Vec<&str> = Vec::with_capacity(words.len());
    let mut i = 0;
    while i < words.len() {
        let w = words[i].as_ref();
        if SAFE_REDIRECTS.iter().any(|r| r.eq_ignore_ascii_case(w)) {
            i += 1;
            continue;
        }
        if NULL_TARGET_OPS.contains(&w)
            && words.get(i + 1).is_some_and(|t| {
                let t = t.as_ref();
                t == "/dev/null" || t.eq_ignore_ascii_case("$null")
            })
        {
            i += 2;
            continue;
        }
        out.push(w);
        i += 1;
    }
    out
}

/// `cmd` (normalised) without its harmless redirections.
fn strip_safe_redirects(cmd: &str) -> String {
    let words: Vec<&str> = cmd.split(' ').collect();
    drop_safe_redirects(&words).join(" ")
}

/// Whether `cmd` has a redirection/substitution/operator character outside quotes.
/// Single-quoted text is literal; inside double quotes only `$` and backticks substitute.
fn has_unsafe_unquoted(dialect: ShellDialect, cmd: &str) -> bool {
    let escape = match dialect {
        ShellDialect::Posix => '\\',
        ShellDialect::PowerShell => '`',
    };
    let mut chars = cmd.chars();
    while let Some(c) = chars.next() {
        match c {
            '\'' => {
                if !chars.by_ref().any(|c| c == '\'') {
                    return true;
                }
            }
            '"' => loop {
                match chars.next() {
                    None => return true,
                    Some('"') => break,
                    Some('$' | '`') => return true,
                    Some(c) if c == escape => {
                        chars.next();
                    }
                    Some(_) => {}
                }
            },
            c if c == escape && dialect == ShellDialect::Posix => {
                chars.next();
            }
            c if UNSAFE_CHARS.contains(&c) => return true,
            _ => {}
        }
    }
    false
}

impl ReadOnlyClassifier for BuiltinReadOnly {
    fn is_read_only(&self, dialect: ShellDialect, command: &str) -> bool {
        let cmd = strip_safe_redirects(&normalize_command(command));
        if cmd.is_empty() || has_unsafe_unquoted(dialect, &cmd) {
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

/// Split a (scanner-produced) sub-command into words, removing quotes and escapes. Good
/// enough for the operand check: the scanner has already rejected anything it can't parse.
pub fn shell_words(dialect: ShellDialect, command: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut has = false;
    let mut chars = command.chars().peekable();
    let escape = match dialect {
        ShellDialect::Posix => '\\',
        ShellDialect::PowerShell => '`',
    };
    while let Some(c) = chars.next() {
        match c {
            c if c.is_whitespace() => {
                if has {
                    out.push(std::mem::take(&mut cur));
                    has = false;
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
            '"' => {
                has = true;
                while let Some(c) = chars.next() {
                    match c {
                        '"' => break,
                        c if c == escape => {
                            if let Some(n) = chars.next() {
                                cur.push(n);
                            }
                        }
                        c => cur.push(c),
                    }
                }
            }
            c if c == escape => {
                has = true;
                if let Some(n) = chars.next() {
                    cur.push(n);
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

/// Read-only programs whose operands are files or directories, with the flags that take
/// a separate value (that value is not a path), and whether the first operand is a
/// pattern rather than a path (`grep PATTERN FILE…`).
const POSIX_PATH_PROGRAMS: &[(&str, &[&str], bool)] = &[
    ("cd", &[], false),
    ("ls", &["-I", "--ignore", "-w", "--width"], false),
    ("cat", &[], false),
    ("head", &["-n", "-c"], false),
    ("tail", &["-n", "-c"], false),
    ("wc", &[], false),
    ("file", &["-m"], false),
    ("stat", &["-f", "-c", "--format", "-t"], false),
    ("du", &["-d", "--max-depth", "-B"], false),
    ("df", &["-t", "-x"], false),
    ("tree", &["-L", "-I", "-P"], false),
    ("find", &[], false),
    ("sort", &["-t", "-k", "-S", "-T"], false),
    ("uniq", &["-f", "-s", "-w"], false),
    ("cut", &["-d", "-f", "-c", "-b"], false),
    ("diff", &[], false),
    ("realpath", &[], false),
    ("nl", &["-s", "-w", "-b"], false),
    (
        "grep",
        &["-e", "-m", "-A", "-B", "-C", "--include", "--exclude"],
        true,
    ),
    (
        "egrep",
        &["-e", "-m", "-A", "-B", "-C", "--include", "--exclude"],
        true,
    ),
    (
        "fgrep",
        &["-e", "-m", "-A", "-B", "-C", "--include", "--exclude"],
        true,
    ),
    (
        "rg",
        &["-e", "-g", "--glob", "-t", "--type", "-m", "-A", "-B", "-C"],
        true,
    ),
    ("jq", &["--arg", "--argjson"], true),
];

const PS_PATH_PROGRAMS: &[(&str, bool)] = &[
    ("set-location", false),
    ("sl", false),
    ("cd", false),
    ("get-childitem", false),
    ("gci", false),
    ("dir", false),
    ("ls", false),
    ("get-content", false),
    ("gc", false),
    ("cat", false),
    ("get-item", false),
    ("test-path", false),
    ("resolve-path", false),
    ("select-string", true),
];
/// PowerShell parameters whose value is a path.
const PS_PATH_PARAMS: &[&str] = &["-path", "-literalpath", "-lp", "-pspath"];
/// PowerShell switches (no value).
const PS_SWITCHES: &[&str] = &[
    "-recurse",
    "-force",
    "-name",
    "-file",
    "-directory",
    "-hidden",
    "-raw",
    "-simplematch",
    "-casesensitive",
    "-quiet",
    "-list",
    "-notmatch",
    "-allmatches",
    "-wait",
];

/// The file/directory operands of a read-only command, unquoted (`~` not expanded). The
/// evaluator checks them against the workspace so that `cat ~/.ssh/id_rsa` or `cd /etc`
/// asks instead of running as "read-only". Empty for commands without path operands
/// (`echo`, `git status`, `ps`). `cd` with no operand stands for the home directory.
pub fn path_operands(dialect: ShellDialect, command: &str) -> Vec<String> {
    let all = shell_words(dialect, command);
    let words: Vec<String> = drop_safe_redirects(&all)
        .into_iter()
        .map(str::to_string)
        .collect();
    let Some(first) = words.first() else {
        return Vec::new();
    };
    let mut out = Vec::new();
    match dialect {
        ShellDialect::Posix => {
            let Some((prog, valued, pattern_first)) =
                POSIX_PATH_PROGRAMS.iter().find(|(p, _, _)| p == first)
            else {
                return Vec::new();
            };
            let mut skip_pattern = *pattern_first;
            let mut i = 1;
            let mut end_of_flags = false;
            while i < words.len() {
                let w = &words[i];
                i += 1;
                if !end_of_flags && w == "--" {
                    end_of_flags = true;
                    continue;
                }
                if !end_of_flags && w.starts_with('-') && w.len() > 1 {
                    if let Some((flag, value)) = w.split_once('=') {
                        if !valued.contains(&flag) {
                            out.push(value.to_string());
                        }
                    } else if valued.contains(&w.as_str()) {
                        if matches!(w.as_str(), "-e") {
                            skip_pattern = false;
                        }
                        i += 1;
                    }
                    continue;
                }
                if skip_pattern {
                    skip_pattern = false;
                    continue;
                }
                out.push(w.clone());
            }
            if *prog == "cd" && out.is_empty() {
                out.push("~".into());
            }
        }
        ShellDialect::PowerShell => {
            let lower = first.to_ascii_lowercase();
            let Some((prog, pattern_first)) = PS_PATH_PROGRAMS.iter().find(|(p, _)| *p == lower)
            else {
                return Vec::new();
            };
            let mut skip_pattern = *pattern_first;
            let mut i = 1;
            while i < words.len() {
                let w = &words[i];
                i += 1;
                if w.starts_with('-') && w.len() > 1 {
                    let lw = w.to_ascii_lowercase();
                    if let Some((p, _)) = lw.split_once(':') {
                        if PS_PATH_PARAMS.contains(&p) {
                            out.push(w[p.len() + 1..].to_string());
                        }
                    } else if PS_PATH_PARAMS.contains(&lw.as_str()) {
                        if let Some(v) = words.get(i) {
                            out.push(v.clone());
                        }
                        i += 1;
                    } else if lw == "-pattern" {
                        skip_pattern = false;
                        i += 1;
                    } else if !PS_SWITCHES.contains(&lw.as_str()) {
                        i += 1;
                    }
                    continue;
                }
                if skip_pattern {
                    skip_pattern = false;
                    continue;
                }
                out.push(w.clone());
            }
            if matches!(*prog, "set-location" | "sl" | "cd") && out.is_empty() {
                out.push("~".into());
            }
        }
    }
    out
}

/// Whether `command` only changes the shell's directory (`cd x`, `Set-Location x`).
pub fn is_cd(dialect: ShellDialect, command: &str) -> bool {
    let words = shell_words(dialect, command);
    match (dialect, words.first().map(String::as_str)) {
        (ShellDialect::Posix, Some("cd")) => true,
        (ShellDialect::PowerShell, Some(w)) => {
            matches!(
                w.to_ascii_lowercase().as_str(),
                "cd" | "sl" | "set-location"
            )
        }
        _ => false,
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
            (ShellDialect::Posix, "curl https://x", false),
            (
                ShellDialect::Posix,
                "lsof -nP -iTCP:1420 -sTCP:LISTEN",
                true,
            ),
            (ShellDialect::Posix, "netstat -an", true),
            (ShellDialect::Posix, "ss -ltnp", true),
            (ShellDialect::Posix, "ss -K dst 1.2.3.4", false),
            (ShellDialect::Posix, "pgrep -fl vite", true),
            (
                ShellDialect::PowerShell,
                "Get-NetTCPConnection -LocalPort 3000",
                true,
            ),
            (ShellDialect::Posix, "sort -o out.txt in.txt", false),
            (ShellDialect::Posix, "git diff --output=x", false),
            (ShellDialect::PowerShell, "Get-ChildItem -Recurse", true),
            (ShellDialect::PowerShell, "get-content a.txt", true),
            (ShellDialect::PowerShell, "Remove-Item x", false),
            (ShellDialect::Posix, "", false),
            (ShellDialect::Posix, "cd src", true),
            (ShellDialect::Posix, "ps aux", true),
            (ShellDialect::Posix, r#"egrep -i "vite|tauri""#, true),
            (ShellDialect::Posix, "grep 'a;b' x", true),
            (ShellDialect::Posix, r#"grep "$(rm -rf /)" x"#, false),
            (ShellDialect::Posix, "grep \\| x", true),
            (ShellDialect::Posix, "ls 2>/dev/null", true),
            (ShellDialect::Posix, "ls 2> /dev/null", true),
            (ShellDialect::Posix, "ls 2>&1", true),
            (ShellDialect::Posix, "ls > out", false),
            (ShellDialect::Posix, "ls 2>err.log", false),
            (ShellDialect::Posix, "grep 'unterminated", false),
            (ShellDialect::PowerShell, "Get-ChildItem 2>$null", true),
            (ShellDialect::PowerShell, "Set-Location src", true),
        ];
        for (d, cmd, want) in cases {
            assert_eq!(c.is_read_only(d, cmd), want, "{cmd:?}");
        }
    }

    #[test]
    fn operands() {
        let p = |d, c| path_operands(d, c);
        assert_eq!(p(ShellDialect::Posix, r#"cd "/a b/c""#), ["/a b/c"]);
        assert_eq!(p(ShellDialect::Posix, "cd"), ["~"]);
        assert_eq!(p(ShellDialect::Posix, "ls -la /etc 2>/dev/null"), ["/etc"]);
        assert_eq!(p(ShellDialect::Posix, "cat a b"), ["a", "b"]);
        assert_eq!(p(ShellDialect::Posix, "grep -rn '/api' src"), ["src"]);
        assert_eq!(p(ShellDialect::Posix, "grep -e x -e y f"), ["f"]);
        assert_eq!(p(ShellDialect::Posix, "cut -d / -f1 f"), ["f"]);
        assert_eq!(p(ShellDialect::Posix, "head -n 5 -- -f"), ["-f"]);
        assert_eq!(p(ShellDialect::Posix, "sort --output=/x y"), ["/x", "y"]);
        assert!(p(ShellDialect::Posix, "echo /etc/passwd").is_empty());
        assert!(p(ShellDialect::Posix, "git diff /x").is_empty());
        assert_eq!(
            p(
                ShellDialect::PowerShell,
                r"Get-Content -Path C:\x\y.txt -Raw"
            ),
            [r"C:\x\y.txt"]
        );
        assert_eq!(
            p(ShellDialect::PowerShell, r"gci -Recurse C:\x -Filter *.rs"),
            [r"C:\x"]
        );
        assert_eq!(
            p(ShellDialect::PowerShell, "Select-String foo a.txt"),
            ["a.txt"]
        );
        assert!(is_cd(ShellDialect::Posix, "cd x"));
        assert!(is_cd(ShellDialect::PowerShell, "Set-Location x"));
        assert!(!is_cd(ShellDialect::Posix, "cdk deploy"));
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
