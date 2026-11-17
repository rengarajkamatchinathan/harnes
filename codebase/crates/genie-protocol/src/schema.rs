//! JSON Schema export for codegen and the compatibility check (02 §11).

use schemars::generate::SchemaSettings;
use serde_json::{Map, Value, json};

use crate::methods::{Direction, table};
use crate::version::PROTOCOL_VERSION;

/// `protocol.schema.json`: every type reachable from the method table, as JSON Schema
/// 2020-12 `$defs`.
///
/// `methods.json`: one entry per method with its direction, lease flag and params /
/// result schemas (usually `$ref`s into `$defs`).
pub struct SchemaBundle {
    pub schema: Value,
    pub methods: Value,
}

pub fn bundle() -> SchemaBundle {
    let mut g = SchemaSettings::draft2020_12()
        .with(|s| s.definitions_path = "#/$defs/".into())
        .into_generator();
    let mut methods = Vec::new();
    for m in table() {
        let params = (m.params)(&mut g);
        let result = (m.result)(&mut g);
        methods.push(json!({
            "name": m.name,
            "ident": m.ident,
            "direction": direction_str(m.direction),
            "needs_lease": m.needs_lease,
            "params": params.to_value(),
            "result": result.to_value(),
        }));
    }
    let defs: Map<String, Value> = g.take_definitions(true).into_iter().collect();
    // Deterministic order for a stable diff.
    let defs: Map<String, Value> = {
        let mut v: Vec<(String, Value)> = defs.into_iter().collect();
        v.sort_by(|a, b| a.0.cmp(&b.0));
        v.into_iter().collect()
    };
    let schema = json!({
        "$schema": "https://json-schema.org/draft/2020-12/schema",
        "$id": "https://genie.synergech.com/schema/protocol.schema.json",
        "title": "Genie protocol",
        "version": PROTOCOL_VERSION,
        "$defs": defs,
    });
    SchemaBundle {
        schema,
        methods: json!({ "version": PROTOCOL_VERSION, "methods": methods }),
    }
}

fn direction_str(d: Direction) -> &'static str {
    match d {
        Direction::Request => "request",
        Direction::ServerRequest => "server_request",
        Direction::Notification => "notification",
        Direction::ClientNotification => "client_notification",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bundle_has_core_types() {
        let b = bundle();
        let defs = b.schema["$defs"].as_object().unwrap();
        for t in [
            "SessionEventEnvelope",
            "Ask",
            "PromptParams",
            "InitializeParams",
        ] {
            assert!(defs.contains_key(t), "missing {t}");
        }
        let methods = b.methods["methods"].as_array().unwrap();
        assert!(methods.iter().any(|m| m["name"] == "session.subscribe"));
    }
}
