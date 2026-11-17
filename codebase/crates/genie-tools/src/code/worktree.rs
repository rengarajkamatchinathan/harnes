//! Git worktrees for parallel Code sessions (06 §15, D50): a sibling folder
//! `../<repo>.genie-worktrees/<slug>/` on a named branch `genie/<slug>` from the current
//! HEAD, and the four ways to finish (merge / PR / keep / discard). Network-free: "PR"
//! returns the push + `gh pr create` commands instead of running them.

use std::path::{Path, PathBuf};

use super::git::{Git, repo_info};

/// A worktree Genie created.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorktreeInfo {
    pub slug: String,
    /// The worktree's root folder.
    pub path: PathBuf,
    /// Where the session works: `path`, or the matching subfolder when the session was
    /// started in a subfolder of the repository.
    pub workspace: PathBuf,
    pub branch: String,
    /// The commit the branch started from.
    pub base_ref: String,
    /// The branch checked out in the main checkout at creation (merge target).
    pub base_branch: Option<String>,
    /// The main checkout's root.
    pub main: PathBuf,
}

/// Options for [`create`].
#[derive(Debug, Clone)]
pub struct CreateSpec {
    /// The folder the session was started in (inside the repository).
    pub workspace: PathBuf,
    /// Name to derive the slug from (`--worktree <name>`, session title …).
    pub name: String,
    /// `worktrees.dir`; default `../<repo>.genie-worktrees`.
    pub dir: Option<PathBuf>,
    /// `worktrees.branchPrefix`; default `genie/`.
    pub branch_prefix: String,
}

impl CreateSpec {
    pub fn new(workspace: impl Into<PathBuf>, name: impl Into<String>) -> CreateSpec {
        CreateSpec {
            workspace: workspace.into(),
            name: name.into(),
            dir: None,
            branch_prefix: "genie/".into(),
        }
    }
}

/// Lowercase a-z0-9 and dashes, at most 40 chars; `session` when nothing is left.
pub fn slugify(s: &str) -> String {
    let mut out = String::new();
    for c in s.chars() {
        if c.is_ascii_alphanumeric() {
            out.push(c.to_ascii_lowercase());
        } else if !out.ends_with('-') && !out.is_empty() {
            out.push('-');
        }
        if out.len() >= 40 {
            break;
        }
    }
    let out = out.trim_matches('-').to_string();
    if out.is_empty() {
        "session".into()
    } else {
        out
    }
}

/// Whether `dir` is inside a git working tree.
pub async fn is_repo(dir: &Path) -> bool {
    repo_info(dir).await.is_some()
}

/// Create a worktree for a new session.
pub async fn create(spec: &CreateSpec) -> Result<WorktreeInfo, String> {
    let info = repo_info(&spec.workspace)
        .await
        .ok_or("this folder is not a git repository, so it can't have worktrees")?;
    let top = info.top.clone();
    let git = Git::new(&top);
    let head = git
        .ok(&["rev-parse", "--verify", "HEAD"])
        .await
        .map_err(|_| "the repository has no commits yet; commit once before using worktrees")?;
    let base_branch = git
        .run(&["symbolic-ref", "--short", "-q", "HEAD"], None)
        .await
        .ok()
        .filter(|o| o.ok())
        .map(|o| o.text().trim().to_string())
        .filter(|s| !s.is_empty());
    let name = top
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "repo".into());
    let dir = match &spec.dir {
        Some(d) => d.clone(),
        None => top
            .parent()
            .unwrap_or(&top)
            .join(format!("{name}.genie-worktrees")),
    };
    let base = slugify(&spec.name);
    let mut n = 1;
    let (slug, path, branch) = loop {
        let slug = if n == 1 {
            base.clone()
        } else {
            format!("{base}-{n}")
        };
        let path = dir.join(&slug);
        let branch = format!("{}{slug}", spec.branch_prefix);
        let taken = git
            .run(
                &[
                    "rev-parse",
                    "--verify",
                    "--quiet",
                    &format!("refs/heads/{branch}"),
                ],
                None,
            )
            .await
            .is_ok_and(|o| o.ok());
        if !path.exists() && !taken {
            break (slug, path, branch);
        }
        n += 1;
        if n > 1000 {
            return Err("no free worktree name".into());
        }
    };
    std::fs::create_dir_all(&dir).map_err(|e| format!("can't create {}: {e}", dir.display()))?;
    git.ok(&[
        "worktree",
        "add",
        "-q",
        "-b",
        &branch,
        &path.display().to_string(),
        &head,
    ])
    .await?;
    let path = std::fs::canonicalize(&path).unwrap_or(path);
    let rel = std::fs::canonicalize(&spec.workspace)
        .ok()
        .and_then(|w| w.strip_prefix(&top).ok().map(Path::to_path_buf))
        .unwrap_or_default();
    Ok(WorktreeInfo {
        slug,
        workspace: path.join(rel),
        path,
        branch,
        base_ref: head,
        base_branch,
        main: top,
    })
}

/// No uncommitted changes (untracked files count as changes).
pub async fn is_clean(path: &Path) -> Result<bool, String> {
    let s = Git::new(path).ok(&["status", "--porcelain"]).await?;
    Ok(s.trim().is_empty())
}

/// Commits on `branch` that are neither in `base` nor on any remote.
pub async fn unpushed(main: &Path, branch: &str, base: &str) -> Result<u64, String> {
    let n = Git::new(main)
        .ok(&["rev-list", "--count", branch, "--not", base, "--remotes"])
        .await?;
    Ok(n.trim().parse().unwrap_or(0))
}

/// How to finish a worktree session.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Finish {
    Merge,
    Pr,
    Keep,
    Discard,
}

/// What [`finish`] did.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Finished {
    pub removed: bool,
    pub branch_deleted: bool,
    pub merge_commit: Option<String>,
    /// What happened, for the user (PR instructions, why a branch was kept …).
    pub message: String,
}

async fn remove(w: &WorktreeInfo, force: bool) -> Result<(), String> {
    let git = Git::new(&w.main);
    let p = w.path.display().to_string();
    let mut args = vec!["worktree", "remove"];
    if force {
        args.push("--force");
    }
    args.push(&p);
    git.ok(&args).await?;
    let _ = git.ok(&["worktree", "prune"]).await;
    Ok(())
}

pub async fn finish(w: &WorktreeInfo, how: Finish) -> Result<Finished, String> {
    let git = Git::new(&w.main);
    match how {
        Finish::Keep => Ok(Finished {
            message: format!(
                "Kept the worktree at {} on branch {}.",
                w.path.display(),
                w.branch
            ),
            ..Finished::default()
        }),
        Finish::Pr => {
            let dirty = !is_clean(&w.path).await?;
            let base = w.base_branch.clone().unwrap_or_else(|| "main".into());
            let mut message = String::new();
            if dirty {
                message.push_str("The worktree has uncommitted changes; commit them first.\n");
            }
            message.push_str(&format!(
                "To open a pull request, run:\n  git -C {} push -u origin {}\n  gh pr create \
                 --head {} --base {base} --fill\n(or use your Git host's web page). The \
                 worktree is kept until you merge or discard it.",
                w.path.display(),
                w.branch,
                w.branch
            ));
            Ok(Finished {
                message,
                ..Finished::default()
            })
        }
        Finish::Merge => {
            if !is_clean(&w.path).await? {
                return Err(
                    "the worktree has uncommitted changes; commit them (git.commit) before \
                     merging, or discard the worktree"
                        .into(),
                );
            }
            let base = w
                .base_branch
                .clone()
                .ok_or("the main checkout was on a detached HEAD; merge by hand")?;
            let current = git
                .run(&["symbolic-ref", "--short", "-q", "HEAD"], None)
                .await?
                .text()
                .trim()
                .to_string();
            if current != base {
                return Err(format!(
                    "the main checkout is on `{current}`, not `{base}`; switch back to merge"
                ));
            }
            let ahead = Git::new(&w.main)
                .ok(&["rev-list", "--count", &format!("{base}..{}", w.branch)])
                .await?;
            let mut merge_commit = None;
            if ahead.trim() != "0" {
                let msg = format!("Merge {} (Genie worktree)", w.branch);
                let o = git
                    .run(
                        &["merge", "--no-ff", "--no-edit", "-m", &msg, &w.branch],
                        None,
                    )
                    .await?;
                if !o.ok() {
                    let _ = git.run(&["merge", "--abort"], None).await;
                    let why = if o.stderr.is_empty() {
                        o.text()
                    } else {
                        o.stderr.clone()
                    };
                    return Err(format!(
                        "merging {} into {base} failed and was undone: {}",
                        w.branch,
                        why.trim()
                    ));
                }
                merge_commit = Some(git.ok(&["rev-parse", "HEAD"]).await?);
            }
            remove(w, false).await?;
            let deleted = git.ok(&["branch", "-d", &w.branch]).await.is_ok();
            Ok(Finished {
                removed: true,
                branch_deleted: deleted,
                message: match &merge_commit {
                    Some(c) => format!(
                        "Merged {} into {base} ({}).",
                        w.branch,
                        &c[..c.len().min(12)]
                    ),
                    None => format!("{} had no new commits; nothing to merge.", w.branch),
                },
                merge_commit,
            })
        }
        Finish::Discard => {
            remove(w, true).await?;
            let base = w.base_branch.clone().unwrap_or_else(|| w.base_ref.clone());
            let n = unpushed(&w.main, &w.branch, &base).await.unwrap_or(1);
            if n == 0 {
                let deleted = git.ok(&["branch", "-D", &w.branch]).await.is_ok();
                Ok(Finished {
                    removed: true,
                    branch_deleted: deleted,
                    message: format!("Discarded the worktree and branch {}.", w.branch),
                    merge_commit: None,
                })
            } else {
                Ok(Finished {
                    removed: true,
                    branch_deleted: false,
                    message: format!(
                        "Discarded the worktree. Kept branch {}: it has {n} commit(s) that are \
                         neither merged nor pushed (delete it with `git branch -D {}`).",
                        w.branch, w.branch
                    ),
                    merge_commit: None,
                })
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slugs() {
        assert_eq!(slugify("Fix the Login bug!"), "fix-the-login-bug");
        assert_eq!(slugify("---"), "session");
        assert_eq!(slugify("a/b c"), "a-b-c");
        assert!(slugify(&"x".repeat(100)).len() <= 40);
    }
}
