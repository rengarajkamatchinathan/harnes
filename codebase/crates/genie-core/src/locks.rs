//! File mutation locks: one async mutex per canonical path, shared across sessions and
//! clients (01 §3, t06).

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, Weak};

use tokio::sync::{Mutex as AsyncMutex, OwnedMutexGuard};

#[derive(Default)]
pub struct PathLocks {
    map: Mutex<HashMap<PathBuf, Weak<AsyncMutex<()>>>>,
}

impl PathLocks {
    pub fn new() -> Self {
        Self::default()
    }

    /// Lock `path` for mutation. The path is canonicalised when it exists; for a new file
    /// the parent is canonicalised and the file name appended.
    pub async fn lock(&self, path: &Path) -> OwnedMutexGuard<()> {
        let key = canonical_key(path);
        let m = {
            let mut map = self.map.lock().unwrap_or_else(|p| p.into_inner());
            map.retain(|_, w| w.strong_count() > 0);
            match map.get(&key).and_then(Weak::upgrade) {
                Some(m) => m,
                None => {
                    let m = Arc::new(AsyncMutex::new(()));
                    map.insert(key, Arc::downgrade(&m));
                    m
                }
            }
        }; // std lock released before awaiting
        m.lock_owned().await
    }
}

fn canonical_key(path: &Path) -> PathBuf {
    if let Ok(p) = std::fs::canonicalize(path) {
        return p;
    }
    match (path.parent(), path.file_name()) {
        (Some(parent), Some(name)) => std::fs::canonicalize(parent)
            .map(|p| p.join(name))
            .unwrap_or_else(|_| path.to_path_buf()),
        _ => path.to_path_buf(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[tokio::test]
    async fn same_file_via_different_spellings_is_one_lock() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("a")).unwrap();
        let direct = dir.path().join("a").join("f.txt");
        let dotted = dir.path().join("a").join("..").join("a").join("f.txt");
        let locks = PathLocks::new();
        let g = locks.lock(&direct).await;
        let second = tokio::time::timeout(Duration::from_millis(100), locks.lock(&dotted)).await;
        assert!(second.is_err(), "second lock must wait");
        drop(g);
        let _g2 = tokio::time::timeout(Duration::from_millis(500), locks.lock(&dotted))
            .await
            .expect("acquired after release");
    }
}
