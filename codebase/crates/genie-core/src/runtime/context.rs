//! Context assembly (06 §2.1–§2.3, §2.6.4, §2.6.6).
//!
//! - Instruction sources are rendered in the formats of §2.6.4 and ordered
//!   most-shared-first (SA-42).
//! - A session **freezes** its baseline at the first turn. Later source changes are sent
//!   as frozen `<system-update>` messages in the conversation; the cached prefix is never
//!   rewritten. Compaction rebuilds the baseline from current sources (§3).
//! - Updates are lowered per protocol (SA-45) when the request is assembled.
//! - Token accounting: last measured usage on the same provider/model + an estimate
//!   (4 chars/token + 15 %) for what follows (t11).

use std::fmt::Write as _;

use genie_providers::compat::Compat;
use genie_providers::model::{Message, Part, ProtocolKind, Role, ToolOutput};
use serde::{Deserialize, Serialize};

use crate::runtime::conversation::{MsgKind, SessionMeta, StoredMessage};
use crate::runtime::prompts::system_update;

/// Most-shared-first ranks (06 §2.1 item 3, SA-42).
pub mod rank {
    pub const ORG: u8 = 1;
    pub const CUSTOM: u8 = 2;
    pub const SKILLS: u8 = 3;
    pub const MCP: u8 = 4;
    pub const GLOBAL_MEMORY: u8 = 5;
    pub const PROJECT_INSTRUCTIONS: u8 = 6;
    pub const PROJECT_KNOWLEDGE: u8 = 7;
    pub const PROJECT_MEMORY: u8 = 8;
    pub const INSTRUCTION_FILE: u8 = 9;
    pub const ENV: u8 = 10;
    pub const DATE: u8 = 11;
}

/// MCP server guidance is capped at 2 KB per server (06 §2.6.4).
pub const MCP_GUIDANCE_MAX: usize = 2 * 1024;

/// One baseline source `{key, hash, text}` (06 §2.2).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InstructionSource {
    pub key: String,
    pub rank: u8,
    pub hash: String,
    pub text: String,
}

/// FNV-1a 64: a stable content hash (change detection, not security).
pub fn content_hash(s: &str) -> String {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    for b in s.as_bytes() {
        h ^= u64::from(*b);
        h = h.wrapping_mul(0x0100_0000_01b3);
    }
    format!("{h:016x}")
}

fn attr(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('"', "&quot;")
        .replace('<', "&lt;")
}

fn cut(s: &str, max: usize) -> &str {
    if s.len() <= max {
        return s;
    }
    let mut end = max;
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    &s[..end]
}

/// Environment facts for `<env>`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct EnvInfo {
    pub cwd: Option<String>,
    pub os: String,
    pub shell: Option<String>,
    /// Code sessions: whether the workspace is a git repo, and its branch.
    pub git: Option<bool>,
    pub branch: Option<String>,
    pub app_version: String,
}

impl InstructionSource {
    pub fn new(key: impl Into<String>, rank: u8, text: impl Into<String>) -> Self {
        let text = text.into();
        InstructionSource {
            key: key.into(),
            rank,
            hash: content_hash(&text),
            text,
        }
    }

    pub fn org(org_name: &str, text: &str) -> Self {
        Self::new(
            "org",
            rank::ORG,
            format!(
                "<org_instructions org=\"{}\">\n{}\n</org_instructions>",
                attr(org_name),
                text.trim()
            ),
        )
    }

    pub fn user_instructions(text: &str, user_name: Option<&str>) -> Self {
        let mut body = text.trim().to_string();
        if let Some(n) = user_name.filter(|n| !n.trim().is_empty()) {
            if !body.is_empty() {
                body.push('\n');
            }
            let _ = write!(body, "The user's name is {}.", n.trim());
        }
        Self::new(
            "custom",
            rank::CUSTOM,
            format!("<user_instructions>\n{body}\n</user_instructions>"),
        )
    }

    pub fn skills_index(skills: &[(String, String)]) -> Self {
        let mut s = String::from("<available_skills>\n");
        for (name, desc) in skills {
            let _ = writeln!(s, "<skill name=\"{}\">{}</skill>", attr(name), desc.trim());
        }
        s.push_str("</available_skills>");
        Self::new("skills", rank::SKILLS, s)
    }

    pub fn mcp_server(name: &str, instructions: &str) -> Self {
        Self::new(
            format!("mcp:{name}"),
            rank::MCP,
            format!(
                "<mcp_server name=\"{}\">{}</mcp_server>",
                attr(name),
                cut(instructions.trim(), MCP_GUIDANCE_MAX)
            ),
        )
    }

    /// `scope` is `global` or `project`.
    pub fn memory(scope: &str, entries: &[String]) -> Self {
        let mut s = format!("<memory scope=\"{}\">\n", attr(scope));
        for e in entries {
            let _ = writeln!(s, "- {}", e.trim());
        }
        s.push_str("</memory>");
        let r = if scope == "global" {
            rank::GLOBAL_MEMORY
        } else {
            rank::PROJECT_MEMORY
        };
        Self::new(format!("memory:{scope}"), r, s)
    }

    pub fn project_instructions(project: &str, text: &str) -> Self {
        Self::new(
            "project_instructions",
            rank::PROJECT_INSTRUCTIONS,
            format!(
                "<project_instructions project=\"{}\">\n{}\n</project_instructions>",
                attr(project),
                text.trim()
            ),
        )
    }

    /// Full mode only (D32).
    pub fn project_knowledge(files: &[(String, String)]) -> Self {
        let mut s = String::from("<project_knowledge>\n");
        for (name, body) in files {
            let _ = writeln!(
                s,
                "<file name=\"{}\">\n{}\n</file>",
                attr(name),
                body.trim_end()
            );
        }
        s.push_str("</project_knowledge>");
        Self::new("project_knowledge", rank::PROJECT_KNOWLEDGE, s)
    }

    /// Code: GENIE.md / AGENTS.md / rules, passed farthest-first (D31).
    pub fn instruction_file(relative_path: &str, text: &str) -> Self {
        Self::new(
            format!("file:{relative_path}"),
            rank::INSTRUCTION_FILE,
            format!(
                "<instructions source=\"{}\">\n{}\n</instructions>",
                attr(relative_path),
                text.trim()
            ),
        )
    }

    pub fn env(e: &EnvInfo) -> Self {
        let mut s = String::from("<env>\n");
        if let Some(c) = &e.cwd {
            let _ = writeln!(s, "cwd: {c}");
        }
        let _ = writeln!(s, "os: {}", e.os);
        if let Some(sh) = &e.shell {
            let _ = writeln!(s, "shell: {sh}");
        }
        if let Some(g) = e.git {
            let _ = writeln!(s, "git: {}", if g { "yes" } else { "no" });
        }
        if let Some(b) = &e.branch {
            let _ = writeln!(s, "branch: {b}");
        }
        let _ = writeln!(s, "app_version: {}", e.app_version);
        s.push_str("</env>");
        Self::new("env", rank::ENV, s)
    }

    /// `YYYY-MM-DD`.
    pub fn date(day: &str) -> Self {
        Self::new("date", rank::DATE, format!("<date>{day}</date>"))
    }
}

/// Sort most-shared-first; ties keep the caller's order (instruction files farthest-first).
pub fn order(mut sources: Vec<InstructionSource>) -> Vec<InstructionSource> {
    sources.sort_by_key(|s| s.rank);
    sources
}

/// The frozen context baseline of a session (system block 2).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct FrozenBaseline {
    /// Bumped on every rebuild (compaction).
    pub version: u32,
    pub prompt_version: String,
    pub sources: Vec<InstructionSource>,
    /// `(key, hash)` of what the model has been told so far (baseline + updates).
    pub delivered: Vec<(String, String)>,
}

impl FrozenBaseline {
    pub fn freeze(sources: Vec<InstructionSource>, version: u32) -> Self {
        let sources = order(sources);
        let delivered = sources
            .iter()
            .map(|s| (s.key.clone(), s.hash.clone()))
            .collect();
        FrozenBaseline {
            version,
            prompt_version: crate::runtime::prompts::PROMPT_VERSION.to_string(),
            sources,
            delivered,
        }
    }

    /// System block 2 text.
    pub fn render(&self) -> String {
        self.sources
            .iter()
            .map(|s| s.text.as_str())
            .collect::<Vec<_>>()
            .join("\n\n")
    }

    /// Whether any source holds project knowledge (1 h cache TTL, 03 §6).
    pub fn has_project_knowledge(&self) -> bool {
        self.sources
            .iter()
            .any(|s| s.rank == rank::PROJECT_KNOWLEDGE)
    }

    /// The `<system-update>` body for sources that changed since they were last
    /// delivered, and mark them delivered. `None` when nothing changed.
    pub fn delta(&mut self, current: &[InstructionSource]) -> Option<String> {
        let current = order(current.to_vec());
        let mut lines: Vec<String> = vec![];
        for s in &current {
            match self.delivered.iter().find(|(k, _)| *k == s.key) {
                Some((_, h)) if *h == s.hash => {}
                Some(_) if s.key == "date" => {
                    let day = s
                        .text
                        .trim_start_matches("<date>")
                        .trim_end_matches("</date>");
                    lines.push(format!("Today's date is now {day}."));
                }
                Some(_) => lines.push(format!("Updated {}:\n{}", s.key, s.text)),
                None => lines.push(format!("Added {}:\n{}", s.key, s.text)),
            }
        }
        for (k, _) in &self.delivered {
            if !current.iter().any(|s| s.key == *k) {
                lines.push(format!("Removed {k}; it no longer applies."));
            }
        }
        self.delivered = current
            .iter()
            .map(|s| (s.key.clone(), s.hash.clone()))
            .collect();
        (!lines.is_empty()).then(|| system_update(&lines.join("\n\n")))
    }
}

/// `YYYY-MM-DD` (UTC) for a Unix-ms timestamp (civil-from-days, H. Hinnant).
pub fn utc_day(ms: u64) -> String {
    let days = i64::try_from(ms / 86_400_000).unwrap_or(0);
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    format!("{y:04}-{m:02}-{d:02}")
}

// ------------------------------------------------------------------ token accounting

/// Estimated tokens for a text: 4 chars/token + 15 % margin (t11).
pub fn estimate_text(s: &str) -> u64 {
    let chars = s.chars().count() as u64;
    (chars.div_ceil(4) * 115).div_ceil(100)
}

fn part_chars(p: &Part) -> usize {
    match p {
        Part::Text(t) => t.text.len(),
        Part::Reasoning(r) => r.text.len(),
        Part::ToolCall(c) => c.name.len() + c.input.to_string().len(),
        Part::ToolResult(r) => match &r.content {
            ToolOutput::Text(t) => t.len(),
            ToolOutput::Json(v) => v.to_string().len(),
            ToolOutput::Rich(ps) => ps.iter().map(part_chars).sum(),
        },
        Part::Compaction(c) => c.summary.as_ref().map_or(0, String::len),
        // Images count as a flat ~1,600 tokens (vendor guidance for ~1 MP).
        Part::Media(m) => m.extracted_text.as_ref().map_or(6_400, String::len),
        Part::EffortMarker(_) => 16,
        Part::Unknown(u) => u.raw.to_string().len(),
    }
}

pub fn estimate_message(m: &Message) -> u64 {
    let chars: usize = m.parts.iter().map(part_chars).sum::<usize>() + 16;
    ((chars as u64).div_ceil(4) * 115).div_ceil(100)
}

pub fn estimate_messages(ms: &[Message]) -> u64 {
    ms.iter().map(estimate_message).sum()
}

/// The compaction threshold: window − max(15 % of window, 16k), or `buffer` when set
/// (`compaction.buffer`, 06 §3).
pub fn compaction_threshold(window: u64, buffer: Option<u64>) -> u64 {
    let reserve = buffer.unwrap_or_else(|| (window * 15 / 100).max(16_000));
    window.saturating_sub(reserve)
}

// ------------------------------------------------------------------ history

/// The conversation the model sees: the active path with the latest compaction applied.
/// Kept tail messages follow the summary; branching from before a compaction sees the
/// uncompacted history because its path has no compaction message (06 §3).
pub fn effective_history(path: &[StoredMessage], meta: &SessionMeta) -> Vec<StoredMessage> {
    let Some(ci) = path.iter().rposition(|m| m.kind == MsgKind::Compaction) else {
        return path.to_vec();
    };
    let comp = &path[ci];
    let kept_from = meta
        .compactions
        .get(&comp.message.id.0)
        .and_then(|k| k.as_deref())
        .and_then(|id| path[..ci].iter().position(|m| m.message.id.0 == id))
        .unwrap_or(ci);
    let mut out = Vec::with_capacity(path.len() - kept_from + 1);
    out.push(comp.clone());
    out.extend_from_slice(&path[kept_from..ci]);
    out.extend_from_slice(&path[ci + 1..]);
    out
}

/// Lower stored `<system-update>` messages for `protocol` (06 §2.6.6, SA-45). Updates are
/// only ever stored right before a user message, so they never sit between a tool call
/// and its result.
pub fn lower_updates(
    history: Vec<StoredMessage>,
    protocol: ProtocolKind,
    compat: &Compat,
) -> Vec<Message> {
    let keep_system = match protocol {
        ProtocolKind::OpenaiResponses => true,
        ProtocolKind::OpenaiChat => compat.mid_conversation_system,
        _ => false,
    };
    let mut out: Vec<Message> = Vec::with_capacity(history.len());
    let mut pending: Vec<String> = vec![];
    for sm in history {
        let mut m = sm.message;
        if sm.kind == MsgKind::Compaction {
            m.role = Role::User;
        }
        if sm.kind == MsgKind::SystemUpdate {
            if keep_system {
                m.role = Role::System;
                out.push(m);
            } else {
                pending.extend(m.parts.into_iter().filter_map(|p| match p {
                    Part::Text(t) => Some(t.text),
                    _ => None,
                }));
            }
            continue;
        }
        if !pending.is_empty() && m.role == Role::User {
            for t in pending.drain(..) {
                m.parts.push(Part::text(t));
            }
        }
        out.push(m);
    }
    if !pending.is_empty() {
        out.push(Message::new(
            "sup_tail",
            Role::User,
            pending.into_iter().map(Part::text).collect(),
        ));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn source_formats() {
        assert_eq!(
            InstructionSource::org("Acme \"Co\"", " Be nice. ").text,
            "<org_instructions org=\"Acme &quot;Co&quot;\">\nBe nice.\n</org_instructions>"
        );
        assert_eq!(
            InstructionSource::user_instructions("Short answers.", Some("Ana")).text,
            "<user_instructions>\nShort answers.\nThe user's name is Ana.\n</user_instructions>"
        );
        assert_eq!(
            InstructionSource::memory("global", &["Likes tea".into()]).text,
            "<memory scope=\"global\">\n- Likes tea\n</memory>"
        );
        assert_eq!(
            InstructionSource::date("2027-03-14").text,
            "<date>2027-03-14</date>"
        );
        let long = "x".repeat(3000);
        assert_eq!(
            InstructionSource::mcp_server("gh", &long).text.len(),
            "<mcp_server name=\"gh\"></mcp_server>".len() + MCP_GUIDANCE_MAX
        );
    }

    #[test]
    fn most_shared_first_order() {
        let v = order(vec![
            InstructionSource::date("2027-01-01"),
            InstructionSource::instruction_file("../AGENTS.md", "a"),
            InstructionSource::instruction_file("GENIE.md", "b"),
            InstructionSource::org("o", "x"),
        ]);
        let keys: Vec<&str> = v.iter().map(|s| s.key.as_str()).collect();
        assert_eq!(keys, ["org", "file:../AGENTS.md", "file:GENIE.md", "date"]);
    }

    #[test]
    fn deltas_never_rewrite_the_frozen_text() {
        let mut b = FrozenBaseline::freeze(
            vec![
                InstructionSource::date("2027-03-14"),
                InstructionSource::user_instructions("A", None),
            ],
            1,
        );
        let frozen = b.render();
        assert_eq!(b.delta(&b.sources.clone()), None);
        let now = vec![
            InstructionSource::date("2027-03-15"),
            InstructionSource::user_instructions("B", None),
            InstructionSource::memory("global", &["x".into()]),
        ];
        let d = b.delta(&now).unwrap();
        assert!(d.starts_with("<system-update>Updated custom:"));
        assert!(d.contains("Added memory:global"));
        assert!(d.contains("Today's date is now 2027-03-15."));
        assert_eq!(b.render(), frozen);
        assert_eq!(b.delta(&now), None, "delivered once");
        let d = b.delta(&now[..2]).unwrap();
        assert!(d.contains("Removed memory:global"));
    }

    #[test]
    fn utc_day_and_estimates() {
        assert_eq!(utc_day(0), "1970-01-01");
        assert_eq!(utc_day(1_804_982_400_000), "2027-03-14");
        assert_eq!(estimate_text("abcd"), 2);
        assert_eq!(compaction_threshold(200_000, None), 170_000);
        assert_eq!(compaction_threshold(64_000, None), 48_000);
        assert_eq!(compaction_threshold(64_000, Some(1_000)), 63_000);
    }
}
