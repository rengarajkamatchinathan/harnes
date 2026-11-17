//! `tool_output_read {ref, offset, limit}` (06 §4.1, SA-39): pages spilled output in
//! chat sessions, which have no `read` tool.

use std::borrow::Cow;

use async_trait::async_trait;
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::Value;

use crate::output::{head, line_count};
use crate::registry::{parse_input, schema_of};
use crate::tool::{Availability, Tool, ToolCtx, ToolEnv, ToolError, ToolKind, ToolOutput};

pub const NAME: &str = "tool_output_read";

#[derive(Debug, Deserialize, JsonSchema)]
struct Input {
    /// The reference from a `[showing … full output: <ref>]` marker.
    #[serde(rename = "ref")]
    reference: String,
    /// First line to show, 1-based. Default 1.
    #[serde(default)]
    offset: Option<u64>,
    /// Number of lines to show. Default and maximum 2000.
    #[serde(default)]
    limit: Option<u64>,
}

pub struct ToolOutputRead {
    schema: Value,
}

impl Default for ToolOutputRead {
    fn default() -> Self {
        ToolOutputRead {
            schema: schema_of::<Input>(),
        }
    }
}

#[async_trait]
impl Tool for ToolOutputRead {
    fn name(&self) -> &str {
        NAME
    }

    fn description(&self, _env: &ToolEnv) -> Cow<'_, str> {
        Cow::Borrowed(
            "Read more of a tool output that was cut. Pass the `ref` from the \
             `[showing 1–N of M; full output: <ref>]` marker, and an `offset` (first line, \
             1-based) and `limit` (lines) to page through it.",
        )
    }

    fn input_schema(&self) -> &Value {
        &self.schema
    }

    fn kind(&self) -> ToolKind {
        ToolKind::ReadOnly
    }

    fn availability(&self, env: &ToolEnv) -> Availability {
        if env.services.spill.is_some() {
            Availability::Available
        } else {
            Availability::Unavailable(Cow::Borrowed("no spill store"))
        }
    }

    async fn execute(&self, input: Value, ctx: ToolCtx) -> Result<ToolOutput, ToolError> {
        let input: Input = parse_input(input, &self.schema)?;
        let Some(spill) = ctx.env.services.spill.as_ref() else {
            return Err(ToolError::failed("spilled output is not available"));
        };
        let full = spill
            .read(&ctx.env.session_id, &input.reference)
            .map_err(ToolError::failed)?;
        let total = line_count(&full);
        let limits = ctx.env.output_limits;
        let offset = usize::try_from(input.offset.unwrap_or(1).max(1)).unwrap_or(usize::MAX);
        if offset > total.max(1) {
            return Err(ToolError::failed(format!(
                "offset {offset} is past the end ({total} lines)"
            )));
        }
        let want = usize::try_from(input.limit.unwrap_or(limits.max_lines as u64))
            .unwrap_or(usize::MAX)
            .clamp(1, limits.max_lines);
        let rest: String = full
            .split_inclusive('\n')
            .skip(offset - 1)
            .take(want)
            .collect();
        let (shown, n) = head(&rest, limits);
        let mut out = shown.trim_end_matches('\n').to_string();
        let last = offset + n.saturating_sub(1);
        if last < total {
            out.push_str(&format!(
                "\n\n[showing {offset}–{last} of {total}; full output: {}]",
                input.reference
            ));
        }
        Ok(ToolOutput::Text(out))
    }
}
