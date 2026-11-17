//! Subcommands (07 §2.2) and `genie chat` listing (§3). Each is a thin wrapper over a
//! protocol method; methods this engine build does not serve yet print one calm line.
//! Output goes to a writer so the commands are testable against an in-memory daemon.

use std::io::Write;
use std::path::Path;

use anyhow::Result;
use genie_client::{ClientError, Connection};
use genie_protocol::api::session::{
    ExportFormat, ExportParams, ListParams, PendingParams, RespondParams,
};
use genie_protocol::ask::{AskAnswer, Decision};
use genie_protocol::common::{RuleScope, SessionKind};
use genie_protocol::methods::{self as m, Method};
use serde_json::{Value, json};

use crate::args::{
    ApprovalsAction, ConfigAction, ExportFormatArg, ImportArgs, ImportSource, McpAction, ScopeArg,
    TrustAction,
};
use crate::errors;
use crate::import::{self, Filter, Level, Roots, Source, Status};
use crate::render;

pub type Out<'a> = &'a mut (dyn Write + Send);

/// Print a failed call; `not_implemented` gets the calm wording. Returns exit code 1.
pub fn fail(err: Out<'_>, what: &str, e: &ClientError) -> u8 {
    let _ = writeln!(err, "genie: {what}: {}", errors::describe(e));
    1
}

fn scope_str(s: ScopeArg) -> &'static str {
    match s {
        ScopeArg::User => "user",
        ScopeArg::Project => "project",
        ScopeArg::Local => "local",
    }
}

// ---------------------------------------------------------------- sessions / chat

pub async fn sessions(
    conn: &Connection,
    kind: Option<SessionKind>,
    json_out: bool,
    out: Out<'_>,
    err: Out<'_>,
) -> u8 {
    let r = conn
        .call::<m::SessionList>(ListParams {
            kind,
            ..Default::default()
        })
        .await;
    let list = match r {
        Ok(l) => l,
        Err(e) => return fail(err, "listing sessions", &e),
    };
    if json_out {
        let _ = writeln!(
            out,
            "{}",
            serde_json::to_string_pretty(&list.items).unwrap_or_default()
        );
        return 0;
    }
    if list.items.is_empty() {
        let _ = writeln!(out, "No sessions.");
    }
    for s in &list.items {
        let _ = writeln!(
            out,
            "{}  {:<5} {:<12} {}  {}",
            s.session_id,
            match s.kind {
                SessionKind::Chat => "chat",
                SessionKind::Code => "code",
                SessionKind::Unknown => "?",
            },
            format!("{:?}", s.status).to_lowercase(),
            s.updated_at,
            s.title.as_deref().unwrap_or("(untitled)")
        );
    }
    0
}

/// `genie chat list`: title, project, updated (§3).
pub async fn chat_list(conn: &Connection, json_out: bool, out: Out<'_>, err: Out<'_>) -> u8 {
    let list = match conn
        .call::<m::SessionList>(ListParams {
            kind: Some(SessionKind::Chat),
            ..Default::default()
        })
        .await
    {
        Ok(l) => l,
        Err(e) => return fail(err, "listing chats", &e),
    };
    if json_out {
        let _ = writeln!(
            out,
            "{}",
            serde_json::to_string_pretty(&list.items).unwrap_or_default()
        );
        return 0;
    }
    if list.items.is_empty() {
        let _ = writeln!(out, "No chats yet. Start one with `genie chat new`.");
        return 0;
    }
    let _ = writeln!(
        out,
        "{:<32} {:<40} {:<16} UPDATED",
        "ID", "TITLE", "PROJECT"
    );
    for s in &list.items {
        let _ = writeln!(
            out,
            "{:<32} {:<40} {:<16} {}",
            s.session_id,
            render::truncate(s.title.as_deref().unwrap_or("(untitled)"), 40),
            s.project_id.as_deref().unwrap_or("—"),
            s.updated_at
        );
    }
    0
}

// ---------------------------------------------------------------- approvals (D51)

pub async fn approvals(
    conn: &Connection,
    action: Option<ApprovalsAction>,
    out: Out<'_>,
    err: Out<'_>,
) -> u8 {
    match action.unwrap_or(ApprovalsAction::List { json: false }) {
        ApprovalsAction::List { json: json_out } => {
            let r = match conn
                .call::<m::PermissionPending>(PendingParams::default())
                .await
            {
                Ok(r) => r,
                Err(e) => return fail(err, "listing approvals", &e),
            };
            if json_out {
                let _ = writeln!(
                    out,
                    "{}",
                    serde_json::to_string_pretty(&r.asks).unwrap_or_default()
                );
                return 0;
            }
            if r.asks.is_empty() {
                let _ = writeln!(out, "No pending approvals.");
            }
            for a in &r.asks {
                let _ = writeln!(
                    out,
                    "{}  {}  {}",
                    a.id,
                    a.session_id,
                    render::ask_summary(a)
                );
                if let Some(l) = render::always_label(a) {
                    let _ = writeln!(out, "    always would save: {l}");
                }
            }
            0
        }
        ApprovalsAction::Approve { id, always } => {
            let scope = if always {
                let pending = conn
                    .call::<m::PermissionPending>(PendingParams::default())
                    .await
                    .map(|p| p.asks)
                    .unwrap_or_default();
                Some(
                    pending
                        .iter()
                        .find(|a| a.id == id)
                        .and_then(|a| a.save_suggestions.first().map(|s| s.scope))
                        .unwrap_or(RuleScope::Session),
                )
            } else {
                None
            };
            let answer = AskAnswer {
                decision: if always {
                    Decision::Always
                } else {
                    Decision::Once
                },
                scope,
                message: None,
                data: None,
            };
            respond(conn, id, answer, "approved", out, err).await
        }
        ApprovalsAction::Deny { id, message } => {
            let answer = AskAnswer {
                decision: Decision::Reject,
                scope: None,
                message,
                data: None,
            };
            respond(conn, id, answer, "denied", out, err).await
        }
    }
}

async fn respond(
    conn: &Connection,
    ask_id: String,
    answer: AskAnswer,
    word: &str,
    out: Out<'_>,
    err: Out<'_>,
) -> u8 {
    match conn
        .call::<m::PermissionRespond>(RespondParams {
            ask_id: ask_id.clone(),
            answer,
        })
        .await
    {
        Ok(_) => {
            let _ = writeln!(out, "{word} {ask_id}");
            0
        }
        Err(e) => fail(err, &format!("answering {ask_id}"), &e),
    }
}

// ---------------------------------------------------------------- export (D24)

pub async fn export(
    conn: &Connection,
    session: String,
    format: ExportFormatArg,
    sanitize: bool,
    out: Out<'_>,
    err: Out<'_>,
) -> u8 {
    let r = conn
        .call::<m::SessionExport>(ExportParams {
            session_id: session.clone(),
            format: match format {
                ExportFormatArg::Json => ExportFormat::Json,
                ExportFormatArg::Md => ExportFormat::Markdown,
            },
            sanitize,
        })
        .await;
    match r {
        Ok(r) => {
            match (r.text, r.blob_id) {
                (Some(t), _) => {
                    let _ = write!(out, "{t}");
                }
                (None, Some(b)) => {
                    let _ = writeln!(out, "exported to blob {b}");
                }
                _ => {}
            }
            0
        }
        Err(e) => fail(err, &format!("exporting {session}"), &e),
    }
}

// ---------------------------------------------------------------- config (D23)

pub async fn config(
    conn: &Connection,
    action: ConfigAction,
    cwd: &Path,
    genie_home: Option<&Path>,
    out: Out<'_>,
    err: Out<'_>,
) -> u8 {
    let cwd_s = cwd.to_string_lossy().into_owned();
    match action {
        ConfigAction::Path => {
            if let Some(h) = genie_home {
                let _ = writeln!(out, "user     {}", h.join("settings.json").display());
            }
            let _ = writeln!(
                out,
                "project  {}",
                cwd.join(".genie/settings.json").display()
            );
            let _ = writeln!(
                out,
                "local    {}",
                cwd.join(".genie/settings.local.json").display()
            );
            0
        }
        ConfigAction::Get { key } => {
            let r = conn
                .request_value(
                    m::ConfigGet::NAME,
                    json!({"keys": key.map(|k| vec![k]), "cwd": cwd_s}),
                )
                .await;
            match r {
                Ok(v) => {
                    let _ = writeln!(
                        out,
                        "{}",
                        serde_json::to_string_pretty(&v["values"]).unwrap_or_default()
                    );
                    0
                }
                Err(e) => fail(err, "config get", &e),
            }
        }
        ConfigAction::Set { key, value, scope } => {
            let v: Value = serde_json::from_str(&value).unwrap_or(Value::String(value));
            let r = conn
                .request_value(
                    m::ConfigSet::NAME,
                    json!({"key": key, "value": v, "scope": scope_str(scope), "cwd": cwd_s}),
                )
                .await;
            match r {
                Ok(_) => 0,
                Err(e) => fail(err, "config set", &e),
            }
        }
        ConfigAction::Doctor => {
            match conn
                .request_value(m::ConfigDiagnostics::NAME, json!({"cwd": cwd_s}))
                .await
            {
                Ok(v) => {
                    let problems = v["problems"].as_array().cloned().unwrap_or_default();
                    if problems.is_empty() {
                        let _ = writeln!(out, "No configuration problems.");
                    }
                    for p in problems {
                        let _ = writeln!(
                            out,
                            "{:<8} {}{}",
                            p["severity"].as_str().unwrap_or("?"),
                            p["key"]
                                .as_str()
                                .map(|k| format!("{k}: "))
                                .unwrap_or_default(),
                            p["message"].as_str().unwrap_or_default()
                        );
                    }
                    0
                }
                Err(e) => fail(err, "config doctor", &e),
            }
        }
    }
}

// ---------------------------------------------------------------- login / mcp / trust

pub async fn login(
    conn: &Connection,
    provider: String,
    device_code: bool,
    secret: Option<String>,
    out: Out<'_>,
    err: Out<'_>,
) -> u8 {
    if device_code {
        let r = conn
            .request_value(
                m::ProviderLoginStart::NAME,
                json!({"provider": provider, "method": "device_code"}),
            )
            .await;
        return match r {
            Ok(v) => {
                let _ = writeln!(
                    out,
                    "Open {} and enter code {}",
                    v["verification_uri"].as_str().unwrap_or("?"),
                    v["user_code"].as_str().unwrap_or("?")
                );
                0
            }
            Err(e) => fail(err, &format!("login {provider}"), &e),
        };
    }
    let Some(secret) = secret else {
        let _ = writeln!(err, "genie: no API key entered");
        return 1;
    };
    match conn
        .request_value(
            m::ProviderCredentialAdd::NAME,
            json!({"provider": provider, "label": "cli", "kind": "key", "secret": secret}),
        )
        .await
    {
        Ok(_) => {
            let _ = writeln!(out, "Saved a key for {provider} in the keychain.");
            0
        }
        Err(e) => fail(err, &format!("login {provider}"), &e),
    }
}

pub async fn logout(conn: &Connection, provider: String, out: Out<'_>, err: Out<'_>) -> u8 {
    let list = match conn.request_value(m::ProviderList::NAME, json!({})).await {
        Ok(v) => v,
        Err(e) => return fail(err, &format!("logout {provider}"), &e),
    };
    let ids: Vec<String> = list["providers"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|p| p["id"] == provider.as_str())
        .flat_map(|p| p["credentials"].as_array().cloned().unwrap_or_default())
        .filter_map(|c| c["credential_id"].as_str().map(str::to_string))
        .collect();
    for id in &ids {
        if let Err(e) = conn
            .request_value(
                m::ProviderCredentialRemove::NAME,
                json!({"credential_id": id}),
            )
            .await
        {
            return fail(err, &format!("logout {provider}"), &e);
        }
    }
    let _ = writeln!(out, "Removed {} credential(s) for {provider}.", ids.len());
    0
}

pub async fn mcp(
    conn: &Connection,
    action: Option<McpAction>,
    cwd: &Path,
    out: Out<'_>,
    err: Out<'_>,
) -> u8 {
    let (what, method, params) = match action.unwrap_or(McpAction::List) {
        McpAction::List => (
            "mcp list",
            m::McpList::NAME,
            json!({"cwd": cwd.to_string_lossy()}),
        ),
        McpAction::Add {
            name,
            scope,
            url,
            command,
        } => {
            let config = match url {
                Some(u) => json!({"type": "http", "url": u}),
                None => {
                    json!({"command": command.first(), "args": command.get(1..).unwrap_or_default()})
                }
            };
            (
                "mcp add",
                m::McpAdd::NAME,
                json!({"scope": scope_str(scope), "name": name, "config": config, "cwd": cwd.to_string_lossy()}),
            )
        }
        McpAction::Remove { name, scope } => (
            "mcp remove",
            m::McpRemove::NAME,
            json!({"scope": scope_str(scope), "name": name, "cwd": cwd.to_string_lossy()}),
        ),
        McpAction::Auth { name } => ("mcp auth", m::McpAuthStart::NAME, json!({"name": name})),
        McpAction::Logs { name, lines } => (
            "mcp logs",
            m::McpLogs::NAME,
            json!({"name": name, "tail": lines}),
        ),
    };
    match conn.request_value(method, params).await {
        Ok(v) => {
            if let Some(servers) = v["servers"].as_array() {
                if servers.is_empty() {
                    let _ = writeln!(out, "No MCP servers.");
                }
                for s in servers {
                    let _ = writeln!(
                        out,
                        "{:<20} {:<10} {:<8} {} tools",
                        s["name"].as_str().unwrap_or("?"),
                        s["status"].as_str().unwrap_or("?"),
                        s["scope"].as_str().unwrap_or("?"),
                        s["tools"]
                    );
                }
            } else if let Some(lines) = v["lines"].as_array() {
                for l in lines {
                    let _ = writeln!(out, "{}", l.as_str().unwrap_or_default());
                }
            } else if let Some(u) = v["auth_url"].as_str() {
                let _ = writeln!(out, "Open {u} to sign in.");
            }
            0
        }
        Err(e) => fail(err, what, &e),
    }
}

pub async fn trust(
    conn: &Connection,
    action: Option<TrustAction>,
    cwd: &Path,
    out: Out<'_>,
    err: Out<'_>,
) -> u8 {
    match action.unwrap_or(TrustAction::List) {
        TrustAction::List => {
            match conn
                .request_value(m::TrustGet::NAME, json!({"path": cwd.to_string_lossy()}))
                .await
            {
                Ok(v) => {
                    let _ = writeln!(
                        out,
                        "{}  {}",
                        v["path"].as_str().unwrap_or_default(),
                        v["state"].as_str().unwrap_or("unknown")
                    );
                    0
                }
                Err(e) => fail(err, "trust list", &e),
            }
        }
        TrustAction::Revoke { path } => {
            match conn
                .request_value(
                    m::TrustSet::NAME,
                    json!({"path": path.to_string_lossy(), "decision": "revoke"}),
                )
                .await
            {
                Ok(_) => {
                    let _ = writeln!(out, "Revoked trust for {}", path.display());
                    0
                }
                Err(e) => fail(err, "trust revoke", &e),
            }
        }
    }
}

// ---------------------------------------------------------------- import (§5)

/// `genie import`: preview, then apply after confirmation. `confirm` asks the user.
pub fn import_cmd(
    args: &ImportArgs,
    roots: &Roots,
    out: Out<'_>,
    confirm: &mut dyn FnMut(&str) -> bool,
    secrets: &mut dyn import::SecretSink,
) -> Result<u8> {
    let filter = Filter {
        source: args.source.map(|s| match s {
            ImportSource::ClaudeCode => Source::ClaudeCode,
            ImportSource::ClaudeDesktop => Source::ClaudeDesktop,
            ImportSource::Opencode => Source::Opencode,
        }),
        level: if args.project {
            Some(Level::Project)
        } else if args.user {
            Some(Level::User)
        } else {
            None
        },
    };
    let mut items = import::preview(roots, filter);
    if args.json && args.dry_run {
        writeln!(out, "{}", serde_json::to_string_pretty(&items)?)?;
        return Ok(0);
    }
    if items.is_empty() {
        writeln!(out, "Nothing to import found.")?;
        return Ok(0);
    }
    for it in &items {
        let status = match it.status {
            Status::Import => "import",
            Status::Skip => "skip",
            Status::Conflict => "conflict",
        };
        writeln!(
            out,
            "{status:<9} {:<14} {:<20} {:<8} {}",
            format!("{:?}", it.source),
            format!("{:?}", it.kind),
            format!("{:?}", it.level).to_lowercase(),
            it.name
        )?;
        if let Some(r) = &it.reason {
            writeln!(out, "          {r}")?;
        }
        for d in &it.diagnostics {
            writeln!(
                out,
                "          ! {:?}{}: {}",
                d.kind,
                d.key
                    .as_deref()
                    .map(|k| format!(" `{k}`"))
                    .unwrap_or_default(),
                d.message
            )?;
        }
    }
    let todo = items.iter().filter(|i| i.status != Status::Skip).count();
    if args.dry_run || todo == 0 {
        writeln!(out, "{todo} item(s) to import. Nothing written.")?;
        return Ok(0);
    }
    if !args.yes && !confirm(&format!("Import {todo} item(s)?")) {
        writeln!(out, "Cancelled; nothing written.")?;
        return Ok(0);
    }
    let needs_append = items.iter().any(|i| {
        i.status == Status::Conflict
            && i.plans
                .iter()
                .any(|p| matches!(p, import::Plan::WriteText { to, .. } if to.exists()))
    });
    let confirm_append =
        needs_append && (args.yes || confirm("Append @-imports to your existing GENIE.md?"));
    let report = import::apply(
        roots,
        &mut items,
        &import::ApplyOptions {
            selected: None,
            confirm_append,
            date: import::today(),
        },
        secrets,
    )?;
    let applied = report.items.iter().filter(|i| i.applied).count();
    writeln!(out, "Imported {applied} item(s).")?;
    if let Some(p) = &report.path {
        writeln!(out, "Report: {}", p.display())?;
    }
    writeln!(
        out,
        "Imported project hooks, MCP servers and `!cmd` commands still need trust and approval before they run."
    )?;
    Ok(0)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]
    use super::*;

    #[test]
    fn import_dry_run_writes_nothing_and_yes_applies() {
        let d = tempfile::tempdir().unwrap();
        let home = d.path().join("home");
        let project = d.path().join("proj");
        std::fs::create_dir_all(project.join(".claude")).unwrap();
        std::fs::write(project.join("CLAUDE.md"), "x").unwrap();
        let roots = Roots {
            genie_home: home.join(".genie"),
            claude_desktop: home.join("cd"),
            opencode: home.join("oc"),
            home,
            project: project.clone(),
        };
        let mut args = ImportArgs {
            source: Some(ImportSource::ClaudeCode),
            project: true,
            user: false,
            dry_run: true,
            yes: false,
            json: false,
        };
        let mut out = Vec::new();
        let mut never = |_: &str| panic!("dry run must not ask");
        import_cmd(&args, &roots, &mut out, &mut never, &mut import::NoKeychain).unwrap();
        let s = String::from_utf8(out).unwrap();
        assert!(s.contains("import    ClaudeCode"), "{s}");
        assert!(s.contains("Nothing written"));
        assert!(!project.join("GENIE.md").exists());

        args.dry_run = false;
        let mut out = Vec::new();
        let mut no = |_: &str| false;
        import_cmd(&args, &roots, &mut out, &mut no, &mut import::NoKeychain).unwrap();
        assert!(!project.join("GENIE.md").exists());
        args.yes = true;
        let mut out = Vec::new();
        import_cmd(&args, &roots, &mut out, &mut no, &mut import::NoKeychain).unwrap();
        assert_eq!(
            std::fs::read_to_string(project.join("GENIE.md")).unwrap(),
            "@CLAUDE.md\n"
        );
    }
}
