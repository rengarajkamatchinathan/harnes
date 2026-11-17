//! Tool names for the model: `mcp__<server>__<tool>` (06 §7, D25, D30).
//!
//! Sanitised to `[A-Za-z0-9_-]` and at most [`MAX_NAME_LEN`] characters (the strictest
//! provider limit), deterministic for a given set of tools, and collision-checked: when two
//! tools map to the same name, **every** colliding entry gets a short hash of its original
//! `(server, tool)` pair, so neither silently shadows the other. The permission resource is
//! the same string.

use std::collections::{BTreeMap, HashMap};

use sha2::{Digest, Sha256};

pub const PREFIX: &str = "mcp__";
pub const SEPARATOR: &str = "__";
/// Anthropic/OpenAI/Gemini all accept 64; the strictest wins.
pub const MAX_NAME_LEN: usize = 64;
const HASH_LEN: usize = 6;

/// Replace anything outside `[A-Za-z0-9_-]` with `_`. Empty becomes `_`.
pub fn sanitize(part: &str) -> String {
    let s: String = part
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '_' || c == '-' {
                c
            } else {
                '_'
            }
        })
        .collect();
    if s.is_empty() { "_".to_string() } else { s }
}

fn short_hash(server: &str, tool: &str) -> String {
    let mut h = Sha256::new();
    h.update(server.as_bytes());
    h.update([0]);
    h.update(tool.as_bytes());
    hex::encode(h.finalize())[..HASH_LEN].to_string()
}

fn with_suffix(base: &str, suffix: &str) -> String {
    let keep = MAX_NAME_LEN - suffix.len() - 1;
    let mut b: String = base.chars().take(keep).collect();
    b.push('_');
    b.push_str(suffix);
    b
}

/// The name for one tool, ignoring collisions.
pub fn tool_name(server: &str, tool: &str) -> String {
    let full = format!("{PREFIX}{}{SEPARATOR}{}", sanitize(server), sanitize(tool));
    if full.len() <= MAX_NAME_LEN {
        full
    } else {
        with_suffix(&full, &short_hash(server, tool))
    }
}

/// Assign collision-free names to every `(server, tool)` pair. Returns them in input order.
pub fn assign(pairs: &[(String, String)]) -> Vec<String> {
    let names: Vec<String> = pairs.iter().map(|(s, t)| tool_name(s, t)).collect();
    let mut count: HashMap<&str, usize> = HashMap::new();
    for n in &names {
        *count.entry(n.as_str()).or_default() += 1;
    }
    let mut out: Vec<String> = names
        .iter()
        .zip(pairs)
        .map(|(n, (s, t))| {
            if count.get(n.as_str()).copied().unwrap_or(0) > 1 {
                with_suffix(n, &short_hash(s, t))
            } else {
                n.clone()
            }
        })
        .collect();
    // Exact duplicates (same server and tool listed twice) keep one name; anything still
    // colliding after hashing is disambiguated by position in sorted order.
    let mut seen: BTreeMap<String, usize> = BTreeMap::new();
    for n in &mut out {
        let k = seen.entry(n.clone()).or_default();
        if *k > 0 {
            *n = with_suffix(n, &format!("{k}"));
        }
        *k += 1;
    }
    out
}

/// Split a model-facing name into its sanitised server and tool parts. Use the index built by
/// the manager to get the original names; this is only a best-effort parse.
pub fn parse(name: &str) -> Option<(&str, &str)> {
    let rest = name.strip_prefix(PREFIX)?;
    rest.split_once(SEPARATOR)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitises_and_prefixes() {
        assert_eq!(
            tool_name("git hub", "create.issue"),
            "mcp__git_hub__create_issue"
        );
        assert_eq!(tool_name("", "x"), "mcp_____x");
        assert_eq!(parse("mcp__a__b"), Some(("a", "b")));
    }

    #[test]
    fn long_names_are_truncated_deterministically() {
        let long = "t".repeat(100);
        let a = tool_name("srv", &long);
        assert_eq!(a.len(), MAX_NAME_LEN);
        assert_eq!(a, tool_name("srv", &long));
        assert_ne!(a, tool_name("srv", &format!("{long}x")));
    }

    #[test]
    fn collisions_get_hash_suffixes() {
        let pairs = vec![
            ("a".to_string(), "b.c".to_string()),
            ("a".to_string(), "b_c".to_string()),
            ("z".to_string(), "ok".to_string()),
        ];
        let names = assign(&pairs);
        assert_ne!(names[0], names[1]);
        assert!(names[0].starts_with("mcp__a__b_c_"));
        assert_eq!(names[2], "mcp__z__ok");
        // Deterministic.
        assert_eq!(names, assign(&pairs));
        // Same pairs, different order → same names per pair.
        let rev: Vec<_> = pairs.iter().rev().cloned().collect();
        let mut r = assign(&rev);
        r.reverse();
        assert_eq!(names, r);
    }

    #[test]
    fn exact_duplicates_are_distinct() {
        let pairs = vec![("a".to_string(), "b".to_string()); 2];
        let names = assign(&pairs);
        assert_ne!(names[0], names[1]);
        assert!(names.iter().all(|n| n.len() <= MAX_NAME_LEN));
    }
}
