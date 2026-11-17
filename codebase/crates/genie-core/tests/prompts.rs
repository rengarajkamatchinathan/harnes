//! Golden snapshots of every rendered prompt (06 §2.6): a wording change shows up in
//! review. Regenerate with `GENIE_UPDATE_GOLDEN=1 cargo test -p genie-core --test prompts`.

#![allow(clippy::unwrap_used, clippy::expect_used)] // test code

use std::path::PathBuf;

use genie_core::runtime::context::{EnvInfo, FrozenBaseline, InstructionSource};
use genie_core::runtime::prompts::{self, BasePromptInput, PromptFeatures, fragments, render};
use genie_protocol::common::{Mode, SessionKind};

fn check(name: &str, actual: &str) {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/golden/prompts")
        .join(format!("{name}.txt"));
    let actual = format!("{}\n", actual.trim_end());
    if std::env::var_os("GENIE_UPDATE_GOLDEN").is_some() {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, &actual).unwrap();
        return;
    }
    let expected = std::fs::read_to_string(&path).unwrap_or_else(|_| {
        panic!(
            "missing golden {}; run with GENIE_UPDATE_GOLDEN=1",
            path.display()
        )
    });
    assert_eq!(
        actual, expected,
        "golden {name} differs; review and regenerate"
    );
}

fn base(kind: SessionKind) -> BasePromptInput {
    BasePromptInput {
        kind,
        model_display_name: "Claude Opus 5.5".into(),
        provider_display_name: "Anthropic".into(),
        workspace_display: "~/src/app".into(),
        mode: Mode::Default,
        tools: vec![],
        shell: None,
        features: PromptFeatures::default(),
        family: Some("claude-opus".into()),
        provider: "anthropic".into(),
        agent_body: None,
    }
}

#[test]
fn base_prompt_chat_minimal() {
    check(
        "chat_minimal",
        &prompts::base_prompt(&base(SessionKind::Chat)),
    );
}

#[test]
fn base_prompt_chat_with_tools_and_features() {
    let mut i = base(SessionKind::Chat);
    i.tools = vec![
        "memory_save".into(),
        "memory_search".into(),
        "tool_output_read".into(),
    ];
    i.features = PromptFeatures {
        artifacts: Some("react, recharts, d3".into()),
        memory: true,
        chat_search: true,
        web: true,
        code_exec: true,
        browser: true,
        mcp: true,
        skills: true,
        project_search: true,
    };
    check("chat_all_features", &prompts::base_prompt(&i));
}

#[test]
fn base_prompt_code_plan_mode_gpt_tuning() {
    let mut i = base(SessionKind::Code);
    i.mode = Mode::Plan;
    i.tools = vec![
        "shell".into(),
        "edit".into(),
        "grep".into(),
        "glob".into(),
        "task".into(),
    ];
    i.shell = Some(("zsh".into(), "macos".into()));
    i.family = Some("gpt-5.1".into());
    i.provider = "openai".into();
    i.model_display_name = "GPT-5.1".into();
    i.provider_display_name = "OpenAI".into();
    check("code_plan_gpt", &prompts::base_prompt(&i));
}

#[test]
fn agent_body_replaces_base_but_keeps_tuning() {
    let mut i = base(SessionKind::Chat);
    i.agent_body = Some("You review pull requests.".into());
    i.family = Some("gemini-3-pro".into());
    i.provider = "google".into();
    check("agent_gemini", &prompts::base_prompt(&i));
}

#[test]
fn hidden_prompts() {
    let all = [
        ("max_steps", render(fragments::MAX_STEPS, &[], &[])),
        (
            "continue_smaller",
            render(fragments::CONTINUE_SMALLER, &[], &[]),
        ),
        ("title", render(fragments::TITLE, &[], &[])),
        ("compaction", prompts::compaction_prompt(None)),
        (
            "compaction_focus",
            prompts::compaction_prompt(Some("the API design")),
        ),
        (
            "subagent_report",
            render(
                fragments::SUBAGENT_REPORT,
                &[
                    ("name", "explore"),
                    ("session", "ses_1"),
                    ("status", "done"),
                    ("text", "Found it."),
                ],
                &[],
            ),
        ),
        (
            "background_done",
            render(
                fragments::BACKGROUND_DONE,
                &[("id", "bg_1"), ("code", "0"), ("ref", "blob_x")],
                &[],
            ),
        ),
        (
            "interrupted_resume",
            render(fragments::INTERRUPTED_RESUME, &[], &[]),
        ),
        (
            "hook_continue",
            render(
                fragments::HOOK_CONTINUE,
                &[("message", "tests failed")],
                &[],
            ),
        ),
    ];
    let text: String = all.iter().map(|(k, v)| format!("=== {k}\n{v}\n")).collect();
    check("hidden", &text);
}

#[test]
fn baseline_formats() {
    let b = FrozenBaseline::freeze(
        vec![
            InstructionSource::date("2027-03-14"),
            InstructionSource::env(&EnvInfo {
                cwd: Some("/home/ana/src/app".into()),
                os: "linux".into(),
                shell: Some("bash".into()),
                git: Some(true),
                branch: Some("main".into()),
                app_version: "1.0.0".into(),
            }),
            InstructionSource::instruction_file("../AGENTS.md", "Use tabs."),
            InstructionSource::instruction_file("GENIE.md", "Run `make test`."),
            InstructionSource::memory("project", &["Deploys on Fridays are frozen".into()]),
            InstructionSource::project_knowledge(&[("spec.md".into(), "# Spec\nBody".into())]),
            InstructionSource::project_instructions("Billing", "Cite the ledger."),
            InstructionSource::memory("global", &["Prefers metric units".into()]),
            InstructionSource::mcp_server("github", "Use search before listing."),
            InstructionSource::skills_index(&[("pdf".into(), "Fill PDF forms.".into())]),
            InstructionSource::user_instructions("Answer briefly.", Some("Ana")),
            InstructionSource::org("Acme", "Never share customer data."),
        ],
        1,
    );
    check("baseline", &b.render());
}
