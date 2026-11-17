//! Importer tests on fixture directories (never the real home dir).

#![allow(clippy::unwrap_used)]

use std::path::Path;

use serde_json::{Value, json};

use super::*;

struct Fx {
    _dir: tempfile::TempDir,
    roots: Roots,
}

fn write(p: &Path, s: &str) {
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(p, s).unwrap();
}

fn fixture() -> Fx {
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path().join("home");
    let project = dir.path().join("proj");
    let roots = Roots {
        genie_home: home.join(".genie"),
        claude_desktop: home.join("desktop"),
        opencode: home.join(".config/opencode"),
        home: home.clone(),
        project: project.clone(),
    };
    // Claude Code
    write(
        &home.join(".claude/settings.json"),
        r#"{
          "permissions": {"allow": ["Bash(git status:*)"], "deny": ["Read(./.env)"], "defaultMode": "acceptEdits"},
          "env": {"LOG_LEVEL": "debug", "ANTHROPIC_API_KEY": "sk-ant-abc123"},
          "hooks": {"PreToolUse": [{"matcher": "Bash", "hooks": [{"type": "command", "command": "~/check.sh", "timeout": 5}]}],
                    "Weird": []},
          "model": "opus",
          "statusLine": {}
        }"#,
    );
    write(
        &project.join(".claude/settings.local.json"),
        r#"{"permissions": {"additionalDirectories": ["../shared"]}}"#,
    );
    let mut claude_json = json!({
        "mcpServers": {"gh": {"command": "gh-mcp", "env": {"GITHUB_TOKEN": "ghp_xxxxxxxxxxxxxxxxxxxx"}}},
        "projects": {}
    });
    claude_json["projects"][project.to_string_lossy().as_ref()] =
        json!({"mcpServers": {"db": {"command": "db-mcp", "args": ["--ro"]}}});
    write(&home.join(".claude.json"), &claude_json.to_string());
    write(&project.join("CLAUDE.md"), "# Project rules\n");
    write(
        &project.join(".claude/commands/deploy.md"),
        "Deploy now: !`git push`\n",
    );
    write(
        &project.join(".claude/agents/reviewer.md"),
        "---\nname: reviewer\n---\nReview.\n",
    );
    write(&home.join(".claude/skills/pdf/SKILL.md"), "# PDF skill\n");
    // Claude Desktop
    write(
        &roots.claude_desktop.join("claude_desktop_config.json"),
        r#"{"mcpServers": {"fs": {"command": "npx", "args": ["fs-mcp"]}}}"#,
    );
    write(
        &roots
            .claude_desktop
            .join("Claude Extensions/ext.notion/manifest.json"),
        r#"{"name": "notion", "display_name": "Notion"}"#,
    );
    // OpenCode (jsonc with comments and trailing commas)
    write(
        &roots.opencode.join("opencode.jsonc"),
        r#"{
          // global config
          "model": "anthropic/claude-x",
          "provider": {"openai": {"options": {"apiKey": "sk-proj-123", "baseURL": "https://api.example"}, "models": {"gpt-x": {}}},
                       "groq": {"options": {"apiKey": "{env:GROQ_KEY}"}}},
          "permission": {"bash": {"rm *": "deny", "rm -i *": "allow", "*": "ask"}, "edit": "allow"},
          "mcp": {"local1": {"type": "local", "command": ["node", "srv.js"], "enabled": false},
                  "remote1": {"type": "remote", "url": "https://mcp.example"}},
          "agent": {"planner": {"description": "Plans", "prompt": "You plan.", "tools": {"bash": false}}},
          "command": {"test": {"template": "Run tests $ARGUMENTS", "description": "tests"}},
          "theme": "dark",
        }"#,
    );
    write(
        &project.join("opencode.json"),
        r#"{"instructions": ["docs/rules.md"]}"#,
    );
    Fx { _dir: dir, roots }
}

fn find(items: &[Item], source: Source, kind: Kind, level: Level) -> &Item {
    items
        .iter()
        .find(|i| i.source == source && i.kind == kind && i.level == level)
        .unwrap_or_else(|| {
            panic!(
                "no {source:?} {kind:?} {level:?} in {:#?}",
                items.iter().map(|i| &i.id).collect::<Vec<_>>()
            )
        })
}

fn read(p: &Path) -> Value {
    serde_json::from_str(&std::fs::read_to_string(p).unwrap()).unwrap()
}

#[test]
fn scan_finds_every_v1_source() {
    let fx = fixture();
    let items = preview(&fx.roots, Filter::default());
    let s = find(&items, Source::ClaudeCode, Kind::Settings, Level::User);
    assert_eq!(s.status, Status::Import);
    let keys: Vec<_> = s.diagnostics.iter().filter_map(|d| d.key.clone()).collect();
    assert!(keys.contains(&"env.ANTHROPIC_API_KEY".to_string()));
    assert!(keys.contains(&"model".to_string()));
    assert!(keys.contains(&"statusLine".to_string()));
    let h = find(&items, Source::ClaudeCode, Kind::Hooks, Level::User);
    assert!(
        h.diagnostics
            .iter()
            .any(|d| d.key.as_deref() == Some("hooks.Weird"))
    );
    find(&items, Source::ClaudeCode, Kind::Mcp, Level::User);
    find(&items, Source::ClaudeCode, Kind::Mcp, Level::Project);
    find(
        &items,
        Source::ClaudeCode,
        Kind::Instructions,
        Level::Project,
    );
    find(&items, Source::ClaudeCode, Kind::Skill, Level::User);
    find(&items, Source::ClaudeCode, Kind::Agent, Level::Project);
    let c = find(&items, Source::ClaudeCode, Kind::Command, Level::Project);
    assert!(c.diagnostics.iter().any(|d| d.kind == DiagKind::NeedsTrust));
    find(&items, Source::ClaudeDesktop, Kind::Mcp, Level::User);
    let ext = find(
        &items,
        Source::ClaudeDesktop,
        Kind::ConnectorSuggestion,
        Level::User,
    );
    assert_eq!(ext.status, Status::Skip);
    let oc = find(&items, Source::Opencode, Kind::Settings, Level::User);
    assert!(
        oc.diagnostics
            .iter()
            .any(|d| d.message.contains("Kept the deny")),
        "{:?}",
        oc.diagnostics
    );
    assert!(
        oc.diagnostics
            .iter()
            .any(|d| d.key.as_deref() == Some("theme"))
    );
    find(&items, Source::Opencode, Kind::Instructions, Level::Project);
    // Nothing written by a preview.
    assert!(!fx.roots.genie_home.exists());
    assert!(!fx.roots.project.join("GENIE.md").exists());
}

#[test]
fn filters_by_source_and_level() {
    let fx = fixture();
    let items = preview(
        &fx.roots,
        Filter {
            source: Some(Source::ClaudeCode),
            level: Some(Level::Project),
        },
    );
    assert!(!items.is_empty());
    assert!(
        items
            .iter()
            .all(|i| i.source == Source::ClaudeCode && i.level == Level::Project)
    );
}

#[test]
fn apply_writes_settings_secrets_and_report_then_is_idempotent() {
    let fx = fixture();
    let r = &fx.roots;
    let mut items = preview(r, Filter::default());
    let mut secrets = MemorySecrets::default();
    let opts = ApplyOptions {
        selected: None,
        confirm_append: false,
        date: "2026-11-17".into(),
    };
    let report = apply(r, &mut items, &opts, &mut secrets).unwrap();
    assert_eq!(
        report.path.as_deref(),
        Some(r.genie_home.join("imports/2026-11-17.json").as_path())
    );

    let user = read(&r.user_settings());
    assert_eq!(user["permissions"]["allow"][0], "Bash(git status:*)");
    assert_eq!(user["permissions"]["defaultMode"], "accept-edits");
    assert_eq!(user["env"]["LOG_LEVEL"], "debug");
    // Secrets never land in settings.
    assert_eq!(
        user["env"]["ANTHROPIC_API_KEY"],
        "keychain:env.ANTHROPIC_API_KEY"
    );
    assert_eq!(
        user["mcpServers"]["gh"]["env"]["GITHUB_TOKEN"],
        "keychain:mcp.gh.GITHUB_TOKEN"
    );
    let text = std::fs::read_to_string(r.user_settings()).unwrap();
    assert!(
        !text.contains("sk-ant-abc123") && !text.contains("ghp_") && !text.contains("sk-proj-123")
    );
    assert_eq!(secrets.0["env.ANTHROPIC_API_KEY"], "sk-ant-abc123");
    assert_eq!(secrets.0["provider.openai"], "sk-proj-123");
    // Hooks wrap the script in the adapter.
    assert_eq!(
        user["hooks"][0],
        json!({"event": "tool.before", "match": {"tool": "Bash"}, "run": ["genie", "hook-adapter", "claude", "--", "~/check.sh"], "timeoutMs": 5000})
    );
    // Claude Desktop + OpenCode MCP.
    assert_eq!(user["mcpServers"]["fs"]["command"], "npx");
    assert_eq!(
        user["mcpServers"]["local1"],
        json!({"command": "node", "args": ["srv.js"], "disabled": true})
    );
    assert_eq!(user["mcpServers"]["remote1"]["type"], "http");
    // OpenCode permissions: deny kept, exception dropped (SA-40).
    assert_eq!(
        user["permissions"]["deny"],
        json!(["Read(./.env)", "Bash(rm:*)"])
    );
    assert!(
        user["permissions"]["ask"]
            .as_array()
            .unwrap()
            .contains(&json!("Bash"))
    );
    assert_eq!(
        user["providers"]["openai"]["baseURL"],
        "https://api.example"
    );
    assert_eq!(user["providers"]["groq"]["apiKeyEnv"], "GROQ_KEY");
    assert_eq!(user["model"], "anthropic/claude-x");

    let local = read(&r.local_settings());
    assert_eq!(
        local["permissions"]["additionalDirectories"][0],
        "../shared"
    );
    assert_eq!(local["mcpServers"]["db"]["args"][0], "--ro");
    assert_eq!(
        std::fs::read_to_string(r.project.join("GENIE.md")).unwrap(),
        "@CLAUDE.md\n"
    );
    assert!(r.genie_home.join("skills/pdf/SKILL.md").is_file());
    assert!(r.project.join(".genie/agents/reviewer.md").is_file());
    assert!(r.project.join(".genie/commands/deploy.md").is_file());
    let planner = std::fs::read_to_string(r.genie_home.join("agents/planner.md")).unwrap();
    assert!(planner.contains("description: Plans") && planner.contains("You plan."));
    assert!(
        std::fs::read_to_string(r.genie_home.join("commands/test.md"))
            .unwrap()
            .contains("Run tests $ARGUMENTS")
    );

    // Re-run: everything already imported, except OpenCode's instructions: GENIE.md was
    // created by the Claude item first and appending was not confirmed (deferred).
    let again = preview(r, Filter::default());
    let not_skipped: Vec<_> = again
        .iter()
        .filter(|i| i.status != Status::Skip)
        .filter(|i| !(i.source == Source::Opencode && i.kind == Kind::Instructions))
        .map(|i| (&i.id, &i.status, &i.reason))
        .collect();
    assert!(not_skipped.is_empty(), "{not_skipped:#?}");
}

#[test]
fn genie_md_appends_only_after_confirmation() {
    let fx = fixture();
    let r = &fx.roots;
    write(&r.project.join("GENIE.md"), "# Mine\n");
    let filter = Filter {
        source: Some(Source::ClaudeCode),
        level: Some(Level::Project),
    };
    let mut items = preview(r, filter);
    let md = items.iter().find(|i| i.kind == Kind::Instructions).unwrap();
    assert_eq!(md.status, Status::Conflict);
    let ids = vec![md.id.clone()];
    let mut opts = ApplyOptions {
        selected: Some(ids),
        confirm_append: false,
        date: "2026-11-17".into(),
    };
    apply(r, &mut items, &opts, &mut NoKeychain).unwrap();
    assert_eq!(
        std::fs::read_to_string(r.project.join("GENIE.md")).unwrap(),
        "# Mine\n"
    );
    opts.confirm_append = true;
    let mut items = preview(r, filter);
    apply(r, &mut items, &opts, &mut NoKeychain).unwrap();
    assert_eq!(
        std::fs::read_to_string(r.project.join("GENIE.md")).unwrap(),
        "# Mine\n\n@CLAUDE.md\n"
    );
    // The second report got its own name.
    assert!(r.genie_home.join("imports/2026-11-17-2.json").is_file());
}

#[test]
fn without_a_keychain_secrets_are_reported_not_written() {
    let fx = fixture();
    let r = &fx.roots;
    let mut items = preview(
        r,
        Filter {
            source: Some(Source::ClaudeCode),
            level: Some(Level::User),
        },
    );
    let report = apply(
        r,
        &mut items,
        &ApplyOptions {
            selected: None,
            confirm_append: false,
            date: "d".into(),
        },
        &mut NoKeychain,
    )
    .unwrap();
    let s = report
        .items
        .iter()
        .find(|i| i.kind == Kind::Settings)
        .unwrap();
    assert!(
        s.diagnostics
            .iter()
            .any(|d| d.message.starts_with("secret not imported"))
    );
    let report_text = std::fs::read_to_string(report.path.unwrap()).unwrap();
    assert!(!report_text.contains("sk-ant-abc123"));
}

#[test]
fn conflicting_scalar_keeps_existing_value() {
    let fx = fixture();
    let r = &fx.roots;
    write(&r.user_settings(), r#"{"env": {"LOG_LEVEL": "warn"}}"#);
    let items = preview(
        r,
        Filter {
            source: Some(Source::ClaudeCode),
            level: Some(Level::User),
        },
    );
    let s = items.iter().find(|i| i.kind == Kind::Settings).unwrap();
    assert_eq!(s.status, Status::Conflict);
    assert!(s.reason.as_deref().unwrap().contains("env.LOG_LEVEL"));
    let mut items = items;
    apply(
        r,
        &mut items,
        &ApplyOptions {
            selected: None,
            confirm_append: false,
            date: "d".into(),
        },
        &mut MemorySecrets::default(),
    )
    .unwrap();
    assert_eq!(read(&r.user_settings())["env"]["LOG_LEVEL"], "warn");
}

#[test]
fn jsonc_and_secret_heuristics() {
    let v = parse_jsonc("{\n // c\n \"a\": \"http://x//y\", /* b */ \"b\": [1,2,],\n}").unwrap();
    assert_eq!(v, json!({"a": "http://x//y", "b": [1, 2]}));
    assert!(looks_secret("API_KEY", "abc"));
    assert!(looks_secret("X", "sk-123"));
    assert!(!looks_secret("API_KEY", "${API_KEY}"));
    assert!(!looks_secret("LOG_LEVEL", "debug"));
}
