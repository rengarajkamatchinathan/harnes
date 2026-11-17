//! No `tokio::spawn` outside a `TaskTracker` (01 §2.1 rule 4, §6).
//!
//! Every task must hang off the cancellation tree: use `genie_core::TaskScope::spawn`
//! (or a `TaskTracker` in crates below core). Test code is exempt.

use std::path::Path;

const BANNED: &[&str] = &[
    "tokio::spawn(",
    "tokio::task::spawn(",
    "task::spawn(",
    "tokio::task::spawn_local(",
];

pub fn check(root: &Path) -> Result<String, Vec<String>> {
    let mut errors = vec![];
    let mut files = 0;
    for dir in ["crates", "apps"] {
        walk(&root.join(dir), &mut |path| {
            // Integration tests (`tests/`) and build output are exempt.
            let s = path.to_string_lossy().replace('\\', "/");
            if !s.contains("/src/") || s.contains("/target/") || s.contains("/node_modules/") {
                return;
            }
            files += 1;
            let Ok(text) = std::fs::read_to_string(path) else {
                return;
            };
            for (i, line) in text.lines().enumerate() {
                let t = line.trim_start();
                if t.starts_with("#[cfg(test)]") {
                    break; // unit-test modules sit at the end of the file by convention
                }
                if t.starts_with("//") {
                    continue;
                }
                if BANNED.iter().any(|b| t.contains(b)) {
                    errors.push(format!(
                        "{}:{}: spawn on a TaskScope/TaskTracker instead of `{}`",
                        path.display(),
                        i + 1,
                        t.trim()
                    ));
                }
            }
        });
    }
    if errors.is_empty() {
        Ok(format!("no untracked spawns in {files} source files"))
    } else {
        Err(errors)
    }
}

fn walk(dir: &Path, f: &mut dyn FnMut(&Path)) {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    for e in rd.flatten() {
        let p = e.path();
        if p.is_dir() {
            if p.file_name()
                .is_some_and(|n| n == "target" || n == "node_modules")
            {
                continue;
            }
            walk(&p, f);
        } else if p.extension().is_some_and(|x| x == "rs") {
            f(&p);
        }
    }
}
