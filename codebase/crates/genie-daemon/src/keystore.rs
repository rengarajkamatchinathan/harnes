//! TODO(spec 08): PLACEHOLDER keychain for the store's data key.
//!
//! The real `KeyStore` is the OS keychain (or `secrets.enc` with Argon2id) from
//! `genie-secrets`, owned by spec 08. Until it lands the daemon keeps the data key in a
//! user-only (0600) file in the channel data dir so the encrypted DB survives restarts.
//! This gives **no protection at rest** beyond file permissions and must be replaced
//! before release; the key is migrated into the keychain by spec 08's first run.

use std::path::PathBuf;
use std::sync::Mutex;

use genie_store::{KeyStore, StoreError};
use zeroize::Zeroizing;

pub const PLACEHOLDER_FILE: &str = "keystore.placeholder.json";

pub struct PlaceholderKeyStore {
    path: PathBuf,
    lock: Mutex<()>,
}

impl PlaceholderKeyStore {
    pub fn new(data_dir: &std::path::Path) -> Self {
        PlaceholderKeyStore {
            path: data_dir.join(PLACEHOLDER_FILE),
            lock: Mutex::new(()),
        }
    }

    fn load(&self) -> Result<serde_json::Map<String, serde_json::Value>, StoreError> {
        match std::fs::read(&self.path) {
            Ok(b) => {
                let v: serde_json::Value = serde_json::from_slice(&b)?;
                Ok(v.as_object().cloned().unwrap_or_default())
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Default::default()),
            Err(e) => Err(e.into()),
        }
    }

    fn save(&self, m: serde_json::Map<String, serde_json::Value>) -> Result<(), StoreError> {
        if let Some(dir) = self.path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let tmp = self.path.with_extension("tmp");
        let bytes = serde_json::to_vec(&serde_json::Value::Object(m))?;
        {
            let mut o = std::fs::OpenOptions::new();
            o.write(true).create(true).truncate(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                o.mode(0o600);
            }
            use std::io::Write;
            let mut f = o.open(&tmp)?;
            f.write_all(&bytes)?;
            f.sync_all()?;
        }
        std::fs::rename(&tmp, &self.path)?;
        Ok(())
    }
}

impl KeyStore for PlaceholderKeyStore {
    fn get(&self, account: &str) -> Result<Option<Zeroizing<Vec<u8>>>, StoreError> {
        let _g = self.lock.lock().unwrap_or_else(|p| p.into_inner());
        let m = self.load()?;
        match m.get(account).and_then(|v| v.as_str()) {
            Some(h) => hex::decode(h)
                .map(|b| Some(Zeroizing::new(b)))
                .map_err(|e| StoreError::KeyStore(format!("placeholder keystore: {e}"))),
            None => Ok(None),
        }
    }

    fn set(&self, account: &str, secret: &[u8]) -> Result<(), StoreError> {
        let _g = self.lock.lock().unwrap_or_else(|p| p.into_inner());
        let mut m = self.load()?;
        m.insert(account.to_string(), hex::encode(secret).into());
        self.save(m)
    }

    fn delete(&self, account: &str) -> Result<(), StoreError> {
        let _g = self.lock.lock().unwrap_or_else(|p| p.into_inner());
        let mut m = self.load()?;
        m.remove(account);
        self.save(m)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_and_survives_reopen() {
        let dir = tempfile::tempdir().unwrap();
        let k = PlaceholderKeyStore::new(dir.path());
        assert!(k.get("data-key").unwrap().is_none());
        k.set("data-key", &[1, 2, 3]).unwrap();
        let k2 = PlaceholderKeyStore::new(dir.path());
        assert_eq!(k2.get("data-key").unwrap().unwrap().as_slice(), [1, 2, 3]);
        k2.delete("data-key").unwrap();
        assert!(k.get("data-key").unwrap().is_none());
    }
}
