//! Settings / policy file watcher (D23 hot reload; ticket 08: "the daemon watches files,
//! including ones that don't exist yet, with a 100 ms debounce").
//!
//! [`PathWatcher`] watches the deepest existing directory above each target file
//! (non-recursively) with the OS watcher (FSEvents, inotify, ReadDirectoryChangesW via
//! `notify`) and calls `on_change` for events that touch a target or one of its missing
//! ancestors. Debouncing and re-reading are the caller's job: the daemon waits 100 ms
//! after the last signal, refreshes (content-hash suppressed), then calls
//! [`PathWatcher::set_paths`] again so newly created directories are watched.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, RwLock};

use notify::{RecommendedWatcher, RecursiveMode, Watcher};

/// The deepest existing ancestor directory of `file` (its parent when that exists),
/// canonicalised, plus the file path rebuilt on top of it.
fn anchor(file: &Path) -> Option<(PathBuf, PathBuf)> {
    let mut missing: Vec<&std::ffi::OsStr> = vec![file.file_name()?];
    let mut dir = file.parent()?;
    loop {
        if dir.is_dir() {
            let canon = std::fs::canonicalize(dir).unwrap_or_else(|_| dir.to_path_buf());
            let mut target = canon.clone();
            for part in missing.iter().rev() {
                target.push(part);
            }
            return Some((canon, target));
        }
        missing.push(dir.file_name()?);
        dir = dir.parent()?;
    }
}

/// Directories to watch for `files` (deepest existing ancestors).
pub fn watch_dirs(files: &[PathBuf]) -> BTreeSet<PathBuf> {
    files.iter().filter_map(|f| anchor(f)).map(|(d, _)| d).collect()
}

fn relevant(event_path: &Path, targets: &BTreeSet<PathBuf>) -> bool {
    let p = std::fs::canonicalize(event_path).unwrap_or_else(|_| {
        // Deleted paths can't be canonicalised; canonicalise the parent instead.
        match (event_path.parent(), event_path.file_name()) {
            (Some(d), Some(n)) => std::fs::canonicalize(d)
                .map(|d| d.join(n))
                .unwrap_or_else(|_| event_path.to_path_buf()),
            _ => event_path.to_path_buf(),
        }
    });
    targets.iter().any(|t| *t == p || t.starts_with(&p))
}

struct Inner {
    watcher: RecommendedWatcher,
    dirs: BTreeSet<PathBuf>,
}

/// An OS file watcher over a changing set of target files.
pub struct PathWatcher {
    inner: Mutex<Inner>,
    targets: Arc<RwLock<BTreeSet<PathBuf>>>,
}

impl std::fmt::Debug for PathWatcher {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PathWatcher").finish_non_exhaustive()
    }
}

impl PathWatcher {
    /// `on_change` runs on the watcher's thread; keep it cheap (send a signal).
    pub fn new(on_change: impl Fn() + Send + Sync + 'static) -> Result<PathWatcher, String> {
        let targets: Arc<RwLock<BTreeSet<PathBuf>>> = Arc::default();
        let t = targets.clone();
        let watcher = notify::recommended_watcher(move |res: notify::Result<notify::Event>| {
            let fire = match res {
                Ok(ev) => {
                    if matches!(ev.kind, notify::EventKind::Access(_)) {
                        false
                    } else if ev.paths.is_empty() {
                        true // rescan / overflow
                    } else {
                        let g = t.read().unwrap_or_else(|e| e.into_inner());
                        ev.paths.iter().any(|p| relevant(p, &g))
                    }
                }
                Err(_) => true,
            };
            if fire {
                on_change();
            }
        })
        .map_err(|e| e.to_string())?;
        Ok(PathWatcher {
            inner: Mutex::new(Inner {
                watcher,
                dirs: BTreeSet::new(),
            }),
            targets,
        })
    }

    /// Replace the watched files. Returns the directories that couldn't be watched.
    pub fn set_paths(&self, files: &[PathBuf]) -> Vec<(PathBuf, String)> {
        let anchors: Vec<(PathBuf, PathBuf)> = files.iter().filter_map(|f| anchor(f)).collect();
        *self.targets.write().unwrap_or_else(|e| e.into_inner()) =
            anchors.iter().map(|(_, t)| t.clone()).collect();
        let want: BTreeSet<PathBuf> = anchors.into_iter().map(|(d, _)| d).collect();
        let mut g = self.inner.lock().unwrap_or_else(|e| e.into_inner());
        let old: Vec<PathBuf> = g.dirs.difference(&want).cloned().collect();
        for d in old {
            let _ = g.watcher.unwatch(&d);
            g.dirs.remove(&d);
        }
        let mut failed = Vec::new();
        for d in want {
            if g.dirs.contains(&d) {
                continue;
            }
            match g.watcher.watch(&d, RecursiveMode::NonRecursive) {
                Ok(()) => {
                    g.dirs.insert(d);
                }
                Err(e) => failed.push((d, e.to_string())),
            }
        }
        failed
    }

    /// Directories currently watched.
    pub fn dirs(&self) -> Vec<PathBuf> {
        self.inner
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .dirs
            .iter()
            .cloned()
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;
    use std::time::Duration;

    #[test]
    fn anchors_missing_files_at_the_deepest_existing_dir() {
        let dir = tempfile::tempdir().unwrap();
        let root = std::fs::canonicalize(dir.path()).unwrap();
        let f = dir.path().join("proj/.genie/settings.json");
        let dirs = watch_dirs(std::slice::from_ref(&f));
        assert_eq!(dirs.into_iter().collect::<Vec<_>>(), vec![root.clone()]);
        std::fs::create_dir_all(dir.path().join("proj/.genie")).unwrap();
        let dirs = watch_dirs(&[f]);
        assert_eq!(
            dirs.into_iter().collect::<Vec<_>>(),
            vec![root.join("proj/.genie")]
        );
        let targets: BTreeSet<PathBuf> = [root.join("proj/.genie/settings.json")].into();
        assert!(relevant(&root.join("proj"), &targets));
        assert!(relevant(&root.join("proj/.genie/settings.json"), &targets));
        assert!(!relevant(&root.join("proj/src/main.rs"), &targets));
    }

    #[test]
    fn signals_creation_and_edits() {
        let dir = tempfile::tempdir().unwrap();
        let (tx, rx) = mpsc::channel::<()>();
        let tx = Mutex::new(tx);
        let w = PathWatcher::new(move || {
            let _ = tx.lock().unwrap().send(());
        })
        .unwrap();
        let target = dir.path().join("p/.genie/settings.json");
        assert!(w.set_paths(std::slice::from_ref(&target)).is_empty());
        std::thread::sleep(Duration::from_millis(200));
        std::fs::create_dir_all(target.parent().unwrap()).unwrap();
        rx.recv_timeout(Duration::from_secs(10))
            .expect("directory creation signalled");
        // Re-anchor on the new directory, then edit the file.
        w.set_paths(std::slice::from_ref(&target));
        assert_eq!(w.dirs().len(), 1);
        std::thread::sleep(Duration::from_millis(200));
        while rx.try_recv().is_ok() {}
        std::fs::write(&target, "{}").unwrap();
        rx.recv_timeout(Duration::from_secs(10))
            .expect("file write signalled");
    }
}
