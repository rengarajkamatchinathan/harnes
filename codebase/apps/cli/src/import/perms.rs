//! OpenCode permission conversion (07 §5.3, SA-40).
//!
//! OpenCode rules are last-match-wins over `{tool: action | {pattern: action}}`; Genie
//! evaluates by class (deny → ask → allow, 06 §5.2). Rules are grouped by class. An
//! allow (or ask) that follows an overlapping deny is an exception Genie cannot express:
//! it is dropped, the deny is kept and a diagnostic is emitted (the safe side, C4).

use serde::Deserialize;
use serde::de::{Deserializer, MapAccess, Visitor};
use serde_json::Value;

use super::{Diag, DiagKind};

/// A JSON value whose objects keep their key order (serde_json's `Map` sorts keys).
#[derive(Debug, Clone, PartialEq)]
pub enum Ordered {
    Scalar(Value),
    Map(Vec<(String, Ordered)>),
}

impl<'de> Deserialize<'de> for Ordered {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        struct V;
        impl<'de> Visitor<'de> for V {
            type Value = Ordered;
            fn expecting(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
                f.write_str("a JSON value")
            }
            fn visit_map<A: MapAccess<'de>>(self, mut a: A) -> Result<Ordered, A::Error> {
                let mut v = vec![];
                while let Some((k, val)) = a.next_entry::<String, Ordered>()? {
                    v.push((k, val));
                }
                Ok(Ordered::Map(v))
            }
            fn visit_str<E>(self, s: &str) -> Result<Ordered, E> {
                Ok(Ordered::Scalar(Value::String(s.into())))
            }
            fn visit_string<E>(self, s: String) -> Result<Ordered, E> {
                Ok(Ordered::Scalar(Value::String(s)))
            }
            fn visit_bool<E>(self, b: bool) -> Result<Ordered, E> {
                Ok(Ordered::Scalar(Value::Bool(b)))
            }
            fn visit_i64<E>(self, n: i64) -> Result<Ordered, E> {
                Ok(Ordered::Scalar(Value::from(n)))
            }
            fn visit_u64<E>(self, n: u64) -> Result<Ordered, E> {
                Ok(Ordered::Scalar(Value::from(n)))
            }
            fn visit_f64<E>(self, n: f64) -> Result<Ordered, E> {
                Ok(Ordered::Scalar(Value::from(n)))
            }
            fn visit_unit<E>(self) -> Result<Ordered, E> {
                Ok(Ordered::Scalar(Value::Null))
            }
            fn visit_seq<A: serde::de::SeqAccess<'de>>(
                self,
                mut a: A,
            ) -> Result<Ordered, A::Error> {
                let mut v = vec![];
                while let Some(x) = a.next_element::<Value>()? {
                    v.push(x);
                }
                Ok(Ordered::Scalar(Value::Array(v)))
            }
        }
        d.deserialize_any(V)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Class {
    Deny,
    Ask,
    Allow,
}

impl Class {
    fn parse(s: &str) -> Option<Class> {
        match s {
            "deny" => Some(Class::Deny),
            "ask" => Some(Class::Ask),
            "allow" => Some(Class::Allow),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
struct Rule {
    tool: String,
    pattern: String,
    class: Class,
}

/// Genie permission lists from an OpenCode `permission` value.
#[derive(Debug, Default, Clone, PartialEq)]
pub struct Converted {
    pub allow: Vec<String>,
    pub ask: Vec<String>,
    pub deny: Vec<String>,
    pub diagnostics: Vec<Diag>,
}

fn tool_name(t: &str) -> Option<&'static str> {
    Some(match t {
        "bash" => "Bash",
        "edit" | "write" | "patch" => "Edit",
        "read" => "Read",
        "webfetch" => "WebFetch",
        "websearch" => "WebSearch",
        "glob" => "Glob",
        "grep" => "Grep",
        "list" => "LS",
        "task" => "Task",
        _ => return None,
    })
}

fn rule_string(tool: &str, pattern: &str) -> String {
    if pattern == "*" || pattern.is_empty() {
        return tool.to_string();
    }
    if tool == "Bash" {
        // `git status*` / `git status *` → Claude/Genie prefix form `Bash(git status:*)`.
        if let Some(prefix) = pattern.strip_suffix('*') {
            let prefix = prefix.trim_end();
            if !prefix.contains('*') {
                return format!("Bash({prefix}:*)");
            }
        }
    }
    format!("{tool}({pattern})")
}

fn glob_match(glob: &str, s: &str) -> bool {
    // `*` matches any run of characters.
    let parts: Vec<&str> = glob.split('*').collect();
    if parts.len() == 1 {
        return glob == s;
    }
    let mut rest = s;
    for (i, p) in parts.iter().enumerate() {
        if i == 0 {
            match rest.strip_prefix(p) {
                Some(r) => rest = r,
                None => return false,
            }
        } else if i == parts.len() - 1 {
            return rest.ends_with(p);
        } else {
            match rest.find(p) {
                Some(j) => rest = &rest[j + p.len()..],
                None => return false,
            }
        }
    }
    true
}

/// Can some input match both patterns? (Conservative: prefix comparison of globs.)
pub fn overlap(a: &str, b: &str) -> bool {
    match (a.contains('*'), b.contains('*')) {
        (false, false) => a == b,
        (true, false) => glob_match(a, b),
        (false, true) => glob_match(b, a),
        (true, true) => {
            let pa = &a[..a.find('*').unwrap_or(a.len())];
            let pb = &b[..b.find('*').unwrap_or(b.len())];
            pa.starts_with(pb) || pb.starts_with(pa)
        }
    }
}

pub fn convert(perm: &Ordered) -> Converted {
    let mut out = Converted::default();
    let mut rules: Vec<Rule> = vec![];
    let entries: Vec<(String, Ordered)> = match perm {
        Ordered::Map(m) => m.clone(),
        Ordered::Scalar(Value::String(s)) => {
            vec![("*".into(), Ordered::Scalar(Value::String(s.clone())))]
        }
        Ordered::Scalar(_) => {
            out.diagnostics.push(Diag::new(
                DiagKind::Unsupported,
                Some("permission"),
                "permission is not an object; skipped",
            ));
            return out;
        }
    };
    for (tool, v) in entries {
        let genie_tool = if tool == "*" {
            Some("*")
        } else {
            tool_name(&tool)
        };
        let Some(gt) = genie_tool else {
            out.diagnostics.push(Diag::new(
                DiagKind::Unsupported,
                Some(&format!("permission.{tool}")),
                &format!("OpenCode permission `{tool}` has no Genie equivalent; skipped"),
            ));
            continue;
        };
        match v {
            Ordered::Scalar(Value::String(a)) => match Class::parse(&a) {
                Some(c) => rules.push(Rule {
                    tool: gt.into(),
                    pattern: "*".into(),
                    class: c,
                }),
                None => out.diagnostics.push(Diag::new(
                    DiagKind::Unsupported,
                    Some(&format!("permission.{tool}")),
                    &format!("unknown action `{a}`"),
                )),
            },
            Ordered::Map(pats) => {
                for (pat, a) in pats {
                    let action = match &a {
                        Ordered::Scalar(Value::String(s)) => Class::parse(s),
                        _ => None,
                    };
                    match action {
                        Some(c) => rules.push(Rule {
                            tool: gt.into(),
                            pattern: pat,
                            class: c,
                        }),
                        None => out.diagnostics.push(Diag::new(
                            DiagKind::Unsupported,
                            Some(&format!("permission.{tool}.{pat}")),
                            "unknown action; skipped",
                        )),
                    }
                }
            }
            _ => out.diagnostics.push(Diag::new(
                DiagKind::Unsupported,
                Some(&format!("permission.{tool}")),
                "unsupported value; skipped",
            )),
        }
    }

    // Walk in order; compare each rule with earlier, stricter, overlapping rules.
    let mut kept: Vec<Rule> = vec![];
    for r in rules {
        let same_tool = |e: &Rule| e.tool == r.tool || e.tool == "*" || r.tool == "*";
        let shadow = kept
            .iter()
            .filter(|e| same_tool(e) && e.class < r.class && overlap(&e.pattern, &r.pattern))
            .min_by_key(|e| e.class)
            .cloned();
        let label = rule_string(&r.tool, &r.pattern);
        match shadow {
            Some(e) if e.class == Class::Deny && r.class == Class::Allow => {
                // SA-40: keep the deny, never import the exception as an allow.
                out.diagnostics.push(Diag::new(
                    DiagKind::SemanticDifference,
                    Some(&format!("permission.{label}")),
                    &format!(
                        "OpenCode allow `{label}` came after deny `{}` (last match wins); \
                         Genie checks deny first, so the exception cannot be expressed. Kept \
                         the deny, dropped the allow.",
                        rule_string(&e.tool, &e.pattern),
                    ),
                ));
                continue;
            }
            Some(e) if e.class == Class::Deny => {
                // An ask after a deny: keeping the ask is still the safe side.
                out.diagnostics.push(Diag::new(
                    DiagKind::SemanticDifference,
                    Some(&format!("permission.{label}")),
                    &format!(
                        "ask `{label}` came after deny `{}`; in Genie the deny wins where they \
                         overlap and the ask covers the rest",
                        rule_string(&e.tool, &e.pattern)
                    ),
                ));
            }
            Some(e) => {
                out.diagnostics.push(Diag::new(
                    DiagKind::SemanticDifference,
                    Some(&format!("permission.{}", label)),
                    &format!(
                        "allow `{label}` overlaps the earlier ask `{}`; in Genie the ask wins \
                         where they overlap",
                        rule_string(&e.tool, &e.pattern)
                    ),
                ));
            }
            None => {}
        }
        kept.push(r);
    }
    for r in kept {
        let tools: Vec<&str> = if r.tool == "*" {
            vec!["Bash", "Edit", "WebFetch"]
        } else {
            vec![r.tool.as_str()]
        };
        for t in tools {
            let s = rule_string(t, &r.pattern);
            let list = match r.class {
                Class::Deny => &mut out.deny,
                Class::Ask => &mut out.ask,
                Class::Allow => &mut out.allow,
            };
            if !list.contains(&s) {
                list.push(s);
            }
        }
    }
    out
}

#[allow(dead_code)]
fn class_word(c: Class) -> &'static str {
    match c {
        Class::Deny => "deny",
        Class::Ask => "ask",
        Class::Allow => "allow",
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    fn conv(json: &str) -> Converted {
        convert(&serde_json::from_str::<Ordered>(json).unwrap())
    }

    #[test]
    fn groups_by_class_and_maps_tools() {
        let c = conv(
            r#"{"edit": "ask", "bash": {"git status*": "allow", "*": "ask"}, "webfetch": "allow"}"#,
        );
        assert_eq!(c.allow, ["Bash(git status:*)", "WebFetch"]);
        assert_eq!(c.ask, ["Edit", "Bash"]);
        assert!(c.deny.is_empty());
        // allow `git status*` then ask `*`: the later ask overlaps but is stricter → no
        // diagnostic needed (Genie's ask also wins, same as last-match-wins).
        assert!(c.diagnostics.is_empty(), "{:?}", c.diagnostics);
    }

    #[test]
    fn allow_after_deny_keeps_the_deny_sa40() {
        let c = conv(r#"{"bash": {"rm *": "deny", "rm -i *": "allow", "ls*": "allow"}}"#);
        assert_eq!(c.deny, ["Bash(rm:*)"]);
        assert_eq!(c.allow, ["Bash(ls:*)"]);
        assert_eq!(c.diagnostics.len(), 1);
        assert_eq!(c.diagnostics[0].kind, DiagKind::SemanticDifference);
        assert!(c.diagnostics[0].message.contains("Kept the deny"));
    }

    #[test]
    fn deny_after_allow_is_fine_and_allow_after_ask_is_flagged() {
        let c = conv(r#"{"bash": {"*": "allow", "git push*": "deny"}}"#);
        assert_eq!(c.allow, ["Bash"]);
        assert_eq!(c.deny, ["Bash(git push:*)"]);
        assert!(c.diagnostics.is_empty());
        let c = conv(r#"{"bash": {"*": "ask", "git log*": "allow"}}"#);
        assert_eq!(c.allow, ["Bash(git log:*)"]);
        assert_eq!(c.diagnostics.len(), 1);
    }

    #[test]
    fn unknown_tools_are_diagnosed() {
        let c = conv(r#"{"doom_loop": "ask", "external_directory": "deny"}"#);
        assert_eq!(c.diagnostics.len(), 2);
        assert!(
            c.diagnostics
                .iter()
                .all(|d| d.kind == DiagKind::Unsupported)
        );
    }

    #[test]
    fn overlap_rules() {
        assert!(overlap("rm *", "rm -i *"));
        assert!(overlap("*", "anything"));
        assert!(!overlap("ls*", "rm *"));
        assert!(overlap("git *", "git status"));
        assert!(!overlap("git status", "git log"));
    }
}
