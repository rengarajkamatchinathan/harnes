//! Streaming Markdown for the inline TUI (07 §1.1–1.2).
//!
//! Explicit block stability: [`stable_prefix`] says how much of a growing text is made of
//! settled blocks, which are rendered once and committed to scrollback; the unstable tail
//! is re-rendered in the footer on every delta. The renderer "heals" unclosed inline
//! markers (bold, code) at end of line so the tail styles correctly while streaming.
//! Offsets are byte offsets.
//!
//! This is the CLI's own small renderer; the full patchmark port belongs in
//! `genie-markdown` (see the README gaps).

use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use unicode_width::UnicodeWidthChar;

use super::theme::{Role, Theme};

fn fence_marker(line: &str) -> Option<&str> {
    let t = line.trim_start_matches(' ');
    if line.len() - t.len() > 3 {
        return None;
    }
    if t.starts_with("```") {
        Some("```")
    } else if t.starts_with("~~~") {
        Some("~~~")
    } else {
        None
    }
}

/// Byte offset up to which the text consists of complete blocks: a block ends at a blank
/// line, after a complete heading line, or at a closing code fence. Never inside a fence.
pub fn stable_prefix(text: &str) -> usize {
    let mut stable = 0;
    let mut pos = 0;
    let mut fence: Option<&str> = None;
    for line in text.split_inclusive('\n') {
        let end = pos + line.len();
        if !line.ends_with('\n') {
            break; // an incomplete line is never settled
        }
        let body = line.trim_end_matches(['\n', '\r']);
        match fence {
            Some(open) => {
                if fence_marker(body) == Some(open)
                    && body.trim().trim_start_matches(open).is_empty()
                {
                    fence = None;
                    stable = end;
                }
            }
            None => {
                if let Some(m) = fence_marker(body) {
                    fence = Some(m);
                } else if body.trim().is_empty() || body.trim_start().starts_with('#') {
                    stable = end;
                }
            }
        }
        pos = end;
    }
    stable
}

/// Render Markdown into styled lines (not wrapped).
pub fn render(text: &str, theme: &Theme) -> Vec<Line<'static>> {
    let mut out = vec![];
    let mut fence: Option<(String, String)> = None; // (marker, lang)
    for raw in text.lines() {
        if let Some((marker, lang)) = &fence {
            if fence_marker(raw) == Some(marker.as_str())
                && raw.trim().trim_start_matches(marker.as_str()).is_empty()
            {
                if lang == "mermaid" {
                    out.push(Line::from(Span::styled(
                        "  ↗ Mermaid diagram — open in desktop to view".to_string(),
                        theme.style(Role::Link),
                    )));
                }
                fence = None;
                continue;
            }
            out.push(code_line(raw, lang, theme));
            continue;
        }
        if let Some(m) = fence_marker(raw) {
            let lang = raw.trim().trim_start_matches(m).trim().to_lowercase();
            if !lang.is_empty() {
                out.push(Line::from(Span::styled(
                    format!("  ┌ {lang}"),
                    theme.style(Role::Muted),
                )));
            }
            fence = Some((m.to_string(), lang));
            continue;
        }
        out.push(block_line(raw, theme));
    }
    out
}

fn code_line(raw: &str, lang: &str, theme: &Theme) -> Line<'static> {
    let role = if lang == "diff" || lang == "patch" {
        if raw.starts_with('+') && !raw.starts_with("+++") {
            Role::DiffAdd
        } else if raw.starts_with('-') && !raw.starts_with("---") {
            Role::DiffDel
        } else if raw.starts_with("@@") {
            Role::DiffHunk
        } else {
            Role::Muted
        }
    } else {
        Role::Code
    };
    Line::from(vec![
        Span::styled("  │ ".to_string(), theme.style(Role::Muted)),
        Span::styled(raw.to_string(), theme.style(role)),
    ])
}

fn block_line(raw: &str, theme: &Theme) -> Line<'static> {
    let trimmed = raw.trim_start();
    let indent = &raw[..raw.len() - trimmed.len()];
    if let Some(rest) = heading(trimmed) {
        let mut spans = inline(rest, theme);
        for s in &mut spans {
            s.style = s.style.patch(theme.style(Role::Heading));
        }
        return Line::from(spans);
    }
    if matches!(trimmed, "---" | "***" | "___") {
        return Line::from(Span::styled("─".repeat(40), theme.style(Role::Muted)));
    }
    if let Some(rest) = trimmed
        .strip_prefix("> ")
        .or(trimmed.strip_prefix('>').filter(|_| trimmed.len() == 1))
    {
        let mut spans = vec![Span::styled("│ ".to_string(), theme.style(Role::Muted))];
        spans.extend(inline(rest, theme));
        return Line::from(spans);
    }
    for bullet in ["- ", "* ", "+ "] {
        if let Some(rest) = trimmed.strip_prefix(bullet) {
            let mut spans = vec![Span::raw(format!("{indent}• "))];
            if let Some(r) = rest.strip_prefix("[ ] ") {
                spans.push(Span::raw("☐ "));
                spans.extend(inline(r, theme));
            } else if let Some(r) = rest.strip_prefix("[x] ").or(rest.strip_prefix("[X] ")) {
                spans.push(Span::raw("☑ "));
                spans.extend(inline(r, theme));
            } else {
                spans.extend(inline(rest, theme));
            }
            return Line::from(spans);
        }
    }
    let mut spans = vec![];
    if !indent.is_empty() {
        spans.push(Span::raw(indent.to_string()));
    }
    spans.extend(inline(trimmed, theme));
    Line::from(spans)
}

fn heading(s: &str) -> Option<&str> {
    let hashes = s.chars().take_while(|c| *c == '#').count();
    if (1..=6).contains(&hashes) && s[hashes..].starts_with(' ') {
        Some(s[hashes..].trim())
    } else {
        None
    }
}

/// Inline spans: `code`, **bold**, *italic* / _italic_, [text](url). Unclosed markers
/// run to the end of the line (the heal step).
pub fn inline(s: &str, theme: &Theme) -> Vec<Span<'static>> {
    let mut spans: Vec<Span<'static>> = vec![];
    let mut buf = String::new();
    let mut bold = false;
    let mut italic = false;
    let flush = |buf: &mut String, spans: &mut Vec<Span<'static>>, bold: bool, italic: bool| {
        if buf.is_empty() {
            return;
        }
        let mut st = Style::default();
        if bold {
            st = st.add_modifier(Modifier::BOLD);
        }
        if italic {
            st = st.add_modifier(Modifier::ITALIC);
        }
        spans.push(Span::styled(std::mem::take(buf), st));
    };
    let mut rest = s;
    while let Some(c) = rest.chars().next() {
        if c == '`' {
            flush(&mut buf, &mut spans, bold, italic);
            let after = &rest[1..];
            let (code, next) = match after.find('`') {
                Some(i) => (&after[..i], &after[i + 1..]),
                None => (after, ""),
            };
            spans.push(Span::styled(code.to_string(), theme.style(Role::Code)));
            rest = next;
            continue;
        }
        if rest.starts_with("**") || rest.starts_with("__") {
            flush(&mut buf, &mut spans, bold, italic);
            bold = !bold;
            rest = &rest[2..];
            continue;
        }
        if (c == '*' || c == '_')
            && !rest[1..].starts_with(' ')
            && (c == '*' || buf.is_empty() || buf.ends_with(' '))
        {
            flush(&mut buf, &mut spans, bold, italic);
            italic = !italic;
            rest = &rest[1..];
            continue;
        }
        if c == '['
            && let Some(close) = rest.find("](")
            && let Some(end) = rest[close + 2..].find(')')
        {
            flush(&mut buf, &mut spans, bold, italic);
            let label = &rest[1..close];
            let url = &rest[close + 2..close + 2 + end];
            spans.push(Span::styled(label.to_string(), theme.style(Role::Link)));
            if url != label {
                spans.push(Span::styled(format!(" ({url})"), theme.style(Role::Muted)));
            }
            rest = &rest[close + 3 + end..];
            continue;
        }
        buf.push(c);
        rest = &rest[c.len_utf8()..];
    }
    flush(&mut buf, &mut spans, bold, italic);
    spans
}

/// Hard-wrap lines to `width` display columns (char-level; spans keep their style).
pub fn wrap(lines: Vec<Line<'static>>, width: u16) -> Vec<Line<'static>> {
    let width = usize::from(width.max(10));
    let mut out = vec![];
    for line in lines {
        let mut cur: Vec<Span<'static>> = vec![];
        let mut col = 0;
        for span in line.spans {
            let mut piece = String::new();
            for ch in span.content.chars() {
                let w = ch.width().unwrap_or(0);
                if col + w > width {
                    if !piece.is_empty() {
                        cur.push(Span::styled(std::mem::take(&mut piece), span.style));
                    }
                    out.push(Line::from(std::mem::take(&mut cur)));
                    col = 0;
                }
                piece.push(ch);
                col += w;
            }
            if !piece.is_empty() {
                cur.push(Span::styled(piece, span.style));
            }
        }
        out.push(Line::from(cur));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn plain(lines: &[Line<'_>]) -> Vec<String> {
        lines
            .iter()
            .map(|l| l.spans.iter().map(|s| s.content.as_ref()).collect())
            .collect()
    }

    #[test]
    fn stability_follows_blocks() {
        assert_eq!(stable_prefix("Hello wor"), 0);
        assert_eq!(stable_prefix("Hello world\n"), 0);
        let t = "Para one\n\nPara two in prog";
        assert_eq!(stable_prefix(t), "Para one\n\n".len());
        let t = "# Title\nbody";
        assert_eq!(stable_prefix(t), "# Title\n".len());
        // Never settle inside an open fence, even across blank lines.
        let t = "```rust\nfn a() {}\n\nfn b() {}\n";
        assert_eq!(stable_prefix(t), 0);
        let t = "```rust\nfn a() {}\n```\nmore";
        assert_eq!(stable_prefix(t), "```rust\nfn a() {}\n```\n".len());
    }

    #[test]
    fn renders_blocks_and_heals_inline() {
        let th = Theme::default();
        let l = render("# Hi\n- **bold** and `co\n> quote", &th);
        assert_eq!(plain(&l), ["Hi", "• bold and co", "│ quote"]);
        assert!(l[1].spans[1].style.add_modifier.contains(Modifier::BOLD));
        // Unclosed bold heals to the end of the line.
        let l = render("so **stro", &th);
        assert!(l[0].spans[1].style.add_modifier.contains(Modifier::BOLD));
    }

    #[test]
    fn diff_and_mermaid_blocks() {
        let th = Theme {
            depth: super::super::theme::ColorDepth::TrueColor,
            dark: true,
        };
        let l = render("```diff\n+a\n-b\n```\n```mermaid\ngraph TD\n```", &th);
        let p = plain(&l);
        assert_eq!(p[0], "  ┌ diff");
        assert_eq!(p[1], "  │ +a");
        assert_eq!(l[1].spans[1].style.fg, th.style(Role::DiffAdd).fg);
        assert_eq!(l[2].spans[1].style.fg, th.style(Role::DiffDel).fg);
        assert!(p.iter().any(|x| x.contains("open in desktop")));
    }

    #[test]
    fn links_and_wrap() {
        let th = Theme::default();
        let l = render("see [docs](https://d.test)", &th);
        assert_eq!(plain(&l), ["see docs (https://d.test)"]);
        let w = wrap(vec![Line::from("abcdefghijklmnop")], 10);
        assert_eq!(plain(&w), ["abcdefghij", "klmnop"]);
    }
}
