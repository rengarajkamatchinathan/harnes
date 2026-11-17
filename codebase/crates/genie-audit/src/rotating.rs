//! Size-based rotating log file: `name.log`, `name.log.1` … `name.log.{N-1}`.

use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard};

use tracing_subscriber::fmt::MakeWriter;

#[derive(Clone)]
pub struct RotatingLog {
    inner: Arc<Mutex<Inner>>,
}

struct Inner {
    dir: PathBuf,
    name: String,
    keep: usize,
    max_bytes: u64,
    /// `None` only while rotating (closed so Windows can rename it).
    file: Option<File>,
    written: u64,
}

impl RotatingLog {
    pub fn open(dir: &Path, name: &str, keep: usize, max_bytes: u64) -> io::Result<Self> {
        fs::create_dir_all(dir)?;
        let path = dir.join(format!("{name}.log"));
        let file = OpenOptions::new().create(true).append(true).open(&path)?;
        let written = file.metadata()?.len();
        Ok(RotatingLog {
            inner: Arc::new(Mutex::new(Inner {
                dir: dir.to_path_buf(),
                name: name.to_string(),
                keep: keep.max(1),
                max_bytes,
                file: Some(file),
                written,
            })),
        })
    }

    fn lock(&self) -> MutexGuard<'_, Inner> {
        // A poisoned log lock must not take the daemon down; keep logging.
        self.inner.lock().unwrap_or_else(|p| p.into_inner())
    }
}

impl Inner {
    fn path(&self, idx: usize) -> PathBuf {
        if idx == 0 {
            self.dir.join(format!("{}.log", self.name))
        } else {
            self.dir.join(format!("{}.log.{idx}", self.name))
        }
    }

    fn rotate(&mut self) -> io::Result<()> {
        if let Some(mut f) = self.file.take() {
            f.flush()?;
        }
        let last = self.keep - 1;
        if last > 0 {
            let _ = fs::remove_file(self.path(last));
            for i in (0..last).rev() {
                let from = self.path(i);
                if from.exists() {
                    fs::rename(&from, self.path(i + 1))?;
                }
            }
        }
        let file = OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .open(self.path(0))?;
        self.file = Some(file);
        self.written = 0;
        Ok(())
    }

    fn file(&mut self) -> io::Result<&mut File> {
        if self.file.is_none() {
            let f = OpenOptions::new()
                .create(true)
                .append(true)
                .open(self.path(0))?;
            self.file = Some(f);
        }
        self.file
            .as_mut()
            .ok_or_else(|| io::Error::other("log file unavailable"))
    }

    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        if self.written > 0 && self.written + buf.len() as u64 > self.max_bytes {
            self.rotate()?;
        }
        self.file()?.write_all(buf)?;
        self.written += buf.len() as u64;
        Ok(buf.len())
    }
}

pub struct Handle<'a>(MutexGuard<'a, Inner>);

impl Write for Handle<'_> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.0.write(buf)
    }
    fn flush(&mut self) -> io::Result<()> {
        match self.0.file.as_mut() {
            Some(f) => f.flush(),
            None => Ok(()),
        }
    }
}

impl<'a> MakeWriter<'a> for RotatingLog {
    type Writer = Handle<'a>;
    fn make_writer(&'a self) -> Self::Writer {
        Handle(self.lock())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rotates_and_keeps_n_files() {
        let dir = tempfile::tempdir().unwrap();
        let log = RotatingLog::open(dir.path(), "daemon", 3, 100).unwrap();
        for _ in 0..10 {
            let mut w = log.make_writer();
            w.write_all(&[b'x'; 60]).unwrap();
        }
        let mut names: Vec<_> = fs::read_dir(dir.path())
            .unwrap()
            .map(|e| e.unwrap().file_name().into_string().unwrap())
            .collect();
        names.sort();
        assert_eq!(names, ["daemon.log", "daemon.log.1", "daemon.log.2"]);
        assert!(fs::metadata(dir.path().join("daemon.log")).unwrap().len() <= 100);
    }
}
