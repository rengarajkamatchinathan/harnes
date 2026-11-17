//! Per-directory context (01 §3; a light version of OpenCode's Location, t01).
//!
//! Keyed by canonical project root, built lazily, rebuilt on file-watch events, shared by
//! all sessions in that folder. Chat sessions use the synthetic [`USER_CONTEXT`].
//! The layers it will hold (config, trust, instructions, rules, skills/agents/commands,
//! git identity) are filled in by specs 06 and 08; this milestone fixes the lifecycle.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

/// Key used by chat sessions, which have no project folder.
pub const USER_CONTEXT: &str = "<user>";

#[derive(Debug)]
pub struct DirContext {
    /// Canonical project root, or [`USER_CONTEXT`].
    pub root: PathBuf,
    /// Bumped on every rebuild so holders can detect staleness.
    pub generation: u64,
}

#[derive(Default)]
pub struct DirContexts {
    map: Mutex<HashMap<PathBuf, Arc<DirContext>>>,
    generation: Mutex<u64>,
}

impl DirContexts {
    pub fn new() -> Self {
        Self::default()
    }

    /// Get (building if needed) the context for `root`. `None` gives the user context.
    pub fn get(&self, root: Option<&Path>) -> Arc<DirContext> {
        let key = match root {
            Some(p) => std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf()),
            None => PathBuf::from(USER_CONTEXT),
        };
        let mut map = self.map.lock().unwrap_or_else(|p| p.into_inner());
        if let Some(c) = map.get(&key) {
            return c.clone();
        }
        let ctx = Arc::new(DirContext {
            root: key.clone(),
            generation: self.next_gen(),
        });
        map.insert(key, ctx.clone());
        ctx
    }

    /// Drop the cached context so the next `get` rebuilds it (file-watch event).
    pub fn invalidate(&self, root: &Path) {
        let key = std::fs::canonicalize(root).unwrap_or_else(|_| root.to_path_buf());
        self.map
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .remove(&key);
    }

    fn next_gen(&self) -> u64 {
        let mut g = self.generation.lock().unwrap_or_else(|p| p.into_inner());
        *g += 1;
        *g
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn shared_until_invalidated() {
        let dir = tempfile::tempdir().unwrap();
        let ctxs = DirContexts::new();
        let a = ctxs.get(Some(dir.path()));
        let b = ctxs.get(Some(dir.path()));
        assert!(Arc::ptr_eq(&a, &b));
        ctxs.invalidate(dir.path());
        let c = ctxs.get(Some(dir.path()));
        assert!(c.generation > a.generation);
        assert_eq!(ctxs.get(None).root, PathBuf::from(USER_CONTEXT));
    }
}
