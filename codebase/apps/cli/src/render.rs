//! Plain-text rendering shared by `genie chat open`, `genie approvals` and the TUI:
//! asks, tool rows, citations as `[n]` + a reference list, rich parts as one-line
//! summaries with a `genie://` link (07 §1.2, §3).

use genie_protocol::ask::{Ask, AskKind, Preview};
use genie_protocol::message::{Citation, Message, Part, Role};
use serde_json::Value;

/// Deep link that opens a part in the desktop app (D42).
pub fn part_link(session_id: &str, part_id: &str) -> String {
    format!("genie://session/{session_id}#{part_id}")
}

/// One line describing what an ask wants.
pub fn ask_summary(ask: &Ask) -> String {
    let who = ask
        .subagent
        .as_ref()
        .map(|s| format!("[{}] ", s.agent))
        .unwrap_or_default();
    let body = match ask.kind {
        AskKind::Permission => {
            let tool = ask.tool.as_deref().unwrap_or("tool");
            match &ask.preview {
                Some(Preview::Command { command, .. }) => format!("{tool}: {command}"),
                Some(Preview::Diff { path, .. }) => format!("{tool}: edit {path}"),
                Some(Preview::Url { url }) => format!("{tool}: {url}"),
                Some(Preview::McpArgs { server, tool, .. }) => format!("MCP {server}/{tool}"),
                Some(Preview::Text { text }) => format!("{tool}: {}", first_line(text)),
                _ => match &ask.resource {
                    Some(r) => format!("{tool}: {r}"),
                    None => tool.to_string(),
                },
            }
        }
        AskKind::Elicitation | AskKind::Question => "question / form".into(),
        AskKind::Sampling => match &ask.sampling {
            Some(s) => format!("MCP {} wants a model call ({})", s.server, s.model),
            None => "MCP sampling request".into(),
        },
        AskKind::BrowserConfirm => match &ask.browser {
            Some(b) => format!("browser {} on {}: {}", b.action, b.origin, b.summary),
            None => "browser confirmation".into(),
        },
        AskKind::DefinitionHash => "approve a changed definition".into(),
        AskKind::Unknown => "decision needed".into(),
    };
    format!("{who}{body}")
}

/// Short label for the "Always" option: the first save suggestion and its scope.
pub fn always_label(ask: &Ask) -> Option<String> {
    ask.save_suggestions.first().map(|s| {
        let scope = match s.scope {
            genie_protocol::common::RuleScope::Session => "this session",
            genie_protocol::common::RuleScope::Local => "this project (local)",
            genie_protocol::common::RuleScope::User => "all projects",
            genie_protocol::common::RuleScope::Unknown => "saved",
        };
        format!("{} · {scope}", s.rule)
    })
}

pub fn first_line(s: &str) -> &str {
    s.lines().next().unwrap_or_default()
}

pub fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let mut t: String = s.chars().take(max.saturating_sub(1)).collect();
        t.push('…');
        t
    }
}

/// Compact `name(arg summary)` for a tool call.
pub fn tool_row(name: &str, input: &Value) -> String {
    let arg = match input {
        Value::Object(map) => ["command", "path", "file_path", "url", "pattern", "query"]
            .iter()
            .find_map(|k| map.get(*k).and_then(Value::as_str))
            .map(str::to_string)
            .unwrap_or_else(|| map.keys().take(3).cloned().collect::<Vec<_>>().join(", ")),
        Value::Null => String::new(),
        other => other.to_string(),
    };
    format!("{name}({})", truncate(first_line(&arg), 80))
}

/// Text with citation markers `[n]` inserted at each citation's end, plus the numbered
/// reference lines.
pub fn cite(text: &str, citations: &[Citation], first_n: usize) -> (String, Vec<String>) {
    if citations.is_empty() {
        return (text.to_string(), vec![]);
    }
    let mut order: Vec<(usize, usize)> = citations
        .iter()
        .enumerate()
        .map(|(i, c)| (c.end as usize, i))
        .collect();
    order.sort();
    let mut out = String::with_capacity(text.len() + citations.len() * 4);
    let mut last = 0;
    for (end, i) in &order {
        // Citation offsets are byte offsets; clamp to a char boundary.
        let mut e = (*end).min(text.len());
        while e > last && !text.is_char_boundary(e) {
            e -= 1;
        }
        if e >= last {
            out.push_str(&text[last..e]);
            last = e;
        }
        out.push_str(&format!("[{}]", first_n + i));
    }
    out.push_str(&text[last..]);
    let refs = citations
        .iter()
        .enumerate()
        .map(|(i, c)| {
            let title = c.title.as_deref().unwrap_or("source");
            match (&c.url, &c.session_id) {
                (Some(u), _) => format!("[{}] {title} — {u}", first_n + i),
                (None, Some(s)) => format!("[{}] {title} — genie://session/{s}", first_n + i),
                _ => format!("[{}] {title}", first_n + i),
            }
        })
        .collect();
    (out, refs)
}

/// One part as plain lines (no styling). Rich parts become a summary + link.
pub fn part_lines(session_id: &str, part: &Part, refs: &mut Vec<String>) -> Vec<String> {
    match part {
        Part::Text {
            text, citations, ..
        } => {
            let (t, r) = cite(text, citations, refs.len() + 1);
            refs.extend(r);
            t.lines().map(str::to_string).collect()
        }
        Part::Reasoning { text, redacted, .. } => {
            if *redacted || text.is_empty() {
                vec!["∴ thinking…".into()]
            } else {
                vec![format!("∴ {}", truncate(first_line(text), 100))]
            }
        }
        Part::ToolCall { name, input, .. } => vec![format!("⏺ {}", tool_row(name, input))],
        Part::ToolResult {
            content, is_error, ..
        } => {
            let s = match content {
                Value::String(s) => s.clone(),
                other => other.to_string(),
            };
            let mark = if *is_error { "✗" } else { "⎿" };
            vec![format!("  {mark} {}", truncate(first_line(&s), 100))]
        }
        Part::Media { id, mime, name, .. } => vec![format!(
            "▣ {} ({mime}) — open in desktop: {}",
            name.as_deref().unwrap_or("attachment"),
            part_link(session_id, id)
        )],
        Part::Compaction { summary, .. } => vec![format!(
            "── conversation compacted{} ──",
            summary
                .as_deref()
                .map(|s| format!(": {}", truncate(first_line(s), 60)))
                .unwrap_or_default()
        )],
        Part::Unknown => vec!["(unsupported content — open in desktop)".into()],
    }
}

/// A whole active-branch path (`session.get`), as `genie chat open` prints it.
pub fn transcript(session_id: &str, path: &[Message]) -> Vec<String> {
    let mut out = vec![];
    let mut refs = vec![];
    for msg in path {
        let label = match msg.role {
            Role::User => "you",
            Role::Assistant => "genie",
            Role::System => "system",
            Role::Tool => "tool",
            Role::Unknown => "?",
        };
        out.push(format!("{label}:"));
        for p in &msg.parts {
            for l in part_lines(session_id, p, &mut refs) {
                out.push(format!("  {l}"));
            }
        }
        out.push(String::new());
    }
    if !refs.is_empty() {
        out.push("References:".into());
        out.extend(refs.into_iter().map(|r| format!("  {r}")));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn citations_become_numbers_and_references() {
        let c = Citation {
            start: 0,
            end: 5,
            url: Some("https://x.test".into()),
            title: Some("X".into()),
            session_id: None,
            message_id: None,
        };
        let (t, r) = cite("Hello world", &[c], 1);
        assert_eq!(t, "Hello[1] world");
        assert_eq!(r, ["[1] X — https://x.test"]);
    }

    #[test]
    fn transcript_has_rows_and_links() {
        let path = vec![
            Message {
                id: "m1".into(),
                parent_id: None,
                role: Role::User,
                parts: vec![Part::Text {
                    id: "p0".into(),
                    text: "hi".into(),
                    citations: vec![],
                }],
                created_at: String::new(),
                model: None,
                turn_id: None,
                usage: None,
                cost: None,
            },
            Message {
                id: "m2".into(),
                parent_id: Some("m1".into()),
                role: Role::Assistant,
                parts: vec![
                    Part::ToolCall {
                        id: "p1".into(),
                        call_id: "c1".into(),
                        name: "shell".into(),
                        input: json!({"command": "ls -la"}),
                        provider_executed: false,
                    },
                    Part::Media {
                        id: "p2".into(),
                        blob_id: "b".into(),
                        mime: "image/png".into(),
                        name: Some("chart.png".into()),
                    },
                ],
                created_at: String::new(),
                model: None,
                turn_id: None,
                usage: None,
                cost: None,
            },
        ];
        let t = transcript("ses_9", &path);
        assert!(t.contains(&"  ⏺ shell(ls -la)".to_string()));
        assert!(t.iter().any(|l| l.contains("genie://session/ses_9#p2")));
    }
}
