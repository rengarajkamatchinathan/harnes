//! Markdown frontmatter (`---` … `---`) shared by `SKILL.md` and command files.
//!
//! The parser accepts the YAML subset that skills and commands use in practice, without a
//! YAML dependency: `key: value` maps (nested by indentation), block lists (`- item`),
//! flow lists (`[a, "b"]`) and flow maps (`{k: v}`), single/double-quoted strings,
//! `|`/`>` block scalars, booleans, integers, floats, `null`/`~` and `#` comments.
//! Anchors, tags, multi-document streams and complex keys are rejected or read as text.
//! The result is a JSON object so callers can deserialize it with serde.

use serde_json::{Map, Number, Value};

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum FrontmatterError {
    #[error("frontmatter starts with `---` but has no closing `---`")]
    Unterminated,
    #[error("frontmatter line {line}: {message}")]
    Syntax { line: usize, message: String },
}

/// Splits `text` into `(frontmatter source, body)`. No leading `---` line → no frontmatter.
pub fn split(text: &str) -> Result<(Option<&str>, &str), FrontmatterError> {
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    let Some(first_end) = text.find('\n') else {
        return Ok(if text.trim_end() == "---" {
            (Some(""), "")
        } else {
            (None, text)
        });
    };
    if text[..first_end].trim_end() != "---" {
        return Ok((None, text));
    }
    let rest = &text[first_end + 1..];
    let mut offset = 0;
    for line in rest.split_inclusive('\n') {
        let t = line.trim_end();
        if t == "---" || t == "..." {
            let fm = &rest[..offset];
            let body = &rest[offset + line.len()..];
            return Ok((Some(fm), body));
        }
        offset += line.len();
    }
    Err(FrontmatterError::Unterminated)
}

/// Parses frontmatter source into a JSON object.
pub fn parse(src: &str) -> Result<Map<String, Value>, FrontmatterError> {
    let mut p = Parser {
        lines: src.lines().map(|l| l.replace('\t', "  ")).collect(),
        i: 0,
    };
    p.skip_blank();
    if p.i >= p.lines.len() {
        return Ok(Map::new());
    }
    let ind = indent(&p.lines[p.i]);
    let map = p.map(ind)?;
    p.skip_blank();
    if p.i < p.lines.len() {
        return Err(p.err("unexpected indentation"));
    }
    Ok(map)
}

/// `split` + `parse`. Missing frontmatter gives an empty map.
pub fn parse_document(text: &str) -> Result<(Map<String, Value>, &str), FrontmatterError> {
    match split(text)? {
        (Some(fm), body) => Ok((parse(fm)?, body)),
        (None, body) => Ok((Map::new(), body)),
    }
}

struct Parser {
    lines: Vec<String>,
    i: usize,
}

fn indent(line: &str) -> usize {
    line.len() - line.trim_start_matches(' ').len()
}

fn is_blank(line: &str) -> bool {
    let t = line.trim();
    t.is_empty() || t.starts_with('#')
}

fn is_list_item(content: &str) -> bool {
    content == "-" || content.starts_with("- ")
}

impl Parser {
    fn err(&self, message: &str) -> FrontmatterError {
        FrontmatterError::Syntax {
            line: self.i + 1,
            message: message.to_string(),
        }
    }

    fn skip_blank(&mut self) {
        while self.i < self.lines.len() && is_blank(&self.lines[self.i]) {
            self.i += 1;
        }
    }

    /// Indentation and content of the next non-blank line, if any.
    fn peek(&mut self) -> Option<(usize, String)> {
        self.skip_blank();
        let l = self.lines.get(self.i)?;
        Some((indent(l), l.trim_start().to_string()))
    }

    fn map(&mut self, ind: usize) -> Result<Map<String, Value>, FrontmatterError> {
        let mut out = Map::new();
        while let Some((li, content)) = self.peek() {
            if li < ind {
                break;
            }
            if li > ind {
                return Err(self.err("unexpected indentation"));
            }
            if is_list_item(&content) {
                return Err(self.err("list item where a `key: value` was expected"));
            }
            let (key, rest) =
                split_key(&content).ok_or_else(|| self.err("expected `key: value`"))?;
            if out.contains_key(&key) {
                return Err(self.err(&format!("duplicate key `{key}`")));
            }
            self.i += 1;
            let value = self.value_after_key(li, &rest)?;
            out.insert(key, value);
        }
        Ok(out)
    }

    /// The value of `key:` whose inline remainder is `rest`, at key indentation `li`.
    fn value_after_key(&mut self, li: usize, rest: &str) -> Result<Value, FrontmatterError> {
        let rest = strip_comment(rest);
        if rest.is_empty() {
            return match self.peek() {
                Some((ni, c)) if ni > li => {
                    if is_list_item(&c) {
                        self.list(ni)
                    } else {
                        self.map(ni).map(Value::Object)
                    }
                }
                // YAML allows a block list at the key's own indentation.
                Some((ni, c)) if ni == li && is_list_item(&c) => self.list(ni),
                _ => Ok(Value::Null),
            };
        }
        if rest.starts_with('|') || rest.starts_with('>') {
            return Ok(Value::String(self.block_scalar(li, rest)));
        }
        if rest.starts_with('&') || rest.starts_with('*') || rest.starts_with('!') {
            return Err(self.err("anchors, aliases and tags are not supported"));
        }
        inline(rest).map_err(|m| FrontmatterError::Syntax {
            line: self.i,
            message: m,
        })
    }

    fn list(&mut self, ind: usize) -> Result<Value, FrontmatterError> {
        let mut out = vec![];
        while let Some((li, content)) = self.peek() {
            if li != ind || !is_list_item(&content) {
                if li > ind {
                    return Err(self.err("unexpected indentation"));
                }
                break;
            }
            let item = content[1..].trim_start().to_string();
            if item.is_empty() {
                self.i += 1;
                let v = match self.peek() {
                    Some((ni, c)) if ni > ind => {
                        if is_list_item(&c) {
                            self.list(ni)?
                        } else {
                            Value::Object(self.map(ni)?)
                        }
                    }
                    _ => Value::Null,
                };
                out.push(v);
            } else if !item.starts_with(['"', '\'', '[', '{']) && split_key(&item).is_some() {
                // `- key: value` starts a map whose keys sit at the item's content column.
                let col = indent(&self.lines[self.i]) + (content.len() - item.len());
                self.lines[self.i] = format!("{}{}", " ".repeat(col), item);
                out.push(Value::Object(self.map(col)?));
            } else {
                self.i += 1;
                out.push(
                    inline(strip_comment(&item)).map_err(|m| FrontmatterError::Syntax {
                        line: self.i,
                        message: m,
                    })?,
                );
            }
        }
        Ok(Value::Array(out))
    }

    fn block_scalar(&mut self, li: usize, header: &str) -> String {
        let folded = header.starts_with('>');
        let chomp = header[1..].trim();
        let mut raw: Vec<&str> = vec![];
        let start = self.i;
        while self.i < self.lines.len() {
            let l = &self.lines[self.i];
            if !l.trim().is_empty() && indent(l) <= li {
                break;
            }
            self.i += 1;
        }
        for l in &self.lines[start..self.i] {
            raw.push(l);
        }
        let block_ind = raw
            .iter()
            .filter(|l| !l.trim().is_empty())
            .map(|l| indent(l))
            .min()
            .unwrap_or(0);
        let lines: Vec<&str> = raw
            .iter()
            .map(|l| {
                if l.len() >= block_ind {
                    &l[block_ind..]
                } else {
                    ""
                }
            })
            .collect();
        let mut text = if folded {
            let mut s = String::new();
            let mut prev_blank = true;
            for l in &lines {
                if l.trim().is_empty() {
                    s.push('\n');
                    prev_blank = true;
                } else {
                    if !prev_blank {
                        s.push(' ');
                    }
                    s.push_str(l);
                    prev_blank = false;
                }
            }
            s
        } else {
            lines.join("\n")
        };
        let trimmed_len = text.trim_end_matches('\n').len();
        if chomp.starts_with('+') {
            text.push('\n');
        } else {
            text.truncate(trimmed_len);
            if !chomp.starts_with('-') && !text.is_empty() {
                text.push('\n');
            }
        }
        text
    }
}

/// `key: rest` → `(key, rest)`. The colon must be followed by a space or end the line.
fn split_key(content: &str) -> Option<(String, String)> {
    let (key, rest) = if let Some(q) = content.chars().next().filter(|c| *c == '"' || *c == '\'') {
        let end = content[1..].find(q)? + 1;
        let after = content[end + 1..].strip_prefix(':')?;
        (content[1..end].to_string(), after)
    } else {
        let bytes = content.as_bytes();
        let pos = (0..bytes.len())
            .find(|&k| bytes[k] == b':' && (k + 1 == bytes.len() || bytes[k + 1] == b' '))?;
        (content[..pos].trim().to_string(), &content[pos + 1..])
    };
    if key.is_empty() || key.starts_with('#') {
        return None;
    }
    if rest.is_empty() || rest.starts_with(' ') {
        Some((key, rest.trim().to_string()))
    } else {
        None
    }
}

/// Removes a trailing ` # comment` outside quotes.
fn strip_comment(s: &str) -> &str {
    let mut quote: Option<char> = None;
    let mut prev_space = true;
    for (i, c) in s.char_indices() {
        match quote {
            Some(q) if c == q => quote = None,
            Some(_) => {}
            None if c == '"' || c == '\'' => quote = Some(c),
            None if c == '#' && prev_space => return s[..i].trim_end(),
            None => {}
        }
        prev_space = c == ' ';
    }
    s.trim()
}

/// An inline (single-line) value.
fn inline(s: &str) -> Result<Value, String> {
    let s = s.trim();
    let (v, rest) = inline_prefix(s)?;
    if !rest.trim().is_empty() {
        // Plain scalars may contain anything; only quoted/flow values must end cleanly.
        return Err(format!("unexpected text after value: `{}`", rest.trim()));
    }
    Ok(v)
}

/// Parses one value from the start of `s`; returns it and the remaining text.
fn inline_prefix(s: &str) -> Result<(Value, &str), String> {
    let s = s.trim_start();
    match s.chars().next() {
        Some('"') => {
            let mut out = String::new();
            let mut chars = s[1..].char_indices();
            while let Some((i, c)) = chars.next() {
                match c {
                    '"' => return Ok((Value::String(out), &s[i + 2..])),
                    '\\' => match chars.next().map(|(_, c)| c) {
                        Some('n') => out.push('\n'),
                        Some('t') => out.push('\t'),
                        Some('r') => out.push('\r'),
                        Some('0') => out.push('\0'),
                        Some(c @ ('"' | '\\' | '/')) => out.push(c),
                        Some(' ') => out.push(' '),
                        Some(c) => return Err(format!("unsupported escape `\\{c}`")),
                        None => return Err("unterminated string".into()),
                    },
                    c => out.push(c),
                }
            }
            Err("unterminated string".into())
        }
        Some('\'') => {
            let mut out = String::new();
            let body = &s[1..];
            let mut i = 0;
            let b = body.as_bytes();
            while i < b.len() {
                if b[i] == b'\'' {
                    if b.get(i + 1) == Some(&b'\'') {
                        out.push('\'');
                        i += 2;
                        continue;
                    }
                    return Ok((Value::String(out), &body[i + 1..]));
                }
                let ch = body[i..].chars().next().ok_or("bad string")?;
                out.push(ch);
                i += ch.len_utf8();
            }
            Err("unterminated string".into())
        }
        Some('[') => {
            let mut items = vec![];
            let mut rest = s[1..].trim_start();
            if let Some(r) = rest.strip_prefix(']') {
                return Ok((Value::Array(items), r));
            }
            loop {
                let (v, r) = flow_item(rest, &[',', ']'])?;
                items.push(v);
                let r = r.trim_start();
                if let Some(r) = r.strip_prefix(',') {
                    rest = r.trim_start();
                    if let Some(r) = rest.strip_prefix(']') {
                        return Ok((Value::Array(items), r));
                    }
                } else if let Some(r) = r.strip_prefix(']') {
                    return Ok((Value::Array(items), r));
                } else {
                    return Err("unterminated `[` list".into());
                }
            }
        }
        Some('{') => {
            let mut map = Map::new();
            let mut rest = s[1..].trim_start();
            if let Some(r) = rest.strip_prefix('}') {
                return Ok((Value::Object(map), r));
            }
            loop {
                let colon = rest.find(':').ok_or("expected `key: value` in `{}`")?;
                let key = rest[..colon].trim().trim_matches(['"', '\'']).to_string();
                let (v, r) = flow_item(&rest[colon + 1..], &[',', '}'])?;
                map.insert(key, v);
                let r = r.trim_start();
                if let Some(r) = r.strip_prefix(',') {
                    rest = r.trim_start();
                } else if let Some(r) = r.strip_prefix('}') {
                    return Ok((Value::Object(map), r));
                } else {
                    return Err("unterminated `{` map".into());
                }
            }
        }
        _ => Ok((plain(s), "")),
    }
}

/// One item inside a flow collection, ending at one of `ends` (outside brackets/quotes).
fn flow_item<'a>(s: &'a str, ends: &[char]) -> Result<(Value, &'a str), String> {
    let t = s.trim_start();
    if t.starts_with(['"', '\'', '[', '{']) {
        return inline_prefix(t);
    }
    let end = t
        .find(|c| ends.contains(&c))
        .ok_or("unterminated flow collection")?;
    Ok((plain(t[..end].trim()), &t[end..]))
}

fn plain(s: &str) -> Value {
    match s {
        "" | "~" | "null" | "Null" | "NULL" => Value::Null,
        "true" | "True" | "TRUE" => Value::Bool(true),
        "false" | "False" | "FALSE" => Value::Bool(false),
        _ => {
            if let Ok(i) = s.parse::<i64>() {
                return Value::Number(i.into());
            }
            if s.contains('.')
                && s.chars()
                    .all(|c| c.is_ascii_digit() || matches!(c, '.' | '-' | '+' | 'e' | 'E'))
                && let Some(n) = s.parse::<f64>().ok().and_then(Number::from_f64)
            {
                return Value::Number(n);
            }
            Value::String(s.to_string())
        }
    }
}

/// A list-of-strings field that may also be written as one string (`"Read, Grep"` or
/// `"Read Grep"`). Splits on commas and spaces outside parentheses, so
/// `Bash(git add:*)` stays one entry.
pub fn string_list(v: Option<&Value>) -> Result<Vec<String>, String> {
    match v {
        None | Some(Value::Null) => Ok(vec![]),
        Some(Value::String(s)) => Ok(split_tools(s)),
        Some(Value::Array(items)) => items
            .iter()
            .map(|i| match i {
                Value::String(s) => Ok(s.trim().to_string()),
                other => Err(format!("expected a string, found `{other}`")),
            })
            .filter(|r| r.as_ref().map(|s| !s.is_empty()).unwrap_or(true))
            .collect(),
        Some(other) => Err(format!("expected a list of strings, found `{other}`")),
    }
}

fn split_tools(s: &str) -> Vec<String> {
    let mut out = vec![];
    let mut cur = String::new();
    let mut depth = 0i32;
    for c in s.chars() {
        match c {
            '(' => {
                depth += 1;
                cur.push(c);
            }
            ')' => {
                depth -= 1;
                cur.push(c);
            }
            ',' | ' ' if depth <= 0 => {
                if !cur.trim().is_empty() {
                    out.push(cur.trim().to_string());
                }
                cur.clear();
            }
            c => cur.push(c),
        }
    }
    if !cur.trim().is_empty() {
        out.push(cur.trim().to_string());
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn split_variants() {
        assert_eq!(split("hello").unwrap(), (None, "hello"));
        assert_eq!(
            split("---\na: 1\n---\nbody\n").unwrap(),
            (Some("a: 1\n"), "body\n")
        );
        assert_eq!(
            split("---\r\na: 1\r\n---\r\nbody").unwrap(),
            (Some("a: 1\r\n"), "body")
        );
        assert_eq!(split("\u{feff}---\n---\n").unwrap(), (Some(""), ""));
        assert_eq!(split("---\na: 1\n"), Err(FrontmatterError::Unterminated));
    }

    #[test]
    fn parses_skill_shapes() {
        let src = r#"
name: pdf-tools   # trailing comment
description: "Extract text: from PDFs"
disable-model-invocation: true
allowed-tools: [Read, "Bash(python:*)"]
count: 3
ratio: 1.5
none: ~
metadata:
  author: acme
  tags:
    - pdf
    - 'it''s'
  nested: {a: 1, b: "x, y"}
long: >
  folded line one
  and two

  para
lit: |-
  keep
    indent
list_same_indent:
- a
- b
objs:
  - k: v
    j: w
  - plain
"#;
        let m = parse(src).unwrap();
        assert_eq!(
            Value::Object(m),
            json!({
                "name": "pdf-tools",
                "description": "Extract text: from PDFs",
                "disable-model-invocation": true,
                "allowed-tools": ["Read", "Bash(python:*)"],
                "count": 3,
                "ratio": 1.5,
                "none": null,
                "metadata": {"author": "acme", "tags": ["pdf", "it's"], "nested": {"a": 1, "b": "x, y"}},
                "long": "folded line one and two\npara\n",
                "lit": "keep\n  indent",
                "list_same_indent": ["a", "b"],
                "objs": [{"k": "v", "j": "w"}, "plain"],
            })
        );
    }

    #[test]
    fn rejects_bad_input() {
        assert!(matches!(
            parse("a: 1\na: 2"),
            Err(FrontmatterError::Syntax { line: 2, .. })
        ));
        assert!(parse("a: 1\n   b: 2").is_err());
        assert!(parse("just text").is_err());
        assert!(parse("a: \"open").is_err());
        assert!(parse("a: &anchor x").is_err());
        assert!(parse("a: [1, 2").is_err());
    }

    #[test]
    fn colons_and_urls_in_plain_scalars() {
        let m = parse("url: https://x.y/z\nk: a:b").unwrap();
        assert_eq!(m["url"], json!("https://x.y/z"));
        assert_eq!(m["k"], json!("a:b"));
    }

    #[test]
    fn tool_lists() {
        assert_eq!(
            string_list(Some(&json!("Read, Bash(git add:*) Grep"))).unwrap(),
            vec!["Read", "Bash(git add:*)", "Grep"]
        );
        assert_eq!(
            string_list(Some(&json!(["Read", " "]))).unwrap(),
            vec!["Read"]
        );
        assert!(string_list(Some(&json!(3))).is_err());
        assert!(string_list(None).unwrap().is_empty());
    }
}
