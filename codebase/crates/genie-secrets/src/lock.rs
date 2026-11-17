//! Cross-process file lock (D26, pick-list #44): OAuth refreshes take it so two
//! processes never both rotate a refresh token. The lock is advisory (`File::try_lock`)
//! and released when the guard drops.

use std::fs::File;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

pub struct FileLock {
    _file: File,
    path: PathBuf,
}

impl std::fmt::Debug for FileLock {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FileLock")
            .field("path", &self.path)
            .finish()
    }
}

fn open(path: &Path) -> std::io::Result<File> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let mut o = std::fs::OpenOptions::new();
    o.create(true).truncate(false).read(true).write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        o.mode(0o600);
    }
    o.open(path)
}

impl FileLock {
    /// One attempt; `Ok(None)` when another holder has it.
    pub fn try_acquire(path: &Path) -> std::io::Result<Option<FileLock>> {
        let f = open(path)?;
        match f.try_lock() {
            Ok(()) => Ok(Some(FileLock {
                _file: f,
                path: path.to_path_buf(),
            })),
            Err(std::fs::TryLockError::WouldBlock) => Ok(None),
            Err(std::fs::TryLockError::Error(e)) => Err(e),
        }
    }

    /// Poll every 25 ms until acquired or `timeout` passes (no blocking thread).
    pub async fn acquire(path: &Path, timeout: Duration) -> std::io::Result<FileLock> {
        let deadline = Instant::now() + timeout;
        loop {
            if let Some(l) = Self::try_acquire(path)? {
                return Ok(l);
            }
            if Instant::now() >= deadline {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::TimedOut,
                    format!("timed out waiting for {}", path.display()),
                ));
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn second_holder_waits_until_release() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("x.lock");
        let a = FileLock::try_acquire(&p).unwrap().unwrap();
        assert!(FileLock::try_acquire(&p).unwrap().is_none());
        drop(a);
        assert!(FileLock::try_acquire(&p).unwrap().is_some());
    }

    #[tokio::test]
    async fn async_acquire_times_out_then_succeeds() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("y.lock");
        let held = FileLock::try_acquire(&p).unwrap().unwrap();
        let r = FileLock::acquire(&p, Duration::from_millis(80)).await;
        assert_eq!(r.unwrap_err().kind(), std::io::ErrorKind::TimedOut);
        drop(held);
        assert!(
            FileLock::acquire(&p, Duration::from_millis(80))
                .await
                .is_ok()
        );
    }
}
