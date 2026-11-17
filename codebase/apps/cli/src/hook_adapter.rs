//! `genie hook-adapter claude -- <script>` (D48, 06 §12): runs an imported Claude hook
//! under Genie's hook envelope.
//!
//! In: Genie `{genieHook:1, event, session, cwd, agent, mode, data}` on stdin, rewritten
//! to Claude's hook input. Out: Claude's exit-code / JSON conventions mapped to Genie's
//! `{decision?, message?, updatedInput?, addContext?}`.

use std::io::{Read, Write};
use std::process::{Command, Stdio};

use anyhow::{Context, Result};
use serde_json::{Map, Value, json};

fn claude_event(genie: &str) -> &str {
    match genie {
        "tool.before" => "PreToolUse",
        "tool.after" => "PostToolUse",
        "prompt.submit" => "UserPromptSubmit",
        "turn.end" => "Stop",
        "subagent.end" => "SubagentStop",
        "session.start" => "SessionStart",
        "session.end" => "SessionEnd",
        "compact.before" => "PreCompact",
        "notification" => "Notification",
        "permission.request" => "PermissionRequest",
        other => other,
    }
}

fn claude_mode(m: &str) -> &str {
    match m {
        "accept-edits" => "acceptEdits",
        "bypass" => "bypassPermissions",
        other => other,
    }
}

/// Genie envelope → Claude hook input.
pub fn translate_input(env: &Value) -> Value {
    let event = env["event"].as_str().unwrap_or_default();
    let data = &env["data"];
    let mut v = Map::new();
    v.insert("session_id".into(), env["session"].clone());
    v.insert("hook_event_name".into(), json!(claude_event(event)));
    v.insert("cwd".into(), env["cwd"].clone());
    v.insert(
        "permission_mode".into(),
        json!(claude_mode(env["mode"].as_str().unwrap_or("default"))),
    );
    for (from, to) in [
        ("tool", "tool_name"),
        ("input", "tool_input"),
        ("output", "tool_response"),
        ("prompt", "prompt"),
        ("message", "message"),
        ("trigger", "trigger"),
    ] {
        if let Some(x) = data.get(from) {
            v.insert(to.into(), x.clone());
        }
    }
    if event == "turn.end" || event == "subagent.end" {
        v.insert("stop_hook_active".into(), json!(false));
    }
    Value::Object(v)
}

/// Claude result → (Genie output, exit code). Exit 2 blocks (stderr is the reason);
/// other non-zero codes are hook errors and pass through.
pub fn translate_output(event: &str, code: i32, stdout: &str, stderr: &str) -> (Value, i32) {
    let blocking_word = match event {
        "tool.before" | "permission.request" => "deny",
        "turn.end" | "subagent.end" => "continue",
        _ => "block",
    };
    if code == 2 {
        return (
            json!({"decision": blocking_word, "message": stderr.trim()}),
            0,
        );
    }
    if code != 0 {
        return (json!({}), code);
    }
    let mut out = Map::new();
    let trimmed = stdout.trim();
    match serde_json::from_str::<Value>(trimmed) {
        Ok(Value::Object(o)) => {
            let hso = o.get("hookSpecificOutput").cloned().unwrap_or_default();
            if let Some(d) = hso.get("permissionDecision").and_then(Value::as_str) {
                out.insert("decision".into(), json!(d));
                if let Some(r) = hso.get("permissionDecisionReason") {
                    out.insert("message".into(), r.clone());
                }
            } else if let Some(d) = o.get("decision").and_then(Value::as_str) {
                let mapped = match d {
                    "block" => blocking_word,
                    "approve" => "allow",
                    other => other,
                };
                out.insert("decision".into(), json!(mapped));
            }
            if let Some(r) = o.get("reason")
                && !out.contains_key("message")
            {
                out.insert("message".into(), r.clone());
            }
            if let Some(u) = hso.get("updatedInput") {
                out.insert("updatedInput".into(), u.clone());
            }
            if let Some(c) = hso.get("additionalContext") {
                out.insert("addContext".into(), c.clone());
            }
            if o.get("continue") == Some(&Value::Bool(false)) {
                out.insert("decision".into(), json!("block"));
                if let Some(r) = o.get("stopReason") {
                    out.insert("message".into(), r.clone());
                }
            }
        }
        _ if !trimmed.is_empty() && matches!(event, "prompt.submit" | "session.start") => {
            // Plain stdout on these events is extra context in Claude Code.
            out.insert("addContext".into(), json!(trimmed));
        }
        _ => {}
    }
    (Value::Object(out), 0)
}

/// Run the script; returns the process exit code.
pub fn run(script: &[String]) -> Result<i32> {
    let mut input = String::new();
    std::io::stdin().read_to_string(&mut input)?;
    let env: Value = serde_json::from_str(&input).unwrap_or(Value::Null);
    let event = env["event"].as_str().unwrap_or_default().to_string();
    let claude_in = translate_input(&env);

    let line = script.join(" ");
    let mut cmd = if cfg!(windows) {
        let mut c = Command::new("cmd");
        c.arg("/C").arg(&line);
        c
    } else {
        let mut c = Command::new("sh");
        c.arg("-c").arg(&line);
        c
    };
    if let Some(cwd) = env["cwd"].as_str() {
        cmd.current_dir(cwd);
    }
    let mut child = cmd
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .with_context(|| format!("running hook `{line}`"))?;
    if let Some(mut stdin) = child.stdin.take() {
        let _ = stdin.write_all(claude_in.to_string().as_bytes());
    }
    let out = child.wait_with_output()?;
    let (genie_out, code) = translate_output(
        &event,
        out.status.code().unwrap_or(1),
        &String::from_utf8_lossy(&out.stdout),
        &String::from_utf8_lossy(&out.stderr),
    );
    if code != 0 {
        std::io::stderr().write_all(&out.stderr)?;
    } else {
        println!("{genie_out}");
    }
    Ok(code)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn input_mapping() {
        let env = json!({"genieHook": 1, "event": "tool.before", "session": "ses_1", "cwd": "/p",
                         "mode": "accept-edits", "data": {"tool": "Bash", "input": {"command": "ls"}}});
        let c = translate_input(&env);
        assert_eq!(c["hook_event_name"], "PreToolUse");
        assert_eq!(c["tool_name"], "Bash");
        assert_eq!(c["tool_input"]["command"], "ls");
        assert_eq!(c["permission_mode"], "acceptEdits");
    }

    #[test]
    fn output_mapping() {
        assert_eq!(
            translate_output("tool.before", 2, "", "no rm\n"),
            (json!({"decision": "deny", "message": "no rm"}), 0)
        );
        assert_eq!(translate_output("tool.before", 1, "", "boom").1, 1);
        let (v, _) = translate_output(
            "tool.before",
            0,
            r#"{"hookSpecificOutput": {"permissionDecision": "ask", "permissionDecisionReason": "check", "updatedInput": {"command": "ls -l"}}}"#,
            "",
        );
        assert_eq!(
            v,
            json!({"decision": "ask", "message": "check", "updatedInput": {"command": "ls -l"}})
        );
        let (v, _) = translate_output(
            "turn.end",
            0,
            r#"{"decision": "block", "reason": "run tests"}"#,
            "",
        );
        assert_eq!(v, json!({"decision": "continue", "message": "run tests"}));
        let (v, _) = translate_output("prompt.submit", 0, "today is Monday\n", "");
        assert_eq!(v, json!({"addContext": "today is Monday"}));
    }

    #[cfg(unix)]
    #[test]
    fn runs_through_the_shell() {
        // Exercise the process path without stdin plumbing: sh -c with exit 2.
        let out = Command::new("sh")
            .arg("-c")
            .arg("echo nope >&2; exit 2")
            .output()
            .unwrap_or_else(|e| panic!("{e}"));
        let (v, code) = translate_output(
            "tool.before",
            out.status.code().unwrap_or(1),
            "",
            &String::from_utf8_lossy(&out.stderr),
        );
        assert_eq!(code, 0);
        assert_eq!(v["decision"], "deny");
        assert_eq!(v["message"], "nope");
    }
}
