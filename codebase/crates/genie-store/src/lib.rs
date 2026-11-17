//! `genie-store`: SQLCipher DB, migrations, blobs, FTS, retrieval index, export
//! (spec/04-storage.md).
//!
//! * [`Store`]: the encrypted DB (one writer, a read pool), migrations on open, the
//!   blob store, maintenance. Repository functions in the modules below take a
//!   `&rusqlite::Connection`, so several of them compose inside one
//!   [`Store::write`] transaction (a durable event and its projections commit together).
//! * [`keys`]: data key, the [`keys::KeyStore`] keychain port, recovery key (SA-16).
//! * [`crypto`] / [`blobs`]: XChaCha20-Poly1305, the chunked GNB1 blob format.
//!
//! Dependency rule (01 §2.1 rule 3): this crate depends on `genie-protocol` only.
//! `genie-core` adapts it to its `Journal` port.

pub mod artifacts;
pub mod blobs;
pub mod crypto;
pub mod error;
pub mod events;
pub mod export;
pub mod keys;
pub mod kv;
pub mod memory;
pub mod messages;
pub mod migrate;
mod ops;
pub mod projects;
pub mod retrieval;
pub mod search;
pub mod sessions;
mod store;
pub mod transfer;
pub mod util;

pub use error::{Result, StoreError};
pub use keys::{KeyStore, MemoryKeyStore};
pub use ops::{BLOB_GRACE_MS, BlobRow, DataCounts, MaintenanceReport};
pub use rusqlite::{Connection, Transaction};
pub use store::{READERS, Store, StoreOptions};
