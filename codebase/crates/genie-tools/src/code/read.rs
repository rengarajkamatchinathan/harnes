//! `read {path, offset?, limit?}` (06 §4.2).

use std::borrow::Cow;

use async_trait::async_trait;
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::Value;

use super::{
    CodeConfig, MAX_LINE_CHARS, MAX_READ_BYTES, MAX_READ_LINES, Stamp, alias_keys, authorize_read,
    code_only, display, mtime,
};
use crate::registry::{parse_input, schema_of};
use crate::tool::{Availability, Tool, ToolCtx, ToolEnv, ToolError, ToolKind, ToolOutput};

pub const NAME: &str = "read";

#[derive(Debug, Deserialize, JsonSchema)]
struct Input {
    /// File to read: relative to the workspace root, or absolute. A spilled-output
    /// reference (`[… full output: <ref>]`) also works.
    path: String,
    /// First line to show, 1-based. Default 1.
    #[serde(default)]
    offset: Option<u64>,
    /// Number of lines to show. Default and maximum 2000.
    #[serde(default)]
    limit: Option<u64>,
}

pub struct ReadTool {
    cfg: CodeConfig,
    schema: Value,
}

impl ReadTool {
    pub fn new(cfg: CodeConfig) -> Self {
        ReadTool {
            cfg,
            schema: schema_of::<Input>(),
        }
    }
}

const IMAGE_EXT: &[&str] = &["png", "jpg", "jpeg", "gif", "webp", "bmp", "ico", "tiff"];

/// Render `text` from line `offset` (1-based): `cat -n` style numbers, long lines cut,
/// ≤ `limit` lines and ≤ 50 KiB, with a note naming the next offset.
pub fn render(text: &str, offset: usize, limit: usize) -> String {
    let total = text.lines().count();
    if total == 0 {
        return "[the file is empty]".into();
    }
    if offset > total {
        return format!("[offset {offset} is past the end of the file ({total} lines)]");
    }
    let mut out = String::new();
    let mut last = offset - 1;
    for (i, line) in text.lines().enumerate().skip(offset - 1).take(limit) {
        let line = line.strip_suffix('\r').unwrap_or(line);
        let shown: Cow<'_, str> = if line.chars().count() > MAX_LINE_CHARS {
            let cut: String = line.chars().take(MAX_LINE_CHARS).collect();
            Cow::Owned(format!("{cut}… [line truncated]"))
        } else {
            Cow::Borrowed(line)
        };
        let row = format!("{:>6}\t{shown}\n", i + 1);
        if out.len() + row.len() > MAX_READ_BYTES && last >= offset {
            break;
        }
        out.push_str(&row);
        last = i + 1;
    }
    if last < total {
        out.push_str(&format!(
            "\n[showing lines {offset}–{last} of {total}; read with offset={} to continue]",
            last + 1
        ));
    }
    out.trim_end_matches('\n').to_string()
}

#[async_trait]
impl Tool for ReadTool {
    fn name(&self) -> &str {
        NAME
    }

    fn description(&self, _env: &ToolEnv) -> Cow<'_, str> {
        Cow::Borrowed(
            "Read a text file. Lines are numbered (`<n>\\t<text>`); the numbers are not part \
             of the file. Shows up to 2000 lines; use `offset` (1-based) and `limit` to page \
             through longer files. Read a file before editing or overwriting it. Also pages \
             spilled tool output when given its reference.",
        )
    }

    fn input_schema(&self) -> &Value {
        &self.schema
    }

    fn kind(&self) -> ToolKind {
        ToolKind::ReadOnly
    }

    fn availability(&self, env: &ToolEnv) -> Availability {
        code_only(env)
    }

    fn authorizes(&self) -> bool {
        true
    }

    async fn execute(&self, input: Value, ctx: ToolCtx) -> Result<ToolOutput, ToolError> {
        let input = alias_keys(input, &[("file_path", "path"), ("filePath", "path")]);
        let input: Input = parse_input(input, &self.schema)?;
        let sid = ctx.env.session_id.clone();
        let root = self.cfg.root(&sid)?;
        let path = super::resolve_in(&root, &input.path);
        let offset = usize::try_from(input.offset.unwrap_or(1).max(1)).unwrap_or(usize::MAX);
        let limit = usize::try_from(input.limit.unwrap_or(MAX_READ_LINES as u64))
            .unwrap_or(MAX_READ_LINES)
            .clamp(1, MAX_READ_LINES);

        // A spill reference that isn't a file: page the spilled output.
        if !path.exists()
            && let Some(spill) = ctx.env.services.spill.as_ref()
            && let Ok(full) = spill.read(&sid, input.path.trim())
        {
            return Ok(ToolOutput::Text(render(&full, offset, limit)));
        }
        authorize_read(&ctx, &path).await?;
        let shown = display(&root, &path);
        let meta = match std::fs::metadata(&path) {
            Ok(m) => m,
            Err(_) => return Err(ToolError::failed(not_found(&path, &shown))),
        };
        if meta.is_dir() {
            return Err(ToolError::failed(format!(
                "{shown} is a directory; use `list` to see its contents"
            )));
        }
        let ext = path
            .extension()
            .map(|e| e.to_string_lossy().to_ascii_lowercase())
            .unwrap_or_default();
        if IMAGE_EXT.contains(&ext.as_str()) || ext == "pdf" {
            return Ok(ToolOutput::Text(format!(
                "[{shown} is a {} file of {} bytes; this engine build can't show images or \
                 PDFs to the model yet]",
                if ext == "pdf" { "PDF" } else { "image" },
                meta.len()
            )));
        }
        let path2 = path.clone();
        let bytes = tokio::task::spawn_blocking(move || std::fs::read(&path2))
            .await
            .map_err(|e| ToolError::failed(e.to_string()))?
            .map_err(|e| ToolError::failed(format!("can't read {shown}: {e}")))?;
        if bytes[..bytes.len().min(8192)].contains(&0) {
            return Ok(ToolOutput::Text(format!(
                "[{shown} looks binary ({} bytes); not shown]",
                bytes.len()
            )));
        }
        self.cfg
            .reads
            .record(&sid, &path, Stamp::of(&bytes, mtime(&path)));
        let text = String::from_utf8_lossy(&bytes);
        let text = text.strip_prefix('\u{feff}').unwrap_or(&text);
        Ok(ToolOutput::Text(render(text, offset, limit)))
    }
}

/// "not found" with up to three similarly named files in the same folder.
fn not_found(path: &std::path::Path, shown: &str) -> String {
    let mut msg = format!("{shown} does not exist");
    let stem = path
        .file_stem()
        .map(|s| s.to_string_lossy().to_lowercase())
        .unwrap_or_default();
    if let Some(parent) = path.parent()
        && let Ok(rd) = std::fs::read_dir(parent)
        && !stem.is_empty()
    {
        let mut similar: Vec<String> = rd
            .flatten()
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|n| n.to_lowercase().contains(&stem))
            .collect();
        similar.sort();
        similar.truncate(3);
        if !similar.is_empty() {
            msg.push_str(&format!("; similar names here: {}", similar.join(", ")));
        }
    }
    msg
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn render_numbers_pages_and_cuts() {
        let text: String = (1..=5).map(|i| format!("l{i}\n")).collect();
        assert_eq!(
            render(&text, 1, 2000),
            "     1\tl1\n     2\tl2\n     3\tl3\n     4\tl4\n     5\tl5"
        );
        let r = render(&text, 2, 2);
        assert_eq!(
            r,
            "     2\tl2\n     3\tl3\n\n[showing lines 2–3 of 5; read with offset=4 to continue]"
        );
        assert!(render(&text, 9, 1).contains("past the end"));
        let long = "x".repeat(MAX_LINE_CHARS + 10);
        assert!(render(&long, 1, 1).ends_with("… [line truncated]"));
        assert_eq!(render("", 1, 1), "[the file is empty]");
    }
}
