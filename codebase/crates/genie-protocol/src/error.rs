//! JSON-RPC errors with a typed `data.kind` (02 §8, SA-09).

use std::fmt;

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ErrorKind {
    ParseError,
    InvalidRequest,
    MethodNotFound,
    InvalidParams,
    Unauthorized,
    ProtocolMismatch,
    NotDriver,
    NotFound,
    Conflict,
    Locked,
    PolicyBlocked,
    PermissionDenied,
    UntrustedWorkspace,
    ProviderError,
    Busy,
    InvalidState,
    Cancelled,
    InternalError,
    /// Forward compatibility: unknown kinds from a newer peer (02 §10).
    #[serde(other)]
    Unknown,
}

impl ErrorKind {
    /// The numeric JSON-RPC code for this kind (02 §8).
    pub fn code(self) -> i64 {
        match self {
            ErrorKind::ParseError => -32700,
            ErrorKind::InvalidRequest => -32600,
            ErrorKind::MethodNotFound => -32601,
            ErrorKind::InvalidParams => -32602,
            ErrorKind::Unauthorized => -32001,
            ErrorKind::ProtocolMismatch => -32002,
            ErrorKind::NotDriver => -32003,
            ErrorKind::NotFound => -32004,
            ErrorKind::Conflict => -32005,
            ErrorKind::Locked => -32006,
            ErrorKind::PolicyBlocked => -32007,
            ErrorKind::PermissionDenied => -32008,
            ErrorKind::UntrustedWorkspace => -32009,
            ErrorKind::ProviderError => -32010,
            ErrorKind::Busy => -32011,
            ErrorKind::InvalidState => -32012,
            ErrorKind::Cancelled => -32013,
            ErrorKind::InternalError | ErrorKind::Unknown => -32099,
        }
    }

    pub fn from_code(code: i64) -> ErrorKind {
        const ALL: [ErrorKind; 18] = [
            ErrorKind::ParseError,
            ErrorKind::InvalidRequest,
            ErrorKind::MethodNotFound,
            ErrorKind::InvalidParams,
            ErrorKind::Unauthorized,
            ErrorKind::ProtocolMismatch,
            ErrorKind::NotDriver,
            ErrorKind::NotFound,
            ErrorKind::Conflict,
            ErrorKind::Locked,
            ErrorKind::PolicyBlocked,
            ErrorKind::PermissionDenied,
            ErrorKind::UntrustedWorkspace,
            ErrorKind::ProviderError,
            ErrorKind::Busy,
            ErrorKind::InvalidState,
            ErrorKind::Cancelled,
            ErrorKind::InternalError,
        ];
        ALL.into_iter()
            .find(|k| k.code() == code)
            .unwrap_or(ErrorKind::Unknown)
    }

    fn as_str(self) -> &'static str {
        match self {
            ErrorKind::ParseError => "parse_error",
            ErrorKind::InvalidRequest => "invalid_request",
            ErrorKind::MethodNotFound => "method_not_found",
            ErrorKind::InvalidParams => "invalid_params",
            ErrorKind::Unauthorized => "unauthorized",
            ErrorKind::ProtocolMismatch => "protocol_mismatch",
            ErrorKind::NotDriver => "not_driver",
            ErrorKind::NotFound => "not_found",
            ErrorKind::Conflict => "conflict",
            ErrorKind::Locked => "locked",
            ErrorKind::PolicyBlocked => "policy_blocked",
            ErrorKind::PermissionDenied => "permission_denied",
            ErrorKind::UntrustedWorkspace => "untrusted_workspace",
            ErrorKind::ProviderError => "provider_error",
            ErrorKind::Busy => "busy",
            ErrorKind::InvalidState => "invalid_state",
            ErrorKind::Cancelled => "cancelled",
            ErrorKind::InternalError => "internal_error",
            ErrorKind::Unknown => "unknown",
        }
    }
}

/// A JSON-RPC error object. `data.kind` is always present for Genie errors.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema)]
pub struct RpcError {
    pub code: i64,
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data: Option<Value>,
}

impl RpcError {
    pub fn new(kind: ErrorKind, message: impl Into<String>) -> Self {
        RpcError {
            code: kind.code(),
            message: message.into(),
            data: Some(json!({ "kind": kind.as_str() })),
        }
    }

    /// Add extra fields to `data` next to `kind` (e.g. `{daemon_protocol, client_protocol}`).
    pub fn with_data(mut self, extra: Value) -> Self {
        let mut map = match self.data.take() {
            Some(Value::Object(m)) => m,
            _ => Map::new(),
        };
        if let Value::Object(extra) = extra {
            map.extend(extra);
        }
        self.data = Some(Value::Object(map));
        self
    }

    /// The typed kind: from `data.kind` if present, else from the code.
    pub fn kind(&self) -> ErrorKind {
        self.data
            .as_ref()
            .and_then(|d| d.get("kind"))
            .and_then(|k| serde_json::from_value(k.clone()).ok())
            .unwrap_or_else(|| ErrorKind::from_code(self.code))
    }

    pub fn parse_error(msg: impl Into<String>) -> Self {
        Self::new(ErrorKind::ParseError, msg)
    }
    pub fn invalid_request(msg: impl Into<String>) -> Self {
        Self::new(ErrorKind::InvalidRequest, msg)
    }
    pub fn method_not_found(method: &str) -> Self {
        Self::new(
            ErrorKind::MethodNotFound,
            format!("method not found: {method}"),
        )
    }
    pub fn invalid_params(msg: impl Into<String>) -> Self {
        Self::new(ErrorKind::InvalidParams, msg)
    }
    /// Internal defects carry only an incident id; details stay in the logs (01 §7 rule 1).
    pub fn internal(incident_id: impl Into<String>) -> Self {
        Self::new(ErrorKind::InternalError, "internal error")
            .with_data(json!({ "incident_id": incident_id.into() }))
    }
}

impl fmt::Display for RpcError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{} ({}): {}",
            self.kind().as_str(),
            self.code,
            self.message
        )
    }
}

impl std::error::Error for RpcError {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codes_are_stable_and_roundtrip() {
        assert_eq!(ErrorKind::Unauthorized.code(), -32001);
        assert_eq!(ErrorKind::InternalError.code(), -32099);
        for code in [-32700, -32001, -32013, -32099] {
            assert_eq!(ErrorKind::from_code(code).code(), code);
        }
        let e = RpcError::new(ErrorKind::ProtocolMismatch, "x")
            .with_data(json!({"daemon_protocol": "1.0.0"}));
        let back: RpcError = serde_json::from_str(&serde_json::to_string(&e).unwrap()).unwrap();
        assert_eq!(back.kind(), ErrorKind::ProtocolMismatch);
        assert_eq!(back.data.unwrap()["daemon_protocol"], "1.0.0");
    }

    #[test]
    fn unknown_kind_is_tolerated() {
        let e: RpcError =
            serde_json::from_str(r#"{"code":-32050,"message":"m","data":{"kind":"brand_new"}}"#)
                .unwrap();
        assert_eq!(e.kind(), ErrorKind::Unknown);
    }
}
