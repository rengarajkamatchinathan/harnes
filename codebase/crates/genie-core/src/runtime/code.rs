//! Code-mode wiring (06 §4.2, D49): the file tools on the engine's shared
//! [`PathLocks`], session start folders from the settings source, and checkpoints.
//!
//! ```ignore
//! let locks = Arc::new(PathLocks::new());
//! let cps = Checkpoints::new(data_dir.join("snapshots"), registry.clone());
//! rb = rb.code(code_config(registry.clone(), locks)).checkpoints(cps.clone());
//! ```

use std::path::Path;
use std::sync::Arc;

use async_trait::async_trait;
use genie_tools::code::{CodeConfig, PathGuard, PathLocker};

use crate::locks::PathLocks;
use crate::runtime::RuntimeBuilder;
use crate::runtime::checkpoint::Checkpoints;
use crate::runtime::host::SettingsDirs;
use crate::runtime::ports::SettingsSource;

/// [`PathLocks`] as the code tools' [`PathLocker`] port: one lock per canonical path,
/// shared by every session and client of the daemon (06 §4.2).
pub struct EnginePathLocks(pub Arc<PathLocks>);

#[async_trait]
impl PathLocker for EnginePathLocks {
    async fn lock(&self, path: &Path) -> PathGuard {
        Box::new(self.0.lock(path).await)
    }
}

/// The code tools' configuration: workspace roots from `settings`, the shared locks.
pub fn code_config(settings: Arc<dyn SettingsSource>, locks: Arc<PathLocks>) -> CodeConfig {
    CodeConfig::new(
        Arc::new(SettingsDirs(settings)),
        Arc::new(EnginePathLocks(locks)),
    )
}

impl RuntimeBuilder {
    /// The Code file tools (`read`, `write`, `edit`, `multi_edit`, `patch`, `grep`,
    /// `glob`, `list`, `todo_write`), Code registry only.
    pub fn code(mut self, cfg: CodeConfig) -> Self {
        for t in genie_tools::code::tools(cfg) {
            self.rt.code_tools.register(t);
        }
        self
    }

    /// Shadow-git checkpoints around mutating steps (D49).
    pub fn checkpoints(mut self, c: Arc<Checkpoints>) -> Self {
        self.rt.checkpoints = Some(c);
        self
    }
}
