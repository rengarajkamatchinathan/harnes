//! Tool registry, input repair and validation (06 §4).
//!
//! - The registry only **hides** tools that are unavailable (or fully denied by the
//!   caller's filter); it never authorises calls — each tool does (t06).
//! - Input repair: name aliases (`bash`→`shell`, `str_replace`→`edit`) and camelCase keys
//!   that match a snake_case schema property.
//! - Validation lists at most five issues (01 §7).

use std::collections::BTreeMap;
use std::sync::Arc;

use schemars::JsonSchema;
use schemars::generate::SchemaSettings;
use serde::de::DeserializeOwned;
use serde_json::{Map, Value};

use crate::tool::{MAX_INPUT_ISSUES, Tool, ToolEnv, ToolError, ToolKind};

/// Name aliases models commonly use (06 §4 "input repair").
pub const ALIASES: &[(&str, &str)] = &[
    ("bash", "shell"),
    ("str_replace", "edit"),
    ("str_replace_editor", "edit"),
    ("read_file", "read"),
    ("write_file", "write"),
];

/// A tool definition as sent to the provider.
#[derive(Debug, Clone, PartialEq)]
pub struct ToolSpec {
    pub name: String,
    pub description: String,
    pub input_schema: Value,
    pub kind: ToolKind,
}

#[derive(Clone, Default)]
pub struct ToolRegistry {
    tools: BTreeMap<String, Arc<dyn Tool>>,
}

impl ToolRegistry {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn register(&mut self, tool: Arc<dyn Tool>) -> &mut Self {
        self.tools.insert(tool.name().to_string(), tool);
        self
    }

    pub fn with(mut self, tool: Arc<dyn Tool>) -> Self {
        self.register(tool);
        self
    }

    pub fn len(&self) -> usize {
        self.tools.len()
    }

    pub fn is_empty(&self) -> bool {
        self.tools.is_empty()
    }

    /// Keep only the tools whose name passes `keep` (agent tool lists, 06 §11).
    pub fn retain(&mut self, keep: impl Fn(&str) -> bool) {
        self.tools.retain(|name, _| keep(name));
    }

    /// Tools available in `env`, sorted by name (stable bytes → stable cache prefix).
    pub fn available(&self, env: &ToolEnv) -> Vec<Arc<dyn Tool>> {
        self.tools
            .values()
            .filter(|t| t.availability(env).is_available())
            .cloned()
            .collect()
    }

    /// Definitions for the request (06 §2.1 item 1).
    pub fn specs(&self, env: &ToolEnv) -> Vec<ToolSpec> {
        self.available(env)
            .iter()
            .map(|t| ToolSpec {
                name: t.name().to_string(),
                description: t.description(env).into_owned(),
                input_schema: t.input_schema().clone(),
                kind: t.kind(),
            })
            .collect()
    }

    /// Find a tool by the name the model used, repairing aliases and case.
    pub fn resolve(&self, name: &str) -> Option<Arc<dyn Tool>> {
        if let Some(t) = self.tools.get(name) {
            return Some(t.clone());
        }
        let lower = name.to_ascii_lowercase();
        let canonical = ALIASES
            .iter()
            .find(|(a, _)| *a == lower)
            .map_or(lower.as_str(), |(_, c)| c);
        self.tools.get(canonical).cloned()
    }
}

/// JSON Schema for a tool input type: draft 2020-12, subschemas inlined, no `$schema` or
/// `title` (providers reject or ignore them).
pub fn schema_of<T: JsonSchema>() -> Value {
    let mut settings = SchemaSettings::draft2020_12();
    settings.inline_subschemas = true;
    let schema = settings.into_generator().into_root_schema_for::<T>();
    let mut v = serde_json::to_value(&schema).unwrap_or(Value::Null);
    if let Some(o) = v.as_object_mut() {
        o.remove("$schema");
        o.remove("title");
    }
    v
}

fn snake(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 4);
    for (i, c) in s.chars().enumerate() {
        if c.is_ascii_uppercase() {
            if i > 0 {
                out.push('_');
            }
            out.push(c.to_ascii_lowercase());
        } else if c == '-' {
            out.push('_');
        } else {
            out.push(c);
        }
    }
    out
}

/// Repair top-level keys: `camelCase` / `kebab-case` → the matching `snake_case`
/// property. A JSON string holding an object is parsed (some models double-encode).
pub fn repair_input(input: Value, schema: &Value) -> Value {
    let input = match input {
        Value::String(s) => match serde_json::from_str::<Value>(&s) {
            Ok(v @ Value::Object(_)) => v,
            _ => Value::String(s),
        },
        Value::Null => Value::Object(Map::new()),
        v => v,
    };
    let Value::Object(map) = input else {
        return input;
    };
    let props = schema.get("properties").and_then(Value::as_object);
    let mut out = Map::new();
    for (k, v) in map {
        let key = match props {
            Some(p) if !p.contains_key(&k) && p.contains_key(&snake(&k)) => snake(&k),
            _ => k,
        };
        out.insert(key, v);
    }
    Value::Object(out)
}

fn type_ok(v: &Value, ty: &str) -> bool {
    match ty {
        "string" => v.is_string(),
        "integer" => v.is_i64() || v.is_u64(),
        "number" => v.is_number(),
        "boolean" => v.is_boolean(),
        "array" => v.is_array(),
        "object" => v.is_object(),
        "null" => v.is_null(),
        _ => true,
    }
}

/// Check the input against the top level of `schema`: object, required properties,
/// property types, enums, unknown properties when `additionalProperties: false`.
pub fn check_input(input: &Value, schema: &Value) -> Vec<String> {
    let mut issues = vec![];
    let Some(obj) = input.as_object() else {
        return vec!["input must be a JSON object".into()];
    };
    if let Some(req) = schema.get("required").and_then(Value::as_array) {
        for r in req.iter().filter_map(Value::as_str) {
            if !obj.contains_key(r) {
                issues.push(format!("missing required property `{r}`"));
            }
        }
    }
    let props = schema.get("properties").and_then(Value::as_object);
    for (k, v) in obj {
        let Some(p) = props.and_then(|p| p.get(k)) else {
            if schema.get("additionalProperties") == Some(&Value::Bool(false)) {
                issues.push(format!("unknown property `{k}`"));
            }
            continue;
        };
        let types: Vec<&str> = match p.get("type") {
            Some(Value::String(t)) => vec![t.as_str()],
            Some(Value::Array(ts)) => ts.iter().filter_map(Value::as_str).collect(),
            _ => vec![],
        };
        if !types.is_empty() && !types.iter().any(|t| type_ok(v, t)) {
            issues.push(format!("`{k}` must be {}", types.join(" or ")));
            continue;
        }
        if let Some(en) = p.get("enum").and_then(Value::as_array)
            && !en.contains(v)
        {
            let allowed: Vec<String> = en.iter().map(Value::to_string).collect();
            issues.push(format!("`{k}` must be one of {}", allowed.join(", ")));
        }
    }
    issues.truncate(MAX_INPUT_ISSUES);
    issues
}

/// Repair, validate and deserialise a tool input.
pub fn parse_input<T: DeserializeOwned>(input: Value, schema: &Value) -> Result<T, ToolError> {
    let input = repair_input(input, schema);
    let issues = check_input(&input, schema);
    if !issues.is_empty() {
        return Err(ToolError::invalid_input(issues));
    }
    serde_json::from_value(input).map_err(|e| ToolError::invalid_input(vec![e.to_string()]))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde::Deserialize;
    use serde_json::json;

    #[derive(Debug, Deserialize, JsonSchema, PartialEq)]
    struct In {
        /// The reference.
        spill_ref: String,
        #[serde(default)]
        max_lines: Option<u32>,
    }

    #[test]
    fn schema_is_plain_object_schema() {
        let s = schema_of::<In>();
        assert_eq!(s["type"], "object");
        assert!(s.get("$schema").is_none() && s.get("title").is_none());
        assert_eq!(s["required"], json!(["spill_ref"]));
    }

    #[test]
    fn repair_camel_and_string_inputs() {
        let s = schema_of::<In>();
        let v: In = parse_input(json!({"spillRef": "r1", "maxLines": 3}), &s).unwrap();
        assert_eq!(
            v,
            In {
                spill_ref: "r1".into(),
                max_lines: Some(3)
            }
        );
        let v: In = parse_input(json!("{\"spill_ref\":\"r2\"}"), &s).unwrap();
        assert_eq!(v.spill_ref, "r2");
    }

    #[test]
    fn issues_are_listed_and_capped() {
        let s = schema_of::<In>();
        let e = parse_input::<In>(json!({"max_lines": "x"}), &s).unwrap_err();
        let ToolError::InvalidInput { issues } = e else {
            panic!()
        };
        assert_eq!(
            issues,
            vec![
                "missing required property `spill_ref`".to_string(),
                "`max_lines` must be integer or null".to_string()
            ]
        );
    }
}
