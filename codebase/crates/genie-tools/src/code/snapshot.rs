//! Shadow-git snapshots (04 §10, D49): a private git dir **outside** the user's repo,
//! driven with `GIT_DIR` + `GIT_WORK_TREE`, that records tree snapshots of the working
//! tree. It borrows the user's objects through `objects/info/alternates` and seeds its
//! index from the user's index, so the first snapshot of a big repo hashes only what
//! differs. Untracked files over 2 MiB are skipped and reported. Trees are kept alive by
//! refs (`refs/genie/…`) until their session is deleted; then `gc --prune=now` cleans up.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use super::git::{Git, RepoInfo, nul_split, repo_info};

/// Untracked files larger than this are not captured (04 §10).
pub const MAX_UNTRACKED: u64 = 2 * 1024 * 1024;
/// A working tree with more untracked files than this is not snapshotted at all (the
/// non-git "size/ignore-limited scan").
pub const MAX_UNTRACKED_FILES: usize = 50_000;

/// One snapshot.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Snapshot {
    pub tree: String,
    /// Workspace-relative paths that were too large to capture.
    pub skipped: Vec<String>,
}

/// How a path differs between two trees.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Change {
    Added,
    Modified,
    Deleted,
}

impl Change {
    pub fn as_str(self) -> &'static str {
        match self {
            Change::Added => "added",
            Change::Modified => "modified",
            Change::Deleted => "deleted",
        }
    }
}

/// A shadow repository for one working tree.
pub struct ShadowRepo {
    pub git_dir: PathBuf,
    pub work_tree: PathBuf,
    pub user: Option<RepoInfo>,
    lock: tokio::sync::Mutex<()>,
}

impl ShadowRepo {
    /// The working tree a snapshot of `workspace` covers: the enclosing git repo's top
    /// level, else the folder itself.
    pub async fn work_tree_of(workspace: &Path) -> (PathBuf, Option<RepoInfo>) {
        match repo_info(workspace).await {
            Some(info) => (info.top.clone(), Some(info)),
            None => (workspace.to_path_buf(), None),
        }
    }

    /// Open (initialising on first use) the shadow repo at `git_dir` for `work_tree`.
    pub async fn open(
        git_dir: PathBuf,
        work_tree: PathBuf,
        user: Option<RepoInfo>,
    ) -> Result<ShadowRepo, String> {
        let repo = ShadowRepo {
            git_dir,
            work_tree,
            user,
            lock: tokio::sync::Mutex::new(()),
        };
        if !repo.git_dir.join("HEAD").exists() {
            std::fs::create_dir_all(&repo.git_dir).map_err(|e| e.to_string())?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let _ = std::fs::set_permissions(
                    &repo.git_dir,
                    std::fs::Permissions::from_mode(0o700),
                );
            }
            repo.git().ok(&["init", "-q"]).await?;
            for (k, v) in [
                ("core.autocrlf", "false"),
                ("core.safecrlf", "false"),
                ("core.longpaths", "true"),
                ("core.fsmonitor", "false"),
                ("core.quotepath", "false"),
                ("gc.auto", "0"),
                ("maintenance.auto", "false"),
            ] {
                repo.git().ok(&["config", k, v]).await?;
            }
            if let Some(u) = &repo.user {
                let alt = repo.git_dir.join("objects").join("info");
                std::fs::create_dir_all(&alt).map_err(|e| e.to_string())?;
                std::fs::write(
                    alt.join("alternates"),
                    format!("{}\n", u.common_dir.join("objects").display()),
                )
                .map_err(|e| e.to_string())?;
                // Seed the index: entries whose stat data matches aren't re-hashed.
                let _ = std::fs::copy(u.git_dir.join("index"), repo.git_dir.join("index"));
            }
        }
        Ok(repo)
    }

    /// git with `GIT_DIR` / `GIT_WORK_TREE` set.
    pub fn git(&self) -> Git {
        Git::new(&self.work_tree)
            .with_env("GIT_DIR", self.git_dir.display().to_string())
            .with_env("GIT_WORK_TREE", self.work_tree.display().to_string())
    }

    fn sync_excludes(&self) {
        let Some(u) = &self.user else { return };
        let info = self.git_dir.join("info");
        let _ = std::fs::create_dir_all(&info);
        let user = std::fs::read(u.common_dir.join("info").join("exclude")).unwrap_or_default();
        let _ = std::fs::write(info.join("exclude"), user);
    }

    /// Take a tree snapshot of the working tree.
    pub async fn snapshot(&self) -> Result<Snapshot, String> {
        let _g = self.lock.lock().await;
        self.sync_excludes();
        let git = self.git();
        let untracked = git
            .run(&["ls-files", "--others", "--exclude-standard", "-z"], None)
            .await?;
        if !untracked.ok() {
            return Err(format!("git ls-files failed: {}", untracked.stderr));
        }
        let names = nul_split(&untracked.stdout);
        if names.len() > MAX_UNTRACKED_FILES {
            return Err(format!(
                "{} untracked files; too many to snapshot",
                names.len()
            ));
        }
        let skipped: Vec<String> = names
            .into_iter()
            .filter(|n| {
                std::fs::metadata(self.work_tree.join(n)).is_ok_and(|m| m.len() > MAX_UNTRACKED)
            })
            .collect();
        let mut spec = b".\0".to_vec();
        for s in &skipped {
            spec.extend_from_slice(format!(":(exclude,literal){s}").as_bytes());
            spec.push(0);
        }
        git.ok_with(
            &[
                "add",
                "-A",
                "--ignore-errors",
                "--pathspec-from-file=-",
                "--pathspec-file-nul",
            ],
            &spec,
        )
        .await?;
        let tree = git.ok(&["write-tree"]).await?;
        Ok(Snapshot { tree, skipped })
    }

    /// Paths that differ between two trees.
    pub async fn changes(&self, from: &str, to: &str) -> Result<Vec<(String, Change)>, String> {
        if from == to {
            return Ok(vec![]);
        }
        let o = self
            .git()
            .run(
                &["diff-tree", "-r", "-z", "--no-renames", "--name-status", from, to],
                None,
            )
            .await?;
        if !o.ok() {
            return Err(format!("git diff-tree failed: {}", o.stderr));
        }
        let parts = nul_split(&o.stdout);
        let mut out = Vec::new();
        for pair in parts.chunks(2) {
            if let [status, path] = pair {
                let c = match status.chars().next() {
                    Some('A') => Change::Added,
                    Some('D') => Change::Deleted,
                    _ => Change::Modified,
                };
                out.push((path.clone(), c));
            }
        }
        Ok(out)
    }

    /// The subset of `paths` present in `tree`.
    pub async fn present(&self, tree: &str, paths: &[String]) -> Result<BTreeSet<String>, String> {
        if paths.is_empty() {
            return Ok(BTreeSet::new());
        }
        let o = self
            .git()
            .run(&["ls-tree", "-r", "-z", "--name-only", "--full-tree", tree], None)
            .await?;
        if !o.ok() {
            return Err(format!("git ls-tree failed: {}", o.stderr));
        }
        let want: BTreeSet<&String> = paths.iter().collect();
        Ok(nul_split(&o.stdout)
            .into_iter()
            .filter(|p| want.contains(p))
            .collect())
    }

    /// Put `paths` back to their state in `tree` (absent there → deleted) through a
    /// temporary index; other files are not touched.
    pub async fn restore(&self, tree: &str, paths: &[String]) -> Result<(), String> {
        if paths.is_empty() {
            return Ok(());
        }
        let _g = self.lock.lock().await;
        let present = self.present(tree, paths).await?;
        let idx = self.git_dir.join("index.restore");
        let git = self
            .git()
            .with_env("GIT_INDEX_FILE", idx.display().to_string());
        git.ok(&["read-tree", tree]).await?;
        let mut input = Vec::new();
        for p in paths.iter().filter(|p| present.contains(*p)) {
            input.extend_from_slice(p.as_bytes());
            input.push(0);
        }
        let r = if input.is_empty() {
            Ok(String::new())
        } else {
            git.ok_with(&["checkout-index", "-f", "-z", "--stdin"], &input)
                .await
        };
        let _ = std::fs::remove_file(&idx);
        r?;
        for p in paths.iter().filter(|p| !present.contains(*p)) {
            let full = self.work_tree.join(p);
            if full.is_file() || full.is_symlink() {
                std::fs::remove_file(&full).map_err(|e| format!("can't delete {p}: {e}"))?;
            }
            // Remove folders left empty, up to the working tree.
            let mut dir = full.parent();
            while let Some(d) = dir {
                if d == self.work_tree || std::fs::remove_dir(d).is_err() {
                    break;
                }
                dir = d.parent();
            }
        }
        Ok(())
    }

    /// Keep `object` alive under `refname` (`refs/genie/…`).
    pub async fn keep(&self, refname: &str, object: &str) -> Result<(), String> {
        self.git().ok(&["update-ref", refname, object]).await.map(|_| ())
    }

    /// Delete every ref under `prefix` (e.g. `refs/genie/<session>/`).
    pub async fn drop_refs(&self, prefix: &str) -> Result<usize, String> {
        let list = self
            .git()
            .ok(&["for-each-ref", "--format=%(refname)", prefix])
            .await?;
        let refs: Vec<&str> = list.lines().filter(|l| !l.is_empty()).collect();
        if refs.is_empty() {
            return Ok(0);
        }
        let input: String = refs.iter().map(|r| format!("delete {r}\n")).collect();
        self.git()
            .ok_with(&["update-ref", "--stdin"], input.as_bytes())
            .await?;
        Ok(refs.len())
    }

    /// `git gc --prune=now` (after refs were dropped).
    pub async fn gc(&self) -> Result<(), String> {
        let _g = self.lock.lock().await;
        self.git().ok(&["gc", "--prune=now", "-q"]).await.map(|_| ())
    }

    /// Bytes used by the shadow repository on disk.
    pub fn disk_usage(&self) -> u64 {
        fn walk(p: &Path) -> u64 {
            let Ok(rd) = std::fs::read_dir(p) else {
                return 0;
            };
            rd.flatten()
                .map(|e| match e.metadata() {
                    Ok(m) if m.is_dir() => walk(&e.path()),
                    Ok(m) => m.len(),
                    Err(_) => 0,
                })
                .sum()
        }
        walk(&self.git_dir)
    }
}
