//! Scanners for the v1 sources (07 §5.1): Claude Code, Claude Desktop, OpenCode.

use std::path::{Path, PathBuf};

use serde_json::{Map, Value, json};

use super::perms::{self, Ordered};
use super::{
    Diag, DiagKind, Item, Kind, Level, Plan, Roots, Source, hash_bytes, hash_dir, hash_file,
    looks_secret, parse_jsonc, read_json, secret_ref,
};

pub fn scan(roots: &Roots) -> Vec<Item> {
    let mut items = vec![];
    claude_code(roots, &mut items);
    claude_desktop(roots, &mut items);
    opencode(roots, &mut items);
    items
}

// ---------------------------------------------------------------- Claude Code

fn claude_code(r: &Roots, out: &mut Vec<Item>) {
    let user = r.home.join(".claude");
    let proj = r.project.join(".claude");
    for (path, level, target) in [
        (user.join("settings.json"), Level::User, r.user_settings()),
        (
            proj.join("settings.json"),
            Level::Project,
            r.project_settings(),
        ),
        (
            proj.join("settings.local.json"),
            Level::Project,
            r.local_settings(),
        ),
    ] {
        if let Some(v) = read_json(&path) {
            claude_settings(&path, level, &target, &v, out);
        }
    }

    // MCP from ~/.claude.json: user scope and this project's local scope.
    let cj = r.home.join(".claude.json");
    if let Some(v) = read_json(&cj) {
        if let Some(servers) = v.get("mcpServers").and_then(Value::as_object) {
            mcp_item(
                Source::ClaudeCode,
                Level::User,
                &cj,
                "user",
                servers,
                &r.user_settings(),
                out,
            );
        }
        let key = r.project.to_string_lossy().into_owned();
        if let Some(servers) = v
            .pointer(&format!(
                "/projects/{}/mcpServers",
                key.replace('~', "~0").replace('/', "~1")
            ))
            .and_then(Value::as_object)
        {
            mcp_item(
                Source::ClaudeCode,
                Level::Project,
                &cj,
                "local",
                servers,
                &r.local_settings(),
                out,
            );
        }
    }

    // CLAUDE.md → GENIE.md with an @-import (read natively from then on).
    for (src, dst, line, level) in [
        (
            r.project.join("CLAUDE.md"),
            r.project.join("GENIE.md"),
            "@CLAUDE.md".to_string(),
            Level::Project,
        ),
        (
            r.project.join("CLAUDE.local.md"),
            r.project.join("GENIE.local.md"),
            "@CLAUDE.local.md".to_string(),
            Level::Project,
        ),
        (
            user.join("CLAUDE.md"),
            r.genie_home.join("GENIE.md"),
            format!("@{}", user.join("CLAUDE.md").display()),
            Level::User,
        ),
    ] {
        if src.is_file() {
            let mut it = Item::new(
                Source::ClaudeCode,
                level,
                Kind::Instructions,
                file_name(&src),
                &src,
                hash_file(&src),
            );
            it.plans.push(Plan::WriteText {
                to: dst,
                content: format!("{line}\n"),
                marker: line,
            });
            out.push(it);
        }
    }

    // Skills (dirs), agents and commands (Markdown).
    for (base, level, genie) in [
        (&user, Level::User, r.genie_home.clone()),
        (&proj, Level::Project, r.project.join(".genie")),
    ] {
        for d in dirs(&base.join("skills")) {
            let mut it = Item::new(
                Source::ClaudeCode,
                level,
                Kind::Skill,
                file_name(&d),
                &d,
                hash_dir(&d),
            );
            it.plans.push(Plan::CopyDir {
                to: genie.join("skills").join(file_name(&d)),
                from: d,
            });
            out.push(it);
        }
        for f in md_files(&base.join("agents")) {
            let mut it = Item::new(
                Source::ClaudeCode,
                level,
                Kind::Agent,
                stem(&f),
                &f,
                hash_file(&f),
            );
            it.plans.push(Plan::CopyFile {
                to: genie.join("agents").join(file_name(&f)),
                from: f,
            });
            out.push(it);
        }
        let cmd_root = base.join("commands");
        for f in md_files(&cmd_root) {
            let rel = f
                .strip_prefix(&cmd_root)
                .map(Path::to_path_buf)
                .unwrap_or_else(|_| PathBuf::from(file_name(&f)));
            let mut it = Item::new(
                Source::ClaudeCode,
                level,
                Kind::Command,
                rel.to_string_lossy(),
                &f,
                hash_file(&f),
            );
            if std::fs::read_to_string(&f).is_ok_and(|t| t.contains("!`")) {
                it.diagnostics.push(Diag::new(
                    DiagKind::NeedsTrust,
                    None,
                    "runs shell commands (`!cmd`); needs workspace trust and approval before it runs",
                ));
            }
            it.plans.push(Plan::CopyFile {
                to: genie.join("commands").join(rel),
                from: f,
            });
            out.push(it);
        }
    }
}

fn claude_settings(path: &Path, level: Level, target: &Path, v: &Value, out: &mut Vec<Item>) {
    let Some(obj) = v.as_object() else { return };
    let hash = hash_file(path);
    let mut it = Item::new(
        Source::ClaudeCode,
        level,
        Kind::Settings,
        file_name(path),
        path,
        hash.clone(),
    );
    let mut patch = Map::new();
    let mut secrets = vec![];
    for (k, val) in obj {
        match k.as_str() {
            "permissions" => {
                let mut p = Map::new();
                if let Some(pm) = val.as_object() {
                    for (pk, pv) in pm {
                        match pk.as_str() {
                            "allow" | "ask" | "deny" | "additionalDirectories" => {
                                p.insert(pk.clone(), pv.clone());
                            }
                            "defaultMode" => match pv.as_str().and_then(claude_mode) {
                                Some(m) => {
                                    p.insert("defaultMode".into(), json!(m));
                                }
                                None => it.diagnostics.push(Diag::new(
                                    DiagKind::Unsupported,
                                    Some("permissions.defaultMode"),
                                    &format!("mode {pv} has no Genie equivalent; skipped"),
                                )),
                            },
                            other => it.diagnostics.push(Diag::new(
                                DiagKind::Unsupported,
                                Some(&format!("permissions.{other}")),
                                "not supported; skipped",
                            )),
                        }
                    }
                }
                if !p.is_empty() {
                    patch.insert("permissions".into(), Value::Object(p));
                }
            }
            "env" => {
                let mut env = Map::new();
                for (ek, ev) in val.as_object().into_iter().flatten() {
                    let s = ev.as_str().unwrap_or_default();
                    if looks_secret(ek, s) {
                        let name = format!("env.{ek}");
                        it.diagnostics.push(Diag::new(
                            DiagKind::SecretDetected,
                            Some(&format!("env.{ek}")),
                            "secret detected; moved to the keychain, settings keep a reference",
                        ));
                        env.insert(ek.clone(), json!(secret_ref(&name)));
                        secrets.push(Plan::Secret {
                            name,
                            value: s.to_string(),
                        });
                    } else {
                        env.insert(ek.clone(), ev.clone());
                    }
                }
                if !env.is_empty() {
                    patch.insert("env".into(), Value::Object(env));
                }
            }
            "hooks" => {
                let mut hi = Item::new(
                    Source::ClaudeCode,
                    level,
                    Kind::Hooks,
                    file_name(path),
                    path,
                    hash.clone(),
                );
                let hooks = convert_hooks(val, &mut hi.diagnostics);
                if level == Level::Project {
                    hi.diagnostics.push(Diag::new(
                        DiagKind::NeedsTrust,
                        Some("hooks"),
                        "project hooks need workspace trust and definition-hash approval before they run",
                    ));
                }
                if !hooks.is_empty() {
                    hi.plans.push(Plan::MergeSettings {
                        target: target.to_path_buf(),
                        patch: json!({ "hooks": hooks }),
                    });
                    out.push(hi);
                }
            }
            "model" => it.diagnostics.push(Diag::new(
                DiagKind::SemanticDifference,
                Some("model"),
                "Claude model aliases differ from Genie's model ids; pick a model in Genie",
            )),
            "$schema" => {}
            other => it.diagnostics.push(Diag::new(
                DiagKind::Unsupported,
                Some(other),
                "not supported by the importer; skipped",
            )),
        }
    }
    if !patch.is_empty() {
        it.plans.push(Plan::MergeSettings {
            target: target.to_path_buf(),
            patch: Value::Object(patch),
        });
    }
    it.plans.extend(secrets);
    if !it.plans.is_empty() || !it.diagnostics.is_empty() {
        out.push(it);
    }
}

fn claude_mode(m: &str) -> Option<&'static str> {
    match m {
        "default" => Some("default"),
        "acceptEdits" => Some("accept-edits"),
        "plan" => Some("plan"),
        "bypassPermissions" => Some("bypass"),
        _ => None,
    }
}

fn hook_event(e: &str) -> Option<&'static str> {
    Some(match e {
        "PreToolUse" => "tool.before",
        "PostToolUse" => "tool.after",
        "UserPromptSubmit" => "prompt.submit",
        "Stop" => "turn.end",
        "SubagentStop" => "subagent.end",
        "SessionStart" => "session.start",
        "SessionEnd" => "session.end",
        "PreCompact" => "compact.before",
        "Notification" => "notification",
        "PermissionRequest" => "permission.request",
        _ => return None,
    })
}

/// Claude `{Event: [{matcher, hooks:[{type:"command", command, timeout}]}]}` → Genie hooks
/// that run each script through `genie hook-adapter claude` (D48).
fn convert_hooks(v: &Value, diags: &mut Vec<Diag>) -> Vec<Value> {
    let mut out = vec![];
    for (event, groups) in v.as_object().into_iter().flatten() {
        let Some(ge) = hook_event(event) else {
            diags.push(Diag::new(
                DiagKind::Unsupported,
                Some(&format!("hooks.{event}")),
                "no matching Genie hook event; skipped",
            ));
            continue;
        };
        for g in groups.as_array().into_iter().flatten() {
            let matcher = g.get("matcher").and_then(Value::as_str).unwrap_or("");
            for h in g
                .get("hooks")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
            {
                if h.get("type").and_then(Value::as_str) != Some("command") {
                    diags.push(Diag::new(
                        DiagKind::Unsupported,
                        Some(&format!("hooks.{event}")),
                        "only command hooks are imported",
                    ));
                    continue;
                }
                let Some(cmd) = h.get("command").and_then(Value::as_str) else {
                    continue;
                };
                let mut hook = json!({
                    "event": ge,
                    "run": ["genie", "hook-adapter", "claude", "--", cmd],
                });
                if !matcher.is_empty() && matcher != "*" {
                    hook["match"] = json!({ "tool": matcher });
                }
                if let Some(t) = h.get("timeout").and_then(Value::as_u64) {
                    hook["timeoutMs"] = json!(t * 1000);
                }
                out.push(hook);
            }
        }
    }
    out
}

/// One MCP item for a set of Claude-compatible servers; secret env/header values move to
/// the keychain.
fn mcp_item(
    source: Source,
    level: Level,
    src: &Path,
    scope: &str,
    servers: &Map<String, Value>,
    target: &Path,
    out: &mut Vec<Item>,
) {
    let body = serde_json::to_vec(servers).unwrap_or_default();
    let mut it = Item::new(source, level, Kind::Mcp, scope, src, hash_bytes(&body));
    let mut converted = Map::new();
    let mut secrets = vec![];
    for (name, def) in servers {
        let mut def = def.clone();
        for field in ["env", "headers"] {
            if let Some(m) = def.get_mut(field).and_then(Value::as_object_mut) {
                for (k, v) in m.iter_mut() {
                    let s = v.as_str().unwrap_or_default().to_string();
                    if looks_secret(k, &s) {
                        let sname = format!("mcp.{name}.{k}");
                        it.diagnostics.push(Diag::new(
                            DiagKind::SecretDetected,
                            Some(&format!("mcpServers.{name}.{field}.{k}")),
                            "secret detected; moved to the keychain, settings keep a reference",
                        ));
                        *v = json!(secret_ref(&sname));
                        secrets.push(Plan::Secret {
                            name: sname,
                            value: s,
                        });
                    }
                }
            }
        }
        converted.insert(name.clone(), def);
    }
    it.plans.push(Plan::MergeSettings {
        target: target.to_path_buf(),
        patch: json!({ "mcpServers": converted }),
    });
    it.plans.extend(secrets);
    out.push(it);
}

// ---------------------------------------------------------------- Claude Desktop

fn claude_desktop(r: &Roots, out: &mut Vec<Item>) {
    let cfg = r.claude_desktop.join("claude_desktop_config.json");
    if let Some(v) = read_json(&cfg)
        && let Some(servers) = v.get("mcpServers").and_then(Value::as_object)
    {
        mcp_item(
            Source::ClaudeDesktop,
            Level::User,
            &cfg,
            "user",
            servers,
            &r.user_settings(),
            out,
        );
    }
    // Installed extensions → connector suggestions (matched in the directory, D53).
    for d in dirs(&r.claude_desktop.join("Claude Extensions")) {
        let manifest = d.join("manifest.json");
        let Some(m) = read_json(&manifest) else {
            continue;
        };
        let name = m
            .get("display_name")
            .or_else(|| m.get("name"))
            .and_then(Value::as_str)
            .unwrap_or_else(|| {
                d.file_name()
                    .and_then(|n| n.to_str())
                    .unwrap_or("extension")
            })
            .to_string();
        let mut it = Item::new(
            Source::ClaudeDesktop,
            Level::User,
            Kind::ConnectorSuggestion,
            &name,
            &manifest,
            hash_file(&manifest),
        );
        it.plans.push(Plan::Suggest {
            text: format!(
                "Claude Desktop extension “{name}”: look for a matching connector in Settings → \
                 Connectors (extensions are not copied)"
            ),
        });
        out.push(it);
    }
}

// ---------------------------------------------------------------- OpenCode

fn opencode(r: &Roots, out: &mut Vec<Item>) {
    for (dir, level, genie, settings) in [
        (
            r.opencode.clone(),
            Level::User,
            r.genie_home.clone(),
            r.user_settings(),
        ),
        (
            r.project.clone(),
            Level::Project,
            r.project.join(".genie"),
            r.project_settings(),
        ),
    ] {
        let path = ["opencode.json", "opencode.jsonc"]
            .iter()
            .map(|n| dir.join(n))
            .find(|p| p.is_file());
        if let Some(path) = path
            && let Ok(text) = std::fs::read_to_string(&path)
            && let Some(v) = parse_jsonc(&text)
        {
            opencode_config(&path, &text, &v, level, &genie, &settings, r, out);
        }
        // Markdown agents and commands next to the config.
        let od = if level == Level::User {
            dir.clone()
        } else {
            dir.join(".opencode")
        };
        for (sub, kind) in [("agent", Kind::Agent), ("command", Kind::Command)] {
            for f in md_files(&od.join(sub)) {
                let mut it = Item::new(Source::Opencode, level, kind, stem(&f), &f, hash_file(&f));
                let dest = if kind == Kind::Agent {
                    "agents"
                } else {
                    "commands"
                };
                it.plans.push(Plan::CopyFile {
                    to: genie.join(dest).join(file_name(&f)),
                    from: f,
                });
                out.push(it);
            }
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn opencode_config(
    path: &Path,
    text: &str,
    v: &Value,
    level: Level,
    genie: &Path,
    settings: &Path,
    r: &Roots,
    out: &mut Vec<Item>,
) {
    let hash = hash_file(path);
    let mut it = Item::new(
        Source::Opencode,
        level,
        Kind::Settings,
        file_name(path),
        path,
        hash.clone(),
    );
    let mut patch = Map::new();
    let mut secrets = vec![];

    if let Some(m) = v.get("model").and_then(Value::as_str) {
        patch.insert("model".into(), json!(m));
    }
    // Permissions, order-preserving (last match wins in OpenCode).
    let stripped = super::strip_jsonc(text);
    #[derive(serde::Deserialize)]
    struct PermDoc {
        permission: Option<Ordered>,
    }
    if let Ok(PermDoc {
        permission: Some(p),
    }) = serde_json::from_str::<PermDoc>(&stripped)
    {
        let c = perms::convert(&p);
        let mut pm = Map::new();
        for (k, list) in [("allow", c.allow), ("ask", c.ask), ("deny", c.deny)] {
            if !list.is_empty() {
                pm.insert(k.into(), json!(list));
            }
        }
        if !pm.is_empty() {
            patch.insert("permissions".into(), Value::Object(pm));
        }
        it.diagnostics.extend(c.diagnostics);
    }
    // Providers: endpoints/models into settings, keys into the keychain.
    for (id, p) in v
        .get("provider")
        .and_then(Value::as_object)
        .into_iter()
        .flatten()
    {
        let mut prov = Map::new();
        let opts = p.get("options").cloned().unwrap_or_default();
        if let Some(u) = opts.get("baseURL") {
            prov.insert("baseURL".into(), u.clone());
        }
        if let Some(models) = p.get("models").and_then(Value::as_object) {
            prov.insert("models".into(), json!(models.keys().collect::<Vec<_>>()));
        }
        if let Some(k) = opts.get("apiKey").and_then(Value::as_str) {
            if let Some(var) = k.strip_prefix("{env:").and_then(|s| s.strip_suffix('}')) {
                prov.insert("apiKeyEnv".into(), json!(var));
            } else {
                let name = format!("provider.{id}");
                it.diagnostics.push(Diag::new(
                    DiagKind::SecretDetected,
                    Some(&format!("provider.{id}.options.apiKey")),
                    "API key moved to the keychain",
                ));
                secrets.push(Plan::Secret {
                    name,
                    value: k.to_string(),
                });
            }
        }
        for other in opts.as_object().into_iter().flatten().map(|(k, _)| k) {
            if !["baseURL", "apiKey"].contains(&other.as_str()) {
                it.diagnostics.push(Diag::new(
                    DiagKind::Unsupported,
                    Some(&format!("provider.{id}.options.{other}")),
                    "not imported",
                ));
            }
        }
        let mut providers = patch
            .remove("providers")
            .and_then(|v| v.as_object().cloned())
            .unwrap_or_default();
        providers.insert(id.clone(), Value::Object(prov));
        patch.insert("providers".into(), Value::Object(providers));
    }
    if !patch.is_empty() {
        it.plans.push(Plan::MergeSettings {
            target: settings.to_path_buf(),
            patch: Value::Object(patch),
        });
    }
    it.plans.extend(secrets);
    for k in v.as_object().into_iter().flatten().map(|(k, _)| k) {
        if ![
            "$schema",
            "model",
            "permission",
            "provider",
            "mcp",
            "agent",
            "command",
            "instructions",
        ]
        .contains(&k.as_str())
        {
            it.diagnostics
                .push(Diag::new(DiagKind::Unsupported, Some(k), "not imported"));
        }
    }
    if !it.plans.is_empty() || !it.diagnostics.is_empty() {
        out.push(it);
    }

    // MCP: OpenCode local/remote → Claude-compatible mcpServers.
    if let Some(mcp) = v.get("mcp").and_then(Value::as_object) {
        let mut servers = Map::new();
        let mut diags = vec![];
        for (name, def) in mcp {
            let mut s = match def.get("type").and_then(Value::as_str) {
                Some("remote") => {
                    json!({"type": "http", "url": def.get("url").cloned().unwrap_or_default()})
                }
                _ => {
                    let cmd: Vec<Value> = def
                        .get("command")
                        .and_then(Value::as_array)
                        .cloned()
                        .unwrap_or_default();
                    json!({"command": cmd.first().cloned().unwrap_or_default(), "args": cmd.get(1..).unwrap_or_default()})
                }
            };
            if let Some(env) = def.get("environment") {
                s["env"] = env.clone();
            }
            if let Some(h) = def.get("headers") {
                s["headers"] = h.clone();
            }
            if def.get("enabled") == Some(&Value::Bool(false)) {
                s["disabled"] = json!(true);
                diags.push(Diag::new(
                    DiagKind::Info,
                    Some(&format!("mcp.{name}.enabled")),
                    "disabled in OpenCode; imported disabled",
                ));
            }
            servers.insert(name.clone(), s);
        }
        let before = out.len();
        mcp_item(
            Source::Opencode,
            level,
            path,
            "opencode",
            &servers,
            settings,
            out,
        );
        if let Some(m) = out.get_mut(before) {
            m.diagnostics.extend(diags);
        }
    }

    // Agents and commands defined inline.
    for (name, a) in v
        .get("agent")
        .and_then(Value::as_object)
        .into_iter()
        .flatten()
    {
        let body = serde_json::to_vec(a).unwrap_or_default();
        let mut ai = Item::new(
            Source::Opencode,
            level,
            Kind::Agent,
            name,
            path,
            hash_bytes(&body),
        );
        let mut fm = String::from("---\n");
        fm.push_str(&format!("name: {name}\n"));
        for k in ["description", "model"] {
            if let Some(s) = a.get(k).and_then(Value::as_str) {
                fm.push_str(&format!("{k}: {}\n", yaml_str(s)));
            }
        }
        fm.push_str("---\n");
        fm.push_str(a.get("prompt").and_then(Value::as_str).unwrap_or_default());
        fm.push('\n');
        for k in a.as_object().into_iter().flatten().map(|(k, _)| k) {
            if !["description", "model", "prompt"].contains(&k.as_str()) {
                ai.diagnostics.push(Diag::new(
                    DiagKind::SemanticDifference,
                    Some(&format!("agent.{name}.{k}")),
                    "not converted; review the agent's tools and mode in Genie",
                ));
            }
        }
        let to = genie.join("agents").join(format!("{name}.md"));
        ai.plans.push(Plan::WriteText {
            marker: fm.clone(),
            to,
            content: fm,
        });
        out.push(ai);
    }
    for (name, c) in v
        .get("command")
        .and_then(Value::as_object)
        .into_iter()
        .flatten()
    {
        let body = serde_json::to_vec(c).unwrap_or_default();
        let mut ci = Item::new(
            Source::Opencode,
            level,
            Kind::Command,
            name,
            path,
            hash_bytes(&body),
        );
        let mut md = String::new();
        if let Some(d) = c.get("description").and_then(Value::as_str) {
            md.push_str(&format!("---\ndescription: {}\n---\n", yaml_str(d)));
        }
        let template = c
            .get("template")
            .and_then(Value::as_str)
            .unwrap_or_default();
        md.push_str(template);
        md.push('\n');
        if template.contains("!`") {
            ci.diagnostics.push(Diag::new(
                DiagKind::NeedsTrust,
                None,
                "runs shell commands (`!cmd`); needs workspace trust and approval",
            ));
        }
        ci.plans.push(Plan::WriteText {
            to: genie.join("commands").join(format!("{name}.md")),
            marker: md.clone(),
            content: md,
        });
        out.push(ci);
    }
    // Instructions → @-imports in GENIE.md.
    if let Some(list) = v.get("instructions").and_then(Value::as_array) {
        let lines: Vec<String> = list
            .iter()
            .filter_map(Value::as_str)
            .map(|p| format!("@{p}"))
            .collect();
        if !lines.is_empty() {
            let content = lines.join("\n") + "\n";
            let mut ii = Item::new(
                Source::Opencode,
                level,
                Kind::Instructions,
                "instructions",
                path,
                hash_bytes(content.as_bytes()),
            );
            let to = match level {
                Level::User => r.genie_home.join("GENIE.md"),
                Level::Project => r.project.join("GENIE.md"),
            };
            ii.plans.push(Plan::WriteText {
                to,
                marker: lines[0].clone(),
                content,
            });
            out.push(ii);
        }
    }
}

fn yaml_str(s: &str) -> String {
    if s.contains([':', '#', '"', '\n']) {
        serde_json::to_string(s).unwrap_or_default()
    } else {
        s.to_string()
    }
}

fn file_name(p: &Path) -> String {
    p.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default()
}

fn stem(p: &Path) -> String {
    p.file_stem()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default()
}

fn dirs(p: &Path) -> Vec<PathBuf> {
    let mut v: Vec<PathBuf> = std::fs::read_dir(p)
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.is_dir())
        .collect();
    v.sort();
    v
}

fn md_files(p: &Path) -> Vec<PathBuf> {
    let mut out = vec![];
    let mut stack = vec![p.to_path_buf()];
    while let Some(d) = stack.pop() {
        for e in std::fs::read_dir(&d).into_iter().flatten().flatten() {
            let path = e.path();
            if path.is_dir() {
                stack.push(path);
            } else if path.extension().is_some_and(|x| x == "md") {
                out.push(path);
            }
        }
    }
    out.sort();
    out
}
