//! `patch {patch}` (06 §4.2): the Codex `apply_patch` envelope, **atomic with rollback**.
//!
//! ```text
//! *** Begin Patch
//! *** Add File: path/new.txt
//! +line
//! *** Delete File: path/old.txt
//! *** Update File: path/a.rs
//! *** Move to: path/b.rs
//! @@ fn main() {
//!  context
//! -old
//! +new
//! *** End of File
//! *** End Patch
//! ```
//!
//! Every path is locked (sorted, so two patches can't deadlock), every file is computed
//! in memory, each change is authorised with its diff, then all are written; a failed
//! write restores the files already written.

use std::borrow::Cow;
use std::path::{Path, PathBuf};

use async_trait::async_trait;
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::Value;

use super::{
    CodeConfig, PathGuard, Stamp, TextFile, alias_keys, authorize_edit, code_only, diff, display,
    mtime, write_atomic,
};
use crate::registry::{parse_input, schema_of};
use crate::tool::{Availability, Tool, ToolCtx, ToolEnv, ToolError, ToolKind, ToolOutput};

pub const NAME: &str = "patch";

#[derive(Debug, Deserialize, JsonSchema)]
struct Input {
    /// The whole patch, from `*** Begin Patch` to `*** End Patch`.
    patch: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Chunk {
    /// Text after `@@` (a line to seek to first), if any.
    pub header: Option<String>,
    pub old: Vec<String>,
    pub new: Vec<String>,
    pub eof: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Hunk {
    Add {
        path: String,
        content: String,
    },
    Delete {
        path: String,
    },
    Update {
        path: String,
        move_to: Option<String>,
        chunks: Vec<Chunk>,
    },
}

fn err(line: usize, msg: impl std::fmt::Display) -> String {
    format!("patch line {line}: {msg}")
}

/// Parse the envelope.
pub fn parse(text: &str) -> Result<Vec<Hunk>, String> {
    let lines: Vec<&str> = text.trim().lines().map(|l| l.trim_end_matches('\r')).collect();
    if lines.first().map(|l| l.trim()) != Some("*** Begin Patch") {
        return Err("the patch must start with `*** Begin Patch`".into());
    }
    if lines.last().map(|l| l.trim()) != Some("*** End Patch") {
        return Err("the patch must end with `*** End Patch`".into());
    }
    let body = &lines[1..lines.len() - 1];
    let mut hunks = Vec::new();
    let mut i = 0;
    while i < body.len() {
        let l = body[i];
        let ln = i + 2;
        if let Some(p) = l.strip_prefix("*** Add File: ") {
            let mut content = String::new();
            i += 1;
            while i < body.len() && !body[i].starts_with("*** ") {
                match body[i].strip_prefix('+') {
                    Some(t) => {
                        content.push_str(t);
                        content.push('\n');
                    }
                    None => return Err(err(i + 2, "lines of an added file must start with `+`")),
                }
                i += 1;
            }
            hunks.push(Hunk::Add {
                path: p.trim().to_string(),
                content,
            });
        } else if let Some(p) = l.strip_prefix("*** Delete File: ") {
            hunks.push(Hunk::Delete {
                path: p.trim().to_string(),
            });
            i += 1;
        } else if let Some(p) = l.strip_prefix("*** Update File: ") {
            i += 1;
            let mut move_to = None;
            if i < body.len()
                && let Some(m) = body[i].strip_prefix("*** Move to: ")
            {
                move_to = Some(m.trim().to_string());
                i += 1;
            }
            let mut chunks: Vec<Chunk> = Vec::new();
            let mut cur: Option<Chunk> = None;
            while i < body.len() {
                let t = body[i];
                if t.trim() == "*** End of File" {
                    if let Some(c) = cur.as_mut() {
                        c.eof = true;
                    }
                    i += 1;
                    continue;
                }
                if t.starts_with("*** ") {
                    break;
                }
                if let Some(h) = t.strip_prefix("@@") {
                    if let Some(c) = cur.take() {
                        chunks.push(c);
                    }
                    let h = h.trim();
                    cur = Some(Chunk {
                        header: (!h.is_empty()).then(|| h.to_string()),
                        old: vec![],
                        new: vec![],
                        eof: false,
                    });
                    i += 1;
                    continue;
                }
                let c = cur.get_or_insert_with(|| Chunk {
                    header: None,
                    old: vec![],
                    new: vec![],
                    eof: false,
                });
                match t.chars().next() {
                    Some('+') => c.new.push(t[1..].to_string()),
                    Some('-') => c.old.push(t[1..].to_string()),
                    Some(' ') => {
                        c.old.push(t[1..].to_string());
                        c.new.push(t[1..].to_string());
                    }
                    None => {
                        c.old.push(String::new());
                        c.new.push(String::new());
                    }
                    Some(_) => {
                        return Err(err(
                            i + 2,
                            "hunk lines must start with ` `, `+`, `-` or `@@`",
                        ));
                    }
                }
                i += 1;
            }
            if let Some(c) = cur.take() {
                chunks.push(c);
            }
            if chunks.is_empty() && move_to.is_none() {
                return Err(err(ln, format!("`*** Update File: {}` has no hunks", p.trim())));
            }
            hunks.push(Hunk::Update {
                path: p.trim().to_string(),
                move_to,
                chunks,
            });
        } else if l.trim().is_empty() {
            i += 1;
        } else {
            return Err(err(
                ln,
                "expected `*** Add File:`, `*** Delete File:` or `*** Update File:`",
            ));
        }
    }
    if hunks.is_empty() {
        return Err("the patch changes no files".into());
    }
    Ok(hunks)
}

/// Find `needle` in `hay` at or after `from`: exact, then ignoring trailing whitespace,
/// then ignoring surrounding whitespace. `eof` prefers a match that ends the file.
fn seek(hay: &[String], needle: &[String], from: usize, eof: bool) -> Option<usize> {
    if needle.is_empty() {
        return Some(hay.len());
    }
    if needle.len() > hay.len() {
        return None;
    }
    let cmps: [fn(&str, &str) -> bool; 3] = [
        |a, b| a == b,
        |a, b| a.trim_end() == b.trim_end(),
        |a, b| a.trim() == b.trim(),
    ];
    for cmp in cmps {
        let fits = |s: usize| needle.iter().enumerate().all(|(k, n)| cmp(&hay[s + k], n));
        let last = hay.len() - needle.len();
        if eof && last >= from && fits(last) {
            return Some(last);
        }
        if let Some(s) = (from..=last).find(|&s| fits(s)) {
            return Some(s);
        }
    }
    None
}

/// Apply update chunks to `text` (LF view).
pub fn apply_chunks(text: &str, chunks: &[Chunk], shown: &str) -> Result<String, String> {
    let had_newline = text.ends_with('\n') || text.is_empty();
    let mut lines: Vec<String> = text.split('\n').map(String::from).collect();
    if text.ends_with('\n') || text.is_empty() {
        lines.pop();
    }
    let mut repl: Vec<(usize, usize, Vec<String>)> = Vec::new();
    let mut cursor = 0;
    for (n, c) in chunks.iter().enumerate() {
        if let Some(h) = &c.header {
            if let Some(i) = (cursor..lines.len())
                .find(|&i| lines[i] == *h)
                .or_else(|| (cursor..lines.len()).find(|&i| lines[i].trim() == h.trim()))
            {
                cursor = i + 1;
            } else if c.old.is_empty() {
                return Err(format!("{shown}: hunk {} context `@@ {h}` not found", n + 1));
            }
        }
        if c.old.is_empty() {
            repl.push((lines.len(), 0, c.new.clone()));
            continue;
        }
        let at = seek(&lines, &c.old, cursor, c.eof).ok_or_else(|| {
            format!(
                "{shown}: hunk {} doesn't match the file; expected these lines:\n{}",
                n + 1,
                c.old.join("\n")
            )
        })?;
        repl.push((at, c.old.len(), c.new.clone()));
        cursor = at + c.old.len();
    }
    repl.sort_by_key(|r| r.0);
    for (at, len, new) in repl.into_iter().rev() {
        lines.splice(at..at + len, new);
    }
    let mut out = lines.join("\n");
    if had_newline && !out.is_empty() {
        out.push('\n');
    }
    Ok(out)
}

pub struct PatchTool {
    cfg: CodeConfig,
    schema: Value,
}

impl PatchTool {
    pub fn new(cfg: CodeConfig) -> Self {
        PatchTool {
            cfg,
            schema: schema_of::<Input>(),
        }
    }
}

/// One file change, computed before anything is written.
struct Planned {
    path: PathBuf,
    shown: String,
    /// What was on disk (`None`: absent).
    before: Option<Vec<u8>>,
    /// What to write (`None`: delete).
    after: Option<Vec<u8>>,
    diff: String,
}

#[async_trait]
impl Tool for PatchTool {
    fn name(&self) -> &str {
        NAME
    }

    fn description(&self, _env: &ToolEnv) -> Cow<'_, str> {
        Cow::Borrowed(
            "Apply a patch in the `*** Begin Patch` / `*** End Patch` format: `*** Add File: \
             <path>` (lines start with `+`), `*** Delete File: <path>`, `*** Update File: \
             <path>` (optionally `*** Move to: <path>`) with `@@` hunks of ` ` context, `-` \
             removed and `+` added lines. All files change together or none does.",
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
        let input: Input = parse_input(
            alias_keys(input, &[("input", "patch"), ("diff", "patch")]),
            &self.schema,
        )?;
        let hunks = parse(&input.patch).map_err(ToolError::failed)?;
        let sid = ctx.env.session_id.clone();
        let root = self.cfg.root(&sid)?;
        let resolve = |p: &str| super::resolve_in(&root, p);

        // Lock every path involved, in a fixed order.
        let mut paths: Vec<PathBuf> = Vec::new();
        for h in &hunks {
            match h {
                Hunk::Add { path, .. } | Hunk::Delete { path } => paths.push(resolve(path)),
                Hunk::Update { path, move_to, .. } => {
                    paths.push(resolve(path));
                    if let Some(m) = move_to {
                        paths.push(resolve(m));
                    }
                }
            }
        }
        paths.sort();
        paths.dedup();
        let mut guards: Vec<PathGuard> = Vec::new();
        for p in &paths {
            guards.push(self.cfg.locks.lock(p).await);
        }

        let plan = self.plan(&sid, &root, &hunks)?;
        for p in &plan {
            authorize_edit(&ctx, &p.path, &p.shown, p.diff.clone()).await?;
        }
        // Write, rolling back on the first failure.
        let mut done: Vec<&Planned> = Vec::new();
        for p in &plan {
            let r = match &p.after {
                Some(b) => write_atomic(&p.path, b),
                None => std::fs::remove_file(&p.path),
            };
            if let Err(e) = r {
                let rolled = rollback(&done);
                return Err(ToolError::Failed {
                    message: format!(
                        "can't write {}: {e}; {}",
                        p.shown,
                        if rolled {
                            "no file was changed (rolled back)"
                        } else {
                            "rollback failed for some files; check them"
                        }
                    ),
                    may_have_run: !rolled,
                });
            }
            done.push(p);
        }
        let mut out = String::from("Patch applied:");
        for p in &plan {
            match (&p.before, &p.after) {
                (None, Some(_)) => out.push_str(&format!("\n  A {}", p.shown)),
                (Some(_), None) => out.push_str(&format!("\n  D {}", p.shown)),
                _ => {
                    let (a, r) = diff::stat(&p.diff);
                    out.push_str(&format!("\n  M {} (+{a} −{r})", p.shown));
                }
            }
            if let Some(b) = &p.after {
                self.cfg
                    .reads
                    .record(&sid, &p.path, Stamp::of(b, mtime(&p.path)));
            }
        }
        drop(guards);
        Ok(ToolOutput::Text(out))
    }
}

impl PatchTool {
    /// Staleness: a file this session read must not have changed since (files it never
    /// read may still be patched; the hunks' context lines guard them).
    fn check_fresh(&self, sid: &str, path: &Path, bytes: &[u8]) -> Result<(), ToolError> {
        if self.cfg.reads.get(sid, path).is_some() {
            self.cfg.reads.check(sid, path, bytes)?;
        }
        Ok(())
    }

    fn plan(&self, sid: &str, root: &Path, hunks: &[Hunk]) -> Result<Vec<Planned>, ToolError> {
        let mut plan: Vec<Planned> = Vec::new();
        for h in hunks {
            match h {
                Hunk::Add { path, content } => {
                    let p = super::resolve_in(root, path);
                    let shown = display(root, &p);
                    if p.exists() {
                        return Err(ToolError::failed(format!(
                            "{shown} already exists; use `*** Update File:`"
                        )));
                    }
                    plan.push(Planned {
                        diff: diff::unified(&shown, "", content),
                        path: p,
                        shown,
                        before: None,
                        after: Some(content.clone().into_bytes()),
                    });
                }
                Hunk::Delete { path } => {
                    let p = super::resolve_in(root, path);
                    let shown = display(root, &p);
                    let bytes = std::fs::read(&p)
                        .map_err(|_| ToolError::failed(format!("{shown} does not exist")))?;
                    self.check_fresh(sid, &p, &bytes)?;
                    plan.push(Planned {
                        diff: diff::unified(&shown, &String::from_utf8_lossy(&bytes), ""),
                        path: p,
                        shown,
                        before: Some(bytes),
                        after: None,
                    });
                }
                Hunk::Update {
                    path,
                    move_to,
                    chunks,
                } => {
                    let p = super::resolve_in(root, path);
                    let shown = display(root, &p);
                    let bytes = std::fs::read(&p)
                        .map_err(|_| ToolError::failed(format!("{shown} does not exist")))?;
                    self.check_fresh(sid, &p, &bytes)?;
                    let f = TextFile::decode(&bytes)?;
                    let text = apply_chunks(&f.text, chunks, &shown).map_err(ToolError::failed)?;
                    let nf = f.new_like(text.clone());
                    match move_to.as_deref().filter(|m| super::resolve_in(root, m) != p) {
                        Some(m) => {
                            let dst = super::resolve_in(root, m);
                            let dshown = display(root, &dst);
                            if dst.exists() && dst != p {
                                return Err(ToolError::failed(format!(
                                    "can't move {shown} to {dshown}: it exists"
                                )));
                            }
                            plan.push(Planned {
                                diff: diff::unified(&dshown, &f.text, &text),
                                path: dst,
                                shown: dshown,
                                before: None,
                                after: Some(nf.encode()),
                            });
                            plan.push(Planned {
                                diff: diff::unified(&shown, &f.text, ""),
                                path: p,
                                shown,
                                before: Some(bytes),
                                after: None,
                            });
                        }
                        None => plan.push(Planned {
                            diff: diff::unified(&shown, &f.text, &text),
                            path: p,
                            shown,
                            before: Some(bytes),
                            after: Some(nf.encode()),
                        }),
                    }
                }
            }
        }
        Ok(plan)
    }
}

/// Undo the writes in `done` (newest first). `true` when every file was restored.
fn rollback(done: &[&Planned]) -> bool {
    let mut ok = true;
    for p in done.iter().rev() {
        let r = match &p.before {
            Some(b) => write_atomic(&p.path, b),
            None => std::fs::remove_file(&p.path),
        };
        ok &= r.is_ok();
    }
    ok
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_all_hunk_kinds() {
        let p = "*** Begin Patch\n*** Add File: a.txt\n+one\n+two\n*** Delete File: b.txt\n\
                 *** Update File: c.txt\n*** Move to: d.txt\n@@ fn main\n ctx\n-old\n+new\n\
                 *** End of File\n*** End Patch\n";
        let h = parse(p).unwrap();
        assert_eq!(
            h[0],
            Hunk::Add {
                path: "a.txt".into(),
                content: "one\ntwo\n".into()
            }
        );
        assert_eq!(h[1], Hunk::Delete { path: "b.txt".into() });
        let Hunk::Update {
            move_to, chunks, ..
        } = &h[2]
        else {
            panic!()
        };
        assert_eq!(move_to.as_deref(), Some("d.txt"));
        assert_eq!(chunks[0].header.as_deref(), Some("fn main"));
        assert_eq!(chunks[0].old, ["ctx", "old"]);
        assert_eq!(chunks[0].new, ["ctx", "new"]);
        assert!(chunks[0].eof);
        assert!(parse("nope").is_err());
        assert!(parse("*** Begin Patch\n*** End Patch").is_err());
        assert!(parse("*** Begin Patch\nbad\n*** End Patch").is_err());
    }

    #[test]
    fn applies_chunks_with_fuzz() {
        let text = "a\nb\nc\nd\nb\nc\n";
        let chunks = vec![Chunk {
            header: Some("d".into()),
            old: vec!["b".into(), "c  ".into()],
            new: vec!["B".into(), "C".into()],
            eof: false,
        }];
        assert_eq!(apply_chunks(text, &chunks, "f").unwrap(), "a\nb\nc\nd\nB\nC\n");
        let add = vec![Chunk {
            header: None,
            old: vec![],
            new: vec!["z".into()],
            eof: false,
        }];
        assert_eq!(apply_chunks("x\n", &add, "f").unwrap(), "x\nz\n");
        let miss = vec![Chunk {
            header: None,
            old: vec!["nope".into()],
            new: vec![],
            eof: false,
        }];
        assert!(apply_chunks(text, &miss, "f").is_err());
    }
}
