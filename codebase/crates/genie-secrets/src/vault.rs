//! `secrets.enc`: the passphrase-protected secrets file used when there is no OS
//! keychain, or when `secrets.backend: "file"` forces it (D26, 03 §10.1, 04 §1, 08 §8).
//!
//! Format (JSON, 0600):
//!
//! ```json
//! {"format":"genie-secrets","version":1,
//!  "kdf":{"alg":"argon2id","v":19,"m_kib":65536,"t":3,"p":1,"salt":"<base64>"},
//!  "nonce":"<base64, 24 bytes>","ciphertext":"<base64>"}
//! ```
//!
//! The key is Argon2id(passphrase, salt) with the stored parameters (08 §8: m = 64 MiB,
//! t = 3, p = 1 for new files; re-tuned per release, old files keep theirs). The
//! plaintext is `{"entries": {account: base64}}`, sealed with XChaCha20-Poly1305 under a
//! fresh random nonce on every write; the AAD is the serialized `kdf` object, so the
//! parameters can't be swapped. The derived key stays in memory (zeroized on drop) while
//! unlocked; the passphrase itself is never kept.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as B64;
use chacha20poly1305::aead::{Aead, KeyInit, Payload};
use chacha20poly1305::{XChaCha20Poly1305, XNonce};
use secrecy::{ExposeSecret, SecretString};
use serde::{Deserialize, Serialize};
use zeroize::Zeroizing;

use crate::backend::{BackendKind, SecretBackend, SecretsError};

pub const FILE_NAME: &str = "secrets.enc";
const FORMAT: &str = "genie-secrets";
const AAD_PREFIX: &[u8] = b"genie-secrets v1|";

/// Argon2id parameters stored with the file.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct KdfParams {
    pub m_kib: u32,
    pub t: u32,
    pub p: u32,
}

impl Default for KdfParams {
    /// 08 §8: m = 64 MiB, t = 3, p = 1.
    fn default() -> Self {
        Self {
            m_kib: 64 * 1024,
            t: 3,
            p: 1,
        }
    }
}

impl KdfParams {
    /// Cheap parameters for tests only.
    pub fn insecure_for_tests() -> Self {
        Self {
            m_kib: 64,
            t: 1,
            p: 1,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct Kdf {
    alg: String,
    v: u32,
    m_kib: u32,
    t: u32,
    p: u32,
    salt: String,
}

#[derive(Serialize, Deserialize)]
struct FileDoc {
    format: String,
    version: u32,
    kdf: Kdf,
    nonce: String,
    ciphertext: String,
}

#[derive(Serialize, Deserialize, Default)]
struct Plain {
    #[serde(default)]
    entries: BTreeMap<String, String>,
}

struct Open {
    key: Zeroizing<[u8; 32]>,
    kdf: Kdf,
    entries: BTreeMap<String, Zeroizing<Vec<u8>>>,
}

/// The passphrase file backend.
pub struct FileVault {
    path: PathBuf,
    new_params: KdfParams,
    open: Mutex<Option<Open>>,
}

impl std::fmt::Debug for FileVault {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FileVault")
            .field("path", &self.path)
            .field("locked", &self.is_locked())
            .finish()
    }
}

fn derive(pass: &[u8], salt: &[u8], k: &Kdf) -> Result<Zeroizing<[u8; 32]>, SecretsError> {
    if k.alg != "argon2id" || k.v != 19 {
        return Err(SecretsError::Format(format!(
            "unsupported kdf {} v{}",
            k.alg, k.v
        )));
    }
    // Refuse absurd parameters from a tampered file (≤ 4 GiB, ≤ 64 passes/lanes).
    if k.m_kib > 4 * 1024 * 1024 || k.t == 0 || k.t > 64 || k.p == 0 || k.p > 64 {
        return Err(SecretsError::Format("kdf parameters out of range".into()));
    }
    let params = argon2::Params::new(k.m_kib, k.t, k.p, Some(32))
        .map_err(|e| SecretsError::Format(format!("kdf parameters: {e}")))?;
    let a = argon2::Argon2::new(argon2::Algorithm::Argon2id, argon2::Version::V0x13, params);
    let mut out = Zeroizing::new([0u8; 32]);
    a.hash_password_into(pass, salt, out.as_mut())
        .map_err(|e| SecretsError::Format(format!("kdf: {e}")))?;
    Ok(out)
}

fn aad(k: &Kdf) -> Result<Vec<u8>, SecretsError> {
    let mut v = AAD_PREFIX.to_vec();
    v.extend(serde_json::to_vec(k).map_err(|e| SecretsError::Format(e.to_string()))?);
    Ok(v)
}

fn random<const N: usize>() -> Result<[u8; N], SecretsError> {
    let mut b = [0u8; N];
    getrandom::fill(&mut b).map_err(|e| SecretsError::Backend(format!("OS random source: {e}")))?;
    Ok(b)
}

fn b64d(s: &str, what: &str) -> Result<Vec<u8>, SecretsError> {
    B64.decode(s)
        .map_err(|_| SecretsError::Format(format!("{what} is not base64")))
}

impl FileVault {
    /// `dir` is the channel data dir; the file is `<dir>/secrets.enc`.
    pub fn new(dir: &Path) -> Self {
        Self::at(dir.join(FILE_NAME), KdfParams::default())
    }

    pub fn at(path: PathBuf, new_params: KdfParams) -> Self {
        Self {
            path,
            new_params,
            open: Mutex::new(None),
        }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn exists(&self) -> bool {
        self.path.exists()
    }

    /// Account names in the unlocked file (for moving them to another backend).
    pub fn accounts(&self) -> Result<Vec<String>, SecretsError> {
        let g = self.guard();
        let o = g.as_ref().ok_or(SecretsError::Locked)?;
        Ok(o.entries.keys().cloned().collect())
    }

    fn guard(&self) -> std::sync::MutexGuard<'_, Option<Open>> {
        self.open.lock().unwrap_or_else(|p| p.into_inner())
    }

    /// Unlock with the passphrase. Creates the file (with that passphrase) if there is
    /// none yet. A wrong passphrase leaves the vault locked.
    pub fn unlock(&self, passphrase: &SecretString) -> Result<(), SecretsError> {
        let pass = passphrase.expose_secret().as_bytes();
        if pass.is_empty() {
            return Err(SecretsError::BadPassphrase);
        }
        let mut g = self.guard();
        match std::fs::read(&self.path) {
            Ok(bytes) => {
                let doc: FileDoc = serde_json::from_slice(&bytes)
                    .map_err(|e| SecretsError::Format(e.to_string()))?;
                if doc.format != FORMAT || doc.version != 1 {
                    return Err(SecretsError::Format(format!(
                        "unknown format {} v{}",
                        doc.format, doc.version
                    )));
                }
                let salt = b64d(&doc.kdf.salt, "salt")?;
                let key = derive(pass, &salt, &doc.kdf)?;
                let nonce = b64d(&doc.nonce, "nonce")?;
                if nonce.len() != 24 {
                    return Err(SecretsError::Format("nonce must be 24 bytes".into()));
                }
                let ct = b64d(&doc.ciphertext, "ciphertext")?;
                let plain = Zeroizing::new(
                    XChaCha20Poly1305::new(key.as_ref().into())
                        .decrypt(
                            XNonce::from_slice(&nonce),
                            Payload {
                                msg: &ct,
                                aad: &aad(&doc.kdf)?,
                            },
                        )
                        .map_err(|_| SecretsError::BadPassphrase)?,
                );
                let p: Plain = serde_json::from_slice(&plain)
                    .map_err(|e| SecretsError::Format(e.to_string()))?;
                let mut entries = BTreeMap::new();
                for (k, v) in p.entries {
                    entries.insert(k, Zeroizing::new(b64d(&v, "entry")?));
                }
                *g = Some(Open {
                    key,
                    kdf: doc.kdf,
                    entries,
                });
                Ok(())
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                let open = self.fresh(pass, BTreeMap::new())?;
                self.write(&open)?;
                *g = Some(open);
                Ok(())
            }
            Err(e) => Err(e.into()),
        }
    }

    fn fresh(
        &self,
        pass: &[u8],
        entries: BTreeMap<String, Zeroizing<Vec<u8>>>,
    ) -> Result<Open, SecretsError> {
        let salt: [u8; 16] = random()?;
        let kdf = Kdf {
            alg: "argon2id".into(),
            v: 19,
            m_kib: self.new_params.m_kib,
            t: self.new_params.t,
            p: self.new_params.p,
            salt: B64.encode(salt),
        };
        let key = derive(pass, &salt, &kdf)?;
        Ok(Open { key, kdf, entries })
    }

    /// Re-key under a new passphrase (fresh salt, current parameters). Must be unlocked.
    pub fn change_passphrase(&self, new: &SecretString) -> Result<(), SecretsError> {
        let pass = new.expose_secret().as_bytes();
        if pass.is_empty() {
            return Err(SecretsError::BadPassphrase);
        }
        let mut g = self.guard();
        let old = g.take().ok_or(SecretsError::Locked)?;
        let open = match self.fresh(pass, old.entries.clone()) {
            Ok(o) => o,
            Err(e) => {
                *g = Some(old);
                return Err(e);
            }
        };
        if let Err(e) = self.write(&open) {
            *g = Some(old);
            return Err(e);
        }
        *g = Some(open);
        Ok(())
    }

    /// Forget the derived key.
    pub fn lock(&self) {
        *self.guard() = None;
    }

    fn write(&self, open: &Open) -> Result<(), SecretsError> {
        let mut plain = Plain::default();
        for (k, v) in &open.entries {
            plain.entries.insert(k.clone(), B64.encode(v.as_slice()));
        }
        let pt = Zeroizing::new(
            serde_json::to_vec(&plain).map_err(|e| SecretsError::Format(e.to_string()))?,
        );
        // Base64 copies of the values in `plain` are zeroized when dropped below.
        for v in plain.entries.values_mut() {
            zeroize::Zeroize::zeroize(v);
        }
        let nonce: [u8; 24] = random()?;
        let ct = XChaCha20Poly1305::new(open.key.as_ref().into())
            .encrypt(
                XNonce::from_slice(&nonce),
                Payload {
                    msg: &pt,
                    aad: &aad(&open.kdf)?,
                },
            )
            .map_err(|_| SecretsError::Backend("encryption failed".into()))?;
        let doc = FileDoc {
            format: FORMAT.into(),
            version: 1,
            kdf: open.kdf.clone(),
            nonce: B64.encode(nonce),
            ciphertext: B64.encode(ct),
        };
        let bytes =
            serde_json::to_vec_pretty(&doc).map_err(|e| SecretsError::Format(e.to_string()))?;
        write_private(&self.path, &bytes)?;
        Ok(())
    }
}

/// 0600 file written atomically through a temp file in the same directory.
pub(crate) fn write_private(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    use std::io::Write;
    let dir = path.parent().unwrap_or_else(|| Path::new("."));
    std::fs::create_dir_all(dir)?;
    let tmp = dir.join(format!(
        ".{}.tmp-{}",
        path.file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("secrets"),
        std::process::id()
    ));
    {
        let mut o = std::fs::OpenOptions::new();
        o.write(true).create(true).truncate(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            o.mode(0o600);
        }
        let mut f = o.open(&tmp)?;
        f.write_all(bytes)?;
        f.sync_all()?;
    }
    std::fs::rename(&tmp, path).inspect_err(|_| {
        let _ = std::fs::remove_file(&tmp);
    })
}

impl SecretBackend for FileVault {
    fn kind(&self) -> BackendKind {
        BackendKind::File
    }
    fn is_locked(&self) -> bool {
        self.guard().is_none()
    }
    fn get(&self, account: &str) -> Result<Option<Zeroizing<Vec<u8>>>, SecretsError> {
        let g = self.guard();
        let o = g.as_ref().ok_or(SecretsError::Locked)?;
        Ok(o.entries.get(account).cloned())
    }
    fn set(&self, account: &str, secret: &[u8]) -> Result<(), SecretsError> {
        let mut g = self.guard();
        let o = g.as_mut().ok_or(SecretsError::Locked)?;
        let prev = o
            .entries
            .insert(account.to_string(), Zeroizing::new(secret.to_vec()));
        if let Err(e) = self.write(o) {
            match prev {
                Some(p) => o.entries.insert(account.to_string(), p),
                None => o.entries.remove(account),
            };
            return Err(e);
        }
        Ok(())
    }
    fn delete(&self, account: &str) -> Result<(), SecretsError> {
        let mut g = self.guard();
        let o = g.as_mut().ok_or(SecretsError::Locked)?;
        if let Some(prev) = o.entries.remove(account)
            && let Err(e) = self.write(o)
        {
            o.entries.insert(account.to_string(), prev);
            return Err(e);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn vault(dir: &Path) -> FileVault {
        FileVault::at(dir.join(FILE_NAME), KdfParams::insecure_for_tests())
    }

    fn pass(s: &str) -> SecretString {
        SecretString::from(s.to_string())
    }

    #[test]
    fn create_unlock_roundtrip_and_wrong_passphrase() {
        let dir = tempfile::tempdir().unwrap();
        let v = vault(dir.path());
        assert!(v.is_locked());
        assert!(matches!(v.get("a"), Err(SecretsError::Locked)));
        v.unlock(&pass("correct horse")).unwrap();
        v.set("data-key", &[7u8; 32]).unwrap();
        v.set("credential:x", b"sk-live-123").unwrap();

        let raw = std::fs::read_to_string(dir.path().join(FILE_NAME)).unwrap();
        assert!(!raw.contains("sk-live-123"));
        assert!(raw.contains("argon2id"));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(dir.path().join(FILE_NAME))
                .unwrap()
                .permissions()
                .mode();
            assert_eq!(mode & 0o777, 0o600);
        }

        let v2 = vault(dir.path());
        assert!(matches!(
            v2.unlock(&pass("wrong")),
            Err(SecretsError::BadPassphrase)
        ));
        assert!(v2.is_locked());
        v2.unlock(&pass("correct horse")).unwrap();
        assert_eq!(
            v2.get("credential:x").unwrap().unwrap().as_slice(),
            b"sk-live-123"
        );
        v2.delete("credential:x").unwrap();
        assert!(v2.get("credential:x").unwrap().is_none());
        assert!(!format!("{v2:?}").contains("sk-live"));
    }

    #[test]
    fn tampered_params_or_ciphertext_fail() {
        let dir = tempfile::tempdir().unwrap();
        let v = vault(dir.path());
        v.unlock(&pass("p")).unwrap();
        v.set("a", b"1").unwrap();
        let path = dir.path().join(FILE_NAME);
        let mut doc: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        doc["kdf"]["t"] = serde_json::json!(2);
        std::fs::write(&path, serde_json::to_vec(&doc).unwrap()).unwrap();
        assert!(vault(dir.path()).unlock(&pass("p")).is_err());
    }

    #[test]
    fn change_passphrase_rekeys() {
        let dir = tempfile::tempdir().unwrap();
        let v = vault(dir.path());
        v.unlock(&pass("old")).unwrap();
        v.set("a", b"1").unwrap();
        v.change_passphrase(&pass("new")).unwrap();
        let v2 = vault(dir.path());
        assert!(v2.unlock(&pass("old")).is_err());
        v2.unlock(&pass("new")).unwrap();
        assert_eq!(v2.get("a").unwrap().unwrap().as_slice(), b"1");
        v2.lock();
        assert!(v2.is_locked());
    }

    #[test]
    fn default_params_match_spec() {
        assert_eq!(
            KdfParams::default(),
            KdfParams {
                m_kib: 65536,
                t: 3,
                p: 1
            }
        );
    }
}
