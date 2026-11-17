//! Per-server log ring (`mcp.logs {name, tail}`): stderr lines, server `notifications/message`
//! and lifecycle transitions, newest last. Bounded by line count and line length.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

pub const DEFAULT_LINES: usize = 1000;
pub const MAX_LINE_CHARS: usize = 4096;

#[derive(Clone)]
pub struct LogRing {
    inner: Arc<Mutex<VecDeque<String>>>,
    cap: usize,
}

impl Default for LogRing {
    fn default() -> LogRing {
        LogRing::new(DEFAULT_LINES)
    }
}

impl LogRing {
    pub fn new(cap: usize) -> LogRing {
        LogRing {
            inner: Arc::new(Mutex::new(VecDeque::new())),
            cap: cap.max(1),
        }
    }

    /// Append `[source] text`, one entry per line of `text`.
    pub fn push(&self, source: &str, text: &str) {
        let ts = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| d.as_millis());
        let Ok(mut g) = self.inner.lock() else { return };
        for line in text.lines() {
            let line = line.trim_end_matches('\r');
            let mut l = format!("{ts} [{source}] ");
            if line.chars().count() > MAX_LINE_CHARS {
                l.extend(line.chars().take(MAX_LINE_CHARS));
                l.push('…');
            } else {
                l.push_str(line);
            }
            if g.len() == self.cap {
                g.pop_front();
            }
            g.push_back(l);
        }
    }

    /// The last `n` lines, oldest first.
    pub fn tail(&self, n: usize) -> Vec<String> {
        let Ok(g) = self.inner.lock() else {
            return Vec::new();
        };
        let skip = g.len().saturating_sub(n);
        g.iter().skip(skip).cloned().collect()
    }

    pub fn len(&self) -> usize {
        self.inner.lock().map_or(0, |g| g.len())
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bounded_ring() {
        let r = LogRing::new(3);
        r.push("stderr", "a\nb\nc\nd");
        assert_eq!(r.len(), 3);
        let t = r.tail(2);
        assert!(t[0].ends_with("[stderr] c"));
        assert!(t[1].ends_with("[stderr] d"));
        r.push("x", &"y".repeat(5000));
        assert!(r.tail(1)[0].ends_with('…'));
    }
}
