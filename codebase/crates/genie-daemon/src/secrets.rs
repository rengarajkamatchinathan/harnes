//! Secrets in the daemon (D26, 03 §10.1, 04 §2, 08 §2.5 / §8).
//!
//! * Backend choice: the OS keychain when reachable, else `secrets.enc` (passphrase);
//!   `secrets.backend: "file"` in the user settings forces the file. Tests inject a
//!   backend through [`SecretsChoice`] and never touch the real keychain.
//! * Unlock: `GENIE_PASSPHRASE` at start, else `InitializeResult.locked` +
//!   `secrets.unlock` from a client. While locked the daemon runs a small *locked phase*
//!   ([`locked_phase`]) that answers only the handshake, `secrets.unlock` and status
//!   methods; after the unlock it tells clients `daemon.restarting` and starts the full
//!   engine with the store's data key (clients reconnect through `ensure`).
//! * [`SecretsKeyStore`] adapts the backend to `genie_store::KeyStore` (the DB key).
//! * [`migrate_placeholder`] moves a key from the old `keystore.placeholder.json` into
//!   the backend once, then deletes the file.

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use genie_client::Endpoint;
use genie_core::{EngineBuilder, TaskScope};
use genie_protocol::handshake::LockedInfo;
use genie_secrets::keychain::KeyringBackend;
use genie_secrets::vault::{FileVault, KdfParams};
use genie_secrets::{BackendKind, SecretBackend, SecretString, SecretsError};
use genie_store::StoreError;
use zeroize::Zeroizing;

use crate::server::{DaemonState, ShutdownReason};
use crate::{DaemonConfig, DaemonError, Registry, Services};

/// Which secrets backend the daemon uses.
#[derive(Clone, Default)]
pub enum SecretsChoice {
    /// Keychain if reachable, else `secrets.enc`; `secrets.backend: "file"` forces the file.
    #[default]
    Auto,
    /// Always `secrets.enc` in the data dir, with these KDF parameters for a new file.
    File(KdfParams),
    /// A given backend (tests: a `MemoryBackend` standing in for the keychain).
    Backend(Arc<dyn SecretBackend>),
}

impl std::fmt::Debug for SecretsChoice {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Auto => f.write_str("Auto"),
            Self::File(p) => f.debug_tuple("File").field(p).finish(),
            Self::Backend(b) => f.debug_tuple("Backend").field(&b.kind()).finish(),
        }
    }
}

/// Env var with the passphrase for `secrets.enc` (03 §10.1).
pub const PASSPHRASE_ENV: &str = "GENIE_PASSPHRASE";

/// The opened secrets backend plus its unlock state.
pub struct Secrets {
    backend: Arc<dyn SecretBackend>,
    vault: Option<Arc<FileVault>>,
    unlocked: tokio::sync::Notify,
}

impl std::fmt::Debug for Secrets {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Secrets")
            .field("backend", &self.backend.kind())
            .field("locked", &self.is_locked())
            .finish()
    }
}

/// `secrets.backend` from the user settings file (nested or flat key).
fn configured_backend(config_dir: &Path) -> Option<String> {
    let text = std::fs::read_to_string(config_dir.join("settings.json")).ok()?;
    let v: serde_json::Value = serde_json::from_str(&text).ok()?;
    v.get("secrets")
        .and_then(|s| s.get("backend"))
        .or_else(|| v.get("secrets.backend"))
        .and_then(|b| b.as_str())
        .map(String::from)
}

/// Keychain service name per channel: `genie`, `genie-beta`, `genie-dev`.
pub fn keychain_service(ep: &Endpoint) -> String {
    format!("genie{}", ep.channel.dir_suffix())
}

impl Secrets {
    pub fn open(choice: &SecretsChoice, ep: &Endpoint) -> Arc<Secrets> {
        let file = |params: KdfParams| {
            Arc::new(FileVault::at(
                ep.data_dir.join(genie_secrets::vault::FILE_NAME),
                params,
            ))
        };
        let (backend, vault): (Arc<dyn SecretBackend>, Option<Arc<FileVault>>) = match choice {
            SecretsChoice::Backend(b) => (b.clone(), None),
            SecretsChoice::File(p) => {
                let v = file(*p);
                (v.clone(), Some(v))
            }
            SecretsChoice::Auto => {
                let forced = configured_backend(&ep.config_dir).is_some_and(|b| b == "file");
                let keychain = KeyringBackend::new(keychain_service(ep));
                match (forced, keychain.probe()) {
                    (false, Ok(())) => (Arc::new(keychain), None),
                    (forced, probe) => {
                        if let Err(e) = probe
                            && !forced
                        {
                            tracing::warn!(error = %e, "no keychain; using the passphrase file");
                        }
                        let v = file(KdfParams::default());
                        (v.clone(), Some(v))
                    }
                }
            }
        };
        tracing::info!(backend = backend.kind().as_str(), "secrets backend");
        Arc::new(Secrets {
            backend,
            vault,
            unlocked: tokio::sync::Notify::new(),
        })
    }

    pub fn backend(&self) -> Arc<dyn SecretBackend> {
        self.backend.clone()
    }

    pub fn kind(&self) -> BackendKind {
        self.backend.kind()
    }

    pub fn is_locked(&self) -> bool {
        self.backend.is_locked()
    }

    /// `InitializeResult.locked` (02 §2).
    pub fn locked_info(&self) -> Option<LockedInfo> {
        self.is_locked().then(|| LockedInfo {
            reason: "passphrase_required".into(),
        })
    }

    /// Unlock the passphrase file (or verify the passphrase when already unlocked). With a
    /// keychain there is nothing to unlock.
    pub fn unlock(&self, passphrase: &SecretString) -> Result<(), SecretsError> {
        if let Some(v) = &self.vault {
            v.unlock(passphrase)?;
            self.unlocked.notify_waiters();
        }
        Ok(())
    }

    /// `GENIE_PASSPHRASE`, if set, unlocks at start. A wrong one is logged, not fatal.
    pub fn unlock_from_env(&self) {
        if !self.is_locked() {
            return;
        }
        if let Ok(p) = std::env::var(PASSPHRASE_ENV)
            && !p.is_empty()
            && let Err(e) = self.unlock(&SecretString::from(p))
        {
            tracing::error!(error = %e, "{PASSPHRASE_ENV} did not unlock the secrets file");
        }
    }

    pub async fn wait_unlocked(&self) {
        loop {
            let n = self.unlocked.notified();
            if !self.is_locked() {
                return;
            }
            n.await;
        }
    }
}

/// `genie_store::KeyStore` over the secrets backend (the DB data key, 04 §2).
pub struct SecretsKeyStore(pub Arc<dyn SecretBackend>);

fn store_err(e: SecretsError) -> StoreError {
    StoreError::KeyStore(e.to_string())
}

impl genie_store::KeyStore for SecretsKeyStore {
    fn get(&self, account: &str) -> Result<Option<Zeroizing<Vec<u8>>>, StoreError> {
        self.0.get(account).map_err(store_err)
    }
    fn set(&self, account: &str, secret: &[u8]) -> Result<(), StoreError> {
        self.0.set(account, secret).map_err(store_err)
    }
    fn delete(&self, account: &str) -> Result<(), StoreError> {
        self.0.delete(account).map_err(store_err)
    }
}

/// Move every key from `keystore.placeholder.json` into `keys`, then delete the file.
/// A different key already in the keychain is kept under `<account>.replaced` (the
/// placeholder's key is the one the existing DB was written with). Returns how many
/// accounts moved.
pub fn migrate_placeholder(
    data_dir: &Path,
    keys: &dyn genie_store::KeyStore,
) -> Result<usize, StoreError> {
    let path = data_dir.join(crate::keystore::PLACEHOLDER_FILE);
    if !path.exists() {
        return Ok(0);
    }
    let old = crate::PlaceholderKeyStore::new(data_dir);
    let mut moved = 0;
    for account in old.accounts()? {
        let Some(v) = genie_store::KeyStore::get(&old, &account)? else {
            continue;
        };
        if let Some(cur) = keys.get(&account)? {
            if cur.as_slice() == v.as_slice() {
                continue;
            }
            keys.set(&format!("{account}.replaced"), &cur)?;
        }
        keys.set(&account, &v)?;
        moved += 1;
    }
    // Verify before deleting the only other copy.
    for account in old.accounts()? {
        let want = genie_store::KeyStore::get(&old, &account)?;
        if want.as_ref().map(|w| w.to_vec()) != keys.get(&account)?.map(|g| g.to_vec()) {
            return Err(StoreError::KeyStore(format!(
                "migrating {account} from the placeholder keystore did not verify"
            )));
        }
    }
    std::fs::remove_file(&path)?;
    tracing::info!(
        moved,
        "migrated the placeholder keystore into the secrets backend"
    );
    Ok(moved)
}

/// Methods a locked daemon still answers.
pub(crate) const LOCKED_ALLOWED: &[&str] = &[
    "secrets.unlock",
    "daemon.status",
    "daemon.restart",
    "doctor.run",
    "$/ping",
    "workspace.subscribe",
];

/// Serve the endpoint while the secrets file is locked. Returns `Some(reason)` when the
/// daemon should exit instead (idle, signal, lease end, restart), `None` once unlocked.
pub(crate) async fn locked_phase(
    cfg: &DaemonConfig,
    token: &str,
    secrets: Arc<Secrets>,
) -> Result<Option<ShutdownReason>, DaemonError> {
    let ep = cfg.endpoint.clone();
    let root = TaskScope::root();
    let engine = EngineBuilder::new().build(&root);
    let services = Services {
        providers: Some(crate::providers::ProviderHub::locked(secrets.clone())),
        ..Default::default()
    };
    let state = Arc::new(DaemonState::with_services(
        cfg.clone(),
        engine.clone(),
        token.to_string(),
        root.clone(),
        Arc::new(Registry::default()),
        None,
        Some(Arc::new(services)),
    ));
    let listener = crate::listener::bind(&ep.address)?;
    tracing::info!(address = %ep.address, "daemon listening (secrets locked)");
    let accept = root.child("accept");
    {
        let (st, sc) = (state.clone(), accept.clone());
        accept.spawn(async move { crate::listener::accept_loop(listener, st, sc).await });
    }
    {
        let st = state.clone();
        root.child("idle")
            .spawn_cancellable(async move { st.idle_watch().await });
    }
    if cfg.lease_on_stdin {
        let st = state.clone();
        root.child("lease").spawn_cancellable(async move {
            use tokio::io::AsyncReadExt;
            let mut stdin = tokio::io::stdin();
            let mut buf = [0u8; 256];
            while matches!(stdin.read(&mut buf).await, Ok(n) if n > 0) {}
            st.request_shutdown(ShutdownReason::LeaseEnded);
        });
    }
    {
        let st = state.clone();
        root.child("signals").spawn_cancellable(async move {
            if tokio::signal::ctrl_c().await.is_ok() {
                st.request_shutdown(ShutdownReason::Signal);
            }
        });
    }
    let outcome = tokio::select! {
        () = secrets.wait_unlocked() => None,
        r = state.wait_shutdown() => Some(r),
    };
    if outcome.is_none() {
        // Let the `secrets.unlock` answer reach the client before the restart notice.
        tokio::time::sleep(Duration::from_millis(150)).await;
        tracing::info!("secrets unlocked; starting the engine");
    }
    accept.cancel();
    state.broadcast_restarting(outcome.unwrap_or(ShutdownReason::Restart));
    engine.shutdown();
    root.cancel();
    let _ = root.close_and_wait(Duration::from_secs(2)).await;
    crate::listener::cleanup(&ep.address);
    Ok(outcome)
}

#[cfg(test)]
mod tests {
    use super::*;
    use genie_secrets::MemoryBackend;
    use genie_store::KeyStore;

    #[test]
    fn placeholder_key_moves_into_the_backend_once() {
        let dir = tempfile::tempdir().unwrap();
        let old = crate::PlaceholderKeyStore::new(dir.path());
        old.set("data-key", &[9u8; 32]).unwrap();
        let mem = Arc::new(MemoryBackend::new());
        let ks = SecretsKeyStore(mem.clone());
        ks.set("data-key", &[1u8; 32]).unwrap();
        assert_eq!(migrate_placeholder(dir.path(), &ks).unwrap(), 1);
        assert_eq!(ks.get("data-key").unwrap().unwrap().as_slice(), [9u8; 32]);
        assert_eq!(
            ks.get("data-key.replaced").unwrap().unwrap().as_slice(),
            [1u8; 32]
        );
        assert!(!dir.path().join(crate::keystore::PLACEHOLDER_FILE).exists());
        assert_eq!(migrate_placeholder(dir.path(), &ks).unwrap(), 0);
    }

    #[test]
    fn file_choice_is_locked_until_unlocked() {
        let dir = tempfile::tempdir().unwrap();
        let ep = Endpoint::in_dir(dir.path(), genie_protocol::Channel::Dev).unwrap();
        let s = Secrets::open(&SecretsChoice::File(KdfParams::insecure_for_tests()), &ep);
        assert!(s.is_locked());
        assert_eq!(
            s.locked_info().unwrap().reason,
            "passphrase_required".to_string()
        );
        s.unlock(&SecretString::from("pw")).unwrap();
        assert!(!s.is_locked());
        assert!(s.locked_info().is_none());
        assert!(format!("{s:?}").contains("File"));
    }
}
