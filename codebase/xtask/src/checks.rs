//! Repo checks that run on every PR (09 §5.1, 08 §9):
//! - `check-unsafe`: `unsafe` only in the crates 08 §9 allows (plus listed deviations),
//!   each with a `// SAFETY:` note.
//! - `check-headers`: files adapted from OpenCode carry the MIT notice (C3, t38).
//! - `secret-scan`: no credentials in tracked files, fixtures or cassettes (t34).

use std::path::{Path, PathBuf};
use std::process::Command;

use regex::Regex;

// ---------------------------------------------------------------------------------------
// unsafe

/// 08 §9: `genie-process` (PTY/Job Object FFI), `genie-secrets` (platform APIs), the Tauri
/// side (`apps/desktop`).
const UNSAFE_CRATES: &[&str] = &["crates/genie-process/", "crates/genie-secrets/", "apps/desktop/"];

/// Deviations that predate the check, each a small OS FFI shim. Listed in README "Known
/// gaps"; moving them into `genie-process` (or a `genie-sys` crate) removes them from here.
const UNSAFE_DEVIATIONS: &[&str] = &[
    "crates/genie-client/src/win.rs",    // Windows SIDs / pipe security descriptors
    "crates/genie-client/src/ensure.rs", // detached spawn (setsid / creation flags)
    "crates/genie-daemon/src/listener.rs", // peer credential check (getuid, pipe client pid)
    "crates/genie-browser/src/transport.rs", // --remote-debugging-pipe fds 3/4
];

/// How many lines above an `unsafe` the `SAFETY:` note may sit.
const SAFETY_WINDOW: usize = 4;

pub fn check_unsafe(root: &Path) -> Result<String, Vec<String>> {
    let re = Regex::new(r"\bunsafe\s*(\{|fn\b|impl\b|extern\b)").map_err(|e| vec![e.to_string()])?;
    let mut errors = vec![];
    let mut sites = 0;
    for dir in ["crates", "apps", "xtask"] {
        walk(&root.join(dir), &["rs"], &mut |path| {
            let rel = rel(root, path);
            // Integration tests are exempt (they may poke the OS directly).
            if rel.contains("/tests/") {
                return;
            }
            let Ok(text) = std::fs::read_to_string(path) else {
                return;
            };
            let lines: Vec<&str> = text.lines().collect();
            for (i, line) in lines.iter().enumerate() {
                // Unit-test modules sit at the end of the file by convention (as in check-spawn).
                let t = line.trim_start();
                if t.starts_with("#[cfg(test)]") || t.starts_with("#[cfg(all(test") {
                    break;
                }
                let code = line.split("//").next().unwrap_or_default();
                if !re.is_match(code) {
                    continue;
                }
                sites += 1;
                let allowed = UNSAFE_CRATES.iter().any(|c| rel.starts_with(c))
                    || UNSAFE_DEVIATIONS.contains(&rel.as_str());
                if !allowed {
                    errors.push(format!(
                        "{rel}:{}: `unsafe` is only allowed in genie-process, genie-secrets and the Tauri side (08 §9)",
                        i + 1
                    ));
                }
                // `unsafe fn`/`unsafe impl` document themselves with `# Safety` docs too.
                let lo = i.saturating_sub(SAFETY_WINDOW);
                let noted = lines[lo..=i]
                    .iter()
                    .any(|l| l.contains("SAFETY") || l.contains("# Safety"));
                if !noted {
                    errors.push(format!(
                        "{rel}:{}: `unsafe` without a `// SAFETY:` note (08 §9)",
                        i + 1
                    ));
                }
            }
        });
    }
    if errors.is_empty() {
        Ok(format!("{sites} unsafe sites, all allowed and annotated"))
    } else {
        Err(errors)
    }
}

// ---------------------------------------------------------------------------------------
// OpenCode MIT headers

pub const DERIVED_LIST: &str = "release/opencode-derived.txt";
const MIT_MARKERS: &[&str] = &["OpenCode", "MIT"];

pub fn check_headers(root: &Path) -> Result<String, Vec<String>> {
    let list = root.join(DERIVED_LIST);
    let text = std::fs::read_to_string(&list)
        .map_err(|e| vec![format!("{}: {e}", list.display())])?;
    let mut errors = vec![];
    let mut n = 0;
    for line in text.lines().map(str::trim) {
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        n += 1;
        let p = root.join(line);
        match std::fs::read_to_string(&p) {
            Ok(src) => {
                let head: String = src.lines().take(15).collect::<Vec<_>>().join("\n");
                if !MIT_MARKERS.iter().all(|m| head.contains(m)) {
                    errors.push(format!(
                        "{line}: adapted from OpenCode but its first 15 lines lack the MIT notice (C3)"
                    ));
                }
            }
            Err(e) => errors.push(format!("{line}: listed in {DERIVED_LIST} but {e}")),
        }
    }
    if errors.is_empty() {
        Ok(format!("{n} OpenCode-derived files carry the MIT notice"))
    } else {
        Err(errors)
    }
}

// ---------------------------------------------------------------------------------------
// Secret scan

pub const SECRET_ALLOW: &str = "release/secret-scan-allow.txt";
/// Inline opt-out for a deliberate fake (e.g. a redaction test): put it on the same line.
pub const INLINE_ALLOW: &str = "secret-scan:allow";

const RULES: &[(&str, &str)] = &[
    ("Anthropic API key", r"sk-ant-[a-zA-Z0-9]{2,8}-[A-Za-z0-9_\-]{40,}"),
    ("OpenAI API key", r"\bsk-(proj-|svcacct-|admin-)?[A-Za-z0-9_\-]{20,}T3BlbkFJ[A-Za-z0-9_\-]{20,}"),
    ("OpenAI project key", r"\bsk-proj-[A-Za-z0-9_\-]{80,}"),
    ("Google API key", r"\bAIza[0-9A-Za-z_\-]{35}\b"),
    ("Google OAuth secret", r"\bGOCSPX-[0-9A-Za-z_\-]{28}\b"),
    ("AWS access key", r"\b(AKIA|ASIA)[0-9A-Z]{16}\b"),
    ("GitHub token", r"\b(ghp|gho|ghu|ghs|ghr)_[A-Za-z0-9]{36}\b|\bgithub_pat_[A-Za-z0-9_]{60,}"),
    ("Slack token", r"\bxox[abposr]-[0-9A-Za-z\-]{10,}"),
    ("Azure storage key", r"AccountKey=[A-Za-z0-9+/]{80,}={0,2}"),
    ("Azure SAS signature", r"[?&]sig=[A-Za-z0-9%]{40,}"),
    // At line start (optionally quoted), so prose like "`-----BEGIN PRIVATE KEY-----` → DER" passes.
    ("Private key block", r#"^\s*(r?#*["'])?(\\n)?-----BEGIN ((RSA|EC|DSA|OPENSSH|ENCRYPTED) )?PRIVATE KEY-----"#),
    ("Bearer token in a recording", r"(?i)authorization[\x22']?\s*[:=]\s*[\x22']?bearer\s+[A-Za-z0-9._\-]{40,}"),
    ("x-api-key header in a recording", r"(?i)x-(goog-)?api-key[\x22']?\s*[:=]\s*[\x22']?[A-Za-z0-9_\-]{30,}"),
];

/// Text extensions we read; binaries (images, archives) are skipped.
const SKIP_EXT: &[&str] = &[
    "png", "jpg", "jpeg", "gif", "ico", "icns", "webp", "pdf", "zip", "gz", "woff", "woff2", "ttf",
    "otf", "db", "sqlite", "wasm", "lock",
];

pub fn secret_scan(root: &Path) -> Result<String, Vec<String>> {
    let rules: Vec<(&str, Regex)> = RULES
        .iter()
        .map(|(n, r)| Regex::new(r).map(|re| (*n, re)))
        .collect::<Result<_, _>>()
        .map_err(|e| vec![e.to_string()])?;
    let allow = load_allow(root);
    let files = tracked_files(root)?;
    let mut errors = vec![];
    let mut scanned = 0;
    for rel in &files {
        if allow.iter().any(|a| path_matches(a, rel)) {
            continue;
        }
        if Path::new(rel)
            .extension()
            .and_then(|e| e.to_str())
            .is_some_and(|e| SKIP_EXT.contains(&e.to_ascii_lowercase().as_str()))
        {
            continue;
        }
        let Ok(bytes) = std::fs::read(root.join(rel)) else {
            continue;
        };
        if bytes.len() > 4 * 1024 * 1024 || bytes.contains(&0) {
            continue;
        }
        scanned += 1;
        let text = String::from_utf8_lossy(&bytes);
        for (i, line) in text.lines().enumerate() {
            if line.contains(INLINE_ALLOW) {
                continue;
            }
            for (name, re) in &rules {
                if re.is_match(line) {
                    errors.push(format!(
                        "{rel}:{}: possible {name} (redact it, or mark a deliberate fake with `{INLINE_ALLOW}`)",
                        i + 1
                    ));
                }
            }
        }
    }
    if errors.is_empty() {
        Ok(format!("secret scan: {scanned} files clean"))
    } else {
        Err(errors)
    }
}

/// Rule names matching `text` (tests).
#[cfg(test)]
pub fn scan_text(text: &str) -> Vec<&'static str> {
    RULES
        .iter()
        .filter(|(_, r)| Regex::new(r).is_ok_and(|re| text.lines().any(|l| !l.contains(INLINE_ALLOW) && re.is_match(l))))
        .map(|(n, _)| *n)
        .collect()
}

fn load_allow(root: &Path) -> Vec<String> {
    std::fs::read_to_string(root.join(SECRET_ALLOW))
        .unwrap_or_default()
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .map(String::from)
        .collect()
}

/// `dir/` prefix or exact path.
fn path_matches(pattern: &str, rel: &str) -> bool {
    if pattern.ends_with('/') {
        rel.starts_with(pattern)
    } else {
        rel == pattern
    }
}

/// Tracked + new (not ignored) files under `root`, relative to it.
fn tracked_files(root: &Path) -> Result<Vec<String>, Vec<String>> {
    let out = Command::new("git")
        .args(["ls-files", "-z", "--cached", "--others", "--exclude-standard", "--", "."])
        .current_dir(root)
        .output();
    match out {
        Ok(o) if o.status.success() => Ok(o
            .stdout
            .split(|b| *b == 0)
            .filter(|s| !s.is_empty())
            .map(|s| String::from_utf8_lossy(s).into_owned())
            .filter(|p| root.join(p).is_file())
            .collect()),
        // No git (e.g. a source tarball): walk the tree instead.
        _ => {
            let mut v = vec![];
            walk(root, &[], &mut |p| v.push(rel(root, p)));
            Ok(v)
        }
    }
}

// ---------------------------------------------------------------------------------------

pub fn rel(root: &Path, p: &Path) -> String {
    p.strip_prefix(root)
        .unwrap_or(p)
        .to_string_lossy()
        .replace('\\', "/")
}

/// Walks `dir`, skipping build output and dependencies. Empty `exts` = every file.
pub fn walk(dir: &Path, exts: &[&str], f: &mut dyn FnMut(&Path)) {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    let mut entries: Vec<PathBuf> = rd.flatten().map(|e| e.path()).collect();
    entries.sort();
    for p in entries {
        if p.is_dir() {
            if p.file_name().is_some_and(|n| {
                n == "target" || n == "node_modules" || n == "dist" || n == ".git"
            }) {
                continue;
            }
            walk(&p, exts, f);
        } else if exts.is_empty()
            || p.extension()
                .and_then(|x| x.to_str())
                .is_some_and(|x| exts.contains(&x))
        {
            f(&p);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rules_catch_real_shapes_and_skip_placeholders() {
        // Built at run time so this file doesn't trip the scan itself.
        let ant = format!("key = \"sk-ant-api03-{}\"", "A".repeat(90));
        assert_eq!(scan_text(&ant), vec!["Anthropic API key"]);
        let aws = format!("{}{}", "AKIA", "ABCDEFGHIJKLMNOP");
        assert_eq!(scan_text(&aws), vec!["AWS access key"]);
        let pem = format!("-----BEGIN {}PRIVATE KEY-----", "RSA ");
        assert_eq!(scan_text(&pem), vec!["Private key block"]);
        let bearer = format!("\"authorization\": \"Bearer {}\"", "x".repeat(50));
        assert_eq!(scan_text(&bearer), vec!["Bearer token in a recording"]);
        assert!(scan_text("authorization: Bearer [REDACTED]").is_empty());
        assert!(scan_text("api_key = \"sk-test\"").is_empty());
        assert!(scan_text(&format!("{ant} // {INLINE_ALLOW}")).is_empty());
    }

    #[test]
    fn allow_patterns() {
        assert!(path_matches("crates/x/tests/", "crates/x/tests/a.rs"));
        assert!(!path_matches("crates/x/tests/a.rs", "crates/x/tests/a.rs.bak"));
    }
}
