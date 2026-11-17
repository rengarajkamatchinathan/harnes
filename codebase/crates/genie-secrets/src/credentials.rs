//! Provider credentials model (03 §10.1): several labelled credentials per provider, one
//! active; an explicitly selected stored credential beats environment variables.
//!
//! Only the model, the store port and an in-memory store live here for now. The keychain
//! (`keyring`) and `secrets.enc` (XChaCha20-Poly1305 + Argon2id) backends are spec 08.

use std::collections::BTreeMap;
use std::sync::Mutex;

use secrecy::SecretString;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CredentialKind {
    Key,
    OAuth,
    /// Lives elsewhere (ADC file, env var); we hold a reference only.
    External,
}

/// Where the value comes from, shown in Settings ("Keychain", "Environment: OPENAI_API_KEY").
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CredentialSource {
    Keychain,
    EncryptedFile,
    Environment(String),
    Memory,
}

impl CredentialSource {
    pub fn label(&self) -> String {
        match self {
            Self::Keychain => "Keychain".into(),
            Self::EncryptedFile => "Encrypted file".into(),
            Self::Environment(v) => format!("Environment: {v}"),
            Self::Memory => "Session".into(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CredentialMeta {
    pub id: String,
    pub provider: String,
    pub label: String,
    pub kind: CredentialKind,
    pub source: CredentialSource,
}

#[derive(Clone, Debug)]
pub struct ResolvedCredential {
    pub meta: CredentialMeta,
    pub secret: SecretString,
}

/// The store port; backends implement it.
pub trait CredentialStore: Send + Sync {
    fn list(&self, provider: &str) -> Vec<CredentialMeta>;
    fn active(&self, provider: &str) -> Option<String>;
    fn get(&self, id: &str) -> Option<ResolvedCredential>;
    fn put(&self, meta: CredentialMeta, secret: SecretString);
    fn set_active(&self, provider: &str, id: &str) -> bool;
    fn delete(&self, id: &str) -> bool;
}

/// Environment variables consulted per provider when nothing is stored.
pub fn env_vars(provider: &str) -> &'static [&'static str] {
    match provider {
        "anthropic" => &["ANTHROPIC_API_KEY"],
        "openai" => &["OPENAI_API_KEY"],
        "azure-foundry" | "azure" => &["AZURE_AI_API_KEY", "AZURE_OPENAI_API_KEY"],
        "google" | "gemini" => &["GEMINI_API_KEY", "GOOGLE_API_KEY"],
        "openrouter" => &["OPENROUTER_API_KEY"],
        _ => &[],
    }
}

/// Precedence (03 §10.1): the active stored credential, else the first set env var.
pub fn resolve(
    store: &dyn CredentialStore,
    provider: &str,
    env: &dyn Fn(&str) -> Option<String>,
) -> Option<ResolvedCredential> {
    if let Some(c) = store.active(provider).and_then(|id| store.get(&id)) {
        return Some(c);
    }
    env_vars(provider).iter().find_map(|var| {
        let v = env(var).filter(|v| !v.trim().is_empty())?;
        Some(ResolvedCredential {
            meta: CredentialMeta {
                id: format!("env:{var}"),
                provider: provider.into(),
                label: var.to_string(),
                kind: CredentialKind::Key,
                source: CredentialSource::Environment(var.to_string()),
            },
            secret: SecretString::from(v),
        })
    })
}

#[derive(Default)]
pub struct MemoryStore {
    /// (credentials by id, active id by provider)
    inner: Mutex<(Creds, BTreeMap<String, String>)>,
}

type Creds = BTreeMap<String, (CredentialMeta, SecretString)>;

impl CredentialStore for MemoryStore {
    fn list(&self, provider: &str) -> Vec<CredentialMeta> {
        self.inner
            .lock()
            .map(|g| {
                g.0.values()
                    .filter(|(m, _)| m.provider == provider)
                    .map(|(m, _)| m.clone())
                    .collect()
            })
            .unwrap_or_default()
    }
    fn active(&self, provider: &str) -> Option<String> {
        self.inner.lock().ok()?.1.get(provider).cloned()
    }
    fn get(&self, id: &str) -> Option<ResolvedCredential> {
        let g = self.inner.lock().ok()?;
        g.0.get(id).map(|(m, s)| ResolvedCredential {
            meta: m.clone(),
            secret: s.clone(),
        })
    }
    fn put(&self, meta: CredentialMeta, secret: SecretString) {
        if let Ok(mut g) = self.inner.lock() {
            let provider = meta.provider.clone();
            let id = meta.id.clone();
            g.0.insert(id.clone(), (meta, secret));
            g.1.entry(provider).or_insert(id);
        }
    }
    fn set_active(&self, provider: &str, id: &str) -> bool {
        let Ok(mut g) = self.inner.lock() else {
            return false;
        };
        if g.0.get(id).is_some_and(|(m, _)| m.provider == provider) {
            g.1.insert(provider.into(), id.into());
            true
        } else {
            false
        }
    }
    fn delete(&self, id: &str) -> bool {
        let Ok(mut g) = self.inner.lock() else {
            return false;
        };
        let removed = g.0.remove(id).is_some();
        g.1.retain(|_, v| v != id);
        removed
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use secrecy::ExposeSecret;

    fn meta(id: &str) -> CredentialMeta {
        CredentialMeta {
            id: id.into(),
            provider: "openai".into(),
            label: id.into(),
            kind: CredentialKind::Key,
            source: CredentialSource::Keychain,
        }
    }

    #[test]
    fn stored_beats_env_and_env_is_fallback() {
        let s = MemoryStore::default();
        let env = |v: &str| (v == "OPENAI_API_KEY").then(|| "env-key".to_string());
        let r = resolve(&s, "openai", &env).unwrap();
        assert_eq!(r.secret.expose_secret(), "env-key");
        assert_eq!(r.meta.source.label(), "Environment: OPENAI_API_KEY");

        s.put(meta("work"), SecretString::from("stored"));
        s.put(meta("home"), SecretString::from("stored2"));
        assert_eq!(
            resolve(&s, "openai", &env).unwrap().secret.expose_secret(),
            "stored"
        );
        assert!(s.set_active("openai", "home"));
        assert!(!s.set_active("anthropic", "home"));
        assert_eq!(resolve(&s, "openai", &env).unwrap().meta.id, "home");
        assert!(s.delete("home"));
        assert_eq!(
            resolve(&s, "openai", &env).unwrap().meta.id,
            "env:OPENAI_API_KEY"
        );
        assert_eq!(s.list("openai").len(), 1);
        assert!(format!("{:?}", r.secret).contains("REDACTED"));
    }
}
