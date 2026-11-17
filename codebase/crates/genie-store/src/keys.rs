//! Data key, keychain port and recovery key (04 §2).
//!
//! The data key (256-bit, random, created on first run) keys SQLCipher and wraps the
//! blob key. It lives in the OS keychain; the keychain itself is spec 08's
//! `genie-secrets`, so this crate only defines the [`KeyStore`] port. `genie-core`
//! injects an adapter (01 §2.1 rule 3); [`MemoryKeyStore`] serves tests.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use data_encoding::BASE32_NOPAD;
use zeroize::Zeroizing;

use crate::crypto::{self, Key};
use crate::error::{Result, StoreError};

/// Keychain account name for the data key.
pub const DATA_KEY_ACCOUNT: &str = "data-key";
/// The wrapped data key next to the DB (SA-16).
pub const RECOVERY_FILE: &str = "recovery.bin";
const RECOVERY_MAGIC: &[u8; 4] = b"GNR1";
const RECOVERY_AAD: &[u8] = b"genie recovery v1";

/// The keychain port. Values are raw key bytes.
pub trait KeyStore: Send + Sync {
    fn get(&self, account: &str) -> Result<Option<Zeroizing<Vec<u8>>>>;
    fn set(&self, account: &str, secret: &[u8]) -> Result<()>;
    fn delete(&self, account: &str) -> Result<()>;
}

/// In-memory keychain for tests and incognito.
#[derive(Default)]
pub struct MemoryKeyStore {
    inner: Mutex<HashMap<String, Zeroizing<Vec<u8>>>>,
}

impl MemoryKeyStore {
    pub fn new() -> Self {
        Self::default()
    }

    fn lock(&self) -> Result<std::sync::MutexGuard<'_, HashMap<String, Zeroizing<Vec<u8>>>>> {
        self.inner
            .lock()
            .map_err(|_| StoreError::KeyStore("memory keystore lock poisoned".into()))
    }
}

impl KeyStore for MemoryKeyStore {
    fn get(&self, account: &str) -> Result<Option<Zeroizing<Vec<u8>>>> {
        Ok(self.lock()?.get(account).cloned())
    }

    fn set(&self, account: &str, secret: &[u8]) -> Result<()> {
        self.lock()?
            .insert(account.to_string(), Zeroizing::new(secret.to_vec()));
        Ok(())
    }

    fn delete(&self, account: &str) -> Result<()> {
        self.lock()?.remove(account);
        Ok(())
    }
}

/// Load the data key, creating it on first run.
///
/// * key present → it.
/// * no key, no DB → a fresh key is generated and stored.
/// * no key, DB exists → [`StoreError::KeyMissing`]; the caller offers recovery
///   ([`restore_data_key`]) or, after the user confirms, [`quarantine_unreadable`].
pub fn load_or_create_data_key(keys: &dyn KeyStore, dir: &Path, db_name: &str) -> Result<Key> {
    if let Some(bytes) = keys.get(DATA_KEY_ACCOUNT)? {
        return Key::from_bytes(&bytes);
    }
    if dir.join(db_name).exists() {
        return Err(StoreError::KeyMissing {
            recovery_available: dir.join(RECOVERY_FILE).exists(),
        });
    }
    let key = Key::generate()?;
    keys.set(DATA_KEY_ACCOUNT, key.expose())?;
    Ok(key)
}

/// A recovery key (SA-16): 256 random bits shown once as Base32 groups.
pub struct RecoveryKey(Key);

impl RecoveryKey {
    /// `XXXX-XXXX-…` (13 groups of 4 Base32 characters).
    pub fn display(&self) -> Zeroizing<String> {
        let b32 = Zeroizing::new(BASE32_NOPAD.encode(self.0.expose()));
        let groups: Vec<&str> = b32
            .as_bytes()
            .chunks(4)
            .filter_map(|c| std::str::from_utf8(c).ok())
            .collect();
        Zeroizing::new(groups.join("-"))
    }

    /// Parse what the user typed: case, spaces and dashes don't matter.
    pub fn parse(s: &str) -> Result<Self> {
        let clean: Zeroizing<String> = Zeroizing::new(
            s.chars()
                .filter(|c| !c.is_whitespace() && *c != '-')
                .map(|c| c.to_ascii_uppercase())
                .collect(),
        );
        let bytes = Zeroizing::new(
            BASE32_NOPAD
                .decode(clean.as_bytes())
                .map_err(|_| StoreError::Format("recovery key is not valid Base32".into()))?,
        );
        Ok(RecoveryKey(Key::from_bytes(&bytes)?))
    }
}

impl std::fmt::Debug for RecoveryKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("RecoveryKey([REDACTED])")
    }
}

/// Create (or replace, which revokes the old one) the recovery key: wraps the data key
/// into `recovery.bin` next to the DB and returns the key to show once.
pub fn create_recovery_key(dir: &Path, data_key: &Key) -> Result<RecoveryKey> {
    let rk = Key::generate()?;
    let sealed = crypto::seal(&rk, data_key.expose(), RECOVERY_AAD)?;
    let mut file = RECOVERY_MAGIC.to_vec();
    file.extend_from_slice(&sealed);
    write_private(&dir.join(RECOVERY_FILE), &file)?;
    Ok(RecoveryKey(rk))
}

/// Unwrap the data key with a recovery key and put it back into the keychain.
pub fn restore_data_key(dir: &Path, recovery: &RecoveryKey, keys: &dyn KeyStore) -> Result<Key> {
    let file = std::fs::read(dir.join(RECOVERY_FILE))?;
    if file.len() < 4 || &file[..4] != RECOVERY_MAGIC {
        return Err(StoreError::Format("recovery.bin has no GNR1 header".into()));
    }
    let raw = Zeroizing::new(crypto::open(&recovery.0, &file[4..], RECOVERY_AAD)?);
    let key = Key::from_bytes(&raw)?;
    keys.set(DATA_KEY_ACCOUNT, key.expose())?;
    Ok(key)
}

/// Key loss without recovery (04 §2): move the DB, its WAL/SHM and the blobs aside as
/// `<name>.unreadable-<date>`. Never deletes. Returns the moved paths.
pub fn quarantine_unreadable(dir: &Path, db_name: &str, date: &str) -> Result<Vec<PathBuf>> {
    let mut moved = vec![];
    for name in [
        db_name.to_string(),
        format!("{db_name}-wal"),
        format!("{db_name}-shm"),
        "blobs".to_string(),
        RECOVERY_FILE.to_string(),
    ] {
        let from = dir.join(&name);
        if from.exists() {
            let to = dir.join(format!("{name}.unreadable-{date}"));
            std::fs::rename(&from, &to)?;
            moved.push(to);
        }
    }
    Ok(moved)
}

/// Write a file readable by the owner only (0600 on Unix), atomically via a temp file.
pub(crate) fn write_private(path: &Path, bytes: &[u8]) -> Result<()> {
    use std::io::Write;
    let dir = path
        .parent()
        .ok_or_else(|| StoreError::Invalid(format!("{} has no parent", path.display())))?;
    std::fs::create_dir_all(dir)?;
    let tmp = dir.join(format!(
        ".{}.tmp-{}",
        path.file_name().and_then(|n| n.to_str()).unwrap_or("file"),
        crate::util::new_id("tmp")
    ));
    {
        let mut opts = std::fs::OpenOptions::new();
        opts.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            opts.mode(0o600);
        }
        let mut f = opts.open(&tmp)?;
        f.write_all(bytes)?;
        f.sync_all()?;
    }
    std::fs::rename(&tmp, path).inspect_err(|_| {
        let _ = std::fs::remove_file(&tmp);
    })?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn first_run_creates_and_reuses_key() {
        let dir = tempfile::tempdir().unwrap();
        let ks = MemoryKeyStore::new();
        let k1 = load_or_create_data_key(&ks, dir.path(), "genie.db").unwrap();
        let k2 = load_or_create_data_key(&ks, dir.path(), "genie.db").unwrap();
        assert_eq!(k1, k2);
    }

    #[test]
    fn key_loss_is_reported_not_overwritten() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("genie.db"), b"x").unwrap();
        let ks = MemoryKeyStore::new();
        let err = load_or_create_data_key(&ks, dir.path(), "genie.db").unwrap_err();
        assert!(matches!(
            err,
            StoreError::KeyMissing {
                recovery_available: false
            }
        ));
        let moved = quarantine_unreadable(dir.path(), "genie.db", "2026-11-17").unwrap();
        assert_eq!(moved.len(), 1);
        assert!(dir.path().join("genie.db.unreadable-2026-11-17").exists());
        assert!(!dir.path().join("genie.db").exists());
    }

    #[test]
    fn recovery_key_roundtrip_and_revoke() {
        let dir = tempfile::tempdir().unwrap();
        let data = Key::generate().unwrap();
        let rk = create_recovery_key(dir.path(), &data).unwrap();
        let shown = rk.display();
        assert_eq!(shown.split('-').count(), 13);
        // Lowercase with spaces still parses.
        let typed = shown.to_lowercase().replace('-', " ");
        let parsed = RecoveryKey::parse(&typed).unwrap();
        let ks = MemoryKeyStore::new();
        let restored = restore_data_key(dir.path(), &parsed, &ks).unwrap();
        assert_eq!(restored, data);
        assert!(ks.get(DATA_KEY_ACCOUNT).unwrap().is_some());

        // Re-creating revokes the old key.
        let _new = create_recovery_key(dir.path(), &data).unwrap();
        assert!(restore_data_key(dir.path(), &parsed, &ks).is_err());

        std::fs::write(dir.path().join("genie.db"), b"x").unwrap();
        let empty = MemoryKeyStore::new();
        assert!(matches!(
            load_or_create_data_key(&empty, dir.path(), "genie.db"),
            Err(StoreError::KeyMissing {
                recovery_available: true
            })
        ));
    }
}
