//! Console capture (ticket 47 phase 3): `console.*` calls, uncaught exceptions and browser
//! log entries (failed resource loads, CSP and deprecation warnings) per tab, for
//! `browser_console` and auto-verify.
//!
//! Sources, enabled on every attached page (`Runtime.enable`, `Log.enable`):
//! * `Runtime.consoleAPICalled`: `console.log/info/warn/error/debug/assert/trace`.
//! * `Runtime.exceptionThrown`: uncaught errors and unhandled rejections.
//! * `Log.entryAdded`: the browser's own messages (`network` 404s, `security`, …).
//!
//! A tab's log is cleared when its main frame navigates, so after a reload it holds only
//! what the new page logged. At most [`MAX_ENTRIES`] per tab, each cut to [`MAX_TEXT`].

use std::collections::{HashMap, VecDeque};

use serde_json::Value;

pub const MAX_ENTRIES: usize = 300;
pub const MAX_TEXT: usize = 2_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Level {
    Debug,
    Log,
    Info,
    Warning,
    Error,
}

impl Level {
    fn parse(s: &str) -> Level {
        match s {
            "error" | "assert" => Level::Error,
            "warning" | "warn" => Level::Warning,
            "info" => Level::Info,
            "debug" | "verbose" => Level::Debug,
            _ => Level::Log,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Level::Debug => "debug",
            Level::Log => "log",
            Level::Info => "info",
            Level::Warning => "warning",
            Level::Error => "error",
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Entry {
    pub level: Level,
    /// `console`, `exception`, or the `Log` source (`network`, `security`, `javascript` …).
    pub source: String,
    pub text: String,
    /// `url:line` of the call site or failed resource, when known.
    pub location: Option<String>,
}

impl Entry {
    pub fn line(&self) -> String {
        let src = if self.source == "console" {
            String::new()
        } else {
            format!(" ({})", self.source)
        };
        match &self.location {
            Some(l) => format!("[{}{src}] {} — {l}", self.level.as_str(), self.text),
            None => format!("[{}{src}] {}", self.level.as_str(), self.text),
        }
    }
}

fn cut(s: &str) -> String {
    let s = s.trim();
    if s.chars().count() <= MAX_TEXT {
        return s.to_string();
    }
    let mut t: String = s.chars().take(MAX_TEXT).collect();
    t.push('…');
    t
}

fn location(url: Option<&str>, line: Option<i64>) -> Option<String> {
    let url = url.filter(|u| !u.is_empty())?;
    Some(match line {
        // CDP lines are 0-based.
        Some(l) if l >= 0 => format!("{url}:{}", l + 1),
        _ => url.to_string(),
    })
}

/// One `Runtime.RemoteObject` as console text.
fn remote_text(o: &Value) -> String {
    if let Some(v) = o.get("value") {
        return match v {
            Value::String(s) => s.clone(),
            other => other.to_string(),
        };
    }
    if let Some(u) = o["unserializableValue"].as_str() {
        return u.to_string();
    }
    o["description"]
        .as_str()
        .or_else(|| o["className"].as_str())
        .or_else(|| o["type"].as_str())
        .unwrap_or("")
        .to_string()
}

/// The entry for a CDP event, if it is one of ours.
pub fn from_event(method: &str, p: &Value) -> Option<Entry> {
    match method {
        "Runtime.consoleAPICalled" => {
            let args: Vec<String> = p["args"]
                .as_array()
                .map(|a| a.iter().map(remote_text).collect())
                .unwrap_or_default();
            let kind = p["type"].as_str().unwrap_or("log");
            let mut text = args.join(" ");
            if kind == "assert" {
                text = format!("Assertion failed: {text}");
            }
            let frame = &p["stackTrace"]["callFrames"][0];
            Some(Entry {
                level: Level::parse(kind),
                source: "console".into(),
                text: cut(&text),
                location: location(frame["url"].as_str(), frame["lineNumber"].as_i64()),
            })
        }
        "Runtime.exceptionThrown" => {
            let d = &p["exceptionDetails"];
            let text = d["exception"]["description"]
                .as_str()
                .or_else(|| d["text"].as_str())
                .unwrap_or("Uncaught exception");
            let (url, line) = match d["stackTrace"]["callFrames"].get(0) {
                Some(f) => (f["url"].as_str(), f["lineNumber"].as_i64()),
                None => (d["url"].as_str(), d["lineNumber"].as_i64()),
            };
            let text = if text.starts_with("Uncaught") {
                text.to_string()
            } else {
                format!("Uncaught {text}")
            };
            Some(Entry {
                level: Level::Error,
                source: "exception".into(),
                text: cut(&text),
                location: location(url, line),
            })
        }
        "Log.entryAdded" => {
            let e = &p["entry"];
            // The browser asks every page for a favicon; a dev server without one is noise.
            if e["source"] == "network"
                && e["url"]
                    .as_str()
                    .is_some_and(|u| u.ends_with("/favicon.ico"))
            {
                return None;
            }
            Some(Entry {
                level: Level::parse(e["level"].as_str().unwrap_or("info")),
                source: e["source"].as_str().unwrap_or("other").to_string(),
                text: cut(e["text"].as_str().unwrap_or("")),
                location: location(e["url"].as_str(), e["lineNumber"].as_i64()),
            })
        }
        _ => None,
    }
}

/// Logs by CDP session (one per tab).
#[derive(Debug, Default)]
pub struct ConsoleLog {
    tabs: HashMap<String, VecDeque<Entry>>,
}

impl ConsoleLog {
    pub fn push(&mut self, cdp_session: &str, e: Entry) {
        let q = self.tabs.entry(cdp_session.to_string()).or_default();
        if q.len() >= MAX_ENTRIES {
            q.pop_front();
        }
        q.push_back(e);
    }

    pub fn clear(&mut self, cdp_session: &str) {
        self.tabs.remove(cdp_session);
    }

    /// Entries at `min` or above, oldest first.
    pub fn entries(&self, cdp_session: &str, min: Level) -> Vec<Entry> {
        self.tabs
            .get(cdp_session)
            .map(|q| q.iter().filter(|e| e.level >= min).cloned().collect())
            .unwrap_or_default()
    }

    /// `(errors, warnings)` in the tab's log.
    pub fn counts(&self, cdp_session: &str) -> (usize, usize) {
        let q = self.tabs.get(cdp_session);
        let n = |l| q.map_or(0, |q| q.iter().filter(|e| e.level == l).count());
        (n(Level::Error), n(Level::Warning))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn parses_console_exceptions_and_log_entries() {
        let e = from_event(
            "Runtime.consoleAPICalled",
            &json!({"type": "error", "args": [{"type": "string", "value": "boom"}, {"type": "number", "value": 3}, {"type": "object", "className": "Object", "description": "Object"}],
                    "stackTrace": {"callFrames": [{"url": "http://localhost:5173/src/App.tsx", "lineNumber": 11}]}}),
        )
        .unwrap();
        assert_eq!(e.level, Level::Error);
        assert_eq!(
            e.line(),
            "[error] boom 3 Object — http://localhost:5173/src/App.tsx:12"
        );

        let e = from_event(
            "Runtime.exceptionThrown",
            &json!({"exceptionDetails": {"text": "Uncaught", "exception": {"description": "TypeError: x is undefined\n    at App"},
                    "url": "http://localhost:5173/main.js", "lineNumber": 0}}),
        )
        .unwrap();
        assert_eq!(e.source, "exception");
        assert!(e.text.starts_with("Uncaught TypeError: x is undefined"));
        assert_eq!(
            e.location.as_deref(),
            Some("http://localhost:5173/main.js:1")
        );

        let e = from_event(
            "Log.entryAdded",
            &json!({"entry": {"source": "network", "level": "error", "text": "Failed to load resource: 404", "url": "http://localhost:5173/x.png"}}),
        )
        .unwrap();
        assert_eq!(
            e.line(),
            "[error (network)] Failed to load resource: 404 — http://localhost:5173/x.png"
        );
        assert!(from_event("Page.loadEventFired", &json!({})).is_none());
        assert!(
            from_event(
                "Log.entryAdded",
                &json!({"entry": {"source": "network", "level": "error", "text": "404", "url": "http://localhost:5173/favicon.ico"}}),
            )
            .is_none()
        );
    }

    #[test]
    fn keeps_a_bounded_log_per_tab() {
        let mut log = ConsoleLog::default();
        for i in 0..MAX_ENTRIES + 5 {
            log.push(
                "S1",
                Entry {
                    level: if i % 2 == 0 { Level::Error } else { Level::Log },
                    source: "console".into(),
                    text: i.to_string(),
                    location: None,
                },
            );
        }
        let all = log.entries("S1", Level::Debug);
        assert_eq!(all.len(), MAX_ENTRIES);
        assert_eq!(all[0].text, "5");
        assert_eq!(log.counts("S1").0, MAX_ENTRIES / 2);
        assert!(log.entries("S2", Level::Debug).is_empty());
        log.clear("S1");
        assert!(log.entries("S1", Level::Debug).is_empty());
    }
}
