//! Chunking for project retrieval (04 §7, D32).
//!
//! ~800 tokens per chunk with overlap, split per file type: Markdown at headings, code at
//! top-level items (unindented lines after a blank line), PDF by page (a page break always
//! ends a chunk), plain text by paragraph. A section longer than a chunk is cut at the
//! last paragraph or line break that fits, and the next chunk then repeats ~100 tokens
//! of the previous one (the overlap). Byte ranges index into the file's extracted text.

use crate::runtime::context::estimate_text;
use crate::runtime::extract::{PAGE_BREAK, TextKind};

/// Target chunk size in tokens (04 §7) and its byte equivalent (4 chars/token).
pub const CHUNK_TOKENS: usize = 800;
pub const CHUNK_BYTES: usize = CHUNK_TOKENS * 4;
/// Overlap carried into the next chunk when a section had to be cut (~100 tokens).
pub const OVERLAP_BYTES: usize = 400;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Span {
    pub start: usize,
    pub end: usize,
    pub tokens: u64,
}

/// Byte offsets where a line starts.
fn line_starts(text: &str) -> Vec<usize> {
    let mut v = vec![0];
    for (i, b) in text.bytes().enumerate() {
        if b == b'\n' && i + 1 < text.len() {
            v.push(i + 1);
        }
    }
    v
}

/// Split points: sections (headings, top-level items), paragraphs, and hard ones (pages).
fn boundaries(text: &str, kind: TextKind) -> [Vec<usize>; 3] {
    let starts = line_starts(text);
    let mut soft = vec![];
    let mut para = vec![];
    let mut hard = vec![];
    let mut in_fence = false;
    let mut prev_blank = true;
    for &s in &starts {
        let line_end = text[s..].find('\n').map_or(text.len(), |e| s + e);
        let line = &text[s..line_end];
        let trimmed = line.trim_end();
        let blank = trimmed.trim().is_empty();
        if prev_blank && !blank && !in_fence {
            para.push(s);
        }
        match kind {
            TextKind::Markdown => {
                if trimmed.starts_with("```") || trimmed.starts_with("~~~") {
                    in_fence = !in_fence;
                }
                if !in_fence && trimmed.starts_with('#') {
                    soft.push(s);
                }
            }
            TextKind::Code => {
                let top = !blank
                    && !line.starts_with([' ', '\t', '}', ')', ']'])
                    && !trimmed.starts_with("//")
                    && !trimmed.starts_with('#');
                if prev_blank && top {
                    soft.push(s);
                }
            }
            TextKind::Pdf => {
                if line.contains(PAGE_BREAK) {
                    hard.push(s);
                }
            }
            TextKind::Plain => {}
        }
        prev_blank = blank;
    }
    for v in [&mut soft, &mut para, &mut hard] {
        v.retain(|&b| b > 0);
    }
    [soft, para, hard]
}

fn char_floor(text: &str, mut i: usize) -> usize {
    i = i.min(text.len());
    while !text.is_char_boundary(i) {
        i -= 1;
    }
    i
}

/// Split `text` into chunk spans.
pub fn chunk(text: &str, kind: TextKind) -> Vec<Span> {
    if text.trim().is_empty() {
        return vec![];
    }
    let [soft, para, hard] = boundaries(text, kind);
    let lines = line_starts(text);
    let mut out = vec![];
    let mut s = 0usize;
    while s < text.len() {
        let limit = s + CHUNK_BYTES;
        let min = s + CHUNK_BYTES / 4;
        // A page break inside the window always ends the chunk.
        let page = hard.iter().copied().find(|&h| h > s && h <= limit);
        let (end, clean) = if let Some(h) = page {
            (h, true)
        } else if limit >= text.len() {
            (text.len(), true)
        } else if let Some(b) = [&soft, &para]
            .iter()
            .find_map(|v| v.iter().copied().rev().find(|&b| b > min && b <= limit))
        {
            (b, true)
        } else if let Some(b) = lines.iter().copied().rev().find(|&b| b > min && b <= limit) {
            (b, false)
        } else {
            (char_floor(text, limit).max(s + 1), false)
        };
        let end = char_floor(text, end).max(char_floor(text, s + 1).max(s + 1));
        let piece = &text[s..end];
        if !piece.trim().trim_matches(PAGE_BREAK).trim().is_empty() {
            out.push(Span {
                start: s,
                end,
                tokens: estimate_text(piece),
            });
        }
        if end >= text.len() {
            break;
        }
        s = if clean {
            end
        } else {
            // Overlap: restart ~OVERLAP_BYTES back, at a line start when there is one.
            let back = end.saturating_sub(OVERLAP_BYTES).max(s + 1);
            let at = lines
                .iter()
                .copied()
                .find(|&l| l >= back && l < end)
                .unwrap_or(back);
            char_floor(text, at).max(s + 1)
        };
    }
    out
}

/// 1-based inclusive line range of `start..end` in `text`.
pub fn line_range(text: &str, start: usize, end: usize) -> (usize, usize) {
    let start = char_floor(text, start);
    let end = char_floor(text, end.max(start));
    let a = text[..start].matches('\n').count() + 1;
    let inner = text[start..end]
        .trim_end_matches('\n')
        .matches('\n')
        .count();
    (a, a + inner)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn markdown_splits_at_headings_and_covers_everything() {
        let mut md = String::new();
        for i in 0..6 {
            md.push_str(&format!("# Section {i}\n\n"));
            md.push_str(&"word ".repeat(300));
            md.push_str("\n\n");
        }
        let spans = chunk(&md, TextKind::Markdown);
        assert!(spans.len() >= 3, "{spans:?}");
        assert!(spans.iter().all(|s| s.end - s.start <= CHUNK_BYTES));
        assert!(spans.iter().skip(1).all(|s| md[s.start..].starts_with('#')));
        assert_eq!(spans.first().map(|s| s.start), Some(0));
        assert_eq!(spans.last().map(|s| s.end), Some(md.len()));
    }

    #[test]
    fn long_paragraph_is_cut_with_overlap() {
        let text = (0..400)
            .map(|i| format!("line {i} with some words"))
            .collect::<Vec<_>>()
            .join("\n");
        let spans = chunk(&text, TextKind::Plain);
        assert!(spans.len() >= 3);
        for w in spans.windows(2) {
            assert!(w[1].start < w[0].end, "overlap expected: {w:?}");
            assert!(w[0].end - w[1].start <= OVERLAP_BYTES + 40);
        }
        assert_eq!(spans.last().map(|s| s.end), Some(text.len()));
    }

    #[test]
    fn pdf_pages_always_split() {
        let text = format!("page one\n{PAGE_BREAK}\npage two\n{PAGE_BREAK}\npage three");
        let spans = chunk(&text, TextKind::Pdf);
        assert_eq!(spans.len(), 3, "{spans:?}");
        assert!(text[spans[1].start..spans[1].end].contains("page two"));
    }

    #[test]
    fn code_splits_at_top_level_items() {
        let mut code = String::new();
        for i in 0..40 {
            code.push_str(&format!("fn f{i}() {{\n"));
            code.push_str(&"    let x = 1;\n".repeat(10));
            code.push_str("}\n\n");
        }
        let spans = chunk(&code, TextKind::Code);
        assert!(spans.len() > 1);
        assert!(spans.iter().all(|s| code[s.start..].starts_with("fn ")));
    }

    #[test]
    fn lines() {
        let t = "a\nb\nc\nd\n";
        assert_eq!(line_range(t, 0, 2), (1, 1));
        assert_eq!(line_range(t, 2, 8), (2, 4));
        assert!(chunk("  \n ", TextKind::Plain).is_empty());
    }
}
