//! Code tools (06 §4.2), shadow-git snapshots (04 §10) and worktrees (06 §15) against
//! real temp folders and real `git` repositories.

#![allow(clippy::unwrap_used, clippy::expect_used)] // test code

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use genie_protocol::ask::Preview;
use genie_protocol::common::SessionKind;
use genie_tools::code::git::Git;
use genie_tools::code::snapshot::{Change, ShadowRepo};
use genie_tools::code::worktree::{self, CreateSpec, Finish};
use genie_tools::code::{self, CodeConfig, LocalLocks};
use genie_tools::shell::FixedDir;
use genie_tools::{
    NullSink, PermissionGate, PermissionRequest, Tool, ToolCtx, ToolEnv, ToolError, ToolOutput,
};
use serde_json::{Value, json};
use tokio_util::sync::CancellationToken;

/// Records every permission request; declines when told to.
#[derive(Default)]
struct Gate {
    seen: Mutex<Vec<PermissionRequest>>,
    decline: bool,
}

#[async_trait]
impl PermissionGate for Gate {
    async fn assert(&self, req: PermissionRequest) -> Result<(), ToolError> {
        self.seen.lock().unwrap().push(req);
        if self.decline {
            Err(ToolError::Declined { message: None })
        } else {
            Ok(())
        }
    }
}

struct T {
    dir: tempfile::TempDir,
    cfg: CodeConfig,
    tools: Vec<Arc<dyn Tool>>,
    gate: Arc<Gate>,
}

fn setup() -> T {
    let dir = tempfile::tempdir().unwrap();
    let root = std::fs::canonicalize(dir.path()).unwrap();
    let cfg = CodeConfig::new(Arc::new(FixedDir(root)), Arc::new(LocalLocks::default()));
    T {
        tools: code::tools(cfg.clone()),
        cfg,
        dir,
        gate: Arc::new(Gate::default()),
    }
}

impl T {
    fn root(&self) -> PathBuf {
        std::fs::canonicalize(self.dir.path()).unwrap()
    }

    fn put(&self, rel: &str, text: &str) {
        let p = self.root().join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, text).unwrap();
    }

    fn get(&self, rel: &str) -> String {
        std::fs::read_to_string(self.root().join(rel)).unwrap()
    }

    async fn call_as(&self, session: &str, name: &str, input: Value) -> Result<String, ToolError> {
        let tool = self
            .tools
            .iter()
            .find(|t| t.name() == name)
            .unwrap()
            .clone();
        let ctx = ToolCtx {
            env: Arc::new(ToolEnv::new(session, SessionKind::Code)),
            turn_id: "trn".into(),
            call_id: "call".into(),
            tool_name: name.into(),
            cancel: CancellationToken::new(),
            permissions: self.gate.clone(),
            events: Arc::new(NullSink),
        };
        tool.execute(input, ctx).await.map(|o| match o {
            ToolOutput::Text(t) => t,
            ToolOutput::Json(v) => v.to_string(),
        })
    }

    async fn call(&self, name: &str, input: Value) -> Result<String, ToolError> {
        self.call_as("s1", name, input).await
    }
}

fn msg(e: ToolError) -> String {
    match e {
        ToolError::Failed { message, .. } => message,
        other => other.to_string(),
    }
}

#[tokio::test]
async fn tools_are_code_only() {
    let t = setup();
    let chat = ToolEnv::new("s", SessionKind::Chat);
    let code_env = ToolEnv::new("s", SessionKind::Code);
    for tool in &t.tools {
        assert!(!tool.availability(&chat).is_available(), "{}", tool.name());
        assert!(
            tool.availability(&code_env).is_available(),
            "{}",
            tool.name()
        );
    }
    let names: Vec<&str> = t.tools.iter().map(|t| t.name()).collect();
    assert_eq!(
        names,
        [
            "read",
            "write",
            "edit",
            "multi_edit",
            "patch",
            "grep",
            "glob",
            "list",
            "todo_write"
        ]
    );
}

#[tokio::test]
async fn read_numbers_lines_pages_and_asks_with_the_path() {
    let t = setup();
    let text: String = (1..=30).map(|i| format!("line {i}\n")).collect();
    t.put("src/a.txt", &text);
    let out = t
        .call(
            "read",
            json!({"file_path": "src/a.txt", "offset": 3, "limit": 2}),
        )
        .await
        .unwrap();
    assert_eq!(
        out,
        "     3\tline 3\n     4\tline 4\n\n[showing lines 3–4 of 30; read with offset=5 to continue]"
    );
    let first = t.gate.seen.lock().unwrap()[0].resource.clone();
    assert_eq!(
        first.as_deref(),
        Some(t.root().join("src/a.txt").to_str().unwrap())
    );
    let e = msg(t
        .call("read", json!({"path": "src/b.txt"}))
        .await
        .unwrap_err());
    assert!(e.contains("does not exist"), "{e}");
    let e = msg(t.call("read", json!({"path": "src"})).await.unwrap_err());
    assert!(e.contains("directory"), "{e}");
}

#[tokio::test]
async fn write_creates_and_overwrites_only_after_read() {
    let t = setup();
    let out = t
        .call(
            "write",
            json!({"path": "new/dir/f.txt", "content": "hello\n"}),
        )
        .await
        .unwrap();
    assert_eq!(out, "Created new/dir/f.txt (1 lines).");
    assert_eq!(t.get("new/dir/f.txt"), "hello\n");
    // The writer may overwrite what it just wrote.
    t.call(
        "write",
        json!({"path": "new/dir/f.txt", "content": "again\n"}),
    )
    .await
    .unwrap();
    // A file never read can't be overwritten.
    t.put("other.txt", "x\n");
    let e = msg(t
        .call("write", json!({"path": "other.txt", "content": "y"}))
        .await
        .unwrap_err());
    assert!(e.contains("has not been read"), "{e}");
    // Stale: changed after the read.
    t.call("read", json!({"path": "other.txt"})).await.unwrap();
    t.put("other.txt", "changed\n");
    let e = msg(t
        .call("write", json!({"path": "other.txt", "content": "y"}))
        .await
        .unwrap_err());
    assert!(e.contains("changed since it was last read"), "{e}");
    assert_eq!(t.get("other.txt"), "changed\n");
    // Another session's read doesn't count.
    let e = msg(t
        .call_as(
            "s2",
            "write",
            json!({"path": "new/dir/f.txt", "content": "z"}),
        )
        .await
        .unwrap_err());
    assert!(e.contains("has not been read"), "{e}");
}

#[tokio::test]
async fn edit_matches_asks_after_the_match_and_keeps_crlf_and_bom() {
    let t = setup();
    t.put(
        "a.rs",
        "\u{feff}fn main() {\r\n    println!(\"hi\");\r\n}\r\n",
    );
    t.call("read", json!({"path": "a.rs"})).await.unwrap();
    // A miss never asks.
    let before = t.gate.seen.lock().unwrap().len();
    let e = msg(t
        .call(
            "edit",
            json!({"path": "a.rs", "old_string": "println!(\"hello\");", "new_string": "x"}),
        )
        .await
        .unwrap_err());
    assert!(
        e.contains("not found") && e.contains("Closest lines"),
        "{e}"
    );
    assert_eq!(t.gate.seen.lock().unwrap().len(), before);
    // A hit asks once with a diff, then writes with CRLF + BOM kept.
    let out = t
        .call(
            "edit",
            json!({"path": "a.rs", "old_str": "println!(\"hi\");", "new_str": "println!(\"bye\");\nreturn;"}),
        )
        .await
        .unwrap();
    assert!(out.starts_with("Edited a.rs (+2 −1)."), "{out}");
    let last = t.gate.seen.lock().unwrap().last().unwrap().preview.clone();
    let Some(Preview::Diff { path, unified }) = last else {
        panic!("no diff preview")
    };
    assert_eq!(path, "a.rs");
    assert!(unified.contains("-    println!(\"hi\");\n+    println!(\"bye\");\n+return;"));
    assert_eq!(
        t.get("a.rs"),
        "\u{feff}fn main() {\r\n    println!(\"bye\");\r\nreturn;\r\n}\r\n"
    );
    // Consecutive edits work without a re-read.
    t.call(
        "edit",
        json!({"path": "a.rs", "old_string": "return;", "new_string": "    return;"}),
    )
    .await
    .unwrap();
}

#[tokio::test]
async fn edit_uniqueness_replace_all_tiers_and_decline() {
    let t = setup();
    t.put("f.txt", "a = 1\na = 1\nsay \u{201C}hi\u{201D}\n");
    t.call("read", json!({"path": "f.txt"})).await.unwrap();
    let e = msg(t
        .call(
            "edit",
            json!({"path": "f.txt", "old_string": "a = 1", "new_string": "b"}),
        )
        .await
        .unwrap_err());
    assert!(e.contains("matches 2 places"), "{e}");
    let out = t
        .call(
            "edit",
            json!({"path": "f.txt", "old_string": "say \"hi\"", "new_string": "say 'yo'"}),
        )
        .await
        .unwrap();
    assert!(out.contains("normalising Unicode punctuation"), "{out}");
    t.call(
        "edit",
        json!({"path": "f.txt", "old_string": "a = 1", "new_string": "b = 2", "replace_all": true}),
    )
    .await
    .unwrap();
    assert_eq!(t.get("f.txt"), "b = 2\nb = 2\nsay 'yo'\n");
    // Declined: the file is untouched.
    let t2 = T {
        gate: Arc::new(Gate {
            decline: true,
            ..Gate::default()
        }),
        ..setup()
    };
    t2.put("g.txt", "keep\n");
    t2.call("read", json!({"path": "g.txt"})).await.unwrap_err(); // read asks too
    t2.cfg.reads.record(
        "s1",
        &t2.root().join("g.txt"),
        code::Stamp::of(b"keep\n", None),
    );
    let e = t2
        .call(
            "edit",
            json!({"path": "g.txt", "old_string": "keep", "new_string": "gone"}),
        )
        .await
        .unwrap_err();
    assert!(matches!(e, ToolError::Declined { .. }));
    assert_eq!(t2.get("g.txt"), "keep\n");
}

#[tokio::test]
async fn multi_edit_is_atomic() {
    let t = setup();
    t.put("m.txt", "one\ntwo\nthree\n");
    t.call("read", json!({"path": "m.txt"})).await.unwrap();
    let e = msg(t
        .call(
            "multi_edit",
            json!({"path": "m.txt", "edits": [
                {"old_string": "one", "new_string": "1"},
                {"old_string": "missing", "new_string": "x"}
            ]}),
        )
        .await
        .unwrap_err());
    assert!(e.contains("edits[1].old_string"), "{e}");
    assert_eq!(t.get("m.txt"), "one\ntwo\nthree\n");
    t.call(
        "multi_edit",
        json!({"path": "m.txt", "edits": [
            {"old_string": "one", "new_string": "1"},
            {"old_string": "1\ntwo", "new_string": "1\n2"}
        ]}),
    )
    .await
    .unwrap();
    assert_eq!(t.get("m.txt"), "1\n2\nthree\n");
}

#[tokio::test]
async fn patch_applies_all_or_nothing() {
    let t = setup();
    t.put("keep.txt", "alpha\nbeta\ngamma\n");
    t.put("gone.txt", "bye\n");
    let bad = "*** Begin Patch\n*** Add File: added.txt\n+new\n*** Update File: keep.txt\n@@\n-nope\n+x\n*** End Patch";
    let e = msg(t.call("patch", json!({"patch": bad})).await.unwrap_err());
    assert!(e.contains("doesn't match"), "{e}");
    assert!(!t.root().join("added.txt").exists());
    let good = "*** Begin Patch\n*** Add File: added.txt\n+new\n*** Delete File: gone.txt\n\
                *** Update File: keep.txt\n*** Move to: moved.txt\n@@\n alpha\n-beta\n+BETA\n*** End Patch";
    let out = t.call("patch", json!({"patch": good})).await.unwrap();
    assert!(
        out.contains("A added.txt") && out.contains("D gone.txt"),
        "{out}"
    );
    assert_eq!(t.get("added.txt"), "new\n");
    assert_eq!(t.get("moved.txt"), "alpha\nBETA\ngamma\n");
    assert!(!t.root().join("gone.txt").exists());
    assert!(!t.root().join("keep.txt").exists());
    // One ask per file.
    assert_eq!(t.gate.seen.lock().unwrap().len(), 4);
}

#[tokio::test]
async fn grep_glob_list_respect_ignore_rules() {
    let t = setup();
    // `.gitignore` applies inside a git repo, like rg.
    Git::new(t.root()).ok(&["init", "-q"]).await.unwrap();
    t.put(".gitignore", "target/\n");
    t.put("src/lib.rs", "fn alpha() {}\nfn beta() {}\n");
    t.put("src/main.rs", "fn main() { alpha(); }\n");
    t.put("target/out.rs", "fn alpha() {}\n");
    t.put(".hidden/x.rs", "fn alpha() {}\n");
    let out = t.call("grep", json!({"pattern": "alpha"})).await.unwrap();
    assert_eq!(
        out,
        "src/lib.rs:1:fn alpha() {}\nsrc/main.rs:1:fn main() { alpha(); }\n[2 matching lines]"
    );
    let out = t
        .call(
            "grep",
            json!({"pattern": "ALPHA", "case_insensitive": true, "output_mode": "files_with_matches", "glob": "lib.rs"}),
        )
        .await
        .unwrap();
    assert_eq!(out, "src/lib.rs");
    let out = t
        .call("grep", json!({"pattern": "beta", "context": 1}))
        .await
        .unwrap();
    assert_eq!(out, "src/lib.rs-1-fn alpha() {}\nsrc/lib.rs:2:fn beta() {}");
    assert_eq!(
        t.call("grep", json!({"pattern": "zzz"})).await.unwrap(),
        "No matches."
    );
    assert!(matches!(
        t.call("grep", json!({"pattern": "("})).await.unwrap_err(),
        ToolError::InvalidInput { .. }
    ));
    let out = t.call("glob", json!({"pattern": "*.rs"})).await.unwrap();
    let mut files: Vec<&str> = out.lines().collect();
    files.sort();
    assert_eq!(files, ["src/lib.rs", "src/main.rs"]);
    let out = t
        .call("glob", json!({"pattern": "src/m*.rs"}))
        .await
        .unwrap();
    assert_eq!(out, "src/main.rs");
    let out = t.call("list", json!({})).await.unwrap();
    assert_eq!(out, "./\n  src/\n    lib.rs\n    main.rs");
    let out = t
        .call("list", json!({"path": "src", "ignore": ["main.rs"]}))
        .await
        .unwrap();
    assert_eq!(out, "src/\n  lib.rs");
}

#[tokio::test]
async fn grep_caps_results() {
    let t = setup();
    let text: String = (0..150).map(|i| format!("hit {i}\n")).collect();
    t.put("many.txt", &text);
    let out = t.call("grep", json!({"pattern": "hit"})).await.unwrap();
    assert_eq!(
        out.lines().filter(|l| l.starts_with("many.txt:")).count(),
        100
    );
    assert!(out.ends_with("[results cut at 100; narrow the path or pattern]"));
}

#[tokio::test]
async fn todo_write_replaces_the_list() {
    let t = setup();
    let out = t
        .call(
            "todo_write",
            json!({"todos": [
                {"content": "a", "status": "completed"},
                {"content": "b", "status": "in_progress"},
                {"content": "c", "status": "pending"}
            ]}),
        )
        .await
        .unwrap();
    assert_eq!(out, "Todo list updated: 1/3 done.");
    assert_eq!(t.cfg.todos.get("s1").len(), 3);
    t.call("todo_write", json!({"todos": []})).await.unwrap();
    assert!(t.cfg.todos.get("s1").is_empty());
}

#[tokio::test]
async fn edits_to_one_path_are_serialised() {
    let t = Arc::new(setup());
    t.put("c.txt", "0\n");
    t.call("read", json!({"path": "c.txt"})).await.unwrap();
    let mut handles = vec![];
    for i in 1..=8 {
        let t = t.clone();
        handles.push(tokio::spawn(async move {
            // Each call re-reads (as the model would) and appends one line.
            loop {
                let cur = std::fs::read_to_string(t.root().join("c.txt")).unwrap();
                t.cfg.reads.record(
                    "s1",
                    &t.root().join("c.txt"),
                    code::Stamp::of(cur.as_bytes(), None),
                );
                let last = cur.lines().last().unwrap().to_string();
                let r = t
                    .call(
                        "edit",
                        json!({"path": "c.txt", "old_string": format!("{last}\n"), "new_string": format!("{last}\n{i}-{last}\n")}),
                    )
                    .await;
                if r.is_ok() {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(1)).await;
            }
        }));
    }
    for h in handles {
        h.await.unwrap();
    }
    assert_eq!(t.get("c.txt").lines().count(), 9);
}

// ------------------------------------------------------------------ git

async fn repo(dir: &Path) {
    let g = Git::new(dir);
    g.ok(&["init", "-q", "-b", "main"]).await.unwrap();
    g.ok(&["config", "user.email", "t@example.com"])
        .await
        .unwrap();
    g.ok(&["config", "user.name", "T"]).await.unwrap();
    g.ok(&["config", "commit.gpgsign", "false"]).await.unwrap();
    std::fs::write(dir.join("a.txt"), "a\n").unwrap();
    std::fs::write(dir.join(".gitignore"), "ignored/\n").unwrap();
    g.ok(&["add", "."]).await.unwrap();
    g.ok(&["commit", "-q", "-m", "init"]).await.unwrap();
}

#[tokio::test]
async fn shadow_snapshots_capture_changes_skip_big_files_and_restore() {
    let d = tempfile::tempdir().unwrap();
    let work = std::fs::canonicalize(d.path()).unwrap().join("repo");
    std::fs::create_dir(&work).unwrap();
    repo(&work).await;
    let (top, info) = ShadowRepo::work_tree_of(&work).await;
    assert_eq!(top, work);
    let shadow = ShadowRepo::open(d.path().join("shadow"), top, info)
        .await
        .unwrap();
    assert!(d.path().join("shadow/objects/info/alternates").exists());
    let s1 = shadow.snapshot().await.unwrap();
    std::fs::write(work.join("a.txt"), "changed\n").unwrap();
    std::fs::write(work.join("new.txt"), "n\n").unwrap();
    std::fs::create_dir(work.join("ignored")).unwrap();
    std::fs::write(work.join("ignored/x"), "x").unwrap();
    std::fs::write(work.join("big.bin"), vec![1u8; 3 * 1024 * 1024]).unwrap();
    let s2 = shadow.snapshot().await.unwrap();
    assert_eq!(s2.skipped, ["big.bin"]);
    let mut ch = shadow.changes(&s1.tree, &s2.tree).await.unwrap();
    ch.sort();
    assert_eq!(
        ch,
        [
            ("a.txt".to_string(), Change::Modified),
            ("new.txt".to_string(), Change::Added)
        ]
    );
    // The user's repo is untouched (no new refs, nothing staged).
    let st = Git::new(&work)
        .ok(&["status", "--porcelain"])
        .await
        .unwrap();
    assert!(st.contains(" M a.txt") && st.contains("?? new.txt"), "{st}");
    // Restore only the named paths.
    shadow
        .restore(&s1.tree, &["a.txt".into(), "new.txt".into()])
        .await
        .unwrap();
    assert_eq!(std::fs::read_to_string(work.join("a.txt")).unwrap(), "a\n");
    assert!(!work.join("new.txt").exists());
    assert!(work.join("big.bin").exists());
    // Refs keep trees alive until dropped.
    shadow
        .keep("refs/genie/s1/1/after", &s2.tree)
        .await
        .unwrap();
    assert_eq!(shadow.drop_refs("refs/genie/s1/").await.unwrap(), 1);
    shadow.gc().await.unwrap();
}

#[tokio::test]
async fn shadow_works_in_a_plain_folder() {
    let d = tempfile::tempdir().unwrap();
    let work = std::fs::canonicalize(d.path()).unwrap().join("plain");
    std::fs::create_dir(&work).unwrap();
    std::fs::write(work.join("f"), "1").unwrap();
    let (top, info) = ShadowRepo::work_tree_of(&work).await;
    assert!(info.is_none());
    let shadow = ShadowRepo::open(d.path().join("sh"), top, None)
        .await
        .unwrap();
    let a = shadow.snapshot().await.unwrap();
    std::fs::write(work.join("f"), "2").unwrap();
    let b = shadow.snapshot().await.unwrap();
    assert_eq!(
        shadow.changes(&a.tree, &b.tree).await.unwrap(),
        [("f".to_string(), Change::Modified)]
    );
}

#[tokio::test]
async fn worktree_create_merge_and_discard() {
    let d = tempfile::tempdir().unwrap();
    let base = std::fs::canonicalize(d.path()).unwrap();
    let work = base.join("proj");
    std::fs::create_dir(&work).unwrap();
    repo(&work).await;
    let w = worktree::create(&CreateSpec::new(&work, "Fix Bug"))
        .await
        .unwrap();
    assert_eq!(w.path, base.join("proj.genie-worktrees/fix-bug"));
    assert_eq!(w.branch, "genie/fix-bug");
    assert_eq!(w.base_branch.as_deref(), Some("main"));
    assert!(w.path.join("a.txt").exists());
    // A second one with the same name gets a suffix.
    let w2 = worktree::create(&CreateSpec::new(&work, "Fix Bug"))
        .await
        .unwrap();
    assert_eq!(w2.slug, "fix-bug-2");

    // Merge refuses a dirty worktree, then merges the commit.
    std::fs::write(w.path.join("a.txt"), "from worktree\n").unwrap();
    let e = worktree::finish(&w, Finish::Merge).await.unwrap_err();
    assert!(e.contains("uncommitted"), "{e}");
    let g = Git::new(&w.path);
    g.ok(&["commit", "-qam", "change"]).await.unwrap();
    let done = worktree::finish(&w, Finish::Merge).await.unwrap();
    assert!(done.removed && done.branch_deleted && done.merge_commit.is_some());
    assert_eq!(
        std::fs::read_to_string(work.join("a.txt")).unwrap(),
        "from worktree\n"
    );
    assert!(!w.path.exists());

    // PR: instructions only, nothing removed.
    let pr = worktree::finish(&w2, Finish::Pr).await.unwrap();
    assert!(
        pr.message.contains("push -u origin genie/fix-bug-2"),
        "{}",
        pr.message
    );
    assert!(w2.path.exists());
    // Discard keeps a branch with unpushed commits.
    std::fs::write(w2.path.join("b.txt"), "b\n").unwrap();
    let g2 = Git::new(&w2.path);
    g2.ok(&["add", "."]).await.unwrap();
    g2.ok(&["commit", "-qm", "b"]).await.unwrap();
    let gone = worktree::finish(&w2, Finish::Discard).await.unwrap();
    assert!(gone.removed && !gone.branch_deleted, "{gone:?}");
    assert!(!w2.path.exists());
    // A clean, empty one is fully discarded.
    let w3 = worktree::create(&CreateSpec::new(&work, "tmp"))
        .await
        .unwrap();
    let gone = worktree::finish(&w3, Finish::Discard).await.unwrap();
    assert!(gone.branch_deleted);
    // Non-git folders can't have worktrees.
    let plain = base.join("plain");
    std::fs::create_dir(&plain).unwrap();
    assert!(
        worktree::create(&CreateSpec::new(&plain, "x"))
            .await
            .is_err()
    );
}
