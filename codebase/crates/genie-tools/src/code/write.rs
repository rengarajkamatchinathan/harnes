//! `write {path, content}` (06 §4.2): create a file, or overwrite one **only after it was
//! read** in this session and hasn't changed since.

use std::borrow::Cow;

use async_trait::async_trait;
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::Value;

use super::{
    CodeConfig, Stamp, TextFile, alias_keys, authorize_edit, code_only, diff, display, mtime,
    write_atomic,
};
use crate::registry::{parse_input, schema_of};
use crate::tool::{Availability, Tool, ToolCtx, ToolEnv, ToolError, ToolKind, ToolOutput};

pub const NAME: &str = "write";

#[derive(Debug, Deserialize, JsonSchema)]
struct Input {
    /// File to write: relative to the workspace root, or absolute.
    path: String,
    /// The complete new content of the file.
    content: String,
}

pub struct WriteTool {
    cfg: CodeConfig,
    schema: Value,
}

impl WriteTool {
    pub fn new(cfg: CodeConfig) -> Self {
        WriteTool {
            cfg,
            schema: schema_of::<Input>(),
        }
    }
}

#[async_trait]
impl Tool for WriteTool {
    fn name(&self) -> &str {
        NAME
    }

    fn description(&self, _env: &ToolEnv) -> Cow<'_, str> {
        Cow::Borrowed(
            "Write a whole file. Creates the file (and missing folders) or replaces an \
             existing one; an existing file must have been read first in this session. \
             Prefer `edit` for changes to existing files.",
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
        let input = alias_keys(input, &[("file_path", "path"), ("filePath", "path")]);
        let input: Input = parse_input(input, &self.schema)?;
        let sid = ctx.env.session_id.clone();
        let root = self.cfg.root(&sid)?;
        let path = super::resolve_in(&root, &input.path);
        let shown = display(&root, &path);
        if path.is_dir() {
            return Err(ToolError::failed(format!("{shown} is a directory")));
        }
        let _guard = self.cfg.locks.lock(&path).await;
        let existing = std::fs::read(&path).ok();
        let (old_text, file) = match &existing {
            Some(bytes) => {
                self.cfg.reads.check(&sid, &path, bytes)?;
                let f = TextFile::decode(bytes).unwrap_or(TextFile {
                    text: String::from_utf8_lossy(bytes).into_owned(),
                    bom: false,
                    crlf: false,
                });
                // Keep the file's line endings and BOM unless the model wrote its own.
                let text = input.content.replace("\r\n", "\n");
                let mut nf = f.new_like(text);
                if input.content.contains("\r\n") {
                    nf.crlf = true;
                }
                (f.text, nf)
            }
            None => {
                let crlf = input.content.contains("\r\n");
                (
                    String::new(),
                    TextFile {
                        text: input.content.replace("\r\n", "\n"),
                        bom: false,
                        crlf,
                    },
                )
            }
        };
        let unified = diff::unified(&shown, &old_text, &file.text);
        authorize_edit(&ctx, &path, &shown, unified.clone()).await?;
        let bytes = file.encode();
        write_atomic(&path, &bytes).map_err(|e| ToolError::Failed {
            message: format!("can't write {shown}: {e}"),
            may_have_run: true,
        })?;
        self.cfg
            .reads
            .record(&sid, &path, Stamp::of(&bytes, mtime(&path)));
        let lines = file.text.lines().count();
        Ok(ToolOutput::Text(match existing {
            Some(_) => {
                let (a, r) = diff::stat(&unified);
                format!("Overwrote {shown} ({lines} lines, +{a} −{r}).")
            }
            None => format!("Created {shown} ({lines} lines)."),
        }))
    }
}
