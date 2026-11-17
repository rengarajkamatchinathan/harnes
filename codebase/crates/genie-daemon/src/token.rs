//! Per-start daemon token (01 §1.2, SA-02).

use std::io::{self, Write};
use std::path::Path;

use subtle::ConstantTimeEq;

/// 32 random bytes, hex-encoded.
pub fn generate() -> io::Result<String> {
    let mut b = [0u8; 32];
    getrandom::fill(&mut b).map_err(|e| io::Error::other(e.to_string()))?;
    Ok(hex::encode(b))
}

/// Write the token user-only (`0600` on Unix; on Windows the per-user
/// `%LOCALAPPDATA%` ACL applies).
pub fn write(path: &Path, token: &str) -> io::Result<()> {
    let _ = std::fs::remove_file(path);
    let mut opts = std::fs::OpenOptions::new();
    opts.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    let mut f = opts.open(path)?;
    f.write_all(token.as_bytes())?;
    f.sync_all()
}

/// Constant-time comparison (a KB fix for opencode-pty, D45).
pub fn matches(expected: &str, given: &str) -> bool {
    let (a, b) = (expected.as_bytes(), given.as_bytes());
    a.len() == b.len() && bool::from(a.ct_eq(b))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tokens_are_unique_and_compared_exactly() {
        let a = generate().unwrap();
        let b = generate().unwrap();
        assert_eq!(a.len(), 64);
        assert_ne!(a, b);
        assert!(matches(&a, &a.clone()));
        assert!(!matches(&a, &b));
        assert!(!matches(&a, &a[..63]));
        assert!(!matches(&a, ""));
    }
}
