//! `grep`, `glob` and `list` (06 §4.2): the ripgrep crates in-process, so every OS gets
//! the same hidden-file and ignore rules as `rg` (`.gitignore` in git repos, `.ignore`,
//! `.git/info/exclude`, hidden files skipped). 100 results / 30 s.

use std::borrow::Cow;
use std::path::{Path, PathBuf};
use std::time::{Instant, SystemTime};

use async_trait::async_trait;
use ignore::WalkBuilder;
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::Value;
use tokio_util::sync::CancellationToken;

use super::{
    CodeConfig, MAX_RESULTS, SEARCH_TIMEOUT, alias_keys, authorize_read, code_only, display,
};
use crate::registry::{parse_input, schema_of};
use crate::tool::{Availability, Tool, ToolCtx, ToolEnv, ToolError, ToolKind, ToolOutput};

pub const GREP: &str = "grep";
pub const GLOB: &str = "glob";
pub const LIST: &str = "list";

/// Files larger than this are not searched.
const MAX_SEARCH_FILE: u64 = 20 * 1024 * 1024;
/// `list` stops after this many entries.
const MAX_LIST: usize = 500;

fn walker(root: &Path, hidden: bool) -> WalkBuilder {
    let mut w = WalkBuilder::new(root);
    w.hidden(!hidden)
        .git_ignore(true)
        .git_exclude(true)
        .git_global(true)
        .ignore(true)
        .parents(true)
        .follow_links(false)
        .sort_by_file_path(|a, b| a.cmp(b));
    w
}

/// Why a search stopped early.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
struct Stop {
    limit: bool,
    timeout: bool,
}

impl Stop {
    fn note(self) -> &'static str {
        if self.timeout {
            "\n[search stopped after 30 s; results are partial — narrow the path or pattern]"
        } else if self.limit {
            "\n[results cut at 100; narrow the path or pattern]"
        } else {
            ""
        }
    }
}

async fn blocking<T: Send + 'static>(
    f: impl FnOnce() -> Result<T, ToolError> + Send + 'static,
) -> Result<T, ToolError> {
    tokio::task::spawn_blocking(f)
        .await
        .map_err(|e| ToolError::failed(e.to_string()))?
}

async fn search_root(
    cfg: &CodeConfig,
    ctx: &ToolCtx,
    path: Option<&str>,
) -> Result<(PathBuf, PathBuf), ToolError> {
    let root = cfg.root(&ctx.env.session_id)?;
    let dir = match path.filter(|p| !p.trim().is_empty()) {
        Some(p) => super::resolve_in(&root, p),
        None => root.clone(),
    };
    authorize_read(ctx, &dir).await?;
    if !dir.exists() {
        return Err(ToolError::failed(format!(
            "{} does not exist",
            display(&root, &dir)
        )));
    }
    Ok((root, dir))
}

// ------------------------------------------------------------------ grep

#[derive(Debug, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
enum GrepMode {
    /// Matching lines as `path:line:text` (default).
    Content,
    /// Only the paths of matching files.
    FilesWithMatches,
    /// Match counts per file.
    Count,
}

#[derive(Debug, Deserialize, JsonSchema)]
struct GrepInput {
    /// Regular expression (Rust/ripgrep syntax).
    pattern: String,
    /// File or folder to search; default the workspace root.
    #[serde(default)]
    path: Option<String>,
    /// Only files whose name (or relative path, if it has a `/`) matches this glob,
    /// e.g. `*.rs` or `src/**/*.ts`.
    #[serde(default)]
    glob: Option<String>,
    /// Case-insensitive match.
    #[serde(default)]
    case_insensitive: bool,
    /// Lines of context around each match (content mode).
    #[serde(default)]
    context: Option<u32>,
    /// Output: `content` (default), `files_with_matches` or `count`.
    #[serde(default)]
    output_mode: Option<GrepMode>,
    /// Also search hidden files.
    #[serde(default)]
    hidden: bool,
}

pub struct GrepTool {
    cfg: CodeConfig,
    schema: Value,
}

impl GrepTool {
    pub fn new(cfg: CodeConfig) -> Self {
        GrepTool {
            cfg,
            schema: schema_of::<GrepInput>(),
        }
    }
}

struct FileFilter {
    set: Option<globset::GlobMatcher>,
    by_path: bool,
}

impl FileFilter {
    fn new(glob: Option<&str>) -> Result<FileFilter, ToolError> {
        let Some(g) = glob.filter(|g| !g.trim().is_empty()) else {
            return Ok(FileFilter {
                set: None,
                by_path: false,
            });
        };
        let by_path = g.contains('/');
        let m = globset::GlobBuilder::new(g.trim_start_matches("./"))
            .literal_separator(by_path)
            .build()
            .map_err(|e| ToolError::invalid_input(vec![format!("bad glob `{g}`: {e}")]))?
            .compile_matcher();
        Ok(FileFilter {
            set: Some(m),
            by_path,
        })
    }

    fn accepts(&self, rel: &Path) -> bool {
        match &self.set {
            None => true,
            Some(m) if self.by_path => m.is_match(rel),
            Some(m) => rel.file_name().is_some_and(|n| m.is_match(n)),
        }
    }
}

/// The blocking body of `grep`.
#[allow(clippy::too_many_arguments)]
fn grep_blocking(
    root: &Path,
    dir: &Path,
    re: &regex::Regex,
    filter: &FileFilter,
    mode: &GrepMode,
    context: usize,
    hidden: bool,
    cancel: &CancellationToken,
) -> (Vec<String>, Stop, usize) {
    let deadline = Instant::now() + SEARCH_TIMEOUT;
    let mut out = Vec::new();
    let mut stop = Stop::default();
    let mut matches = 0usize;
    for entry in walker(dir, hidden).build().flatten() {
        if cancel.is_cancelled() || Instant::now() > deadline {
            stop.timeout = !cancel.is_cancelled();
            break;
        }
        if !entry.file_type().is_some_and(|t| t.is_file()) {
            continue;
        }
        let p = entry.path();
        let rel = p.strip_prefix(dir).unwrap_or(p);
        if !filter.accepts(rel) {
            continue;
        }
        if entry.metadata().is_ok_and(|m| m.len() > MAX_SEARCH_FILE) {
            continue;
        }
        let Ok(bytes) = std::fs::read(p) else {
            continue;
        };
        if bytes[..bytes.len().min(8192)].contains(&0) {
            continue; // binary, like rg
        }
        let text = String::from_utf8_lossy(&bytes);
        let shown = display(root, p);
        let lines: Vec<&str> = text.lines().collect();
        let hits: Vec<usize> = lines
            .iter()
            .enumerate()
            .filter(|(_, l)| re.is_match(l))
            .map(|(i, _)| i)
            .collect();
        if hits.is_empty() {
            continue;
        }
        matches += hits.len();
        match mode {
            GrepMode::FilesWithMatches => out.push(shown),
            GrepMode::Count => out.push(format!("{shown}:{}", hits.len())),
            GrepMode::Content => {
                let mut last_shown: Option<usize> = None;
                for &h in &hits {
                    let from = h.saturating_sub(context);
                    let to = (h + context).min(lines.len() - 1);
                    if context > 0 && last_shown.is_some_and(|l| from > l + 1) {
                        out.push("--".into());
                    }
                    for (i, line) in lines.iter().enumerate().take(to + 1).skip(from) {
                        if last_shown.is_some_and(|l| i <= l) {
                            continue;
                        }
                        let sep = if i == h { ':' } else { '-' };
                        let line: String = line.chars().take(super::MAX_LINE_CHARS).collect();
                        out.push(format!("{shown}{sep}{}{sep}{line}", i + 1));
                        last_shown = Some(i);
                    }
                    if out.len() >= MAX_RESULTS {
                        break;
                    }
                }
            }
        }
        if out.len() >= MAX_RESULTS {
            out.truncate(MAX_RESULTS);
            stop.limit = true;
            break;
        }
    }
    (out, stop, matches)
}

#[async_trait]
impl Tool for GrepTool {
    fn name(&self) -> &str {
        GREP
    }

    fn description(&self, _env: &ToolEnv) -> Cow<'_, str> {
        Cow::Borrowed(
            "Search file contents with a regular expression (ripgrep syntax). Skips \
             .gitignored, hidden and binary files. `output_mode`: `content` (matching lines \
             as path:line:text), `files_with_matches` or `count`. Filter files with `glob`. \
             At most 100 results.",
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
        let input: GrepInput = parse_input(
            alias_keys(input, &[("include", "glob"), ("-i", "case_insensitive")]),
            &self.schema,
        )?;
        let re = regex::RegexBuilder::new(&input.pattern)
            .case_insensitive(input.case_insensitive)
            .size_limit(10 * 1024 * 1024)
            .build()
            .map_err(|e| ToolError::invalid_input(vec![format!("bad pattern: {e}")]))?;
        let filter = FileFilter::new(input.glob.as_deref())?;
        let (root, dir) = search_root(&self.cfg, &ctx, input.path.as_deref()).await?;
        let mode = input.output_mode.unwrap_or(GrepMode::Content);
        let context = input.context.unwrap_or(0).min(10) as usize;
        let hidden = input.hidden;
        let cancel = ctx.cancel.clone();
        let (lines, stop, n) = blocking(move || {
            Ok(grep_blocking(
                &root, &dir, &re, &filter, &mode, context, hidden, &cancel,
            ))
        })
        .await?;
        if ctx.cancel.is_cancelled() {
            return Err(ToolError::Cancelled);
        }
        if lines.is_empty() {
            return Ok(ToolOutput::text("No matches."));
        }
        let mut out = lines.join("\n");
        if !stop.limit && !stop.timeout && n > 1 {
            out.push_str(&format!("\n[{n} matching lines]"));
        }
        out.push_str(stop.note());
        Ok(ToolOutput::Text(out))
    }
}

// ------------------------------------------------------------------ glob

#[derive(Debug, Deserialize, JsonSchema)]
struct GlobInput {
    /// Glob such as `**/*.rs` or `src/*.ts`; a pattern without `/` matches file names at
    /// any depth.
    pattern: String,
    /// Folder to search; default the workspace root.
    #[serde(default)]
    path: Option<String>,
}

pub struct GlobTool {
    cfg: CodeConfig,
    schema: Value,
}

impl GlobTool {
    pub fn new(cfg: CodeConfig) -> Self {
        GlobTool {
            cfg,
            schema: schema_of::<GlobInput>(),
        }
    }
}

#[async_trait]
impl Tool for GlobTool {
    fn name(&self) -> &str {
        GLOB
    }

    fn description(&self, _env: &ToolEnv) -> Cow<'_, str> {
        Cow::Borrowed(
            "Find files by name pattern (e.g. `**/*.rs`, `src/**/test_*.py`). Skips \
             .gitignored and hidden files. Newest files first; at most 100.",
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
        let input: GlobInput = parse_input(input, &self.schema)?;
        let filter = FileFilter::new(Some(&input.pattern))?;
        let (root, dir) = search_root(&self.cfg, &ctx, input.path.as_deref()).await?;
        let cancel = ctx.cancel.clone();
        let (mut found, stop) = blocking(move || {
            let deadline = Instant::now() + SEARCH_TIMEOUT;
            let mut found: Vec<(SystemTime, String)> = Vec::new();
            let mut stop = Stop::default();
            for e in walker(&dir, false).build().flatten() {
                if cancel.is_cancelled() || Instant::now() > deadline {
                    stop.timeout = !cancel.is_cancelled();
                    break;
                }
                if !e.file_type().is_some_and(|t| t.is_file()) {
                    continue;
                }
                let rel = e.path().strip_prefix(&dir).unwrap_or(e.path());
                if filter.accepts(rel) {
                    let t = e
                        .metadata()
                        .ok()
                        .and_then(|m| m.modified().ok())
                        .unwrap_or(SystemTime::UNIX_EPOCH);
                    found.push((t, display(&root, e.path())));
                }
            }
            Ok((found, stop))
        })
        .await?;
        if found.is_empty() {
            return Ok(ToolOutput::text("No files found."));
        }
        found.sort_by(|a, b| b.0.cmp(&a.0).then(a.1.cmp(&b.1)));
        let mut stop = stop;
        if found.len() > MAX_RESULTS {
            found.truncate(MAX_RESULTS);
            stop.limit = true;
        }
        let mut out: String = found
            .into_iter()
            .map(|(_, p)| p)
            .collect::<Vec<_>>()
            .join("\n");
        out.push_str(stop.note());
        Ok(ToolOutput::Text(out))
    }
}

// ------------------------------------------------------------------ list

#[derive(Debug, Deserialize, JsonSchema)]
struct ListInput {
    /// Folder to list; default the workspace root.
    #[serde(default)]
    path: Option<String>,
    /// Extra globs to leave out (e.g. `*.log`).
    #[serde(default)]
    ignore: Vec<String>,
}

pub struct ListTool {
    cfg: CodeConfig,
    schema: Value,
}

impl ListTool {
    pub fn new(cfg: CodeConfig) -> Self {
        ListTool {
            cfg,
            schema: schema_of::<ListInput>(),
        }
    }
}

#[async_trait]
impl Tool for ListTool {
    fn name(&self) -> &str {
        LIST
    }

    fn description(&self, _env: &ToolEnv) -> Cow<'_, str> {
        Cow::Borrowed(
            "List a folder as an indented tree (folders end with `/`). Skips .gitignored and \
             hidden files; `ignore` adds globs to leave out. At most 500 entries.",
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
        let input = alias_keys(input, &[("dir", "path"), ("directory", "path")]);
        let input: ListInput = parse_input(input, &self.schema)?;
        let mut ignores = globset::GlobSetBuilder::new();
        for g in &input.ignore {
            ignores.add(
                globset::Glob::new(g)
                    .map_err(|e| ToolError::invalid_input(vec![format!("bad glob `{g}`: {e}")]))?,
            );
        }
        let ignores = ignores
            .build()
            .map_err(|e| ToolError::invalid_input(vec![e.to_string()]))?;
        let (root, dir) = search_root(&self.cfg, &ctx, input.path.as_deref()).await?;
        if !dir.is_dir() {
            return Err(ToolError::failed(format!(
                "{} is not a folder",
                display(&root, &dir)
            )));
        }
        let (lines, cut) = blocking(move || {
            let mut lines = vec![format!("{}/", display(&root, &dir))];
            let mut cut = false;
            for e in walker(&dir, false).build().flatten() {
                if e.depth() == 0 {
                    continue;
                }
                let rel = e.path().strip_prefix(&dir).unwrap_or(e.path());
                if rel
                    .ancestors()
                    .any(|a| !a.as_os_str().is_empty() && ignores.is_match(a))
                    || e.file_name().to_str().is_some_and(|n| ignores.is_match(n))
                {
                    continue;
                }
                if lines.len() > MAX_LIST {
                    cut = true;
                    break;
                }
                let name = e.file_name().to_string_lossy();
                let slash = if e.file_type().is_some_and(|t| t.is_dir()) {
                    "/"
                } else {
                    ""
                };
                lines.push(format!("{}{name}{slash}", "  ".repeat(e.depth())));
            }
            Ok((lines, cut))
        })
        .await?;
        let mut out = lines.join("\n");
        if cut {
            out.push_str("\n[listing cut at 500 entries; list a subfolder]");
        }
        Ok(ToolOutput::Text(out))
    }
}
