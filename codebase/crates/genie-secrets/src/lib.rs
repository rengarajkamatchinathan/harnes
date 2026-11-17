//! `genie-secrets`: where secrets live (D26, 03 §10, 08 §2.5 / §8).
//!
//! * [`backend::SecretBackend`]: raw bytes by account name. Backends: the OS keychain
//!   ([`keychain::KeyringBackend`]: macOS Keychain, Windows Credential Manager, Linux
//!   Secret Service), the passphrase file [`vault::FileVault`] (`secrets.enc`, Argon2id +
//!   XChaCha20-Poly1305) when there is no keychain, and [`backend::MemoryBackend`] for
//!   tests and incognito.
//! * [`credentials`]: the provider credential model, the store port, an in-memory store
//!   and [`store::BackendStore`] (credentials kept in a backend).
//! * [`lock::FileLock`]: the cross-process lock used around OAuth refreshes.
//!
//! Secrets are always carried as [`SecretString`] / `Zeroizing` buffers; their `Debug`
//! never prints the value (01 §7 rule 2).
pub use secrecy::{ExposeSecret, SecretString};

pub mod backend;
pub mod credentials;
pub mod keychain;
pub mod lock;
pub mod store;
pub mod vault;

pub use backend::{BackendKind, MemoryBackend, SecretBackend, SecretsError};
