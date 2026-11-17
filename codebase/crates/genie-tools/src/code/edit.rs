//! `edit {path, old_string, new_string, replace_all?}` and `multi_edit {path, edits}`
//! (06 §4.2): string replacement in the LF view of a file, preserving CRLF and the BOM.
//!
//! Order inside the per-path lock: read the file → read-before-edit + staleness check →
//! match (3 tiers, [`matcher`](super::matcher)) → **ask** (diff preview) → write. A miss
//! never asks.

use std::borrow::Cow;

use async_trait::async_trait;
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::Value;

use super::matcher;
use super::{
    CodeConfig, Stamp, TextFile, alias_keys, authorize_edit, code_only, diff, display, mtime,
    write_atomic,
};
use crate::registry::{parse_input, schema_of};
use crate::tool::{Availability, Tool, ToolCtx, ToolEnv, ToolError, ToolKind, ToolOutput};

pub const NAME: &str = "edit";
pub const MULTI_NAME: &str = "multi_edit";

const KEY_ALIASES: &[(&str, &str)] = &[
    ("file_path", "path"),
    ("filePath", "path"),
    ("old_str", "old_string"),
    ("new_str", "new_string"),
    ("oldString", "old_string"),
    ("newString", "new_string"),
];

#[derive(Debug, Clone, Deserialize, JsonSchema)]
struct One {
    /// Exact text to replace (copy it from `read` output without the line numbers).
    old_string: String,
    /// Replacement text.
    new_string: String,
    /// Replace every occurrence instead of requiring a unique match.
    #[serde(default)]
    replace_all: bool,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct Input {
    /// File to edit: relative to the workspace root, or absolute.
    path: String,
    /// Exact text to replace (copy it from `read` output without the line numbers).
    /// Empty creates a new file with `new_string`.
    old_string: String,
    /// Replacement text.
    new_string: String,
    /// Replace every occurrence instead of requiring a unique match.
    #[serde(default)]
    replace_all: bool,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct MultiInput {
    /// File to edit: relative to the workspace root, or absolute.
    path: String,
    /// Edits applied in order; all succeed or none is written.
    edits: Vec<One>,
}

pub struct EditTool {
    cfg: CodeConfig,
    schema: Value,
}

impl EditTool {
    pub fn new(cfg: CodeConfig) -> Self {
        EditTool {
            cfg,
            schema: schema_of::<Input>(),
        }
    }
}

pub struct MultiEditTool {
    cfg: CodeConfig,
    schema: Value,
}

impl MultiEditTool {
    pub fn new(cfg: CodeConfig) -> Self {
        MultiEditTool {
            cfg,
            schema: schema_of::<MultiInput>(),
        }
    }
}

/// Apply `edits` to `text` (LF view). Returns the new text and per-edit notes.
fn apply_all(text: &str, edits: &[One], multi: bool) -> Result<(String, Vec<String>), ToolError> {
    let mut cur = text.to_string();
    let mut notes = Vec::new();
    for (i, e) in edits.iter().enumerate() {
        let what = if multi {
            format!("edits[{i}].old_string")
        } else {
            "old_string".to_string()
        };
        let old = e.old_string.replace("\r\n", "\n");
        let new = e.new_string.replace("\r\n", "\n");
        if old == new {
            return Err(ToolError::failed(format!(
                "{what} and new_string are identical; nothing to change"
            )));
        }
        if old.is_empty() {
            return Err(ToolError::failed(format!(
                "{what} is empty; to create a file use `write` (or `edit` on a file that \
                 doesn't exist yet)"
            )));
        }
        let (next, n, tier) = matcher::replace(&cur, &old, &new, e.replace_all)
            .map_err(|m| ToolError::failed(m.message(&what)))?;
        if let Some(note) = tier.note() {
            notes.push(format!("{what}: {note}"));
        } else if n > 1 {
            notes.push(format!("{what}: replaced {n} occurrences"));
        }
        cur = next;
    }
    Ok((cur, notes))
}

/// Shared body of `edit` and `multi_edit`.
async fn run(
    cfg: &CodeConfig,
    ctx: &ToolCtx,
    path_in: &str,
    edits: &[One],
    multi: bool,
) -> Result<ToolOutput, ToolError> {
    let sid = ctx.env.session_id.clone();
    let root = cfg.root(&sid)?;
    let path = super::resolve_in(&root, path_in);
    let shown = display(&root, &path);
    if path.is_dir() {
        return Err(ToolError::failed(format!("{shown} is a directory")));
    }
    let _guard = cfg.locks.lock(&path).await;
    let existing = std::fs::read(&path).ok();

    // Creating a new file: a single edit with an empty old_string.
    let creating = !multi && edits.first().is_some_and(|e| e.old_string.is_empty());
    let (file, old_text, new_text, notes) = match (&existing, creating) {
        (None, true) => {
            let text = edits[0].new_string.replace("\r\n", "\n");
            let f = TextFile {
                text: text.clone(),
                bom: false,
                crlf: edits[0].new_string.contains("\r\n"),
            };
            (f, String::new(), text, vec![])
        }
        (None, false) => {
            return Err(ToolError::failed(format!(
                "{shown} does not exist. Use `write` to create it (or `edit` with an empty \
                 old_string)."
            )));
        }
        (Some(bytes), _) => {
            cfg.reads.check(&sid, &path, bytes)?;
            let f = TextFile::decode(bytes)?;
            if creating {
                if !f.text.is_empty() {
                    return Err(ToolError::failed(format!(
                        "{shown} already exists and is not empty; old_string can't be empty"
                    )));
                }
                let text = edits[0].new_string.replace("\r\n", "\n");
                (f.new_like(text.clone()), String::new(), text, vec![])
            } else {
                let (text, notes) = apply_all(&f.text, edits, multi)?;
                (f.new_like(text.clone()), f.text.clone(), text, notes)
            }
        }
    };
    let unified = diff::unified(&shown, &old_text, &new_text);
    authorize_edit(ctx, &path, &shown, unified.clone()).await?;
    let bytes = file.encode();
    write_atomic(&path, &bytes).map_err(|e| ToolError::Failed {
        message: format!("can't write {shown}: {e}"),
        may_have_run: true,
    })?;
    cfg.reads
        .record(&sid, &path, Stamp::of(&bytes, mtime(&path)));
    let mut out = if existing.is_none() {
        format!("Created {shown}.")
    } else {
        let (a, r) = diff::stat(&unified);
        format!("Edited {shown} (+{a} −{r}).")
    };
    for n in notes {
        out.push_str(&format!("\nNote: {n}."));
    }
    out.push_str("\n\n");
    out.push_str(&diff::clip(&unified, 60));
    Ok(ToolOutput::Text(out.trim_end().to_string()))
}

#[async_trait]
impl Tool for EditTool {
    fn name(&self) -> &str {
        NAME
    }

    fn description(&self, _env: &ToolEnv) -> Cow<'_, str> {
        Cow::Borrowed(
            "Replace text in a file. `old_string` must match the file exactly (copy it from \
             `read` output without the line-number prefix) and be unique unless \
             `replace_all` is set; include surrounding lines to make it unique. Read the \
             file first. An empty `old_string` creates a new file.",
        )
    }

    fn input_schema(&self) -> &Value {
        &self.schema
    }

    fn kind(&self) -> ToolKind {
        ToolKind::Mutating
    }

    fn availability(&self, env: &ToolEnv) -> Availability {
        code_only(env)
    }

    async fn execute(&self, input: Value, ctx: ToolCtx) -> Result<ToolOutput, ToolError> {
        let input: Input = parse_input(alias_keys(input, KEY_ALIASES), &self.schema)?;
        let one = One {
            old_string: input.old_string,
            new_string: input.new_string,
            replace_all: input.replace_all,
        };
        run(&self.cfg, &ctx, &input.path, &[one], false).await
    }
}

#[async_trait]
impl Tool for MultiEditTool {
    fn name(&self) -> &str {
        MULTI_NAME
    }

    fn description(&self, _env: &ToolEnv) -> Cow<'_, str> {
        Cow::Borrowed(
            "Make several replacements in one file at once. Each edit works like `edit` and \
             applies to the result of the previous one; if any edit fails, nothing is \
             written.",
        )
    }

    fn input_schema(&self) -> &Value {
        &self.schema
    }

    fn kind(&self) -> ToolKind {
        ToolKind::Mutating
    }

    fn availability(&self, env: &ToolEnv) -> Availability {
        code_only(env)
    }

    async fn execute(&self, input: Value, ctx: ToolCtx) -> Result<ToolOutput, ToolError> {
        let input: MultiInput = parse_input(alias_keys(input, KEY_ALIASES), &self.schema)?;
        if input.edits.is_empty() {
            return Err(ToolError::invalid_input(vec!["`edits` is empty".into()]));
        }
        run(&self.cfg, &ctx, &input.path, &input.edits, true).await
    }
}
