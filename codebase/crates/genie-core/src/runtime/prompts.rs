//! Prompt content (06 §2.6): Genie-written, versioned fragments in
//! `crates/genie-core/prompts/`, rendered by a tiny template engine. Every rendered prompt
//! has a golden snapshot (`tests/golden/prompts/`), so wording changes show up in review.
//!
//! Template syntax: `{name}` substitutes a variable; `{if flag}…{/if}` keeps its body only
//! when `flag` is set (no nesting). Identical inputs render identical bytes (cache).

use genie_protocol::common::{Mode, SessionKind};

/// Bumped when any fragment changes meaning; recorded with the frozen baseline.
pub const PROMPT_VERSION: &str = "v0";

macro_rules! prompt {
    ($path:literal) => {
        include_str!(concat!(env!("CARGO_MANIFEST_DIR"), "/prompts/", $path))
    };
}

pub mod fragments {
    pub const IDENTITY: &str = prompt!("base/identity.md");
    pub const HARNESS: &str = prompt!("base/harness.md");
    pub const CHAT: &str = prompt!("base/chat.md");
    pub const CODE: &str = prompt!("base/code.md");
    pub const STYLE: &str = prompt!("base/style.md");

    pub const TOOLS_PARALLEL: &str = prompt!("tools/parallel.md");
    pub const TOOLS_SHELL: &str = prompt!("tools/shell.md");
    pub const TOOLS_EDIT: &str = prompt!("tools/edit.md");
    pub const TOOLS_SEARCH: &str = prompt!("tools/search.md");
    pub const TOOLS_TASK: &str = prompt!("tools/task.md");
    pub const TOOLS_OUTPUT: &str = prompt!("tools/output.md");

    pub const FEATURE_ARTIFACTS: &str = prompt!("features/artifacts.md");
    pub const FEATURE_MEMORY: &str = prompt!("features/memory.md");
    pub const FEATURE_CHAT_SEARCH: &str = prompt!("features/chat_search.md");
    pub const FEATURE_WEB: &str = prompt!("features/web.md");
    pub const FEATURE_CODE_EXEC: &str = prompt!("features/code_exec.md");
    pub const FEATURE_BROWSER: &str = prompt!("features/browser.md");
    pub const FEATURE_MCP: &str = prompt!("features/mcp.md");
    pub const FEATURE_SKILLS: &str = prompt!("features/skills.md");
    pub const FEATURE_PROJECT_SEARCH: &str = prompt!("features/project_search.md");

    pub const MAX_STEPS: &str = prompt!("hidden/max_steps.md");
    pub const CONTINUE_SMALLER: &str = prompt!("hidden/continue_smaller.md");
    pub const TITLE: &str = prompt!("hidden/title.md");
    pub const COMPACTION: &str = prompt!("hidden/compaction.md");
    pub const SUBAGENT_REPORT: &str = prompt!("hidden/subagent_report.md");
    pub const BACKGROUND_DONE: &str = prompt!("hidden/background_done.md");
    pub const INTERRUPTED_RESUME: &str = prompt!("hidden/interrupted_resume.md");
    pub const HOOK_CONTINUE: &str = prompt!("hidden/hook_continue.md");

    pub const TUNING_GPT: &str = prompt!("tuning/gpt.md");
    pub const TUNING_GEMINI: &str = prompt!("tuning/gemini.md");
    pub const TUNING_SMALL: &str = prompt!("tuning/small.md");
}

use fragments as f;

/// Render a template: `{if flag}…{/if}` blocks, then `{name}` variables.
pub fn render(template: &str, vars: &[(&str, &str)], flags: &[&str]) -> String {
    let mut s = String::with_capacity(template.len());
    let mut rest = template;
    while let Some(start) = rest.find("{if ") {
        s.push_str(&rest[..start]);
        let after = &rest[start + 4..];
        let (Some(close), Some(end)) = (after.find('}'), after.find("{/if}")) else {
            s.push_str(&rest[start..]);
            rest = "";
            break;
        };
        let flag = after[..close].trim();
        if flags.contains(&flag) && close < end {
            s.push_str(&after[close + 1..end]);
        }
        rest = &after[end + 5..];
    }
    s.push_str(rest);
    for (k, v) in vars {
        s = s.replace(&format!("{{{k}}}"), v);
    }
    s.trim_end().to_string()
}

/// Per-model tuning appendix (06 §2.6.5, SA-43), keyed by catalog family. At most a short
/// appendix; never replaces the base prompt. Claude family: none.
pub fn tuning_for(family: Option<&str>, provider: &str) -> Option<&'static str> {
    let fam = family.unwrap_or("").to_ascii_lowercase();
    if provider == "ollama" {
        return Some(f::TUNING_SMALL);
    }
    if fam.starts_with("gpt") || fam.starts_with("o3") || fam.starts_with("o4") {
        Some(f::TUNING_GPT)
    } else if fam.starts_with("gemini") {
        Some(f::TUNING_GEMINI)
    } else {
        None
    }
}

/// Features that switch guidance fragments on (06 §2.6.3).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PromptFeatures {
    /// Artifacts enabled, with the bundled library list.
    pub artifacts: Option<String>,
    pub memory: bool,
    pub chat_search: bool,
    pub web: bool,
    pub code_exec: bool,
    pub browser: bool,
    pub mcp: bool,
    pub skills: bool,
    pub project_search: bool,
}

/// Inputs to the base prompt (system block 1).
#[derive(Debug, Clone, Default)]
pub struct BasePromptInput {
    pub kind: SessionKind,
    pub model_display_name: String,
    pub provider_display_name: String,
    pub workspace_display: String,
    pub mode: Mode,
    /// Names of the tools offered this step.
    pub tools: Vec<String>,
    /// `(shell name, os)` when `shell` is offered.
    pub shell: Option<(String, String)>,
    pub features: PromptFeatures,
    pub family: Option<String>,
    pub provider: String,
    /// An agent body that replaces the base prompt (D46). The tuning appendix still applies.
    pub agent_body: Option<String>,
}

fn mode_str(m: Mode) -> &'static str {
    match m {
        Mode::Default => "default",
        Mode::AcceptEdits => "accept edits",
        Mode::Plan => "plan",
        Mode::Bypass => "bypass permissions",
        Mode::Unknown => "default",
    }
}

/// System block 1: base prompt + guidance for what is available + tuning (06 §2.6.2).
pub fn base_prompt(i: &BasePromptInput) -> String {
    let has = |n: &str| i.tools.iter().any(|t| t == n);
    let mut parts: Vec<String> = vec![];
    if let Some(body) = &i.agent_body {
        parts.push(body.trim_end().to_string());
    } else {
        let vars = [
            ("model_display_name", i.model_display_name.as_str()),
            ("provider_display_name", i.provider_display_name.as_str()),
            ("workspace_display", i.workspace_display.as_str()),
            ("permission_mode", mode_str(i.mode)),
        ];
        let flags: &[&str] = if i.mode == Mode::Plan {
            &["plan_mode"]
        } else {
            &[]
        };
        parts.push(render(f::IDENTITY, &vars, flags));
        parts.push(render(f::HARNESS, &vars, flags));
        parts.push(render(
            match i.kind {
                SessionKind::Code => f::CODE,
                _ => f::CHAT,
            },
            &vars,
            flags,
        ));

        let mut guidance: Vec<String> = vec![];
        if i.tools.len() >= 2 {
            guidance.push(render(f::TOOLS_PARALLEL, &[], &[]));
        }
        if let Some((shell, os)) = &i.shell
            && has("shell")
        {
            guidance.push(render(
                f::TOOLS_SHELL,
                &[("shell_name", shell), ("os", os)],
                &[],
            ));
        }
        if has("edit") || has("write") || has("patch") {
            guidance.push(render(f::TOOLS_EDIT, &[], &[]));
        }
        if has("grep") || has("glob") {
            guidance.push(render(f::TOOLS_SEARCH, &[], &[]));
        }
        if has("task") {
            guidance.push(render(f::TOOLS_TASK, &[], &[]));
        }
        if has("tool_output_read") {
            guidance.push(render(f::TOOLS_OUTPUT, &[], &[]));
        }
        let ft = &i.features;
        if let Some(libs) = &ft.artifacts {
            guidance.push(render(
                f::FEATURE_ARTIFACTS,
                &[("artifact_libraries", libs)],
                &[],
            ));
        }
        for (on, frag) in [
            (ft.memory, f::FEATURE_MEMORY),
            (ft.chat_search, f::FEATURE_CHAT_SEARCH),
            (ft.web, f::FEATURE_WEB),
            (ft.code_exec, f::FEATURE_CODE_EXEC),
            (ft.browser, f::FEATURE_BROWSER),
            (ft.mcp, f::FEATURE_MCP),
            (ft.skills, f::FEATURE_SKILLS),
            (ft.project_search, f::FEATURE_PROJECT_SEARCH),
        ] {
            if on {
                guidance.push(render(frag, &[], &[]));
            }
        }
        if !guidance.is_empty() {
            parts.push(guidance.join("\n"));
        }
        parts.push(render(f::STYLE, &[], &[]));
    }
    if let Some(t) = tuning_for(i.family.as_deref(), &i.provider) {
        parts.push(render(t, &[], &[]));
    }
    parts.join("\n\n")
}

/// The compaction instruction (06 §2.6.7), with an optional `/compact [focus]`.
pub fn compaction_prompt(focus: Option<&str>) -> String {
    compaction_prompt_with(focus, None)
}

/// [`compaction_prompt`] plus the user's newest message when it is being summarised
/// mid-turn: the summary must carry it as the goal, or the turn loses the request.
pub fn compaction_prompt_with(focus: Option<&str>, request: Option<&str>) -> String {
    let focus = focus.map(str::trim).filter(|s| !s.is_empty());
    let request = request.map(str::trim).filter(|s| !s.is_empty());
    let mut vars = vec![];
    let mut flags = vec![];
    if let Some(fx) = focus {
        vars.push(("focus", fx));
        flags.push("focus");
    }
    if let Some(r) = request {
        vars.push(("request", r));
        flags.push("request");
    }
    render(f::COMPACTION, &vars, &flags)
}

/// Template headings the summary must keep (06 §3 "template check").
pub const COMPACTION_HEADINGS: &[&str] = &[
    "## Goal",
    "## User preferences & constraints",
    "## Decisions",
    "## Current state",
    "## Open threads",
    "## Key references",
];

pub fn summary_follows_template(s: &str) -> bool {
    COMPACTION_HEADINGS.iter().all(|h| s.contains(h))
}

/// `<system-update>` wrapper (06 §2.6.6).
pub fn system_update(body: &str) -> String {
    format!("<system-update>{}</system-update>", body.trim())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn render_vars_and_flags() {
        assert_eq!(
            render(
                "a {x}{if on} b{/if}{if off} c{/if}.",
                &[("x", "1")],
                &["on"]
            ),
            "a 1 b."
        );
        assert_eq!(render("{if z}never{/if}", &[], &[]), "");
        assert_eq!(render("{unknown} stays", &[], &[]), "{unknown} stays");
    }

    #[test]
    fn tuning_never_for_claude() {
        assert!(tuning_for(Some("claude-opus"), "anthropic").is_none());
        assert!(tuning_for(Some("gpt-5"), "openai").is_some());
        assert!(tuning_for(None, "ollama").is_some());
    }

    #[test]
    fn template_check() {
        let ok = COMPACTION_HEADINGS.join("\nx\n");
        assert!(summary_follows_template(&ok));
        assert!(!summary_follows_template("## Goal only"));
    }
}
