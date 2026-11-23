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

/// File next to `secrets.enc` holding the generated passphrase of a development build.
pub const DEV_PASSPHRASE_FILE: &str = "secrets.dev-passphrase";

/// Development builds (`debug_assertions`) don't use the OS keychain unless settings say
/// `secrets.backend: "keychain"`. An unsigned dev binary changes code identity on every
/// rebuild, so macOS would ask for keychain access again and again. Instead the secrets
/// live in `secrets.enc`, unlocked with a random per-install passphrase kept in a 0600
/// file beside it. Release builds are unchanged: keychain (a signed app is never asked).
pub fn dev_secrets_mode(config_dir: &Path) -> bool {
    cfg!(debug_assertions) && configured_backend(config_dir).is_none()
}

/// The dev passphrase, created on first use (32 random bytes, hex, file mode 0600).
fn dev_passphrase(data_dir: &Path) -> std::io::Result<SecretString> {
    let path = data_dir.join(DEV_PASSPHRASE_FILE);
    match std::fs::read_to_string(&path) {
        Ok(s) if !s.trim().is_empty() => return Ok(SecretString::from(s.trim().to_string())),
        Ok(_) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(e),
    }
    std::fs::create_dir_all(data_dir)?;
    let mut bytes = [0u8; 32];
    getrandom::fill(&mut bytes).map_err(|e| std::io::Error::other(e.to_string()))?;
    let pass = hex::encode(bytes);
    let tmp = path.with_extension("tmp");
    {
        let mut opts = std::fs::OpenOptions::new();
        opts.write(true).create(true).truncate(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            opts.mode(0o600);
        }
        use std::io::Write;
        opts.open(&tmp)?.write_all(pass.as_bytes())?;
    }
    std::fs::rename(&tmp, &path)?;
    Ok(SecretString::from(pass))
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
            SecretsChoice::Auto if dev_secrets_mode(&ep.config_dir) => {
                let v = file(KdfParams::default());
                match dev_passphrase(&ep.data_dir) {
                    Ok(p) => {
                        if let Err(e) = v.unlock(&p) {
                            tracing::error!(error = %e, "dev secrets file did not unlock");
                        }
                    }
                    Err(e) => tracing::error!(error = %e, "could not create the dev passphrase"),
                }
                tracing::warn!(
                    "development build: secrets are in secrets.enc with a local dev passphrase, \
                     not the OS keychain (set secrets.backend = \"keychain\" to use it)"
                );
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

/// The key store this run is NOT using, when it may hold this data dir's keys. Builds
/// differ on purpose (a dev build uses `secrets.enc`, a release build the keychain,
/// [`dev_secrets_mode`]), so a DB written by one is unreadable to the other unless the
/// key is looked up in both places.
///
/// * Using the keychain → an existing `secrets.enc`, opened with the dev passphrase
///   file or `GENIE_PASSPHRASE` (never created here).
/// * Using `secrets.enc` → the OS keychain, if reachable.
pub fn alternate_backend(ep: &Endpoint, current: BackendKind) -> Option<AltKeys> {
    match current {
        BackendKind::Keychain => {
            let path = ep.data_dir.join(genie_secrets::vault::FILE_NAME);
            if !path.exists() {
                return None;
            }
            let v = FileVault::at(path, KdfParams::default());
            let dev = std::fs::read_to_string(ep.data_dir.join(DEV_PASSPHRASE_FILE))
                .ok()
                .map(|s| s.trim().to_string());
            let env = std::env::var(PASSPHRASE_ENV).ok();
            for p in [dev, env].into_iter().flatten().filter(|p| !p.is_empty()) {
                if v.unlock(&SecretString::from(p)).is_ok() {
                    let accounts = v.accounts().unwrap_or_default();
                    return Some(AltKeys {
                        backend: Arc::new(v),
                        accounts,
                    });
                }
            }
            None
        }
        BackendKind::File => {
            let k = KeyringBackend::new(keychain_service(ep));
            k.probe().ok().map(|()| AltKeys {
                backend: Arc::new(k),
                // A keychain can't be enumerated: only the DB key is looked for.
                accounts: vec![genie_store::keys::DATA_KEY_ACCOUNT.into()],
            })
        }
        BackendKind::Memory => None,
    }
}

/// Another key store and the accounts worth copying out of it.
pub struct AltKeys {
    pub backend: Arc<dyn SecretBackend>,
    pub accounts: Vec<String>,
}

/// Copy the accounts `to` lacks from `from` (the DB data key, provider credentials, …),
/// so switching between a dev and a release build keeps both the chats and the keys.
/// Existing values in `to` are never overwritten. `accounts` lists what to try (a
/// keychain can't be enumerated). Returns how many were copied.
pub fn adopt_accounts(
    from: &dyn SecretBackend,
    to: &dyn SecretBackend,
    accounts: &[String],
) -> usize {
    let mut n = 0;
    for a in accounts {
        match (to.get(a), from.get(a)) {
            (Ok(None), Ok(Some(v))) => match to.set(a, &v) {
                Ok(()) => n += 1,
                Err(e) => tracing::warn!(account = %a, error = %e, "secret not copied"),
            },
            (Err(e), _) | (_, Err(e)) => {
                tracing::warn!(account = %a, error = %e, "secret not read while copying");
            }
            _ => {}
        }
    }
    n
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
        secrets: Some(secrets.clone()),
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

    #[test]
    fn dev_passphrase_is_created_once_private_and_reused() {
        use genie_secrets::ExposeSecret;
        let dir = tempfile::tempdir().unwrap();
        let a = dev_passphrase(dir.path()).unwrap();
        let b = dev_passphrase(dir.path()).unwrap();
        assert_eq!(a.expose_secret(), b.expose_secret());
        assert_eq!(a.expose_secret().len(), 64);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(dir.path().join(DEV_PASSPHRASE_FILE))
                .unwrap()
                .permissions()
                .mode();
            assert_eq!(mode & 0o777, 0o600);
        }
    }

    #[test]
    fn dev_mode_only_in_debug_builds_without_an_explicit_backend() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(dev_secrets_mode(dir.path()), cfg!(debug_assertions));
        std::fs::write(
            dir.path().join("settings.json"),
            r#"{"secrets":{"backend":"keychain"}}"#,
        )
        .unwrap();
        assert!(!dev_secrets_mode(dir.path()));
    }
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

    /// A memory backend that reports itself as the OS keychain.
    struct FakeKeychain(MemoryBackend);
    impl SecretBackend for FakeKeychain {
        fn kind(&self) -> BackendKind {
            BackendKind::Keychain
        }
        fn get(&self, a: &str) -> Result<Option<Zeroizing<Vec<u8>>>, SecretsError> {
            self.0.get(a)
        }
        fn set(&self, a: &str, s: &[u8]) -> Result<(), SecretsError> {
            self.0.set(a, s)
        }
        fn delete(&self, a: &str) -> Result<(), SecretsError> {
            self.0.delete(a)
        }
    }

    /// The Nov 2026 incident: a dev build wrote the DB with its key in `secrets.enc`; a
    /// release build (keychain) then found no key and ran with no store at all.
    #[test]
    fn release_build_opens_a_dev_build_db_and_adopts_its_secrets() {
        let dir = tempfile::tempdir().unwrap();
        let ep = Endpoint::in_dir(dir.path(), genie_protocol::Channel::Dev).unwrap();
        {
            let vault = Arc::new(FileVault::at(
                ep.data_dir.join(genie_secrets::vault::FILE_NAME),
                KdfParams::insecure_for_tests(),
            ));
            std::fs::create_dir_all(&ep.data_dir).unwrap();
            std::fs::write(ep.data_dir.join(DEV_PASSPHRASE_FILE), "devpass").unwrap();
            vault.unlock(&SecretString::from("devpass")).unwrap();
            vault.set("provider.azure", b"api-key").unwrap();
            let dev = Secrets::open(&SecretsChoice::Backend(vault), &ep);
            let store = crate::open_store_any_key(&ep, &dev).unwrap();
            store
                .write(|tx| genie_store::sessions::ensure(tx, "ses_old"))
                .unwrap();
        }

        let keychain = Arc::new(FakeKeychain(MemoryBackend::new()));
        let release = Secrets::open(&SecretsChoice::Backend(keychain.clone()), &ep);
        let store = crate::open_store_any_key(&ep, &release).expect("falls back to secrets.enc");
        assert!(
            store
                .read(|c| genie_store::sessions::get(c, "ses_old"))
                .unwrap()
                .is_some()
        );
        // Copied, so the next start needs no fallback; the API key came along.
        assert!(
            keychain
                .get(genie_store::keys::DATA_KEY_ACCOUNT)
                .unwrap()
                .is_some()
        );
        assert_eq!(
            keychain.get("provider.azure").unwrap().unwrap().as_slice(),
            b"api-key"
        );
    }

    #[test]
    fn a_key_that_does_not_open_the_db_is_not_adopted() {
        let dir = tempfile::tempdir().unwrap();
        let ep = Endpoint::in_dir(dir.path(), genie_protocol::Channel::Dev).unwrap();
        // DB written with a key that lives nowhere any more.
        let lost = Secrets::open(&SecretsChoice::Backend(Arc::new(MemoryBackend::new())), &ep);
        drop(crate::open_store_any_key(&ep, &lost).unwrap());
        // `secrets.enc` holds a different data key.
        let vault = FileVault::at(
            ep.data_dir.join(genie_secrets::vault::FILE_NAME),
            KdfParams::insecure_for_tests(),
        );
        std::fs::write(ep.data_dir.join(DEV_PASSPHRASE_FILE), "devpass").unwrap();
        vault.unlock(&SecretString::from("devpass")).unwrap();
        vault
            .set(genie_store::keys::DATA_KEY_ACCOUNT, &[7u8; 32])
            .unwrap();

        let keychain = Arc::new(FakeKeychain(MemoryBackend::new()));
        let release = Secrets::open(&SecretsChoice::Backend(keychain.clone()), &ep);
        assert!(matches!(
            crate::open_store_any_key(&ep, &release),
            Err(StoreError::KeyMissing { .. })
        ));
        assert!(
            keychain
                .get(genie_store::keys::DATA_KEY_ACCOUNT)
                .unwrap()
                .is_none()
        );
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
