//! Find the installed Chrome / Edge / Chromium (06 §10, SA-36: version ≥ 120).

use std::path::{Path, PathBuf};
use std::time::Duration;

use genie_process::pipe::{Env, OutputCaps, PipeSpec, run};
use genie_protocol::api::browser::{BrowserKind, InstalledBrowser};
use tokio_util::sync::CancellationToken;

/// Oldest supported major version (SA-36).
pub const MIN_MAJOR: u32 = 120;

/// Where each browser usually lives on this OS, in preference order (Chrome, Edge,
/// Chromium).
pub fn candidates() -> Vec<(BrowserKind, PathBuf)> {
    let mut v = Vec::new();
    #[cfg(target_os = "macos")]
    {
        let apps: Vec<PathBuf> = std::iter::once(PathBuf::from("/Applications"))
            .chain(std::env::var_os("HOME").map(|h| Path::new(&h).join("Applications")))
            .collect();
        for dir in &apps {
            v.push((
                BrowserKind::Chrome,
                dir.join("Google Chrome.app/Contents/MacOS/Google Chrome"),
            ));
            v.push((
                BrowserKind::Edge,
                dir.join("Microsoft Edge.app/Contents/MacOS/Microsoft Edge"),
            ));
            v.push((
                BrowserKind::Chromium,
                dir.join("Chromium.app/Contents/MacOS/Chromium"),
            ));
        }
    }
    #[cfg(windows)]
    {
        let roots: Vec<PathBuf> = ["ProgramFiles", "ProgramFiles(x86)", "LOCALAPPDATA"]
            .iter()
            .filter_map(std::env::var_os)
            .map(PathBuf::from)
            .collect();
        for r in &roots {
            v.push((
                BrowserKind::Chrome,
                r.join(r"Google\Chrome\Application\chrome.exe"),
            ));
        }
        for r in &roots {
            v.push((
                BrowserKind::Edge,
                r.join(r"Microsoft\Edge\Application\msedge.exe"),
            ));
        }
        for r in &roots {
            v.push((
                BrowserKind::Chromium,
                r.join(r"Chromium\Application\chrome.exe"),
            ));
        }
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        let names = [
            (BrowserKind::Chrome, "google-chrome"),
            (BrowserKind::Chrome, "google-chrome-stable"),
            (BrowserKind::Edge, "microsoft-edge"),
            (BrowserKind::Edge, "microsoft-edge-stable"),
            (BrowserKind::Chromium, "chromium"),
            (BrowserKind::Chromium, "chromium-browser"),
        ];
        let path = std::env::var_os("PATH").unwrap_or_default();
        let mut dirs: Vec<PathBuf> = std::env::split_paths(&path).collect();
        dirs.push(PathBuf::from("/snap/bin"));
        for (k, n) in names {
            for d in &dirs {
                v.push((k, d.join(n)));
            }
        }
    }
    v
}

/// Parse "Google Chrome 131.0.6778.85" / "Chromium 120.0.6099.71 snap" → "131.0.6778.85".
pub fn parse_version(s: &str) -> Option<String> {
    s.split_whitespace()
        .find(|w| {
            let mut parts = w.split('.');
            parts.clone().count() >= 2
                && parts.all(|p| !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit()))
        })
        .map(String::from)
}

pub fn major(version: &str) -> Option<u32> {
    version.split('.').next()?.parse().ok()
}

pub fn supported(version: &str) -> bool {
    major(version).is_some_and(|m| m >= MIN_MAJOR)
}

/// The version of the browser at `path`. On Windows `chrome.exe --version` opens a window
/// instead of printing, so the version-named sibling directory is used.
pub async fn version_of(path: &Path) -> Option<String> {
    if cfg!(windows) {
        let dir = path.parent()?;
        let mut best: Option<String> = None;
        for e in std::fs::read_dir(dir).ok()?.flatten() {
            let name = e.file_name().to_string_lossy().into_owned();
            if e.path().is_dir()
                && let Some(v) = parse_version(&name)
                && best
                    .as_deref()
                    .is_none_or(|b| version_key(&v) > version_key(b))
            {
                best = Some(v);
            }
        }
        return best;
    }
    let spec = PipeSpec::new(path.as_os_str())
        .args(["--version"])
        .env(Env::inherit());
    let r = run(
        spec,
        None,
        OutputCaps::default(),
        Some(Duration::from_secs(10)),
        &CancellationToken::new(),
    )
    .await
    .ok()?;
    parse_version(&r.output.text())
}

fn version_key(v: &str) -> Vec<u32> {
    v.split('.').filter_map(|p| p.parse().ok()).collect()
}

/// Every installed browser (deduplicated by path), with its version.
pub async fn find_installed() -> Vec<InstalledBrowser> {
    let mut out: Vec<InstalledBrowser> = Vec::new();
    for (kind, path) in candidates() {
        if !path.is_file() {
            continue;
        }
        let canon = std::fs::canonicalize(&path).unwrap_or_else(|_| path.clone());
        let p = canon.to_string_lossy().into_owned();
        if out.iter().any(|b| b.path == p) {
            continue;
        }
        let version = version_of(&path).await.unwrap_or_default();
        out.push(InstalledBrowser {
            kind,
            version,
            path: p,
        });
    }
    out
}

/// The browser to drive: the first supported one, honouring a preferred kind.
pub fn pick(
    installed: &[InstalledBrowser],
    prefer: Option<BrowserKind>,
) -> Option<&InstalledBrowser> {
    let ok = |b: &&InstalledBrowser| supported(&b.version);
    prefer
        .and_then(|k| installed.iter().filter(ok).find(|b| b.kind == k))
        .or_else(|| installed.iter().find(ok))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn versions() {
        assert_eq!(
            parse_version("Google Chrome 131.0.6778.85 \n").as_deref(),
            Some("131.0.6778.85")
        );
        assert_eq!(
            parse_version("Chromium 120.0.6099.71 snap").as_deref(),
            Some("120.0.6099.71")
        );
        assert_eq!(
            parse_version("Microsoft Edge 142.0.3595.53").as_deref(),
            Some("142.0.3595.53")
        );
        assert_eq!(parse_version("garbage"), None);
        assert!(supported("120.0.1.2"));
        assert!(!supported("119.9"));
        assert!(!supported(""));
    }

    #[test]
    fn picks_supported_preferred() {
        let b = |kind, v: &str| InstalledBrowser {
            kind,
            version: v.into(),
            path: format!("{kind:?}"),
        };
        let list = vec![
            b(BrowserKind::Chrome, "110.0"),
            b(BrowserKind::Edge, "131.0"),
            b(BrowserKind::Chromium, "125.0"),
        ];
        assert_eq!(pick(&list, None).unwrap().kind, BrowserKind::Edge);
        assert_eq!(
            pick(&list, Some(BrowserKind::Chromium)).unwrap().kind,
            BrowserKind::Chromium
        );
        assert_eq!(
            pick(&list, Some(BrowserKind::Chrome)).unwrap().kind,
            BrowserKind::Edge
        );
    }
}
