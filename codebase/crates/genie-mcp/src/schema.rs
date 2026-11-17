//! Input-schema sanitising for MCP tools before they reach a provider.
//!
//! Spec assumption (06 §7 says names are "sanitised"; schemas are not detailed): servers
//! send arbitrary JSON Schema, and providers reject some of it. We normalise only what every
//! provider needs; per-provider lowering (e.g. Gemini's subset) stays in `genie-providers`
//! (03 §4):
//!
//! * the root is an object schema (`type: "object"`, `properties: {}` when missing);
//! * root `$schema` / `$id` are dropped;
//! * `required` keeps only strings that name a declared property;
//! * schemas larger than [`MAX_SCHEMA_BYTES`] or nested deeper than [`MAX_DEPTH`] are replaced
//!   by a permissive object (the server still validates its own input).

use serde_json::{Map, Value, json};

pub const MAX_SCHEMA_BYTES: usize = 64 * 1024;
pub const MAX_DEPTH: usize = 32;
/// Tool descriptions longer than this are cut (same cap as Claude Code).
pub const MAX_DESCRIPTION_CHARS: usize = 2048;

#[derive(Debug, Clone, PartialEq)]
pub struct Sanitized {
    pub schema: Value,
    pub warnings: Vec<String>,
}

pub fn sanitize_input_schema(schema: Option<&Value>) -> Sanitized {
    let mut warnings = Vec::new();
    let mut root = match schema {
        Some(Value::Object(m)) => m.clone(),
        Some(Value::Null) | None => Map::new(),
        Some(_) => {
            warnings.push("input schema is not an object; replaced".into());
            Map::new()
        }
    };
    root.remove("$schema");
    root.remove("$id");
    match root.get("type") {
        None => {
            root.insert("type".into(), "object".into());
        }
        Some(Value::String(t)) if t == "object" => {}
        Some(_) => {
            warnings.push("input schema root is not `type: object`; replaced".into());
            root = Map::new();
            root.insert("type".into(), "object".into());
        }
    }
    if !matches!(root.get("properties"), Some(Value::Object(_))) {
        root.insert("properties".into(), json!({}));
    }
    if let Some(req) = root.get("required").cloned() {
        let props = root.get("properties").and_then(Value::as_object);
        let kept: Vec<Value> = req
            .as_array()
            .into_iter()
            .flatten()
            .filter(|v| {
                v.as_str()
                    .is_some_and(|s| props.is_some_and(|p| p.contains_key(s)))
            })
            .cloned()
            .collect();
        if kept.len() != req.as_array().map_or(usize::MAX, Vec::len) {
            warnings.push("`required` named undeclared properties; filtered".into());
        }
        if kept.is_empty() {
            root.remove("required");
        } else {
            root.insert("required".into(), Value::Array(kept));
        }
    }
    let value = Value::Object(root);
    let size = serde_json::to_vec(&value).map_or(usize::MAX, |v| v.len());
    if size > MAX_SCHEMA_BYTES || depth(&value) > MAX_DEPTH {
        warnings.push(format!(
            "input schema too large or deep ({size} bytes); replaced by a permissive object"
        ));
        return Sanitized {
            schema: json!({"type": "object", "properties": {}, "additionalProperties": true}),
            warnings,
        };
    }
    Sanitized {
        schema: value,
        warnings,
    }
}

fn depth(v: &Value) -> usize {
    match v {
        Value::Object(m) => 1 + m.values().map(depth).max().unwrap_or(0),
        Value::Array(a) => 1 + a.iter().map(depth).max().unwrap_or(0),
        _ => 0,
    }
}

/// Cut a description to [`MAX_DESCRIPTION_CHARS`] characters, marking the cut.
pub fn cap_description(d: &str) -> String {
    if d.chars().count() <= MAX_DESCRIPTION_CHARS {
        return d.to_string();
    }
    let mut s: String = d.chars().take(MAX_DESCRIPTION_CHARS).collect();
    s.push_str("… [truncated]");
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fills_and_cleans_root() {
        let s = sanitize_input_schema(None);
        assert_eq!(s.schema, json!({"type": "object", "properties": {}}));
        let s = sanitize_input_schema(Some(&json!({
            "$schema": "http://json-schema.org/draft-07/schema#",
            "properties": {"a": {"type": "string"}},
            "required": ["a", "ghost", 3]
        })));
        assert_eq!(
            s.schema,
            json!({"type": "object", "properties": {"a": {"type": "string"}}, "required": ["a"]})
        );
        assert_eq!(s.warnings.len(), 1);
    }

    #[test]
    fn non_object_root_is_replaced() {
        let s = sanitize_input_schema(Some(&json!({"type": "array"})));
        assert_eq!(s.schema["type"], "object");
        assert!(!s.warnings.is_empty());
        let s = sanitize_input_schema(Some(&json!("x")));
        assert_eq!(s.schema["type"], "object");
    }

    #[test]
    fn oversized_is_replaced() {
        let mut nested = json!({"type": "string"});
        for _ in 0..40 {
            nested = json!({"type": "object", "properties": {"n": nested}});
        }
        let s = sanitize_input_schema(Some(&nested));
        assert_eq!(s.schema["additionalProperties"], true);
        assert_eq!(
            cap_description(&"x".repeat(5000)).chars().count(),
            2048 + 13
        );
    }
}
