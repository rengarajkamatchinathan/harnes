//! Central truncation and spill of tool output (06 §2.4, t11).
//!
//! Output over 2,000 lines or 50 KiB is cut; the full text goes to a [`SpillStore`] and
//! the model sees `[showing 1–N of M; full output: <ref>]`, which `tool_output_read`
//! (Chat, SA-39) or `read` (Code) can page.

use std::collections::HashMap;
use std::sync::Mutex;

use crate::tool::SpillStore;

pub const DEFAULT_MAX_LINES: usize = 2_000;
pub const DEFAULT_MAX_BYTES: usize = 50 * 1024;

/// `toolOutput.maxLines / maxBytes`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OutputLimits {
    pub max_lines: usize,
    pub max_bytes: usize,
}

impl Default for OutputLimits {
    fn default() -> Self {
        OutputLimits {
            max_lines: DEFAULT_MAX_LINES,
            max_bytes: DEFAULT_MAX_BYTES,
        }
    }
}

/// What the model gets after limits were applied.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Limited {
    pub text: String,
    /// Set when the full output was spilled.
    pub spill_ref: Option<String>,
    pub truncated: bool,
}

/// The first lines of `text` that fit both limits: `(kept, lines_kept)`.
pub fn head(text: &str, limits: OutputLimits) -> (&str, usize) {
    let mut end = 0;
    let mut n = 0;
    for line in text.split_inclusive('\n') {
        if n >= limits.max_lines || end + line.len() > limits.max_bytes {
            break;
        }
        end += line.len();
        n += 1;
    }
    if n == 0 && !text.is_empty() {
        // A single over-long line: cut it on a char boundary.
        let mut cut = limits.max_bytes.min(text.len());
        while !text.is_char_boundary(cut) {
            cut -= 1;
        }
        return (&text[..cut], 1);
    }
    (&text[..end], n)
}

pub fn line_count(text: &str) -> usize {
    if text.is_empty() {
        0
    } else {
        text.split_inclusive('\n').count()
    }
}

/// Apply the limits; spill the full text when it is cut and a store is present.
pub fn apply_limits(
    text: String,
    limits: OutputLimits,
    spill: Option<&dyn SpillStore>,
    session_id: &str,
) -> Limited {
    let total_lines = line_count(&text);
    if total_lines <= limits.max_lines && text.len() <= limits.max_bytes {
        return Limited {
            text,
            spill_ref: None,
            truncated: false,
        };
    }
    let (kept, n) = head(&text, limits);
    let mut out = kept.trim_end_matches('\n').to_string();
    // A failed spill degrades to plain truncation (the marker says so).
    let spill_ref = spill.and_then(|s| s.put(session_id, &text).ok());
    match &spill_ref {
        Some(r) => out.push_str(&format!(
            "\n\n[showing 1–{n} of {total_lines}; full output: {r}]"
        )),
        None => out.push_str(&format!(
            "\n\n[showing 1–{n} of {total_lines}; the rest was cut]"
        )),
    }
    Limited {
        text: out,
        spill_ref,
        truncated: true,
    }
}

/// In-memory spill store (tests, incognito sessions).
#[derive(Default)]
pub struct MemorySpill {
    map: Mutex<HashMap<String, (String, String)>>,
}

impl MemorySpill {
    pub fn new() -> Self {
        Self::default()
    }
}

impl SpillStore for MemorySpill {
    fn put(&self, session_id: &str, text: &str) -> Result<String, String> {
        let mut m = self.map.lock().map_err(|_| "spill lock poisoned")?;
        let r = format!("spill_{}", m.len() + 1);
        m.insert(r.clone(), (session_id.to_string(), text.to_string()));
        Ok(r)
    }

    fn read(&self, session_id: &str, reference: &str) -> Result<String, String> {
        let m = self.map.lock().map_err(|_| "spill lock poisoned")?;
        match m.get(reference) {
            Some((s, t)) if s == session_id => Ok(t.clone()),
            _ => Err(format!("no spilled output {reference} in this session")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn small_output_passes_through() {
        let l = apply_limits("a\nb".into(), OutputLimits::default(), None, "s");
        assert_eq!(l.text, "a\nb");
        assert!(!l.truncated);
    }

    #[test]
    fn long_output_is_spilled_with_a_marker() {
        let text: String = (1..=10).map(|i| format!("line {i}\n")).collect();
        let spill = MemorySpill::new();
        let lim = OutputLimits {
            max_lines: 3,
            max_bytes: 1024,
        };
        let l = apply_limits(text.clone(), lim, Some(&spill), "s");
        assert!(l.truncated);
        let r = l.spill_ref.clone().unwrap();
        assert_eq!(
            l.text,
            format!("line 1\nline 2\nline 3\n\n[showing 1–3 of 10; full output: {r}]")
        );
        assert_eq!(spill.read("s", &r).unwrap(), text);
        assert!(spill.read("other", &r).is_err());
    }

    #[test]
    fn byte_limit_and_long_single_line() {
        let lim = OutputLimits {
            max_lines: 100,
            max_bytes: 8,
        };
        let (h, n) = head("ééééééééé", lim);
        assert_eq!((h, n), ("éééé", 1));
        let (h, n) = head("abc\ndefgh\nij", lim);
        assert_eq!((h, n), ("abc\n", 1));
    }
}
