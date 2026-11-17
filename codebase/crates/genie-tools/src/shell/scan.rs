//! The command scanner (06 §13.3): splits a compound shell command into the sub-commands
//! that are checked one by one against `Bash(…)` / `PowerShell(…)` rules.
//!
//! - **bash / zsh / Git Bash:** `&&`, `||`, `;`, `&`, newlines, pipes (`|`, `|&`),
//!   subshells `( … )`, groups `{ …; }`, command substitution `$( … )` and backticks,
//!   process substitution `<( … )` / `>( … )`, substitutions inside double quotes and
//!   `${…}`, here-documents, comments, `if/while/until` keywords.
//! - **PowerShell:** `;`, newlines, pipelines, `&&`/`||` (pwsh 7), `&` (background or call
//!   operator), script blocks `{ … }`, grouping `( … )`, subexpressions `$( … )` / `@( … )`,
//!   hashtables, here-strings, comments (`#`, `<# #>`), control keywords, `$x = …`
//!   assignments (the right-hand side is the command).
//!
//! Each part is the command's words joined by single spaces, **redirections included**
//! (`cat a > b`), so the read-only list never auto-allows a part that writes a file. A
//! part that contains a substitution keeps it verbatim (`echo $(rm x)`) and the inner
//! command is a part of its own (`rm x`), so both must pass.
//!
//! Anything the scanner does not understand is **unparseable** ([`ScanError`]), which the
//! evaluator turns into an ask (deny rules are still checked on the raw text): unbalanced
//! quotes or brackets, dangling operators, `for`/`case`/`select`/function definitions,
//! arithmetic commands, substitutions inside unquoted here-documents or arithmetic,
//! PowerShell `function`/`switch`/`class`/`param` blocks, `--%` and nesting deeper than
//! [`MAX_DEPTH`].

use std::fmt;

/// Which grammar a command is scanned with.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Dialect {
    /// bash, zsh, sh, Git Bash: `Bash(…)` rules.
    Posix,
    /// pwsh / Windows PowerShell: `PowerShell(…)` rules.
    PowerShell,
}

impl Dialect {
    /// The rule family (`Bash` / `PowerShell`), as in `genie_permissions::ShellDialect`.
    pub fn rule_tool(self) -> &'static str {
        match self {
            Dialect::Posix => "Bash",
            Dialect::PowerShell => "PowerShell",
        }
    }
}

/// The command can't be split safely: the caller asks (06 §13.3 "unparseable → ask").
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScanError(pub String);

impl fmt::Display for ScanError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "can't parse the command: {}", self.0)
    }
}

impl std::error::Error for ScanError {}

/// Maximum nesting of subshells, groups, substitutions and blocks.
pub const MAX_DEPTH: usize = 32;

type R<T> = Result<T, ScanError>;

fn err<T>(msg: impl Into<String>) -> R<T> {
    Err(ScanError(msg.into()))
}

/// Split `command` into its sub-commands (in source order, outer command before the
/// commands substituted into it; exact duplicates removed).
pub fn scan(dialect: Dialect, command: &str) -> Result<Vec<String>, ScanError> {
    let parts = scan_at(dialect, command, 0)?;
    let mut out: Vec<String> = Vec::with_capacity(parts.len());
    for p in parts {
        let p = p.trim().to_string();
        if !p.is_empty() && !out.contains(&p) {
            out.push(p);
        }
    }
    Ok(out)
}

fn scan_at(dialect: Dialect, command: &str, depth: usize) -> R<Vec<String>> {
    if depth > MAX_DEPTH {
        return err("nested too deeply");
    }
    let mut out = Vec::new();
    match dialect {
        Dialect::Posix => {
            let mut p = Posix {
                c: Cur::new(command, depth),
                heredocs: Vec::new(),
            };
            p.list(Term::Eof, &mut out)?;
        }
        Dialect::PowerShell => {
            let mut p = Ps {
                c: Cur::new(command, depth),
            };
            p.list(Term::Eof, &mut out)?;
        }
    }
    Ok(out)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Term {
    Eof,
    Paren,
    Brace,
}

struct Cur<'a> {
    src: &'a str,
    i: usize,
    depth: usize,
}

impl<'a> Cur<'a> {
    fn new(src: &'a str, depth: usize) -> Self {
        Cur { src, i: 0, depth }
    }

    fn peek(&self) -> Option<char> {
        self.src[self.i..].chars().next()
    }

    fn peek_at(&self, n: usize) -> Option<char> {
        self.src[self.i..].chars().nth(n)
    }

    fn bump(&mut self) {
        if let Some(c) = self.peek() {
            self.i += c.len_utf8();
        }
    }

    fn bump_n(&mut self, n: usize) {
        for _ in 0..n {
            self.bump();
        }
    }

    fn rest(&self) -> &'a str {
        &self.src[self.i..]
    }

    fn enter(&mut self) -> R<()> {
        self.depth += 1;
        if self.depth > MAX_DEPTH {
            return err("nested too deeply");
        }
        Ok(())
    }

    fn leave(&mut self) {
        self.depth -= 1;
    }

    fn skip_to_newline(&mut self) {
        match self.rest().find('\n') {
            Some(n) => self.i += n,
            None => self.i = self.src.len(),
        }
    }
}

// ---------------------------------------------------------------------------------------
// bash / zsh
// ---------------------------------------------------------------------------------------

/// Reserved words that only introduce or close a construct; the command after them is
/// what runs.
const POSIX_SKIP: &[&str] = &[
    "if", "then", "elif", "else", "fi", "do", "done", "while", "until", "!", "time",
];
/// Constructs the scanner does not split: the whole command is unparseable (→ ask).
const POSIX_REFUSE: &[&str] = &["for", "case", "esac", "select", "function", "coproc", "in"];

struct Heredoc {
    delim: String,
    strip_tabs: bool,
    quoted: bool,
}

struct Posix<'a> {
    c: Cur<'a>,
    heredocs: Vec<Heredoc>,
}

fn posix_word_end(c: Option<char>) -> bool {
    matches!(
        c,
        None | Some(' ' | '\t' | '\n' | ';' | '&' | '|' | '(' | ')' | '<' | '>')
    )
}

impl Posix<'_> {
    fn skip_blank(&mut self) {
        loop {
            match self.c.peek() {
                Some(' ' | '\t' | '\r') => self.c.bump(),
                Some('\\') if self.c.peek_at(1) == Some('\n') => self.c.bump_n(2),
                _ => break,
            }
        }
    }

    /// Blanks and a comment (`#` at a word start runs to the end of the line).
    fn skip_blank_comment(&mut self) {
        self.skip_blank();
        if self.c.peek() == Some('#') {
            self.c.skip_to_newline();
        }
    }

    fn list(&mut self, term: Term, out: &mut Vec<String>) -> R<()> {
        let mut need = false;
        loop {
            self.skip_blank_comment();
            match self.c.peek() {
                None => {
                    if need {
                        return err("the command ends with an operator");
                    }
                    if term != Term::Eof {
                        return err("unclosed `(` or `{`");
                    }
                    if !self.heredocs.is_empty() {
                        return err("unterminated here-document");
                    }
                    return Ok(());
                }
                Some('\n') => {
                    self.c.bump();
                    self.read_heredocs()?;
                    continue;
                }
                Some(')') => {
                    if term == Term::Paren && !need {
                        self.c.bump();
                        return Ok(());
                    }
                    return err("unexpected `)`");
                }
                Some('}') if term == Term::Brace && !need && posix_word_end(self.c.peek_at(1)) => {
                    self.c.bump();
                    return Ok(());
                }
                Some(';' | '|') => return err("unexpected operator"),
                Some('&') if self.c.peek_at(1) != Some('>') => {
                    return err("unexpected operator");
                }
                _ => {}
            }
            self.command(out)?;
            need = false;
            self.skip_blank_comment();
            match self.c.peek() {
                Some('|') => {
                    self.c.bump();
                    if matches!(self.c.peek(), Some('|' | '&')) {
                        self.c.bump();
                    }
                    need = true;
                }
                Some('&') => {
                    self.c.bump();
                    if self.c.peek() == Some('&') {
                        self.c.bump();
                        need = true;
                    }
                }
                Some(';') => {
                    self.c.bump();
                    if self.c.peek() == Some(';') {
                        return err("`;;` outside `case`");
                    }
                }
                _ => {}
            }
        }
    }

    fn command(&mut self, out: &mut Vec<String>) -> R<()> {
        let mut words: Vec<String> = Vec::new();
        let mut nested: Vec<String> = Vec::new();
        let mut compound = false;
        loop {
            self.skip_blank();
            let Some(c) = self.c.peek() else { break };
            match c {
                '#' => self.c.skip_to_newline(),
                '\n' | ';' | '|' | ')' => break,
                '&' => {
                    if self.c.peek_at(1) == Some('>') {
                        let t = self.redirect(self.c.i, &mut nested)?;
                        words.push(t);
                    } else {
                        break;
                    }
                }
                '(' => {
                    if !words.is_empty() || compound {
                        return err("unexpected `(` (function definitions are not split)");
                    }
                    if self.c.peek_at(1) == Some('(') {
                        return err("arithmetic command `((`");
                    }
                    self.c.bump();
                    self.c.enter()?;
                    self.list(Term::Paren, out)?;
                    self.c.leave();
                    compound = true;
                }
                '<' | '>' if self.c.peek_at(1) != Some('(') => {
                    let t = self.redirect(self.c.i, &mut nested)?;
                    words.push(t);
                }
                _ => {
                    let start = self.c.i;
                    let w = self.word(&mut nested)?;
                    if !w.is_empty()
                        && w.bytes().all(|b| b.is_ascii_digit())
                        && matches!(self.c.peek(), Some('<' | '>'))
                        && self.c.peek_at(1) != Some('(')
                    {
                        let t = self.redirect(start, &mut nested)?;
                        words.push(t);
                        continue;
                    }
                    if words.is_empty() && !compound {
                        if POSIX_SKIP.contains(&w.as_str()) {
                            continue;
                        }
                        if POSIX_REFUSE.contains(&w.as_str()) {
                            return err(format!("`{w}` is not split"));
                        }
                        if w == "{" {
                            self.c.enter()?;
                            self.list(Term::Brace, out)?;
                            self.c.leave();
                            compound = true;
                            continue;
                        }
                        if w == "}" {
                            return err("unexpected `}`");
                        }
                    }
                    words.push(w);
                }
            }
        }
        if !words.is_empty() {
            out.push(words.join(" "));
        }
        out.extend(nested);
        Ok(())
    }

    /// One word, raw (quotes kept). Substitutions inside it are scanned into `nested`.
    fn word(&mut self, nested: &mut Vec<String>) -> R<String> {
        let start = self.c.i;
        while let Some(c) = self.c.peek() {
            match c {
                '<' | '>' if self.c.i == start && self.c.peek_at(1) == Some('(') => {
                    // Process substitution.
                    self.c.bump_n(2);
                    self.c.enter()?;
                    self.list(Term::Paren, nested)?;
                    self.c.leave();
                }
                '(' if self.c.src[..self.c.i].ends_with('=') && self.c.i > start => {
                    // Array assignment `a=( … )`: scanned like a subshell (conservative).
                    self.c.bump();
                    self.c.enter()?;
                    self.list(Term::Paren, nested)?;
                    self.c.leave();
                }
                ' ' | '\t' | '\r' | '\n' | ';' | '&' | '|' | '(' | ')' | '<' | '>' => break,
                '\\' => {
                    self.c.bump();
                    if self.c.peek().is_none() {
                        return err("trailing backslash");
                    }
                    self.c.bump();
                }
                '\'' => {
                    self.c.bump();
                    match self.c.rest().find('\'') {
                        Some(n) => self.c.i += n + 1,
                        None => return err("unterminated single quote"),
                    }
                }
                '"' => self.dquote(nested)?,
                '`' => self.backtick(nested)?,
                '$' => self.dollar(nested)?,
                _ => self.c.bump(),
            }
        }
        if self.c.i == start {
            return err("expected a word");
        }
        Ok(self.c.src[start..self.c.i].to_string())
    }

    fn dquote(&mut self, nested: &mut Vec<String>) -> R<()> {
        self.c.bump();
        loop {
            match self.c.peek() {
                None => return err("unterminated double quote"),
                Some('"') => {
                    self.c.bump();
                    return Ok(());
                }
                Some('\\') => self.c.bump_n(2),
                Some('`') => self.backtick(nested)?,
                Some('$') => self.dollar(nested)?,
                Some(_) => self.c.bump(),
            }
        }
    }

    fn backtick(&mut self, nested: &mut Vec<String>) -> R<()> {
        self.c.bump();
        let mut inner = String::new();
        loop {
            match self.c.peek() {
                None => return err("unterminated backtick"),
                Some('`') => {
                    self.c.bump();
                    break;
                }
                Some('\\') => {
                    self.c.bump();
                    match self.c.peek() {
                        Some(c @ ('`' | '\\' | '$')) => inner.push(c),
                        Some(c) => {
                            inner.push('\\');
                            inner.push(c);
                        }
                        None => return err("unterminated backtick"),
                    }
                    self.c.bump();
                }
                Some(c) => {
                    inner.push(c);
                    self.c.bump();
                }
            }
        }
        nested.extend(scan_at(Dialect::Posix, &inner, self.c.depth + 1)?);
        Ok(())
    }

    /// At `$`: command substitution, arithmetic, `${…}`, `$'…'` or a plain variable.
    fn dollar(&mut self, nested: &mut Vec<String>) -> R<()> {
        match self.c.peek_at(1) {
            Some('(') if self.c.peek_at(2) == Some('(') => self.arith(),
            Some('(') => {
                self.c.bump_n(2);
                self.c.enter()?;
                self.list(Term::Paren, nested)?;
                self.c.leave();
                Ok(())
            }
            Some('{') => self.param(nested),
            Some('\'') => {
                self.c.bump_n(2);
                loop {
                    match self.c.peek() {
                        None => return err("unterminated $'…'"),
                        Some('\\') => self.c.bump_n(2),
                        Some('\'') => {
                            self.c.bump();
                            return Ok(());
                        }
                        Some(_) => self.c.bump(),
                    }
                }
            }
            _ => {
                self.c.bump();
                Ok(())
            }
        }
    }

    /// `$(( … ))`: data, unless it hides a substitution (unparseable).
    fn arith(&mut self) -> R<()> {
        self.c.bump_n(3);
        let mut depth = 0usize;
        loop {
            match self.c.peek() {
                None => return err("unterminated $(( … ))"),
                Some('(') => {
                    depth += 1;
                    self.c.bump();
                }
                Some(')') if depth == 0 => {
                    if self.c.peek_at(1) != Some(')') {
                        return err("unbalanced $(( … ))");
                    }
                    self.c.bump_n(2);
                    return Ok(());
                }
                Some(')') => {
                    depth -= 1;
                    self.c.bump();
                }
                Some('`') => return err("substitution inside $(( … ))"),
                Some('$') if self.c.peek_at(1) == Some('(') => {
                    return err("substitution inside $(( … ))");
                }
                Some(_) => self.c.bump(),
            }
        }
    }

    /// `${ … }`: substitutions inside defaults (`${a:-$(cmd)}`) are scanned.
    fn param(&mut self, nested: &mut Vec<String>) -> R<()> {
        self.c.bump_n(2);
        let mut depth = 1usize;
        loop {
            match self.c.peek() {
                None => return err("unterminated ${ … }"),
                Some('{') => {
                    depth += 1;
                    self.c.bump();
                }
                Some('}') => {
                    depth -= 1;
                    self.c.bump();
                    if depth == 0 {
                        return Ok(());
                    }
                }
                Some('\\') => self.c.bump_n(2),
                Some('\'') => {
                    self.c.bump();
                    match self.c.rest().find('\'') {
                        Some(n) => self.c.i += n + 1,
                        None => return err("unterminated single quote"),
                    }
                }
                Some('"') => self.dquote(nested)?,
                Some('`') => self.backtick(nested)?,
                Some('$') => self.dollar(nested)?,
                Some(_) => self.c.bump(),
            }
        }
    }

    /// A redirection starting at `start` (fd prefix included); returns its source text.
    fn redirect(&mut self, start: usize, nested: &mut Vec<String>) -> R<String> {
        let rest = self.c.rest();
        let (len, heredoc, strip) = if rest.starts_with("<<<") {
            (3, false, false)
        } else if rest.starts_with("<<-") {
            (3, true, true)
        } else if rest.starts_with("<<") {
            (2, true, false)
        } else if rest.starts_with("&>>") {
            (3, false, false)
        } else if ["<&", "<>", "&>", ">>", ">&", ">|"]
            .iter()
            .any(|p| rest.starts_with(p))
        {
            (2, false, false)
        } else {
            (1, false, false)
        };
        self.c.bump_n(len);
        self.skip_blank();
        match self.c.peek() {
            Some('<' | '>') if self.c.peek_at(1) == Some('(') => {}
            c if posix_word_end(c) => return err("redirection without a target"),
            _ => {}
        }
        let target = self.word(nested)?;
        if heredoc {
            let quoted = target.contains(['\'', '"', '\\']);
            let delim: String = target
                .chars()
                .filter(|c| !matches!(c, '\'' | '"' | '\\'))
                .collect();
            self.heredocs.push(Heredoc {
                delim,
                strip_tabs: strip,
                quoted,
            });
        }
        Ok(self.c.src[start..self.c.i].to_string())
    }

    /// Consume the bodies of pending here-documents (after a newline).
    fn read_heredocs(&mut self) -> R<()> {
        for h in std::mem::take(&mut self.heredocs) {
            loop {
                if self.c.i >= self.c.src.len() {
                    return err("unterminated here-document");
                }
                let rest = self.c.rest();
                let (line, adv) = match rest.find('\n') {
                    Some(n) => (&rest[..n], n + 1),
                    None => (rest, rest.len()),
                };
                self.c.i += adv;
                let cmp = if h.strip_tabs {
                    line.trim_start_matches('\t')
                } else {
                    line
                };
                if cmp.trim_end_matches('\r') == h.delim {
                    break;
                }
                if !h.quoted && (line.contains("$(") || line.contains('`')) {
                    return err("substitution inside a here-document");
                }
            }
        }
        Ok(())
    }
}

// ---------------------------------------------------------------------------------------
// PowerShell
// ---------------------------------------------------------------------------------------

/// Statements led by these emit no part themselves; their `( … )` and `{ … }` are scanned.
const PS_KEYWORDS: &[&str] = &[
    "if", "elseif", "else", "foreach", "for", "while", "do", "until", "try", "catch", "finally",
    "trap",
];
const PS_REFUSE: &[&str] = &[
    "function",
    "filter",
    "class",
    "enum",
    "switch",
    "workflow",
    "configuration",
    "param",
    "begin",
    "process",
    "end",
    "dynamicparam",
    "data",
    "using",
    "inlinescript",
    "parallel",
    "sequence",
];
const PS_ASSIGN: &[&str] = &["=", "+=", "-=", "*=", "/=", "%=", "??="];

fn ps_squote(c: char) -> bool {
    matches!(c, '\'' | '\u{2018}' | '\u{2019}' | '\u{201A}' | '\u{201B}')
}

fn ps_dquote(c: char) -> bool {
    matches!(c, '"' | '\u{201C}' | '\u{201D}' | '\u{201E}')
}

struct Ps<'a> {
    c: Cur<'a>,
}

impl Ps<'_> {
    fn skip_blank_comment(&mut self) -> R<()> {
        loop {
            match self.c.peek() {
                Some(' ' | '\t' | '\r' | '\u{A0}') => self.c.bump(),
                Some('`') if matches!(self.c.peek_at(1), Some('\n')) => self.c.bump_n(2),
                Some('`') if self.c.rest().starts_with("`\r\n") => self.c.bump_n(3),
                Some('#') => self.c.skip_to_newline(),
                Some('<') if self.c.peek_at(1) == Some('#') => match self.c.rest().find("#>") {
                    Some(n) => self.c.i += n + 2,
                    None => return err("unterminated <# comment"),
                },
                _ => return Ok(()),
            }
        }
    }

    fn list(&mut self, term: Term, out: &mut Vec<String>) -> R<()> {
        let mut need = false;
        loop {
            self.skip_blank_comment()?;
            match self.c.peek() {
                None => {
                    if need {
                        return err("the command ends with an operator");
                    }
                    if term != Term::Eof {
                        return err("unclosed `(` or `{`");
                    }
                    return Ok(());
                }
                Some('\n' | ';') => {
                    self.c.bump();
                    continue;
                }
                Some(')') => {
                    if term == Term::Paren && !need {
                        self.c.bump();
                        return Ok(());
                    }
                    return err("unexpected `)`");
                }
                Some('}') => {
                    if term == Term::Brace && !need {
                        self.c.bump();
                        return Ok(());
                    }
                    return err("unexpected `}`");
                }
                Some('|') => return err("unexpected `|`"),
                Some('&') if self.c.peek_at(1) == Some('&') => return err("unexpected `&&`"),
                _ => {}
            }
            self.command(out)?;
            need = false;
            self.skip_blank_comment()?;
            match self.c.peek() {
                Some('|') => {
                    self.c.bump();
                    if self.c.peek() == Some('|') {
                        self.c.bump();
                    }
                    need = true;
                }
                Some('&') => {
                    self.c.bump();
                    if self.c.peek() == Some('&') {
                        self.c.bump();
                        need = true;
                    }
                }
                _ => {}
            }
        }
    }

    fn command(&mut self, out: &mut Vec<String>) -> R<()> {
        let mut words: Vec<String> = Vec::new();
        let mut nested: Vec<String> = Vec::new();
        loop {
            self.skip_blank_comment()?;
            let Some(c) = self.c.peek() else { break };
            match c {
                '\n' | ';' | '|' | ')' | '}' => break,
                '&' => {
                    if words.is_empty() {
                        // Call operator: `& 'C:\tool.exe' args`.
                        self.c.bump();
                        words.push("&".into());
                    } else {
                        break;
                    }
                }
                '<' => return err("`<` is reserved in PowerShell"),
                '>' => {
                    let t = self.redirect(self.c.i, &mut nested)?;
                    words.push(t);
                }
                _ => {
                    let start = self.c.i;
                    let w = self.word(&mut nested)?;
                    if (w == "*" || w.bytes().all(|b| b.is_ascii_digit()))
                        && self.c.peek() == Some('>')
                    {
                        let t = self.redirect(start, &mut nested)?;
                        words.push(t);
                        continue;
                    }
                    if w == "--%" {
                        return err("`--%` (stop-parsing) is not split");
                    }
                    if words.is_empty() && PS_REFUSE.contains(&w.to_ascii_lowercase().as_str()) {
                        return err(format!("`{w}` is not split"));
                    }
                    words.push(w);
                }
            }
        }
        let keyword = words
            .first()
            .is_some_and(|w| PS_KEYWORDS.contains(&w.to_ascii_lowercase().as_str()));
        if !keyword {
            if words.len() >= 2
                && is_assign_target(&words[0])
                && PS_ASSIGN.contains(&words[1].as_str())
            {
                words.drain(..2);
            }
            if !words.is_empty() {
                out.push(words.join(" "));
            }
        }
        out.extend(nested);
        Ok(())
    }

    fn group(&mut self, open_len: usize, term: Term, nested: &mut Vec<String>) -> R<()> {
        self.c.bump_n(open_len);
        self.c.enter()?;
        self.list(term, nested)?;
        self.c.leave();
        Ok(())
    }

    fn word(&mut self, nested: &mut Vec<String>) -> R<String> {
        let start = self.c.i;
        while let Some(c) = self.c.peek() {
            match c {
                ' ' | '\t' | '\r' | '\n' | ';' | '|' | '&' | ')' | '}' | '>' | '<' => break,
                '(' => self.group(1, Term::Paren, nested)?,
                '{' => self.group(1, Term::Brace, nested)?,
                '$' => match self.c.peek_at(1) {
                    Some('(') => self.group(2, Term::Paren, nested)?,
                    Some('{') => {
                        // `${env:Path}`: a braced variable name, data.
                        self.c.bump_n(2);
                        loop {
                            match self.c.peek() {
                                None => return err("unterminated ${…}"),
                                Some('`') => self.c.bump_n(2),
                                Some('}') => {
                                    self.c.bump();
                                    break;
                                }
                                Some(_) => self.c.bump(),
                            }
                        }
                    }
                    _ => self.c.bump(),
                },
                '@' => match self.c.peek_at(1) {
                    Some('(') => self.group(2, Term::Paren, nested)?,
                    Some('{') => self.group(2, Term::Brace, nested)?,
                    Some(q) if (ps_squote(q) || ps_dquote(q)) && self.here_string_start() => {
                        self.here_string(ps_dquote(q))?;
                    }
                    _ => self.c.bump(),
                },
                q if ps_squote(q) => {
                    self.c.bump();
                    loop {
                        match self.c.peek() {
                            None => return err("unterminated single quote"),
                            Some(q) if ps_squote(q) => {
                                self.c.bump();
                                if self.c.peek().is_some_and(ps_squote) {
                                    self.c.bump();
                                } else {
                                    break;
                                }
                            }
                            Some(_) => self.c.bump(),
                        }
                    }
                }
                q if ps_dquote(q) => {
                    self.c.bump();
                    loop {
                        match self.c.peek() {
                            None => return err("unterminated double quote"),
                            Some('`') => self.c.bump_n(2),
                            Some(q) if ps_dquote(q) => {
                                self.c.bump();
                                if self.c.peek().is_some_and(ps_dquote) {
                                    self.c.bump();
                                } else {
                                    break;
                                }
                            }
                            Some('$') if self.c.peek_at(1) == Some('(') => {
                                self.group(2, Term::Paren, nested)?;
                            }
                            Some(_) => self.c.bump(),
                        }
                    }
                }
                '`' => {
                    self.c.bump();
                    if self.c.peek().is_none() {
                        return err("trailing backtick");
                    }
                    self.c.bump();
                }
                _ => self.c.bump(),
            }
        }
        if self.c.i == start {
            return err("expected a word");
        }
        Ok(self.c.src[start..self.c.i].to_string())
    }

    /// `@'` / `@"` followed by a line break.
    fn here_string_start(&self) -> bool {
        let mut ch = self.c.rest().chars();
        ch.next();
        ch.next();
        let r = ch.as_str();
        r.starts_with('\n') || r.starts_with("\r\n")
    }

    fn here_string(&mut self, double: bool) -> R<()> {
        self.c.bump_n(2);
        let rest = self.c.rest();
        let end = rest
            .match_indices('\n')
            .find(|(n, _)| {
                let tail = &rest[n + 1..];
                let mut ch = tail.chars();
                match (ch.next(), ch.next()) {
                    (Some(q), Some('@')) => {
                        if double {
                            ps_dquote(q)
                        } else {
                            ps_squote(q)
                        }
                    }
                    _ => false,
                }
            })
            .map(|(n, _)| n);
        let Some(n) = end else {
            return err("unterminated here-string");
        };
        let body = &rest[..n];
        if double && body.contains("$(") {
            return err("subexpression inside a here-string");
        }
        self.c.i += n + 1;
        self.c.bump_n(2);
        Ok(())
    }

    fn redirect(&mut self, start: usize, nested: &mut Vec<String>) -> R<String> {
        self.c.bump(); // '>'
        if self.c.peek() == Some('>') {
            self.c.bump();
        }
        if self.c.peek() == Some('&') {
            self.c.bump();
            match self.c.peek() {
                Some(d) if d.is_ascii_digit() => self.c.bump(),
                _ => return err("bad stream redirection"),
            }
            return Ok(self.c.src[start..self.c.i].to_string());
        }
        self.skip_blank_comment()?;
        match self.c.peek() {
            None | Some('\n' | ';' | '|' | '&' | ')' | '}' | '>' | '<') => {
                return err("redirection without a target");
            }
            _ => {}
        }
        self.word(nested)?;
        Ok(self.c.src[start..self.c.i].to_string())
    }
}

fn is_assign_target(w: &str) -> bool {
    w.starts_with('$') || (w.starts_with('[') && w.contains("]$"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ok(d: Dialect, cmd: &str) -> Vec<String> {
        match scan(d, cmd) {
            Ok(p) => p,
            Err(e) => panic!("{cmd:?}: {e}"),
        }
    }

    #[test]
    fn posix_table() {
        let cases: &[(&str, &[&str])] = &[
            ("ls -la", &["ls -la"]),
            ("  git   status  ", &["git status"]),
            ("git status && git diff", &["git status", "git diff"]),
            ("make || echo failed", &["make", "echo failed"]),
            ("a; b;c", &["a", "b", "c"]),
            ("a & b", &["a", "b"]),
            ("a\nb\n\nc", &["a", "b", "c"]),
            ("cat f | grep x | wc -l", &["cat f", "grep x", "wc -l"]),
            ("make 2>&1 |& tee log", &["make 2>&1", "tee log"]),
            ("(cd sub && make)", &["cd sub", "make"]),
            ("{ a; b; } > out", &["a", "b", "> out"]),
            ("echo $(rm -rf /)", &["echo $(rm -rf /)", "rm -rf /"]),
            ("echo `whoami`", &["echo `whoami`", "whoami"]),
            ("echo \"x $(id -u) y\"", &["echo \"x $(id -u) y\"", "id -u"]),
            ("echo '$(not run)'", &["echo '$(not run)'"]),
            ("echo \"a; b\" 'c && d'", &["echo \"a; b\" 'c && d'"]),
            (
                "diff <(sort a) <(sort b)",
                &["diff <(sort a) <(sort b)", "sort a", "sort b"],
            ),
            ("cat a > b", &["cat a > b"]),
            (
                "cat < in >> out 2>/dev/null",
                &["cat < in >> out 2>/dev/null"],
            ),
            ("ls &> all.log", &["ls &> all.log"]),
            ("x=$(date) && echo $x", &["x=$(date)", "date", "echo $x"]),
            ("echo ${HOME:-$(pwd)}", &["echo ${HOME:-$(pwd)}", "pwd"]),
            ("echo $((1 + 2))", &["echo $((1 + 2))"]),
            (
                "if test -f x; then rm x; else touch x; fi",
                &["test -f x", "rm x", "touch x"],
            ),
            ("while true; do sleep 1; done", &["true", "sleep 1"]),
            ("! grep -q x f", &["grep -q x f"]),
            ("ls # && rm -rf /", &["ls"]),
            ("echo a#b", &["echo a#b"]),
            (
                "git commit -m \"fix: a | b\"",
                &["git commit -m \"fix: a | b\""],
            ),
            (
                "echo $(echo $(id))",
                &["echo $(echo $(id))", "echo $(id)", "id"],
            ),
            ("cat <<EOF\nhello $USER\nEOF\nls", &["cat <<EOF", "ls"]),
            ("cat <<'EOF' | sh\n$(rm x)\nEOF", &["cat <<'EOF'", "sh"]),
            ("echo a\\;b", &["echo a\\;b"]),
            ("echo $'a\\'b'", &["echo $'a\\'b'"]),
            ("npm test -- --watch=false", &["npm test -- --watch=false"]),
            ("[[ -f x ]] && cat x", &["[[ -f x ]]", "cat x"]),
            ("a &&\n b", &["a", "b"]),
            ("ls; ls", &["ls"]),
        ];
        for (cmd, want) in cases {
            assert_eq!(ok(Dialect::Posix, cmd), *want, "{cmd:?}");
        }
    }

    #[test]
    fn posix_unparseable() {
        let cases = [
            "echo 'unterminated",
            "echo \"unterminated",
            "echo $(ls",
            "(ls",
            "ls )",
            "ls &&",
            "ls |",
            "&& ls",
            "; ls",
            "for f in *; do rm $f; done",
            "case $x in a) ls;; esac",
            "f() { rm -rf /; }",
            "function f { ls; }",
            "((i++))",
            "cat <<EOF\n$(rm x)\nEOF",
            "cat <<EOF\nno end",
            "echo $(( $(id) + 1 ))",
            "echo `unterminated",
            "ls >",
            "{ ls",
        ];
        for cmd in cases {
            assert!(
                scan(Dialect::Posix, cmd).is_err(),
                "{cmd:?} should be unparseable"
            );
        }
        let deep = format!("{}ls{}", "$(".repeat(40), ")".repeat(40));
        assert!(scan(Dialect::Posix, &deep).is_err());
    }

    #[test]
    fn powershell_table() {
        let cases: &[(&str, &[&str])] = &[
            ("Get-ChildItem -Recurse", &["Get-ChildItem -Recurse"]),
            (
                "Get-Content a.txt; Remove-Item b",
                &["Get-Content a.txt", "Remove-Item b"],
            ),
            (
                "Get-ChildItem | Where-Object Length -gt 1kb | Remove-Item",
                &[
                    "Get-ChildItem",
                    "Where-Object Length -gt 1kb",
                    "Remove-Item",
                ],
            ),
            ("npm test && npm run build", &["npm test", "npm run build"]),
            ("git pull || exit 1", &["git pull", "exit 1"]),
            (
                "Get-ChildItem | ForEach-Object { Remove-Item $_ }",
                &[
                    "Get-ChildItem",
                    "ForEach-Object { Remove-Item $_ }",
                    "Remove-Item $_",
                ],
            ),
            (
                "Write-Output \"user: $(whoami)\"",
                &["Write-Output \"user: $(whoami)\"", "whoami"],
            ),
            ("Write-Output 'a; b | c'", &["Write-Output 'a; b | c'"]),
            (
                "Write-Output 'it''s; fine'",
                &["Write-Output 'it''s; fine'"],
            ),
            ("$files = Get-ChildItem *.log", &["Get-ChildItem *.log"]),
            (
                "if (Test-Path x) { Remove-Item x } else { New-Item x }",
                &["Test-Path x", "Remove-Item x", "New-Item x"],
            ),
            (
                "try { Invoke-WebRequest u } catch { Write-Error $_ }",
                &["Invoke-WebRequest u", "Write-Error $_"],
            ),
            ("dir > out.txt 2>&1", &["dir > out.txt 2>&1"]),
            ("& 'C:\\tools\\x.exe' -v", &["& 'C:\\tools\\x.exe' -v"]),
            ("Start-Sleep 5 &", &["Start-Sleep 5"]),
            ("Get-Item a # ; Remove-Item b", &["Get-Item a"]),
            (
                "Get-Item a <# ; Remove-Item b #> -Force",
                &["Get-Item a -Force"],
            ),
            (
                "Copy-Item (Join-Path $a b) c",
                &["Copy-Item (Join-Path $a b) c", "Join-Path $a b"],
            ),
            ("Get-Process `\n  -Name x", &["Get-Process -Name x"]),
            (
                "Write-Output @'\na; Remove-Item x\n'@",
                &["Write-Output @'\na; Remove-Item x\n'@"],
            ),
            (
                "Write-Output \u{201C}a; b\u{201D}",
                &["Write-Output \u{201C}a; b\u{201D}"],
            ),
            ("echo ${env:PATH}", &["echo ${env:PATH}"]),
        ];
        for (cmd, want) in cases {
            assert_eq!(ok(Dialect::PowerShell, cmd), *want, "{cmd:?}");
        }
    }

    #[test]
    fn powershell_unparseable() {
        let cases = [
            "Write-Output 'unterminated",
            "Write-Output \"unterminated",
            "Get-Item (x",
            "Get-Item x }",
            "Get-Item x |",
            "| Get-Item",
            "function f { Remove-Item x }",
            "switch ($x) { 1 { Remove-Item a } }",
            "Get-Content < in.txt",
            "icacls x --% /grant a",
            "Write-Output @\"\n$(Remove-Item x)\n\"@",
            "Get-Item x <# open",
        ];
        for cmd in cases {
            assert!(
                scan(Dialect::PowerShell, cmd).is_err(),
                "{cmd:?} should be unparseable"
            );
        }
    }
}
