//! Per-user, per-channel endpoint and file layout (01 §1.2, SA-02).
//!
//! | | macOS/Linux | Windows |
//! |---|---|---|
//! | config dir | `~/.genie[-beta\|-dev]` | `%USERPROFILE%\.genie[-beta\|-dev]` |
//! | data dir (run, blobs, DB, logs) | same as config | `%LOCALAPPDATA%\Genie\<channel>` |
//! | endpoint | `<data>/run/daemon.sock` (dir `0700`) | `\\.\pipe\genie-<channel>-<sid-hash>` |
//! | token | `<data>/run/daemon.token` (`0600`) | same path, user-only profile ACL |
//! | spawn lock | `<data>/run/daemon.lock` | same |

use std::fmt;
use std::io;
use std::path::{Path, PathBuf};

use genie_protocol::Channel;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Address {
    Unix(PathBuf),
    Pipe(String),
}

impl fmt::Display for Address {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Address::Unix(p) => write!(f, "{}", p.display()),
            Address::Pipe(n) => f.write_str(n),
        }
    }
}

#[derive(Debug, Clone)]
pub struct Endpoint {
    pub channel: Channel,
    pub config_dir: PathBuf,
    pub data_dir: PathBuf,
    pub run_dir: PathBuf,
    pub address: Address,
}

impl Endpoint {
    /// The shared daemon endpoint for the current user and `channel`.
    pub fn for_channel(channel: Channel) -> io::Result<Endpoint> {
        let home = home_dir()?;
        let config_dir = home.join(format!(".genie{}", channel.dir_suffix()));
        #[cfg(windows)]
        {
            let local = std::env::var_os("LOCALAPPDATA")
                .map(PathBuf::from)
                .ok_or_else(|| {
                    io::Error::new(io::ErrorKind::NotFound, "LOCALAPPDATA is not set")
                })?;
            let data_dir = local.join("Genie").join(channel.as_str());
            let run_dir = data_dir.join("run");
            let address = Address::Pipe(format!(
                r"\\.\pipe\genie-{}-{}",
                channel.as_str(),
                crate::win::current_user_sid_hash()?
            ));
            Ok(Endpoint {
                channel,
                config_dir,
                data_dir,
                run_dir,
                address,
            })
        }
        #[cfg(unix)]
        {
            let data_dir = config_dir.clone();
            let run_dir = data_dir.join("run");
            let address = Address::Unix(run_dir.join("daemon.sock"));
            Ok(Endpoint {
                channel,
                config_dir,
                data_dir,
                run_dir,
                address,
            })
        }
    }

    /// A private endpoint rooted at `dir` (standalone daemons and tests, 01 §4.4).
    pub fn in_dir(dir: &Path, channel: Channel) -> io::Result<Endpoint> {
        let run_dir = dir.join("run");
        #[cfg(windows)]
        let address = {
            use sha2::Digest;
            let h = sha2::Sha256::digest(dir.to_string_lossy().as_bytes());
            Address::Pipe(format!(
                r"\\.\pipe\genie-{}-{}-{}",
                channel.as_str(),
                crate::win::current_user_sid_hash()?,
                &hex::encode(h)[..12]
            ))
        };
        #[cfg(unix)]
        let address = Address::Unix(run_dir.join("daemon.sock"));
        Ok(Endpoint {
            channel,
            config_dir: dir.to_path_buf(),
            data_dir: dir.to_path_buf(),
            run_dir,
            address,
        })
    }

    pub fn token_path(&self) -> PathBuf {
        self.run_dir.join("daemon.token")
    }

    pub fn lock_path(&self) -> PathBuf {
        self.run_dir.join("daemon.lock")
    }

    pub fn logs_dir(&self) -> PathBuf {
        self.data_dir.join("logs")
    }

    /// Whether a daemon currently holds the spawn lock. `false` means any socket file is
    /// stale (01 §4.1).
    pub fn lock_held(&self) -> bool {
        let Ok(f) = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(self.lock_path())
        else {
            return false;
        };
        match f.try_lock() {
            Ok(()) => {
                let _ = f.unlock();
                false
            }
            Err(_) => true,
        }
    }

    /// Read the daemon token written at daemon start.
    pub fn read_token(&self) -> io::Result<String> {
        Ok(std::fs::read_to_string(self.token_path())?
            .trim()
            .to_string())
    }
}

fn home_dir() -> io::Result<PathBuf> {
    #[cfg(windows)]
    let var = "USERPROFILE";
    #[cfg(unix)]
    let var = "HOME";
    std::env::var_os(var)
        .map(PathBuf::from)
        .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, format!("{var} is not set")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn channel_dirs_are_separate() {
        let s = Endpoint::for_channel(Channel::Stable).unwrap();
        let d = Endpoint::for_channel(Channel::Dev).unwrap();
        assert!(s.config_dir.ends_with(".genie"));
        assert!(d.config_dir.ends_with(".genie-dev"));
        assert_ne!(s.address, d.address);
        assert_ne!(s.run_dir, d.run_dir);
    }

    #[test]
    fn lock_not_held_when_missing() {
        let dir = tempfile::tempdir().unwrap();
        let e = Endpoint::in_dir(dir.path(), Channel::Dev).unwrap();
        assert!(!e.lock_held());
        std::fs::create_dir_all(&e.run_dir).unwrap();
        let f = std::fs::File::create(e.lock_path()).unwrap();
        f.lock().unwrap();
        assert!(e.lock_held());
        f.unlock().unwrap();
        assert!(!e.lock_held());
    }
}
