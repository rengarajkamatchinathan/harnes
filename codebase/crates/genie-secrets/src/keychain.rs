//! OS keychain backend via `keyring` (D26): macOS Keychain, Windows Credential Manager,
//! Linux Secret Service. One generic-password item per account under the service name
//! `genie` (`genie-beta`, `genie-dev` per channel).
//!
//! Automated tests never use this type (they use [`crate::MemoryBackend`]); the
//! `real_keychain_roundtrip` test is `#[ignore]` and must be run by hand.

use zeroize::Zeroizing;

use crate::backend::{BackendKind, SecretBackend, SecretsError};

pub struct KeyringBackend {
    service: String,
}

/// Account used by [`KeyringBackend::probe`].
const PROBE_ACCOUNT: &str = "genie-probe";

impl KeyringBackend {
    pub fn new(service: impl Into<String>) -> Self {
        Self {
            service: service.into(),
        }
    }

    pub fn service(&self) -> &str {
        &self.service
    }

    fn entry(&self, account: &str) -> Result<keyring::Entry, SecretsError> {
        keyring::Entry::new(&self.service, account).map_err(map_err)
    }

    /// Is a keychain reachable? A missing probe item counts as reachable; no Secret
    /// Service on the bus, a locked/denied keychain or no platform store does not.
    pub fn probe(&self) -> Result<(), SecretsError> {
        match self.entry(PROBE_ACCOUNT)?.get_secret() {
            Ok(_) | Err(keyring::Error::NoEntry) => Ok(()),
            Err(e) => Err(SecretsError::Unavailable(e.to_string())),
        }
    }
}

impl std::fmt::Debug for KeyringBackend {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("KeyringBackend")
            .field("service", &self.service)
            .finish()
    }
}

fn map_err(e: keyring::Error) -> SecretsError {
    match e {
        keyring::Error::NoStorageAccess(_) | keyring::Error::PlatformFailure(_) => {
            SecretsError::Unavailable(e.to_string())
        }
        // Never include the secret: keyring's messages carry only attribute names.
        other => SecretsError::Backend(other.to_string()),
    }
}

impl SecretBackend for KeyringBackend {
    fn kind(&self) -> BackendKind {
        BackendKind::Keychain
    }

    fn get(&self, account: &str) -> Result<Option<Zeroizing<Vec<u8>>>, SecretsError> {
        match self.entry(account)?.get_secret() {
            Ok(v) => Ok(Some(Zeroizing::new(v))),
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(e) => Err(map_err(e)),
        }
    }

    fn set(&self, account: &str, secret: &[u8]) -> Result<(), SecretsError> {
        self.entry(account)?.set_secret(secret).map_err(map_err)
    }

    fn delete(&self, account: &str) -> Result<(), SecretsError> {
        match self.entry(account)?.delete_credential() {
            Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
            Err(e) => Err(map_err(e)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Touches the real OS keychain: run by hand with `cargo test -p genie-secrets --
    /// --ignored real_keychain_roundtrip` (macOS may show an access prompt).
    #[test]
    #[ignore = "uses the real OS keychain"]
    fn real_keychain_roundtrip() {
        let k = KeyringBackend::new("genie-test-real-keychain");
        k.probe().unwrap();
        k.set("roundtrip", &[0, 1, 2, 255]).unwrap();
        assert_eq!(
            k.get("roundtrip").unwrap().unwrap().as_slice(),
            [0, 1, 2, 255]
        );
        k.delete("roundtrip").unwrap();
        assert!(k.get("roundtrip").unwrap().is_none());
        k.delete("roundtrip").unwrap();
    }
}
