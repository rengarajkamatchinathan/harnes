//! Data for the `skill` and `skill_read` tools (06 §4.1, §9).
//!
//! `genie-skills` may not depend on `genie-tools` (01 §2.1 rule 3), so the `Tool` impls live
//! on the engine side. They are thin: name/description/schema from here, `kind = ReadOnly`,
//! availability = "≥ 1 skill in [`SkillCatalog::index`]", authorisation = the `skill.use`
//! permission check, and `execute` = [`run_skill`] / [`run_skill_read`].

use serde::Deserialize;
use serde_json::{Value, json};

use crate::catalog::{Invoker, SessionKind, SkillCatalog};
use crate::error::SkillError;

pub const SKILL_TOOL: &str = "skill";
pub const SKILL_READ_TOOL: &str = "skill_read";

pub const SKILL_DESCRIPTION: &str = "Load a skill from <available_skills> by name. Returns the skill's instructions, its folder path and a list of its files. Load a matching skill before starting the task, then follow its instructions.";
pub const SKILL_READ_DESCRIPTION: &str = "Read a file inside a loaded skill's folder (for example references/guide.md). The path is relative to the skill folder.";

pub fn skill_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "name": {"type": "string", "description": "Skill name from <available_skills>."}
        },
        "required": ["name"],
        "additionalProperties": false
    })
}

pub fn skill_read_schema() -> Value {
    json!({
        "type": "object",
        "properties": {
            "name": {"type": "string", "description": "Skill name."},
            "path": {"type": "string", "description": "File path relative to the skill folder."}
        },
        "required": ["name", "path"],
        "additionalProperties": false
    })
}

#[derive(Debug, Clone, Deserialize)]
pub struct SkillInput {
    pub name: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct SkillReadInput {
    pub name: String,
    pub path: String,
}

/// `skill` tool body: the rendered `<skill>` block. The engine records `name` as an invoked
/// skill so it is re-injected after compaction.
pub fn run_skill(
    catalog: &SkillCatalog,
    input: &SkillInput,
    session: SessionKind,
) -> Result<String, SkillError> {
    Ok(catalog.load(&input.name, Invoker::Model, session)?.render())
}

/// `skill_read` tool body: the file text, with a note when it was cut at [`crate::MAX_READ`].
pub fn run_skill_read(
    catalog: &SkillCatalog,
    input: &SkillReadInput,
) -> Result<String, SkillError> {
    // `User`: a user-only skill the user attached must still have readable files. Policy
    // and the enabled switch apply either way.
    let f = catalog.read_file(&input.name, &input.path, Invoker::User)?;
    let mut s = f.content;
    if f.truncated {
        s.push_str(&format!(
            "\n[truncated: showing the first {} of {} bytes]",
            crate::MAX_READ,
            f.size
        ));
    }
    Ok(s)
}
