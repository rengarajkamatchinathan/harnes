//! The secret backend port (D26): opaque bytes per account name.

use std::collections::BTreeMap;
use std::sync::Mutex;

use zeroize::Zeroizing;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BackendKind {
    /// OS keychain (`keyring`).
    Keychain,
    /// `secrets.enc` (passphrase).
    File,
    /// Process memory (tests, incognito).
    Memory,
}

impl BackendKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Keychain => "keychain",
            Self::File => "file",
            Self::Memory => "memory",
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum SecretsError {
    /// The passphrase file is not unlocked yet (02 §2 `locked`).
    #[error("the secrets file is locked; unlock it with your passphrase")]
    Locked,
    #[error("wrong passphrase")]
    BadPassphrase,
    #[error("the system keychain is not available: {0}")]
    Unavailable(String),
    #[error("secrets file is damaged: {0}")]
    Format(String),
    #[error("{0}")]
    Backend(String),
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
}

/// Values are raw bytes; callers decide the encoding. Implementations never log values.
pub trait SecretBackend: Send + Sync {
    fn kind(&self) -> BackendKind;
    /// Only the passphrase file can be locked.
    fn is_locked(&self) -> bool {
        false
    }
    fn get(&self, account: &str) -> Result<Option<Zeroizing<Vec<u8>>>, SecretsError>;
    fn set(&self, account: &str, secret: &[u8]) -> Result<(), SecretsError>;
    /// Deleting a missing account is not an error.
    fn delete(&self, account: &str) -> Result<(), SecretsError>;
}

/// In-memory backend for tests and incognito; also stands in for the OS keychain in
/// automated tests (they must never touch the real one).
#[derive(Default)]
pub struct MemoryBackend {
    inner: Mutex<BTreeMap<String, Zeroizing<Vec<u8>>>>,
}

impl MemoryBackend {
    pub fn new() -> Self {
        Self::default()
    }
    /// Account names currently stored (tests).
    pub fn accounts(&self) -> Vec<String> {
        self.inner
            .lock()
            .map(|m| m.keys().cloned().collect())
            .unwrap_or_default()
    }
}

impl std::fmt::Debug for MemoryBackend {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MemoryBackend")
            .field("accounts", &self.accounts())
            .finish()
    }
}

impl SecretBackend for MemoryBackend {
    fn kind(&self) -> BackendKind {
        BackendKind::Memory
    }
    fn get(&self, account: &str) -> Result<Option<Zeroizing<Vec<u8>>>, SecretsError> {
        let g = self
            .inner
            .lock()
            .map_err(|_| SecretsError::Backend("memory backend poisoned".into()))?;
        Ok(g.get(account).cloned())
    }
    fn set(&self, account: &str, secret: &[u8]) -> Result<(), SecretsError> {
        let mut g = self
            .inner
            .lock()
            .map_err(|_| SecretsError::Backend("memory backend poisoned".into()))?;
        g.insert(account.to_string(), Zeroizing::new(secret.to_vec()));
        Ok(())
    }
    fn delete(&self, account: &str) -> Result<(), SecretsError> {
        let mut g = self
            .inner
            .lock()
            .map_err(|_| SecretsError::Backend("memory backend poisoned".into()))?;
        g.remove(account);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn memory_roundtrip_and_debug_hides_values() {
        let m = MemoryBackend::new();
        assert!(m.get("a").unwrap().is_none());
        m.set("a", b"s3cret").unwrap();
        assert_eq!(m.get("a").unwrap().unwrap().as_slice(), b"s3cret");
        assert!(!format!("{m:?}").contains("s3cret"));
        m.delete("a").unwrap();
        m.delete("a").unwrap();
        assert!(m.get("a").unwrap().is_none());
    }
}
