//! The `edit` matcher (06 §4.2): exact → Unicode-normalised → trailing-whitespace
//! tolerant, with OpenCode's uniqueness rule (a non-unique match fails unless
//! `replace_all`) and a "closest lines" hint on a miss.
//!
//! The tiers follow OpenCode's replacer chain (MIT); the code is written for Genie.

use std::ops::Range;

/// Which tier matched.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tier {
    Exact,
    /// Curly quotes, dashes and non-breaking spaces folded to ASCII.
    Unicode,
    /// Lines compared without trailing whitespace.
    TrailingWhitespace,
}

impl Tier {
    pub fn note(self) -> Option<&'static str> {
        match self {
            Tier::Exact => None,
            Tier::Unicode => Some("matched after normalising Unicode punctuation"),
            Tier::TrailingWhitespace => Some("matched ignoring trailing whitespace"),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MatchError {
    NotFound { hint: Option<String> },
    NotUnique { count: usize },
}

impl MatchError {
    pub fn message(&self, what: &str) -> String {
        match self {
            MatchError::NotFound { hint } => {
                let mut s = format!(
                    "{what} was not found in the file. It must match the file exactly \
                     (including indentation); read the file again if unsure."
                );
                if let Some(h) = hint {
                    s.push_str("\n\n");
                    s.push_str(h);
                }
                s
            }
            MatchError::NotUnique { count } => format!(
                "{what} matches {count} places in the file. Include more surrounding lines to \
                 make it unique, or set replace_all to change every occurrence."
            ),
        }
    }
}

/// Replace `old` with `new` in `text` (both in the LF view). Returns the new text, the
/// number of replacements and the tier that matched.
pub fn replace(
    text: &str,
    old: &str,
    new: &str,
    replace_all: bool,
) -> Result<(String, usize, Tier), MatchError> {
    for (tier, ranges) in [
        (Tier::Exact, exact(text, old)),
        (Tier::Unicode, unicode(text, old)),
        (Tier::TrailingWhitespace, trailing_ws(text, old)),
    ] {
        match ranges.len() {
            0 => continue,
            1 => return Ok((splice(text, &ranges, new), 1, tier)),
            n if replace_all => return Ok((splice(text, &ranges, new), n, tier)),
            n => return Err(MatchError::NotUnique { count: n }),
        }
    }
    Err(MatchError::NotFound {
        hint: closest(text, old),
    })
}

fn splice(text: &str, ranges: &[Range<usize>], new: &str) -> String {
    let mut out = String::with_capacity(text.len() + new.len());
    let mut at = 0;
    for r in ranges {
        out.push_str(&text[at..r.start]);
        out.push_str(new);
        at = r.end;
    }
    out.push_str(&text[at..]);
    out
}

/// Non-overlapping exact occurrences.
fn exact(text: &str, old: &str) -> Vec<Range<usize>> {
    if old.is_empty() {
        return vec![];
    }
    text.match_indices(old)
        .map(|(i, m)| i..i + m.len())
        .collect()
}

/// Fold typographic punctuation to ASCII, one char for one char.
fn fold(c: char) -> char {
    match c {
        '\u{2018}' | '\u{2019}' | '\u{201A}' | '\u{201B}' | '\u{2032}' => '\'',
        '\u{201C}' | '\u{201D}' | '\u{201E}' | '\u{201F}' | '\u{2033}' => '"',
        '\u{2010}' | '\u{2011}' | '\u{2012}' | '\u{2013}' | '\u{2014}' | '\u{2015}'
        | '\u{2212}' => '-',
        '\u{00A0}' | '\u{2007}' | '\u{202F}' | '\u{2009}' | '\u{200A}' => ' ',
        other => other,
    }
}

fn unicode(text: &str, old: &str) -> Vec<Range<usize>> {
    if old.is_empty() {
        return vec![];
    }
    // Folded text + the original byte offset of every folded char boundary.
    let mut folded = String::with_capacity(text.len());
    let mut map: Vec<usize> = Vec::with_capacity(text.len() + 1);
    for (i, c) in text.char_indices() {
        let f = fold(c);
        for _ in 0..f.len_utf8() {
            map.push(i);
        }
        folded.push(f);
    }
    map.push(text.len());
    let fold_old: String = old.chars().map(fold).collect();
    if fold_old == old && folded == text {
        return vec![]; // nothing to normalise: the exact tier already decided
    }
    folded
        .match_indices(&fold_old)
        .map(|(i, m)| map[i]..map[i + m.len()])
        .collect()
}

/// Whole lines compared with trailing whitespace removed.
fn trailing_ws(text: &str, old: &str) -> Vec<Range<usize>> {
    let mut old_lines: Vec<&str> = old.split('\n').collect();
    let keep_newline = old.ends_with('\n');
    if keep_newline {
        old_lines.pop();
    }
    if old_lines.is_empty() || old_lines.iter().all(|l| l.trim().is_empty()) {
        return vec![];
    }
    let want: Vec<&str> = old_lines.iter().map(|l| l.trim_end()).collect();
    // (start, end-without-newline) of each text line.
    let mut lines: Vec<(usize, usize)> = Vec::new();
    let mut at = 0;
    for l in text.split('\n') {
        lines.push((at, at + l.len()));
        at += l.len() + 1;
    }
    let mut out = Vec::new();
    let mut i = 0;
    while i + want.len() <= lines.len() {
        let ok = want
            .iter()
            .enumerate()
            .all(|(k, w)| text[lines[i + k].0..lines[i + k].1].trim_end() == *w);
        if ok {
            let start = lines[i].0;
            let last = lines[i + want.len() - 1];
            let mut end = last.1;
            if keep_newline && end < text.len() {
                end += 1;
            }
            out.push(start..end);
            i += want.len();
        } else {
            i += 1;
        }
    }
    out
}

/// "Closest lines" hint: the file line most similar to the first non-blank line of
/// `old`, with two lines of context, numbered.
pub fn closest(text: &str, old: &str) -> Option<String> {
    let probe = old.lines().find(|l| !l.trim().is_empty())?.trim();
    if probe.is_empty() {
        return None;
    }
    let lines: Vec<&str> = text.lines().collect();
    let mut best: Option<(f64, usize)> = None;
    for (i, l) in lines.iter().enumerate() {
        let t = l.trim();
        if t.is_empty() {
            continue;
        }
        let s = similarity(probe, t);
        if best.is_none_or(|(b, _)| s > b) {
            best = Some((s, i));
        }
    }
    let (score, at) = best?;
    if score < 0.5 {
        return None;
    }
    let from = at.saturating_sub(2);
    let to = (at + 3).min(lines.len());
    let mut s = format!("Closest lines in the file (line {}):\n", at + 1);
    for (k, l) in lines[from..to].iter().enumerate() {
        s.push_str(&format!("{:>6}\t{l}\n", from + k + 1));
    }
    Some(s.trim_end().to_string())
}

/// 1 − normalised Levenshtein distance on chars (inputs capped at 200 chars).
fn similarity(a: &str, b: &str) -> f64 {
    let a: Vec<char> = a.chars().take(200).collect();
    let b: Vec<char> = b.chars().take(200).collect();
    let longest = a.len().max(b.len());
    if longest == 0 {
        return 1.0;
    }
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    let mut cur = vec![0; b.len() + 1];
    for i in 1..=a.len() {
        cur[0] = i;
        for j in 1..=b.len() {
            let cost = usize::from(a[i - 1] != b[j - 1]);
            cur[j] = (prev[j] + 1).min(cur[j - 1] + 1).min(prev[j - 1] + cost);
        }
        std::mem::swap(&mut prev, &mut cur);
    }
    1.0 - prev[b.len()] as f64 / longest as f64
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exact_unique_and_non_unique() {
        let (t, n, tier) = replace("a b c", "b", "x", false).unwrap();
        assert_eq!((t.as_str(), n, tier), ("a x c", 1, Tier::Exact));
        assert_eq!(
            replace("b b", "b", "x", false),
            Err(MatchError::NotUnique { count: 2 })
        );
        assert_eq!(replace("b b", "b", "x", true).unwrap().0, "x x");
    }

    #[test]
    fn unicode_tier_maps_back_to_original_bytes() {
        let text = "say \u{201C}hi\u{201D} \u{2014} ok";
        let (t, _, tier) = replace(text, "\"hi\" - ok", "bye", false).unwrap();
        assert_eq!(tier, Tier::Unicode);
        assert_eq!(t, "say bye");
    }

    #[test]
    fn trailing_whitespace_tier() {
        let text = "fn a() {   \n    x();\t\n}\nrest\n";
        let (t, _, tier) = replace(text, "fn a() {\n    x();\n}\n", "gone\n", false).unwrap();
        assert_eq!(tier, Tier::TrailingWhitespace);
        assert_eq!(t, "gone\nrest\n");
    }

    #[test]
    fn miss_gives_closest_lines() {
        let text = "one\nlet value = compute(1);\nthree\n";
        let Err(MatchError::NotFound { hint }) =
            replace(text, "let valu = compute(2);", "x", false)
        else {
            panic!()
        };
        let h = hint.unwrap();
        assert!(h.contains("line 2"), "{h}");
        assert!(h.contains("     2\tlet value = compute(1);"), "{h}");
        let Err(MatchError::NotFound { hint }) = replace(text, "zzzzzzzzzzzz", "x", false) else {
            panic!()
        };
        assert!(hint.is_none());
    }
}
