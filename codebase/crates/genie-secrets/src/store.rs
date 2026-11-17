//! Provider credentials kept in a [`SecretBackend`] (03 §10.1).
//!
//! * `credentials` account: the index (ids, providers, labels, kinds, hints, the active
//!   id per provider). It holds no secret but lives in the same backend so Settings and
//!   the secrets travel together and nothing lands in plain files (D23).
//! * `credential:<id>` account: the secret (an API key, or the OAuth grant JSON).
//!
//! The index is cached in memory after the first successful read; a locked backend
//! (passphrase file) reports [`SecretsError::Locked`] until it is unlocked.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use secrecy::{ExposeSecret, SecretString};
use serde::{Deserialize, Serialize};
use zeroize::Zeroizing;

use crate::backend::{BackendKind, SecretBackend, SecretsError};
use crate::credentials::{
    CredentialKind, CredentialMeta, CredentialSource, CredentialStore, ResolvedCredential,
};

pub const INDEX_ACCOUNT: &str = "credentials";

pub fn secret_account(id: &str) -> String {
    format!("credential:{id}")
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct Item {
    id: String,
    provider: String,
    label: String,
    kind: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    hint: Option<String>,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
struct Index {
    #[serde(default)]
    items: Vec<Item>,
    #[serde(default)]
    active: BTreeMap<String, String>,
}

fn kind_str(k: CredentialKind) -> &'static str {
    match k {
        CredentialKind::Key => "key",
        CredentialKind::OAuth => "oauth",
        CredentialKind::External => "external",
    }
}

fn kind_of(s: &str) -> CredentialKind {
    match s {
        "oauth" => CredentialKind::OAuth,
        "external" => CredentialKind::External,
        _ => CredentialKind::Key,
    }
}

pub struct BackendStore {
    backend: Arc<dyn SecretBackend>,
    cache: Mutex<Option<Index>>,
}

impl std::fmt::Debug for BackendStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BackendStore")
            .field("backend", &self.backend.kind())
            .finish_non_exhaustive()
    }
}

/// A fresh credential id: `cred_` + 20 hex chars.
pub fn new_id() -> Result<String, SecretsError> {
    let mut b = [0u8; 10];
    getrandom::fill(&mut b).map_err(|e| SecretsError::Backend(format!("OS random source: {e}")))?;
    Ok(format!(
        "cred_{}",
        b.iter().map(|x| format!("{x:02x}")).collect::<String>()
    ))
}

impl BackendStore {
    pub fn new(backend: Arc<dyn SecretBackend>) -> Self {
        Self {
            backend,
            cache: Mutex::new(None),
        }
    }

    pub fn backend(&self) -> &Arc<dyn SecretBackend> {
        &self.backend
    }

    fn source(&self) -> CredentialSource {
        match self.backend.kind() {
            BackendKind::Keychain => CredentialSource::Keychain,
            BackendKind::File => CredentialSource::EncryptedFile,
            BackendKind::Memory => CredentialSource::Memory,
        }
    }

    fn meta(&self, i: &Item) -> CredentialMeta {
        CredentialMeta {
            id: i.id.clone(),
            provider: i.provider.clone(),
            label: i.label.clone(),
            kind: kind_of(&i.kind),
            source: self.source(),
            hint: i.hint.clone(),
        }
    }

    fn with_index<T>(
        &self,
        f: impl FnOnce(&mut Index) -> Result<(T, bool), SecretsError>,
    ) -> Result<T, SecretsError> {
        let mut g = self.cache.lock().unwrap_or_else(|p| p.into_inner());
        if g.is_none() {
            let idx = match self.backend.get(INDEX_ACCOUNT)? {
                Some(b) => serde_json::from_slice(&b)
                    .map_err(|e| SecretsError::Format(format!("credential index: {e}")))?,
                None => Index::default(),
            };
            *g = Some(idx);
        }
        let Some(idx) = g.as_mut() else {
            return Err(SecretsError::Backend("credential index unavailable".into()));
        };
        let mut work = idx.clone();
        let (out, changed) = f(&mut work)?;
        if changed {
            let bytes =
                serde_json::to_vec(&work).map_err(|e| SecretsError::Format(e.to_string()))?;
            self.backend.set(INDEX_ACCOUNT, &bytes)?;
            *idx = work;
        }
        Ok(out)
    }

    /// Drop the cached index (after an unlock, or in tests).
    pub fn reload(&self) {
        *self.cache.lock().unwrap_or_else(|p| p.into_inner()) = None;
    }

    /// All credentials of every provider.
    pub fn all(&self) -> Result<Vec<CredentialMeta>, SecretsError> {
        self.with_index(|i| Ok((i.items.iter().map(|x| self.meta(x)).collect(), false)))
    }

    pub fn try_list(&self, provider: &str) -> Result<Vec<CredentialMeta>, SecretsError> {
        self.with_index(|i| {
            Ok((
                i.items
                    .iter()
                    .filter(|x| x.provider == provider)
                    .map(|x| self.meta(x))
                    .collect(),
                false,
            ))
        })
    }

    pub fn try_active(&self, provider: &str) -> Result<Option<String>, SecretsError> {
        self.with_index(|i| Ok((i.active.get(provider).cloned(), false)))
    }

    pub fn meta_of(&self, id: &str) -> Result<Option<CredentialMeta>, SecretsError> {
        self.with_index(|i| {
            Ok((
                i.items.iter().find(|x| x.id == id).map(|x| self.meta(x)),
                false,
            ))
        })
    }

    pub fn try_get(&self, id: &str) -> Result<Option<ResolvedCredential>, SecretsError> {
        let Some(meta) = self.meta_of(id)? else {
            return Ok(None);
        };
        let Some(raw) = self.backend.get(&secret_account(id))? else {
            return Ok(None);
        };
        let s = Zeroizing::new(
            String::from_utf8(raw.to_vec())
                .map_err(|_| SecretsError::Format("credential is not UTF-8".into()))?,
        );
        Ok(Some(ResolvedCredential {
            meta,
            secret: SecretString::from(s.as_str().to_string()),
        }))
    }

    /// Add a new credential; the first one of a provider becomes active.
    pub fn add(
        &self,
        provider: &str,
        label: &str,
        kind: CredentialKind,
        secret: &SecretString,
        hint: Option<String>,
    ) -> Result<CredentialMeta, SecretsError> {
        let id = new_id()?;
        let meta = CredentialMeta {
            id,
            provider: provider.into(),
            label: label.into(),
            kind,
            source: self.source(),
            hint,
        };
        self.try_put(&meta, secret)?;
        Ok(meta)
    }

    /// Store (or replace) a credential: secret first, then the index.
    pub fn try_put(
        &self,
        meta: &CredentialMeta,
        secret: &SecretString,
    ) -> Result<(), SecretsError> {
        self.backend
            .set(&secret_account(&meta.id), secret.expose_secret().as_bytes())?;
        self.with_index(|i| {
            let item = Item {
                id: meta.id.clone(),
                provider: meta.provider.clone(),
                label: meta.label.clone(),
                kind: kind_str(meta.kind).into(),
                hint: meta.hint.clone(),
            };
            match i.items.iter_mut().find(|x| x.id == meta.id) {
                Some(x) => *x = item,
                None => i.items.push(item),
            }
            i.active
                .entry(meta.provider.clone())
                .or_insert(meta.id.clone());
            Ok(((), true))
        })
    }

    /// Replace only the secret (OAuth token rotation).
    pub fn update_secret(&self, id: &str, secret: &SecretString) -> Result<(), SecretsError> {
        if self.meta_of(id)?.is_none() {
            return Err(SecretsError::Backend(format!("no credential {id}")));
        }
        self.backend
            .set(&secret_account(id), secret.expose_secret().as_bytes())
    }

    pub fn try_set_active(&self, id: &str) -> Result<Option<String>, SecretsError> {
        self.with_index(|i| {
            let Some(p) = i
                .items
                .iter()
                .find(|x| x.id == id)
                .map(|x| x.provider.clone())
            else {
                return Ok((None, false));
            };
            i.active.insert(p.clone(), id.into());
            Ok((Some(p), true))
        })
    }

    /// Delete; when it was active the provider's next credential (if any) takes over.
    /// Returns the provider it belonged to.
    pub fn try_delete(&self, id: &str) -> Result<Option<String>, SecretsError> {
        let provider = self.with_index(|i| {
            let Some(pos) = i.items.iter().position(|x| x.id == id) else {
                return Ok((None, false));
            };
            let item = i.items.remove(pos);
            if i.active.get(&item.provider).is_some_and(|a| a == id) {
                match i.items.iter().find(|x| x.provider == item.provider) {
                    Some(next) => {
                        i.active.insert(item.provider.clone(), next.id.clone());
                    }
                    None => {
                        i.active.remove(&item.provider);
                    }
                }
            }
            Ok((Some(item.provider), true))
        })?;
        if provider.is_some() {
            self.backend.delete(&secret_account(id))?;
        }
        Ok(provider)
    }
}

fn log<T: Default>(what: &str, r: Result<T, SecretsError>) -> T {
    r.unwrap_or_else(|e| {
        tracing::warn!(error = %e, "credential store: {what} failed");
        T::default()
    })
}

impl CredentialStore for BackendStore {
    fn list(&self, provider: &str) -> Vec<CredentialMeta> {
        log("list", self.try_list(provider))
    }
    fn active(&self, provider: &str) -> Option<String> {
        log("active", self.try_active(provider))
    }
    fn get(&self, id: &str) -> Option<ResolvedCredential> {
        log("get", self.try_get(id))
    }
    fn put(&self, meta: CredentialMeta, secret: SecretString) {
        log("put", self.try_put(&meta, &secret));
    }
    fn set_active(&self, provider: &str, id: &str) -> bool {
        match self.meta_of(id) {
            Ok(Some(m)) if m.provider == provider => {
                log("set_active", self.try_set_active(id)).is_some()
            }
            _ => false,
        }
    }
    fn delete(&self, id: &str) -> bool {
        log("delete", self.try_delete(id)).is_some()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::backend::MemoryBackend;
    use crate::credentials::resolve;
    use crate::vault::{FileVault, KdfParams};

    #[test]
    fn add_list_activate_delete_persist() {
        let mem = Arc::new(MemoryBackend::new());
        let s = BackendStore::new(mem.clone());
        let a = s
            .add(
                "openai",
                "Work",
                CredentialKind::Key,
                &SecretString::from("sk-aaaaaaaaaaaa1111"),
                Some("1111".into()),
            )
            .unwrap();
        let b = s
            .add(
                "openai",
                "Home",
                CredentialKind::Key,
                &SecretString::from("sk-bbbbbbbbbbbb2222"),
                None,
            )
            .unwrap();
        assert_eq!(
            s.try_active("openai").unwrap().as_deref(),
            Some(a.id.as_str())
        );
        assert!(CredentialStore::set_active(&s, "openai", &b.id));
        assert!(!CredentialStore::set_active(&s, "anthropic", &b.id));

        // A second store over the same backend sees the same state.
        let s2 = BackendStore::new(mem.clone());
        let env = |_: &str| Some("env".to_string());
        let r = resolve(&s2, "openai", &env).unwrap();
        assert_eq!(r.secret.expose_secret(), "sk-bbbbbbbbbbbb2222");
        assert_eq!(r.meta.source, CredentialSource::Memory);
        assert_eq!(s2.try_list("openai").unwrap().len(), 2);

        // Deleting the active one promotes the remaining one.
        assert_eq!(s2.try_delete(&b.id).unwrap().as_deref(), Some("openai"));
        assert_eq!(
            s2.try_active("openai").unwrap().as_deref(),
            Some(a.id.as_str())
        );
        assert!(mem.get(&secret_account(&b.id)).unwrap().is_none());
        // The index never contains secrets.
        let idx = mem.get(INDEX_ACCOUNT).unwrap().unwrap();
        assert!(!String::from_utf8_lossy(&idx).contains("sk-"));
        assert!(!format!("{s2:?}").contains("sk-"));
    }

    #[test]
    fn locked_vault_reports_locked_then_works() {
        let dir = tempfile::tempdir().unwrap();
        let v = Arc::new(FileVault::at(
            dir.path().join("secrets.enc"),
            KdfParams::insecure_for_tests(),
        ));
        let s = BackendStore::new(v.clone());
        assert!(matches!(s.all(), Err(SecretsError::Locked)));
        assert!(CredentialStore::list(&s, "openai").is_empty());
        v.unlock(&SecretString::from("pw")).unwrap();
        s.reload();
        let m = s
            .add(
                "anthropic",
                "k",
                CredentialKind::Key,
                &SecretString::from("x"),
                None,
            )
            .unwrap();
        assert_eq!(m.source, CredentialSource::EncryptedFile);
        assert_eq!(
            s.try_get(&m.id).unwrap().unwrap().secret.expose_secret(),
            "x"
        );
    }
}
