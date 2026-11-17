//! Path normalisation and glob matching for `Read(...)` / `Edit(...)` rules (06 §5.1).
//!
//! Paths are compared as `/`-separated strings after lexical normalisation (`.` and `..`
//! resolved, `\` → `/`). On Windows (or whenever the evaluator says so) the comparison is
//! case-insensitive.

/// A normalised absolute (or drive-rooted) path: `/`-separated, no `.`/`..`, optionally
/// lower-cased.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NormPath(String);

impl NormPath {
    pub fn new(path: &str, case_insensitive: bool) -> NormPath {
        NormPath(normalize(path, case_insensitive))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// `self` is `base` or lies beneath it (segment-wise, not string prefix).
    pub fn starts_with(&self, base: &NormPath) -> bool {
        let b = base.0.trim_end_matches('/');
        if b.is_empty() {
            // `base` is the filesystem root.
            return self.0.starts_with('/');
        }
        self.0 == b || (self.0.starts_with(b) && self.0[b.len()..].starts_with('/'))
    }

    /// Parent directory (or itself at the root).
    pub fn parent(&self) -> NormPath {
        match self.0.rfind('/') {
            Some(0) => NormPath("/".into()),
            Some(i) if self.0[..i].ends_with(':') => NormPath(self.0[..=i].to_string()),
            Some(i) => NormPath(self.0[..i].to_string()),
            None => self.clone(),
        }
    }

    pub fn join(&self, rel: &str, case_insensitive: bool) -> NormPath {
        NormPath::new(&format!("{}/{}", self.0, rel), case_insensitive)
    }
}

fn is_drive(s: &str) -> bool {
    let b = s.as_bytes();
    b.len() >= 2 && b[0].is_ascii_alphabetic() && b[1] == b':'
}

/// True for `/x`, `\x`, `C:\x`, `C:/x`.
pub fn is_absolute(p: &str) -> bool {
    p.starts_with('/') || p.starts_with('\\') || is_drive(p)
}

/// Lexical normalisation; never touches the filesystem.
pub fn normalize(path: &str, case_insensitive: bool) -> String {
    let s = path.replace('\\', "/");
    let (prefix, rest) = if is_drive(&s) {
        (format!("{}/", &s[..2]), s[2..].to_string())
    } else if let Some(r) = s.strip_prefix('/') {
        ("/".to_string(), r.to_string())
    } else {
        (String::new(), s)
    };
    let mut segs: Vec<&str> = Vec::new();
    for seg in rest.split('/') {
        match seg {
            "" | "." => {}
            ".." => {
                if segs.last().is_some_and(|l| *l != "..") {
                    segs.pop();
                } else if prefix.is_empty() {
                    segs.push("..");
                }
            }
            s => segs.push(s),
        }
    }
    let mut out = prefix;
    out.push_str(&segs.join("/"));
    if case_insensitive {
        out = out.to_lowercase();
    }
    out
}

fn has_meta(s: &str) -> bool {
    s.contains(['*', '?'])
}

/// Match a normalised path against a normalised glob pattern. `**` spans directories,
/// `*` and `?` stay inside one segment. A pattern without wildcards also matches anything
/// beneath it (a directory rule).
pub fn glob_match(pattern: &str, path: &str) -> bool {
    let pat = collapse(pattern.split('/').collect());
    let segs: Vec<&str> = path.split('/').collect();
    if !has_meta(pattern) {
        let mut dir = pat.clone();
        dir.push("**");
        return match_segs(&pat, &segs) || match_segs(&dir, &segs);
    }
    match_segs(&pat, &segs)
}

fn collapse(mut v: Vec<&str>) -> Vec<&str> {
    v.dedup_by(|a, b| *a == "**" && *b == "**");
    // A trailing empty segment comes from `dir/`: everything beneath it.
    if v.len() > 1 && v.last() == Some(&"") {
        v.pop();
        v.push("**");
    }
    v
}

fn match_segs(pat: &[&str], path: &[&str]) -> bool {
    match pat.split_first() {
        None => path.is_empty(),
        Some((&"**", rest)) => (0..=path.len()).any(|i| match_segs(rest, &path[i..])),
        Some((p, rest)) => !path.is_empty() && wildcard(p, path[0]) && match_segs(rest, &path[1..]),
    }
}

/// `*` (any run) / `?` (one char) wildcard match over the whole text.
pub fn wildcard(pattern: &str, text: &str) -> bool {
    let p: Vec<char> = pattern.chars().collect();
    let t: Vec<char> = text.chars().collect();
    let (mut pi, mut ti) = (0usize, 0usize);
    let (mut star, mut mark) = (None::<usize>, 0usize);
    while ti < t.len() {
        if pi < p.len() && (p[pi] == '?' || p[pi] == t[ti]) {
            pi += 1;
            ti += 1;
        } else if pi < p.len() && p[pi] == '*' {
            star = Some(pi);
            mark = ti;
            pi += 1;
        } else if let Some(s) = star {
            pi = s + 1;
            mark += 1;
            ti = mark;
        } else {
            return false;
        }
    }
    while pi < p.len() && p[pi] == '*' {
        pi += 1;
    }
    pi == p.len()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalises() {
        assert_eq!(normalize("/a/./b/../c/", false), "/a/c");
        assert_eq!(normalize(r"C:\Users\Me\..\X", true), "c:/users/x");
        assert_eq!(normalize("/", false), "/");
        assert_eq!(normalize("/..", false), "/");
        assert_eq!(normalize("a/../../b", false), "../b");
    }

    #[test]
    fn starts_with_is_segment_wise() {
        let base = NormPath::new("/repo", false);
        assert!(NormPath::new("/repo/x", false).starts_with(&base));
        assert!(NormPath::new("/repo", false).starts_with(&base));
        assert!(!NormPath::new("/repository", false).starts_with(&base));
        assert!(NormPath::new("/x", false).starts_with(&NormPath::new("/", false)));
        assert_eq!(NormPath::new("/a/b", false).parent().as_str(), "/a");
        assert_eq!(NormPath::new("/a", false).parent().as_str(), "/");
        assert_eq!(NormPath::new("c:/a", false).parent().as_str(), "c:/");
    }

    #[test]
    fn globs() {
        let cases = [
            ("/repo/src/**", "/repo/src/a/b.rs", true),
            ("/repo/src/**", "/repo/src", true),
            ("/repo/src/*.rs", "/repo/src/a.rs", true),
            ("/repo/src/*.rs", "/repo/src/a/b.rs", false),
            ("/repo/**/*.env", "/repo/a/b/x.env", true),
            ("/repo/**/.env", "/repo/.env", true),
            ("/repo/docs", "/repo/docs/x.md", true),
            ("/repo/docs", "/repo/docsx", false),
            ("/repo/src/", "/repo/src/x", true),
            ("c:/work/**", "c:/work/a", true),
            ("/**", "/anything/at/all", true),
        ];
        for (p, path, want) in cases {
            assert_eq!(glob_match(p, path), want, "{p} vs {path}");
        }
    }

    #[test]
    fn wildcards() {
        assert!(wildcard("git *", "git push origin"));
        assert!(wildcard("a?c", "abc"));
        assert!(!wildcard("a*d", "abc"));
        assert!(wildcard("*", ""));
    }
}
