//! JSON-RPC 2.0 envelope, one object per NDJSON line, bidirectional (02 §1).

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::error::RpcError;

/// Request id. Genie clients use numbers; strings are accepted for other tools.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(untagged)]
pub enum Id {
    Num(u64),
    Str(String),
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct Request {
    pub jsonrpc: JsonRpcVersion,
    pub id: Id,
    pub method: String,
    #[serde(default, skip_serializing_if = "Value::is_null")]
    pub params: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct Notification {
    pub jsonrpc: JsonRpcVersion,
    pub method: String,
    #[serde(default, skip_serializing_if = "Value::is_null")]
    pub params: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
pub struct Response {
    pub jsonrpc: JsonRpcVersion,
    pub id: Id,
    #[serde(flatten)]
    pub outcome: Outcome,
}

#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "lowercase")]
pub enum Outcome {
    Result(Value),
    Error(RpcError),
}

/// The literal `"2.0"`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct JsonRpcVersion;

impl JsonSchema for JsonRpcVersion {
    fn schema_name() -> std::borrow::Cow<'static, str> {
        "JsonRpcVersion".into()
    }
    fn json_schema(_: &mut schemars::SchemaGenerator) -> schemars::Schema {
        schemars::json_schema!({ "const": "2.0" })
    }
}

impl Serialize for JsonRpcVersion {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str("2.0")
    }
}

impl<'de> Deserialize<'de> for JsonRpcVersion {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let v = String::deserialize(d)?;
        if v == "2.0" {
            Ok(JsonRpcVersion)
        } else {
            Err(serde::de::Error::custom("jsonrpc must be \"2.0\""))
        }
    }
}

/// Any message that can appear on the wire, in either direction.
#[derive(Debug, Clone)]
pub enum Message {
    Request(Request),
    Notification(Notification),
    Response(Response),
}

impl Message {
    /// Parse one NDJSON line. Errors map to JSON-RPC `-32700` / `-32600`.
    pub fn parse(line: &str) -> Result<Message, RpcError> {
        let v: Value =
            serde_json::from_str(line).map_err(|e| RpcError::parse_error(e.to_string()))?;
        let obj = v
            .as_object()
            .ok_or_else(|| RpcError::invalid_request("not an object"))?;
        let has_id = obj.contains_key("id");
        let has_method = obj.contains_key("method");
        let msg = if has_method && has_id {
            serde_json::from_value(v).map(Message::Request)
        } else if has_method {
            serde_json::from_value(v).map(Message::Notification)
        } else if has_id {
            serde_json::from_value(v).map(Message::Response)
        } else {
            return Err(RpcError::invalid_request(
                "neither request, notification nor response",
            ));
        };
        msg.map_err(|e| RpcError::invalid_request(e.to_string()))
    }

    /// Serialize as one line (no trailing newline; the framer adds it).
    pub fn to_line(&self) -> String {
        let r = match self {
            Message::Request(r) => serde_json::to_string(r),
            Message::Notification(n) => serde_json::to_string(n),
            Message::Response(r) => serde_json::to_string(r),
        };
        // Serializing these plain data types cannot fail.
        r.unwrap_or_else(|_| String::from("{}"))
    }
}

impl Request {
    pub fn new(id: Id, method: impl Into<String>, params: Value) -> Self {
        Request {
            jsonrpc: JsonRpcVersion,
            id,
            method: method.into(),
            params,
        }
    }
}

impl Notification {
    pub fn new(method: impl Into<String>, params: Value) -> Self {
        Notification {
            jsonrpc: JsonRpcVersion,
            method: method.into(),
            params,
        }
    }
}

impl Response {
    pub fn ok(id: Id, result: Value) -> Self {
        Response {
            jsonrpc: JsonRpcVersion,
            id,
            outcome: Outcome::Result(result),
        }
    }
    pub fn err(id: Id, error: RpcError) -> Self {
        Response {
            jsonrpc: JsonRpcVersion,
            id,
            outcome: Outcome::Error(error),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn roundtrip_request_and_response() {
        let req = Message::Request(Request::new(Id::Num(1), "daemon.status", Value::Null));
        let line = req.to_line();
        assert_eq!(line, r#"{"jsonrpc":"2.0","id":1,"method":"daemon.status"}"#);
        assert!(matches!(Message::parse(&line), Ok(Message::Request(_))));

        let resp = Message::Response(Response::ok(Id::Num(1), json!({"ok": true})));
        let line = resp.to_line();
        assert_eq!(line, r#"{"jsonrpc":"2.0","id":1,"result":{"ok":true}}"#);
        assert!(matches!(Message::parse(&line), Ok(Message::Response(_))));
    }

    #[test]
    fn rejects_bad_input() {
        assert_eq!(Message::parse("nope").unwrap_err().code, -32700);
        assert_eq!(Message::parse("[]").unwrap_err().code, -32600);
        assert_eq!(
            Message::parse(r#"{"jsonrpc":"1.0","id":1,"method":"x"}"#)
                .unwrap_err()
                .code,
            -32600
        );
    }
}
