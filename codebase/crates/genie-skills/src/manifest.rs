//! `SKILL.md` frontmatter schema and validation (06 §9 "Format", D37).
//!
//! Claude-compatible fields: `name`, `description`, `disable-model-invocation`,
//! `allowed-tools`, `metadata`. Other keys (`license`, `compatibility`, `version` …) are
//! kept in `extra` and ignored.

use serde_json::{Map, Value};

use crate::frontmatter;

/// Largest `SKILL.md` the scanner reads (bytes).
pub const MAX_SKILL_MD: u64 = 256 * 1024;
pub const MAX_NAME: usize = 64;
pub const MAX_DESCRIPTION: usize = 1024;

#[derive(Debug, Clone, PartialEq)]
pub struct SkillManifest {
    pub name: String,
    pub description: String,
    /// Only the user can invoke it (`/name`, "+"); it is left out of `<available_skills>`.
    pub disable_model_invocation: bool,
    /// Tool rules the skill expects (metadata only: never widens permissions).
    pub allowed_tools: Vec<String>,
    pub metadata: Map<String, Value>,
    pub extra: Map<String, Value>,
}

/// Parses `SKILL.md` text into its manifest and body. All problems are reported together.
pub fn parse_skill_md(text: &str) -> Result<(SkillManifest, &str), Vec<String>> {
    let (fm, body) = match frontmatter::split(text) {
        Ok((Some(fm), body)) => (fm, body),
        Ok((None, _)) => return Err(vec!["SKILL.md has no `---` frontmatter".into()]),
        Err(e) => return Err(vec![e.to_string()]),
    };
    let mut map = frontmatter::parse(fm).map_err(|e| vec![e.to_string()])?;
    let mut problems = vec![];

    let name = match map.remove("name") {
        Some(Value::String(s)) => s.trim().to_string(),
        Some(_) => {
            problems.push("`name` must be a string".into());
            String::new()
        }
        None => {
            problems.push("`name` is required".into());
            String::new()
        }
    };
    if !name.is_empty() {
        problems.extend(validate_name(&name));
    }

    let description = match map.remove("description") {
        Some(Value::String(s)) => s.trim().to_string(),
        Some(_) => {
            problems.push("`description` must be a string".into());
            String::new()
        }
        None => {
            problems.push("`description` is required".into());
            String::new()
        }
    };
    if description.is_empty() && problems.iter().all(|p| !p.contains("description")) {
        problems.push("`description` must not be empty".into());
    }
    if description.chars().count() > MAX_DESCRIPTION {
        problems.push(format!(
            "`description` is longer than {MAX_DESCRIPTION} characters"
        ));
    }
    // The description is rendered into `<available_skills>`; tags could break out of it.
    if description.contains('<') || description.contains('>') {
        problems.push("`description` must not contain `<` or `>`".into());
    }

    let disable_model_invocation = match map.remove("disable-model-invocation") {
        None | Some(Value::Null) => false,
        Some(Value::Bool(b)) => b,
        Some(_) => {
            problems.push("`disable-model-invocation` must be true or false".into());
            false
        }
    };
    let allowed_tools = match frontmatter::string_list(map.get("allowed-tools")) {
        Ok(v) => v,
        Err(e) => {
            problems.push(format!("`allowed-tools`: {e}"));
            vec![]
        }
    };
    map.remove("allowed-tools");
    let metadata = match map.remove("metadata") {
        None | Some(Value::Null) => Map::new(),
        Some(Value::Object(m)) => m,
        Some(_) => {
            problems.push("`metadata` must be a map".into());
            Map::new()
        }
    };

    if problems.is_empty() {
        Ok((
            SkillManifest {
                name,
                description,
                disable_model_invocation,
                allowed_tools,
                metadata,
                extra: map,
            },
            body,
        ))
    } else {
        Err(problems)
    }
}

/// Skill names (and ids): 1–64 of `a-z 0-9 -`, no leading/trailing or doubled hyphen.
pub fn validate_name(name: &str) -> Vec<String> {
    let mut p = vec![];
    if name.is_empty() || name.len() > MAX_NAME {
        p.push(format!("`name` must be 1–{MAX_NAME} characters"));
    }
    if !name
        .chars()
        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
    {
        p.push("`name` may only contain lowercase letters, digits and hyphens".into());
    }
    if name.starts_with('-') || name.ends_with('-') || name.contains("--") {
        p.push("`name` must not start or end with a hyphen or contain `--`".into());
    }
    p
}

pub fn is_valid_id(id: &str) -> bool {
    validate_name(id).is_empty()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn valid_skill() {
        let (m, body) = parse_skill_md(
            "---\nname: pdf\ndescription: Work with PDFs.\nallowed-tools: Read Grep\nlicense: MIT\nmetadata:\n  version: \"1.0\"\n---\n# PDF\nSteps",
        )
        .unwrap();
        assert_eq!(m.name, "pdf");
        assert_eq!(m.allowed_tools, vec!["Read", "Grep"]);
        assert!(!m.disable_model_invocation);
        assert_eq!(m.metadata["version"], "1.0");
        assert_eq!(m.extra["license"], "MIT");
        assert_eq!(body, "# PDF\nSteps");
    }

    #[test]
    fn reports_every_problem() {
        let e = parse_skill_md(
            "---\nname: Bad_Name\ndescription: \"<b>x</b>\"\ndisable-model-invocation: yes\nmetadata: 3\n---\n",
        )
        .unwrap_err();
        assert_eq!(e.len(), 4, "{e:?}");
        assert!(parse_skill_md("no frontmatter").is_err());
        assert!(parse_skill_md("---\nname: ok\n---\n").unwrap_err()[0].contains("description"));
        assert!(parse_skill_md("---\nname: ok\ndescription: \"\"\n---\n").is_err());
    }

    #[test]
    fn names() {
        assert!(is_valid_id("a-b-1"));
        for bad in [
            "",
            "-a",
            "a-",
            "a--b",
            "A",
            "a b",
            "a/b",
            "..",
            &"a".repeat(65),
        ] {
            assert!(!is_valid_id(bad), "{bad}");
        }
    }
}
