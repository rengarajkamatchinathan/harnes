//! User-facing wording for protocol and turn errors.
//!
//! The daemon answers methods whose subsystem is not wired yet with `method_not_found` +
//! `data.reason = "not_implemented"`; every CLI surface turns that into one calm line
//! instead of a stack of JSON-RPC detail.

use genie_client::ClientError;
use genie_protocol::ErrorKind;
use genie_protocol::event::ErrorInfo;

/// The method exists in the protocol but this engine build does not serve it yet.
pub fn is_not_implemented(e: &ClientError) -> bool {
    match e {
        ClientError::Rpc(r) => {
            r.kind() == ErrorKind::MethodNotFound
                && r.data
                    .as_ref()
                    .and_then(|d| d.get("reason"))
                    .and_then(|v| v.as_str())
                    == Some("not_implemented")
        }
        _ => false,
    }
}

/// One line for any client error.
pub fn describe(e: &ClientError) -> String {
    if is_not_implemented(e) {
        return "not available in this engine build yet".into();
    }
    match e {
        ClientError::Rpc(r) => match r.kind() {
            ErrorKind::NotDriver => {
                let who = r
                    .data
                    .as_ref()
                    .and_then(|d| d.pointer("/driver/label"))
                    .and_then(|v| v.as_str())
                    .unwrap_or("another client");
                format!("{who} is driving this session (take over to continue here)")
            }
            ErrorKind::NotFound => r.message.clone(),
            ErrorKind::Unauthorized => "the engine refused this client (bad token)".into(),
            ErrorKind::Busy => format!("the engine is busy: {}", r.message),
            ErrorKind::PolicyBlocked => format!("blocked by policy: {}", r.message),
            ErrorKind::UntrustedWorkspace => {
                "this folder is not trusted (run with --trust or `genie trust`)".into()
            }
            ErrorKind::Locked => "secrets are locked; unlock them in the desktop app".into(),
            _ => r.message.clone(),
        },
        ClientError::Closed => "lost the connection to the engine".into(),
        other => other.to_string(),
    }
}

/// `true` when the error code means no model provider is set up.
pub fn is_no_provider(code: &str) -> bool {
    code == "no_provider_configured" || code == "GENIE-PROV:no_provider_configured"
}

/// A settled turn's error, worded for people (no_provider_configured gets a fix).
pub fn turn_error(err: &ErrorInfo) -> String {
    let code = err.code.as_str();
    if is_no_provider(code) || is_no_provider(&err.message) {
        return "No model provider is configured. Add one with `genie login <provider>` \
                or in the desktop app under Settings → Providers."
            .into();
    }
    match code {
        // Today's core reports every provider failure as bare `GENIE-PROV` (the class is
        // not on the wire yet), and with no provider set up that is the usual cause.
        "GENIE-PROV" if err.message == err.code => "The model provider failed. If none is \
             configured yet, add one with `genie login <provider>` or in the desktop app \
             under Settings → Providers."
            .into(),
        "GENIE-PROV" => format!(
            "The model provider returned an error: {}. Check `genie doctor` and your \
             provider settings.",
            err.message
        ),
        "GENIE-TURN-MAXSTEPS" => "The turn reached its step limit.".into(),
        "GENIE-TURN-COMPACT" => "Compaction failed twice; the turn stopped.".into(),
        "interrupted" => "The engine stopped while this turn was running.".into(),
        c if c.starts_with("GENIE-INTERNAL") => {
            let incident = c.split(':').nth(1).unwrap_or("unknown");
            format!("Something went wrong (incident {incident}). See `genie daemon logs`.")
        }
        _ if err.message != err.code => format!("{} ({})", err.message, err.code),
        _ => format!("The turn failed ({code})."),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use genie_protocol::RpcError;
    use serde_json::json;

    #[test]
    fn not_implemented_is_recognised() {
        let e = ClientError::Rpc(
            RpcError::new(ErrorKind::MethodNotFound, "x")
                .with_data(json!({"reason": "not_implemented"})),
        );
        assert!(is_not_implemented(&e));
        assert_eq!(describe(&e), "not available in this engine build yet");
        let plain = ClientError::Rpc(RpcError::method_not_found("y"));
        assert!(!is_not_implemented(&plain));
    }

    #[test]
    fn provider_errors_read_well() {
        let e = ErrorInfo {
            code: "no_provider_configured".into(),
            message: "no_provider_configured".into(),
            link: None,
        };
        assert!(turn_error(&e).contains("genie login"));
        let e = ErrorInfo {
            code: "GENIE-INTERNAL:inc_1".into(),
            message: "GENIE-INTERNAL:inc_1".into(),
            link: None,
        };
        assert!(turn_error(&e).contains("inc_1"));
    }
}
