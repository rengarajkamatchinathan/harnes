//! On-device scrubbing for anything that leaves the machine or is pasted into a support
//! ticket: crash reports (08 §7, D39), `diagnostics.collect` (D34) and log excerpts.
//!
//! Removes credentials (key-shaped tokens, `Bearer …`, `key=value` secrets, URL user info),
//! e-mail addresses and the user's home directory. [`Scrubber::strict`] also drops every
//! path under the home directory (crash reports must not carry paths in user dirs).

use std::sync::LazyLock;

use regex::Regex;

static SECRETS: LazyLock<Vec<(Regex, &'static str)>> = LazyLock::new(|| {
    let r = |p: &str| Regex::new(p).ok();
    [
        // Authorization headers and bearer tokens.
        (r(r"(?i)\b(bearer|basic)\s+[A-Za-z0-9._~+/=-]{8,}"), "$1 <redacted>"),
        // key=value / key: value secrets.
        (
            r(r#"(?i)\b([a-z0-9_.-]*(?:api[_-]?key|secret|token|password|passphrase|passwd|pwd|cookie|authorization|sig|signature)[a-z0-9_-]*)(["']?\s*[=:]\s*["']?)[^\s"'&,;}]+"#),
            "$1$2<redacted>",
        ),
        // Provider key shapes (sk-…, sk-ant-…, AIza…, ghp_…, xox…).
        (
            r(r"\b(?:sk-[A-Za-z0-9_-]{12,}|AIza[0-9A-Za-z_-]{20,}|gh[pousr]_[A-Za-z0-9]{20,}|xox[abpr]-[A-Za-z0-9-]{10,}|eyJ[A-Za-z0-9_-]{10,}\.[A-Za-z0-9._-]{10,})"),
            "<redacted>",
        ),
        // URL user info.
        (r(r"(?i)\b([a-z][a-z0-9+.-]*://)[^/\s@:]+(?::[^/\s@]*)?@"), "$1<redacted>@"),
        // E-mail addresses.
        (r(r"\b[A-Za-z0-9._%+-]+@[A-Za-z0-9.-]+\.[A-Za-z]{2,}\b"), "<email>"),
        // Long opaque runs (hex / base64) that look like keys.
        (r(r"\b[A-Fa-f0-9]{40,}\b"), "<redacted>"),
        (r(r"\b[A-Za-z0-9+/]{48,}={0,2}"), "<redacted>"),
    ]
    .into_iter()
    .filter_map(|(re, rep)| re.map(|re| (re, rep)))
    .collect()
});

/// Scrubs text for a given home directory.
#[derive(Debug, Clone, Default)]
pub struct Scrubber {
    homes: Vec<String>,
    user: Option<String>,
    strict: bool,
}

impl Scrubber {
    /// Uses `HOME` / `USERPROFILE` and the OS account name.
    pub fn from_env() -> Scrubber {
        let home = std::env::var("HOME")
            .or_else(|_| std::env::var("USERPROFILE"))
            .ok();
        let user = std::env::var("USER")
            .or_else(|_| std::env::var("USERNAME"))
            .ok();
        Scrubber::new(home.as_deref(), user.as_deref())
    }

    pub fn new(home: Option<&str>, user: Option<&str>) -> Scrubber {
        let mut homes = Vec::new();
        if let Some(h) = home.map(|h| h.trim_end_matches(['/', '\\'])).filter(|h| h.len() > 1) {
            homes.push(h.to_string());
            // The other separator style (paths in logs written by another tool).
            let alt = if h.contains('\\') {
                h.replace('\\', "/")
            } else {
                h.replace('/', "\\")
            };
            if alt != h {
                homes.push(alt);
            }
        }
        Scrubber {
            homes,
            user: user.filter(|u| u.len() >= 3).map(str::to_string),
            strict: false,
        }
    }

    /// Also drop the rest of any path under the home directory.
    pub fn strict(mut self) -> Scrubber {
        self.strict = true;
        self
    }

    pub fn scrub(&self, text: &str) -> String {
        let mut s = text.to_string();
        for (re, rep) in SECRETS.iter() {
            if re.is_match(&s) {
                s = re.replace_all(&s, *rep).into_owned();
            }
        }
        for h in &self.homes {
            if self.strict {
                let re = Regex::new(&format!(r"{}[^\s'\x22]*", regex::escape(h)));
                if let Ok(re) = re {
                    s = re.replace_all(&s, "~/<path>").into_owned();
                }
            } else {
                s = s.replace(h.as_str(), "~");
            }
        }
        if let Some(u) = &self.user
            && let Ok(re) = Regex::new(&format!(r"\b{}\b", regex::escape(u)))
        {
            s = re.replace_all(&s, "<user>").into_owned();
        }
        s
    }
}

/// The last components of a source path (`…/crates/genie-core/src/session.rs` →
/// `genie-core/src/session.rs`): the "anonymised module path" of a crash frame.
pub fn module_path(file: &str) -> String {
    let parts: Vec<&str> = file.split(['/', '\\']).filter(|p| !p.is_empty()).collect();
    let n = parts.len();
    parts[n.saturating_sub(3)..].join("/")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scrubs_secrets_paths_and_people() {
        let s = Scrubber::new(Some("/Users/alice"), Some("alice"));
        let out = s.scrub(
            "open /Users/alice/proj/a.rs failed; key sk-ant-api03-abcdefghijklmnop; \
             Authorization: Bearer abc.def.ghijkl; api_key=XYZ123; url https://bob:pw@h.example/x; \
             mail alice@example.com; user alice; hash 0123456789abcdef0123456789abcdef01234567",
        );
        assert!(out.contains("open ~/proj/a.rs"), "{out}");
        for leaked in [
            "sk-ant",
            "abc.def",
            "XYZ123",
            "bob:pw",
            "alice@",
            "0123456789abcdef0123",
        ] {
            assert!(!out.contains(leaked), "{leaked} in {out}");
        }
        assert!(out.contains("user <user>"), "{out}");

        let strict = Scrubber::new(Some(r"C:\Users\alice"), None).strict();
        let out = strict.scrub(r"at C:\Users\alice\secret-client\main.rs:3 and C:/Users/alice/x");
        assert!(!out.contains("secret-client") && !out.contains("/x"), "{out}");
    }

    #[test]
    fn module_paths() {
        assert_eq!(
            module_path("/home/bob/genie/crates/genie-core/src/session.rs"),
            "genie-core/src/session.rs"
        );
        assert_eq!(module_path(r"C:\a\b\c\d.rs"), "b/c/d.rs");
        assert_eq!(module_path("x.rs"), "x.rs");
    }
}
