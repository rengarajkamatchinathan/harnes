//! `BrowserBackend` (06 §10): v1 is [`SystemChromeBackend`], the user's Chrome / Edge /
//! Chromium with a separate persistent Genie profile over `--remote-debugging-pipe`. The
//! v2 CEF helper implements the same trait; tools and safety code only see a
//! [`CdpClient`].

use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;

use async_trait::async_trait;
use genie_process::ProcessTree;
use genie_protocol::api::browser::{BrowserKind, BrowserStatus, InstalledBrowser};
use serde_json::{Value, json};
use tokio::sync::Mutex;
use tokio_util::task::TaskTracker;

use crate::cdp::{CdpClient, CdpError};
use crate::discover;

#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum BrowserError {
    #[error("no Chrome, Edge or Chromium is installed")]
    NotInstalled,
    #[error("the installed browser ({version}) is too old; update to version 120 or newer")]
    Unsupported { version: String },
    #[error("couldn't start the browser: {0}")]
    Launch(String),
    #[error(transparent)]
    Cdp(#[from] CdpError),
}

#[async_trait]
pub trait BrowserBackend: Send + Sync {
    /// `browser.status` (02 §6.6).
    async fn status(&self) -> BrowserStatus;
    /// A live connection, launching the browser if it isn't running.
    async fn connect(&self) -> Result<CdpClient, BrowserError>;
    /// Close the browser (daemon shutdown, `browser.data.clear`).
    async fn shutdown(&self);
}

/// Profile kind (research §4.1, SA-28a).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ProfileKind {
    /// `<data dir>/browser/profile`, cookies kept (Chrome/Edge's own OS-keystore encryption).
    #[default]
    Persistent,
    /// A fresh directory wiped on shutdown (Linux without a keyring, policy).
    Ephemeral,
}

#[derive(Debug, Clone)]
pub struct SystemChromeConfig {
    /// `<data dir>/browser` (SA-28a).
    pub browser_dir: PathBuf,
    pub profile: ProfileKind,
    /// Desktop: headless + screencast (D56). CLI: a real window (SA-28b).
    pub headless: bool,
    pub prefer: Option<BrowserKind>,
    /// Use this executable instead of discovering one (tests, settings).
    pub executable: Option<PathBuf>,
    pub launch_timeout: Duration,
}

impl SystemChromeConfig {
    pub fn new(data_dir: &Path) -> SystemChromeConfig {
        SystemChromeConfig {
            browser_dir: data_dir.join("browser"),
            profile: ProfileKind::Persistent,
            headless: true,
            prefer: None,
            executable: None,
            launch_timeout: Duration::from_secs(20),
        }
    }

    pub fn profile_dir(&self) -> PathBuf {
        match self.profile {
            ProfileKind::Persistent => self.browser_dir.join("profile"),
            ProfileKind::Ephemeral => self
                .browser_dir
                .join(format!("ephemeral-{}", std::process::id())),
        }
    }

    /// Download quarantine (research §4.1).
    pub fn downloads_dir(&self) -> PathBuf {
        self.browser_dir.join("downloads")
    }
}

/// Command-line flags (06 §10): isolated profile, no sync/extensions/notifications, no
/// first-run UI, permission prompts auto-denied. The pipe flags are added by
/// [`crate::transport`].
pub fn launch_args(cfg: &SystemChromeConfig) -> Vec<String> {
    let mut a = vec![
        format!("--user-data-dir={}", cfg.profile_dir().display()),
        "--no-first-run".into(),
        "--no-default-browser-check".into(),
        "--disable-sync".into(),
        "--disable-extensions".into(),
        "--disable-component-extensions-with-background-pages".into(),
        "--disable-default-apps".into(),
        "--disable-notifications".into(),
        "--deny-permission-prompts".into(),
        "--disable-background-mode".into(),
        "--disable-features=Translate,MediaRouter,PasswordManagerOnboarding,AutofillServerCommunication"
            .into(),
        "--window-size=1280,800".into(),
    ];
    if cfg.headless {
        a.push("--headless=new".into());
    }
    a.push("about:blank".into());
    a
}

/// Profile preferences that switch off password saving, autofill and site permission
/// prompts (geolocation, camera, mic, notifications auto-denied: 2 = block).
pub fn profile_prefs() -> Value {
    json!({
        "credentials_enable_service": false,
        "credentials_enable_autosignin": false,
        "profile": {
            "password_manager_enabled": false,
            "default_content_setting_values": {
                "geolocation": 2,
                "media_stream_camera": 2,
                "media_stream_mic": 2,
                "notifications": 2
            }
        },
        "autofill": {"enabled": false, "profile_enabled": false, "credit_card_enabled": false},
        "download": {"prompt_for_download": false},
        "browser": {"has_seen_welcome_page": true}
    })
}

fn merge(dst: &mut Value, src: &Value) {
    match (dst, src) {
        (Value::Object(d), Value::Object(s)) => {
            for (k, v) in s {
                merge(d.entry(k.clone()).or_insert(Value::Null), v);
            }
        }
        (d, s) => *d = s.clone(),
    }
}

/// Create the profile directory (0700) and merge [`profile_prefs`] into
/// `Default/Preferences`.
pub fn prepare_profile(dir: &Path) -> std::io::Result<()> {
    let default = dir.join("Default");
    std::fs::create_dir_all(&default)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))?;
    }
    let path = default.join("Preferences");
    let mut prefs = std::fs::read(&path)
        .ok()
        .and_then(|b| serde_json::from_slice::<Value>(&b).ok())
        .filter(Value::is_object)
        .unwrap_or_else(|| json!({}));
    merge(&mut prefs, &profile_prefs());
    std::fs::write(&path, serde_json::to_vec(&prefs).unwrap_or_default())
}

struct Running {
    _tree: ProcessTree,
    client: CdpClient,
}

/// The user's Chrome / Edge / Chromium (option B).
pub struct SystemChromeBackend {
    cfg: SystemChromeConfig,
    tasks: TaskTracker,
    installed: Mutex<Option<Vec<InstalledBrowser>>>,
    running: Mutex<Option<Running>>,
}

impl SystemChromeBackend {
    pub fn new(cfg: SystemChromeConfig, tasks: TaskTracker) -> SystemChromeBackend {
        SystemChromeBackend {
            cfg,
            tasks,
            installed: Mutex::new(None),
            running: Mutex::new(None),
        }
    }

    pub fn config(&self) -> &SystemChromeConfig {
        &self.cfg
    }

    async fn installed(&self) -> Vec<InstalledBrowser> {
        let mut g = self.installed.lock().await;
        if g.is_none() {
            *g = Some(discover::find_installed().await);
        }
        g.clone().unwrap_or_default()
    }

    async fn executable(&self) -> Result<PathBuf, BrowserError> {
        if let Some(p) = &self.cfg.executable {
            return Ok(p.clone());
        }
        let installed = self.installed().await;
        match discover::pick(&installed, self.cfg.prefer) {
            Some(b) => Ok(PathBuf::from(&b.path)),
            None => match installed.first() {
                Some(b) => Err(BrowserError::Unsupported {
                    version: b.version.clone(),
                }),
                None => Err(BrowserError::NotInstalled),
            },
        }
    }

    async fn launch(&self) -> Result<Running, BrowserError> {
        let exe = self.executable().await?;
        let profile = self.cfg.profile_dir();
        prepare_profile(&profile).map_err(|e| BrowserError::Launch(e.to_string()))?;
        std::fs::create_dir_all(self.cfg.downloads_dir())
            .map_err(|e| BrowserError::Launch(e.to_string()))?;
        let mut cmd = tokio::process::Command::new(&exe);
        cmd.args(launch_args(&self.cfg))
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        let (tree, wire) = crate::transport::spawn_with_pipe(&mut cmd)
            .map_err(|e| BrowserError::Launch(format!("{}: {e}", exe.display())))?;
        let client = CdpClient::connect(wire, &self.tasks);
        client
            .call_timeout(
                None,
                "Browser.getVersion",
                json!({}),
                self.cfg.launch_timeout,
            )
            .await?;
        tracing::info!(exe = %exe.display(), "browser started");
        Ok(Running {
            _tree: tree,
            client,
        })
    }
}

#[async_trait]
impl BrowserBackend for SystemChromeBackend {
    async fn status(&self) -> BrowserStatus {
        let running = self
            .running
            .lock()
            .await
            .as_ref()
            .is_some_and(|r| !r.client.is_closed());
        BrowserStatus {
            installed: self.installed().await,
            running,
            profile_path: self.cfg.profile_dir().to_string_lossy().into_owned(),
        }
    }

    async fn connect(&self) -> Result<CdpClient, BrowserError> {
        let mut g = self.running.lock().await;
        if let Some(r) = g.as_ref()
            && !r.client.is_closed()
        {
            return Ok(r.client.clone());
        }
        *g = None;
        let r = self.launch().await?;
        let c = r.client.clone();
        *g = Some(r);
        Ok(c)
    }

    async fn shutdown(&self) {
        let r = self.running.lock().await.take();
        if let Some(r) = r {
            // Graceful close lets Chrome flush cookies; the tree is killed on drop anyway.
            let _ = r
                .client
                .call_timeout(None, "Browser.close", json!({}), Duration::from_secs(3))
                .await;
        }
        if self.cfg.profile == ProfileKind::Ephemeral {
            let _ = std::fs::remove_dir_all(self.cfg.profile_dir());
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn args_and_prefs() {
        let cfg = SystemChromeConfig::new(Path::new("/data"));
        let a = launch_args(&cfg);
        assert!(
            a.iter()
                .any(|x| x.starts_with("--user-data-dir=") && x.contains("profile"))
        );
        assert!(a.contains(&"--headless=new".to_string()));
        assert!(!a.iter().any(|x| x.contains("remote-debugging-port")));
        assert!(!a.iter().any(|x| x == "--enable-automation"));

        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("profile");
        std::fs::create_dir_all(p.join("Default")).unwrap();
        std::fs::write(
            p.join("Default/Preferences"),
            r#"{"keep": 1, "profile": {"name": "Genie", "password_manager_enabled": true}}"#,
        )
        .unwrap();
        prepare_profile(&p).unwrap();
        let v: Value =
            serde_json::from_slice(&std::fs::read(p.join("Default/Preferences")).unwrap()).unwrap();
        assert_eq!(v["keep"], 1);
        assert_eq!(v["profile"]["name"], "Genie");
        assert_eq!(v["profile"]["password_manager_enabled"], false);
        assert_eq!(
            v["profile"]["default_content_setting_values"]["geolocation"],
            2
        );
        assert_eq!(v["autofill"]["credit_card_enabled"], false);
    }
}
