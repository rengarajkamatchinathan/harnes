//! The SQLCipher database handle (04 §2, §5, §11).

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Mutex, MutexGuard};

use rusqlite::{Connection, ErrorCode, OpenFlags, Transaction, TransactionBehavior};
use zeroize::Zeroizing;

use crate::blobs::{BlobFiles, create_private_dir};
use crate::crypto::{self, Key};
use crate::error::{Result, StoreError};
use crate::keys::{KeyStore, load_or_create_data_key};
use crate::migrate;

/// Read pool size (04 §11).
pub const READERS: usize = 4;
/// Backups kept before schema migrations (04 §5).
pub const BACKUPS_KEPT: usize = 2;
pub(crate) const BLOB_KEY_KV: &str = "blob_key";
const BLOB_KEY_AAD: &[u8] = b"genie blob key v1";

#[derive(Debug, Clone)]
pub struct StoreOptions {
    /// The channel data dir (`~/.genie`, …).
    pub dir: PathBuf,
    /// `genie.db`, `genie-beta.db`, …
    pub db_name: String,
    pub readers: usize,
}

impl StoreOptions {
    pub fn new(dir: impl Into<PathBuf>, db_name: impl Into<String>) -> Self {
        StoreOptions {
            dir: dir.into(),
            db_name: db_name.into(),
            readers: READERS,
        }
    }
}

/// The daemon's single handle on the DB. One writer connection (SQLite allows one
/// writer; 01 §3) and a small pool of read connections (WAL).
///
/// The API is synchronous; async callers wrap it in `spawn_blocking` or the dedicated
/// writer task (see README gaps).
pub struct Store {
    dir: PathBuf,
    db_path: PathBuf,
    data_key: Key,
    /// `None` once [`Store::close`] ran.
    writer: Mutex<Option<Connection>>,
    readers: Vec<Mutex<Option<Connection>>>,
    next_reader: AtomicUsize,
    blobs: BlobFiles,
    /// Serialises blob writes against blob GC, so GC never removes a file that a
    /// concurrent `put_blob` just found already present.
    pub(crate) blob_lock: Mutex<()>,
    migration: migrate::Outcome,
}

impl std::fmt::Debug for Store {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Store")
            .field("db", &self.db_path)
            .finish_non_exhaustive()
    }
}

impl Store {
    /// Open (or create) the DB with the data key from the keychain (created on first run).
    pub fn open_with_keystore(opts: &StoreOptions, keys: &dyn KeyStore) -> Result<Store> {
        let key = load_or_create_data_key(keys, &opts.dir, &opts.db_name)?;
        Store::open(opts, key)
    }

    /// Open (or create) the DB with an explicit data key, migrating it if needed.
    pub fn open(opts: &StoreOptions, data_key: Key) -> Result<Store> {
        create_private_dir(&opts.dir)?;
        let db_path = opts.dir.join(&opts.db_name);
        let mut writer = open_conn(&db_path, &data_key, false)?;
        restrict_perms(&db_path)?;
        let backup = |conn: &Connection, version: i64| {
            backup_before_migration(conn, &db_path, &data_key, version)
        };
        let migration = migrate::run(&mut writer, &backup)?;
        let blob_key = load_or_create_blob_key(&writer, &data_key)?;
        let readers = (0..opts.readers.max(1))
            .map(|_| open_conn(&db_path, &data_key, true).map(|c| Mutex::new(Some(c))))
            .collect::<Result<Vec<_>>>()?;
        Ok(Store {
            dir: opts.dir.clone(),
            db_path,
            data_key,
            writer: Mutex::new(Some(writer)),
            readers,
            next_reader: AtomicUsize::new(0),
            blobs: BlobFiles::new(opts.dir.join("blobs"), blob_key),
            blob_lock: Mutex::new(()),
            migration,
        })
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    pub fn db_path(&self) -> &Path {
        &self.db_path
    }

    /// What `open` did to the schema.
    pub fn migration(&self) -> &migrate::Outcome {
        &self.migration
    }

    pub fn blob_files(&self) -> &BlobFiles {
        &self.blobs
    }

    /// Run `f` in one IMMEDIATE write transaction; commits if it returns `Ok`.
    /// Durable event writes and their projections go through one call (04 §11).
    pub fn write<T>(&self, f: impl FnOnce(&Transaction<'_>) -> Result<T>) -> Result<T> {
        let mut conn = self.writer()?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let out = f(&tx)?;
        tx.commit()?;
        Ok(out)
    }

    /// Run `f` on a read connection (sees the last committed state).
    pub fn read<T>(&self, f: impl FnOnce(&Connection) -> Result<T>) -> Result<T> {
        let n = self.readers.len();
        let start = self.next_reader.fetch_add(1, Ordering::Relaxed) % n;
        for i in 0..n {
            if let Ok(g) = self.readers[(start + i) % n].try_lock() {
                let c = ConnGuard::new(g)?;
                return f(&c);
            }
        }
        let g = self.readers[start]
            .lock()
            .map_err(|_| StoreError::Invalid("reader lock poisoned".into()))?;
        let c = ConnGuard::new(g)?;
        f(&c)
    }

    pub(crate) fn writer(&self) -> Result<ConnGuard<'_>> {
        let g = self
            .writer
            .lock()
            .map_err(|_| StoreError::Invalid("writer lock poisoned".into()))?;
        ConnGuard::new(g)
    }

    /// Close every connection (checkpointing the WAL) so the DB files are released even
    /// while other parts of the daemon still hold this `Store` (01 §4.3 "close store";
    /// on Windows an open handle blocks renaming or deleting the files). Every later
    /// call fails with "store is closed". Idempotent.
    pub fn close(&self) {
        let mut w = self.writer.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(conn) = w.as_ref()
            && let Err(e) = conn.execute_batch("PRAGMA wal_checkpoint(TRUNCATE);")
        {
            tracing::warn!(error = %e, "WAL checkpoint on close failed");
        }
        for r in &self.readers {
            r.lock().unwrap_or_else(|e| e.into_inner()).take();
        }
        w.take();
    }

    pub fn is_closed(&self) -> bool {
        self.writer
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .is_none()
    }

    /// An encrypted copy of the DB via `sqlcipher_export` (04 §5).
    pub fn backup_to(&self, dest: &Path) -> Result<()> {
        let conn = self.writer()?;
        export_encrypted(&conn, dest, &self.data_key)
    }
}

/// A locked connection that is still open (see [`Store::close`]).
pub(crate) struct ConnGuard<'a>(MutexGuard<'a, Option<Connection>>);

impl<'a> ConnGuard<'a> {
    fn new(g: MutexGuard<'a, Option<Connection>>) -> Result<Self> {
        if g.is_none() {
            return Err(StoreError::Invalid("store is closed".into()));
        }
        Ok(ConnGuard(g))
    }
}

impl std::ops::Deref for ConnGuard<'_> {
    type Target = Connection;
    fn deref(&self) -> &Connection {
        match self.0.as_ref() {
            Some(c) => c,
            None => unreachable!("ConnGuard is only built over an open connection"),
        }
    }
}

impl std::ops::DerefMut for ConnGuard<'_> {
    fn deref_mut(&mut self) -> &mut Connection {
        match self.0.as_mut() {
            Some(c) => c,
            None => unreachable!("ConnGuard is only built over an open connection"),
        }
    }
}

fn open_conn(path: &Path, key: &Key, read_only: bool) -> Result<Connection> {
    let flags = if read_only {
        OpenFlags::SQLITE_OPEN_READ_ONLY
            | OpenFlags::SQLITE_OPEN_NO_MUTEX
            | OpenFlags::SQLITE_OPEN_URI
    } else {
        OpenFlags::default()
    };
    let conn = Connection::open_with_flags(path, flags)?;
    apply_key(&conn, key)?;
    // Fails here, not later, when the key is wrong.
    conn.query_row("SELECT count(*) FROM sqlite_master", [], |r| {
        r.get::<_, i64>(0)
    })
    .map_err(|e| match e.sqlite_error_code() {
        Some(ErrorCode::NotADatabase) => StoreError::WrongKey(path.to_path_buf()),
        _ => e.into(),
    })?;
    if !read_only {
        let mode: String = conn.query_row("PRAGMA journal_mode = WAL", [], |r| r.get(0))?;
        if !mode.eq_ignore_ascii_case("wal") {
            return Err(StoreError::Invalid(format!(
                "journal_mode is {mode}, not wal"
            )));
        }
    }
    conn.execute_batch(
        "PRAGMA busy_timeout = 5000;
         PRAGMA synchronous = NORMAL;
         PRAGMA foreign_keys = ON;
         PRAGMA secure_delete = ON;",
    )?;
    if read_only {
        conn.execute_batch("PRAGMA query_only = ON;")?;
    }
    Ok(conn)
}

/// Raw 256-bit key (no PBKDF2), page size 4096, memory security on (04 §2).
fn apply_key(conn: &Connection, key: &Key) -> Result<()> {
    let sql = Zeroizing::new(format!(
        "PRAGMA key = \"x'{}'\";
         PRAGMA cipher_page_size = 4096;
         PRAGMA cipher_memory_security = ON;",
        hex::encode(key.expose())
    ));
    conn.execute_batch(&sql)?;
    Ok(())
}

fn restrict_perms(path: &Path) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    }
    #[cfg(not(unix))]
    let _ = path;
    Ok(())
}

fn export_encrypted(conn: &Connection, dest: &Path, key: &Key) -> Result<()> {
    if dest.exists() {
        std::fs::remove_file(dest)?;
    }
    let dest_s = dest
        .to_str()
        .ok_or_else(|| StoreError::Invalid(format!("non-UTF-8 path {}", dest.display())))?;
    let key_s = Zeroizing::new(format!("x'{}'", hex::encode(key.expose())));
    conn.execute(
        "ATTACH DATABASE ?1 AS genie_bak KEY ?2",
        [dest_s, key_s.as_str()],
    )?;
    let res = conn
        .query_row("SELECT sqlcipher_export('genie_bak')", [], |_| Ok(()))
        .map_err(StoreError::from);
    conn.execute_batch("DETACH DATABASE genie_bak")?;
    res?;
    restrict_perms(dest)
}

fn backup_before_migration(
    conn: &Connection,
    db_path: &Path,
    key: &Key,
    version: i64,
) -> Result<()> {
    let name = db_path
        .file_name()
        .and_then(|n| n.to_str())
        .ok_or_else(|| StoreError::Invalid("bad db path".into()))?;
    let dir = db_path
        .parent()
        .ok_or_else(|| StoreError::Invalid("bad db path".into()))?;
    let prefix = format!("{name}.bak-");
    export_encrypted(conn, &dir.join(format!("{prefix}{version}")), key)?;
    let mut backups: Vec<(i64, PathBuf)> = std::fs::read_dir(dir)?
        .flatten()
        .filter_map(|e| {
            let n = e.file_name().to_str()?.to_string();
            let v = n.strip_prefix(&prefix)?.parse::<i64>().ok()?;
            Some((v, e.path()))
        })
        .collect();
    backups.sort();
    while backups.len() > BACKUPS_KEPT {
        let (_, p) = backups.remove(0);
        std::fs::remove_file(p)?;
    }
    Ok(())
}

fn load_or_create_blob_key(conn: &Connection, data_key: &Key) -> Result<Key> {
    let existing: Option<String> = conn
        .query_row(
            "SELECT json_extract(value, '$') FROM kv WHERE key = ?1",
            [BLOB_KEY_KV],
            |r| r.get(0),
        )
        .map(Some)
        .or_else(|e| match e {
            rusqlite::Error::QueryReturnedNoRows => Ok(None),
            e => Err(e),
        })?;
    if let Some(hex_s) = existing {
        let sealed = hex::decode(hex_s).map_err(|_| StoreError::Format("blob key".into()))?;
        let raw = Zeroizing::new(crypto::open(data_key, &sealed, BLOB_KEY_AAD)?);
        return Key::from_bytes(&raw);
    }
    let key = Key::generate()?;
    let sealed = crypto::seal(data_key, key.expose(), BLOB_KEY_AAD)?;
    conn.execute(
        "INSERT INTO kv (key, value) VALUES (?1, json_quote(?2))",
        [BLOB_KEY_KV, &hex::encode(sealed)],
    )?;
    Ok(key)
}
