//! Host services behind the Platform adapter that are not daemon calls: the channel,
//! the bundled daemon binary, the stable instance id, `state.json` (window and panel
//! state, D23 — never settings), opening URLs/paths with the OS, and saving exports.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::str::FromStr;

use genie_client::{ClientInfo, Endpoint, EnsureOptions};
use genie_protocol::{AskKind, Channel, ClientCapabilities, ClientKind};
use serde_json::Value;

/// Build-time channel (`GENIE_CHANNEL`), overridable at run time with
/// `GENIE_CHANNEL_OVERRIDE` (same rules as the CLI).
pub fn channel() -> Channel {
    let build = option_env!("GENIE_CHANNEL").unwrap_or("stable");
    let name = std::env::var("GENIE_CHANNEL_OVERRIDE").unwrap_or_else(|_| build.to_string());
    Channel::from_str(&name).unwrap_or_default()
}

/// The `genie` binary that runs the daemon: `GENIE_DAEMON_PATH`, else the one bundled
/// next to the app (D40), else `genie` on `PATH`.
pub fn daemon_exe() -> Option<PathBuf> {
    if let Some(p) = std::env::var_os("GENIE_DAEMON_PATH") {
        return Some(PathBuf::from(p));
    }
    let name = if cfg!(windows) { "genie.exe" } else { "genie" };
    if let Ok(exe) = std::env::current_exe() {
        let sibling = exe.with_file_name(name);
        if sibling.is_file() {
            return Some(sibling);
        }
    }
    std::env::var_os("PATH").and_then(|paths| {
        std::env::split_paths(&paths)
            .map(|d| d.join(name))
            .find(|p| p.is_file())
    })
}

/// The desktop's own folder in the channel's data dir.
pub fn desktop_dir(endpoint: &Endpoint) -> PathBuf {
    endpoint.data_dir.join("desktop")
}

/// Stable per install: the lease is reclaimed across app restarts (02 §5).
pub fn instance_id(dir: &Path) -> String {
    let path = dir.join("instance_id");
    if let Ok(s) = std::fs::read_to_string(&path) {
        let s = s.trim();
        if !s.is_empty() {
            return s.to_string();
        }
    }
    let id = format!("desktop_{}", ulid::Ulid::new().to_string().to_lowercase());
    let _ = std::fs::create_dir_all(dir);
    let _ = write_atomic(&path, id.as_bytes());
    id
}

pub fn client_info(instance_id: &str) -> ClientInfo {
    ClientInfo {
        kind: ClientKind::Desktop,
        version: env!("CARGO_PKG_VERSION").to_string(),
        instance_id: instance_id.to_string(),
        capabilities: ClientCapabilities {
            asks: vec![
                AskKind::Permission,
                AskKind::Elicitation,
                AskKind::Sampling,
                AskKind::Question,
                AskKind::BrowserConfirm,
                AskKind::DefinitionHash,
            ],
            notifications: true,
            screencast: Some(true),
            // Terminal tab: terminal.open/attach/input/resize/close (05 §10, D55).
            terminal: Some(true),
        },
    }
}

pub fn ensure_options(endpoint: Endpoint, instance_id: &str) -> EnsureOptions {
    let channel = endpoint.channel;
    let args: Vec<OsString> = vec!["daemon".into(), "--channel".into(), channel.as_str().into()];
    EnsureOptions::new(endpoint, client_info(instance_id), daemon_exe(), args)
}

/// `state.json`: window state, sidebar width, last tab, panel sizes (05 §1, D23).
pub struct StateFile {
    path: PathBuf,
}

impl StateFile {
    pub fn new(dir: &Path) -> Self {
        StateFile {
            path: dir.join("state.json"),
        }
    }

    pub fn load(&self) -> Value {
        std::fs::read(&self.path)
            .ok()
            .and_then(|b| serde_json::from_slice(&b).ok())
            .filter(Value::is_object)
            .unwrap_or_else(|| Value::Object(Default::default()))
    }

    pub fn save(&self, state: &Value) -> std::io::Result<()> {
        if !state.is_object() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "state must be an object",
            ));
        }
        if let Some(dir) = self.path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let bytes = serde_json::to_vec_pretty(state).map_err(std::io::Error::other)?;
        write_atomic(&self.path, &bytes)
    }
}

fn write_atomic(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, bytes)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o600));
    }
    std::fs::rename(&tmp, path)
}

/// Only web and mail links leave the app (citations, sign-in pages, docs).
pub fn check_external_url(url: &str) -> Result<(), String> {
    let lower = url.to_ascii_lowercase();
    if ["https://", "http://", "mailto:"]
        .iter()
        .any(|p| lower.starts_with(p))
        && !url.chars().any(char::is_control)
    {
        Ok(())
    } else {
        Err("only http(s) and mailto links can be opened".into())
    }
}

/// Open a URL or an existing path with the OS default handler.
pub fn os_open(target: &str) -> Result<(), String> {
    use std::process::{Command, Stdio};
    let mut cmd = if cfg!(target_os = "macos") {
        let mut c = Command::new("open");
        c.arg(target);
        c
    } else if cfg!(windows) {
        let mut c = Command::new("rundll32");
        c.arg("url.dll,FileProtocolHandler").arg(target);
        c
    } else {
        let mut c = Command::new("xdg-open");
        c.arg(target);
        c
    };
    cmd.stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map(|_| ())
        .map_err(|e| e.to_string())
}

/// Save an export into the user's Downloads folder without overwriting; returns the path.
pub fn save_download(name: &str, bytes: &[u8]) -> std::io::Result<PathBuf> {
    let home = std::env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" })
        .map(PathBuf::from)
        .ok_or_else(|| std::io::Error::new(std::io::ErrorKind::NotFound, "no home folder"))?;
    let dir = home.join("Downloads");
    std::fs::create_dir_all(&dir)?;
    let path = unique_path(&dir, &sanitize_file_name(name));
    std::fs::write(&path, bytes)?;
    Ok(path)
}

pub fn sanitize_file_name(name: &str) -> String {
    let s: String = name
        .chars()
        .map(|c| {
            if c.is_control() || "/\\:*?\"<>|".contains(c) {
                '_'
            } else {
                c
            }
        })
        .collect();
    let s = s.trim().trim_start_matches('.').to_string();
    if s.is_empty() { "export".into() } else { s }
}

fn unique_path(dir: &Path, name: &str) -> PathBuf {
    let first = dir.join(name);
    if !first.exists() {
        return first;
    }
    let (stem, ext) = match name.rsplit_once('.') {
        Some((s, e)) if !s.is_empty() => (s.to_string(), format!(".{e}")),
        _ => (name.to_string(), String::new()),
    };
    (1..)
        .map(|i| dir.join(format!("{stem} ({i}){ext}")))
        .find(|p| !p.exists())
        .unwrap_or(first)
}

/// Result of "Install `genie` command in PATH" (05 §4 General, D40).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct CliInstall {
    /// The link (Unix) or shim (Windows) that was written.
    pub path: String,
    /// The `genie` binary it runs.
    pub target: String,
    /// Whether the folder is already on `PATH` (otherwise the UI says how to add it).
    pub on_path: bool,
    pub dir: String,
}

/// Where the command goes: `~/.local/bin` on macOS/Linux (no admin rights needed),
/// `%LOCALAPPDATA%\Genie\bin` on Windows.
pub fn cli_bin_dir() -> Option<PathBuf> {
    if cfg!(windows) {
        std::env::var_os("LOCALAPPDATA").map(|d| PathBuf::from(d).join("Genie").join("bin"))
    } else {
        std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local").join("bin"))
    }
}

fn dir_on_path(dir: &Path, path_var: Option<&std::ffi::OsStr>) -> bool {
    path_var.is_some_and(|p| std::env::split_paths(p).any(|d| d == dir))
}

/// Link (Unix) or shim (Windows) `genie` in `dir` to `target`. An existing link or shim we
/// wrote is replaced; any other file named `genie` is left alone (error).
pub fn install_cli(target: &Path, dir: &Path) -> std::io::Result<CliInstall> {
    if !target.is_file() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            format!("the genie binary was not found at {}", target.display()),
        ));
    }
    std::fs::create_dir_all(dir)?;
    #[cfg(windows)]
    let path = {
        let path = dir.join("genie.cmd");
        if let Ok(old) = std::fs::read_to_string(&path)
            && !old.starts_with("@REM genie shim")
        {
            return Err(std::io::Error::new(
                std::io::ErrorKind::AlreadyExists,
                format!("{} exists and wasn't written by Genie", path.display()),
            ));
        }
        let body = format!(
            "@REM genie shim (written by the Genie app)\r\n@\"{}\" %*\r\n",
            target.display()
        );
        std::fs::write(&path, body)?;
        path
    };
    #[cfg(not(windows))]
    let path = {
        let path = dir.join("genie");
        match std::fs::symlink_metadata(&path) {
            Ok(m) if m.file_type().is_symlink() => std::fs::remove_file(&path)?,
            Ok(_) => {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::AlreadyExists,
                    format!("{} exists and isn't a link", path.display()),
                ));
            }
            Err(_) => {}
        }
        std::os::unix::fs::symlink(target, &path)?;
        path
    };
    Ok(CliInstall {
        path: path.display().to_string(),
        target: target.display().to_string(),
        on_path: dir_on_path(dir, std::env::var_os("PATH").as_deref()),
        dir: dir.display().to_string(),
    })
}

/// The third-party notices shipped with the app (05 §4 Account & licence): the
/// `THIRD-PARTY-NOTICES.md` that `cargo xtask notices` / `dist` write, looked up next to the executable, in the resources, or
/// (debug builds) in the workspace.
pub fn notices_text(resource_dir: Option<&Path>) -> Option<String> {
    let mut candidates: Vec<PathBuf> = vec![];
    if let Some(p) = std::env::var_os("GENIE_NOTICES") {
        candidates.push(PathBuf::from(p));
    }
    if let Some(r) = resource_dir {
        candidates.push(r.join("THIRD-PARTY-NOTICES.md"));
    }
    if let Ok(exe) = std::env::current_exe() {
        candidates.push(exe.with_file_name("THIRD-PARTY-NOTICES.md"));
    }
    if cfg!(debug_assertions) {
        let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
        // `cargo xtask notices` (default `--out dist`), else the hand-written extras.
        candidates.push(manifest.join("../../../dist/THIRD-PARTY-NOTICES.md"));
        candidates.push(manifest.join("../../../release/notices/extra.md"));
    }
    candidates
        .iter()
        .find_map(|p| std::fs::read_to_string(p).ok())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn state_file_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let f = StateFile::new(&dir.path().join("desktop"));
        assert_eq!(f.load(), serde_json::json!({}));
        f.save(&serde_json::json!({"sidebar": 280})).unwrap();
        assert_eq!(f.load()["sidebar"], 280);
        assert!(f.save(&serde_json::json!([1])).is_err());
    }

    #[test]
    fn instance_id_is_stable() {
        let dir = tempfile::tempdir().unwrap();
        let a = instance_id(dir.path());
        assert!(a.starts_with("desktop_"));
        assert_eq!(instance_id(dir.path()), a);
    }

    #[test]
    fn external_urls_and_names() {
        assert!(check_external_url("https://example.com/a?b").is_ok());
        assert!(check_external_url("file:///etc/passwd").is_err());
        assert!(check_external_url("javascript:alert(1)").is_err());
        assert!(check_external_url("https://x\n").is_err());
        assert_eq!(sanitize_file_name("../a/b:c.md"), "_a_b_c.md");
        assert_eq!(sanitize_file_name("  "), "export");
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.md"), "x").unwrap();
        assert_eq!(unique_path(dir.path(), "a.md"), dir.path().join("a (1).md"));
    }

    #[test]
    fn desktop_client_declares_its_asks() {
        let info = client_info("desktop_x");
        assert_eq!(info.kind, ClientKind::Desktop);
        assert!(info.capabilities.asks.contains(&AskKind::BrowserConfirm));
        assert_eq!(info.capabilities.terminal, Some(true));
    }

    #[test]
    fn install_cli_links_and_replaces_its_own_link() {
        let dir = tempfile::tempdir().unwrap();
        let bin = dir
            .path()
            .join("app")
            .join(if cfg!(windows) { "genie.exe" } else { "genie" });
        std::fs::create_dir_all(bin.parent().unwrap()).unwrap();
        std::fs::write(&bin, "#!/bin/sh\n").unwrap();
        let target_dir = dir.path().join("bin");
        let a = install_cli(&bin, &target_dir).unwrap();
        assert!(std::path::Path::new(&a.path).exists());
        // Again: our own link/shim is replaced.
        install_cli(&bin, &target_dir).unwrap();
        assert!(install_cli(&dir.path().join("missing"), &target_dir).is_err());
        #[cfg(unix)]
        {
            assert_eq!(std::fs::read_link(&a.path).unwrap(), bin);
            // A foreign file is never overwritten.
            let other = dir.path().join("other");
            std::fs::create_dir_all(&other).unwrap();
            std::fs::write(other.join("genie"), "mine").unwrap();
            assert!(install_cli(&bin, &other).is_err());
            assert_eq!(
                std::fs::read_to_string(other.join("genie")).unwrap(),
                "mine"
            );
        }
        let joined = std::env::join_paths([dir.path().join("x"), target_dir.clone()]).unwrap();
        assert!(dir_on_path(&target_dir, Some(&joined)));
        assert!(!dir_on_path(&dir.path().join("y"), Some(&joined)));
    }

    #[test]
    fn notices_from_env() {
        let dir = tempfile::tempdir().unwrap();
        let f = dir.path().join("n.md");
        std::fs::write(&f, "# Notices").unwrap();
        // SAFETY: tests in this module don't read GENIE_NOTICES concurrently.
        unsafe { std::env::set_var("GENIE_NOTICES", &f) };
        assert_eq!(notices_text(None).as_deref(), Some("# Notices"));
        unsafe { std::env::remove_var("GENIE_NOTICES") };
    }
}
