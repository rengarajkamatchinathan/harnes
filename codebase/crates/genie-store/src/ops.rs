//! Store-level operations that span the DB and blob files: blob rows, GC, retention,
//! VACUUM and "delete all data" (04 §4, §8).

use std::path::Path;

use rusqlite::{OptionalExtension, params};

use crate::artifacts::{self, ArtifactVersion, INLINE_MAX};
use crate::error::{Result, StoreError};
use crate::keys::{DATA_KEY_ACCOUNT, KeyStore, RECOVERY_FILE};
use crate::search;
use crate::store::Store;
use crate::util::now_ms;
use crate::{events, kv};

/// Blobs with refcount 0 are kept this long for in-flight uploads (04 §4).
pub const BLOB_GRACE_MS: i64 = 60 * 60 * 1000;
/// VACUUM at most weekly, when free pages exceed 20% (SA-19).
pub const VACUUM_INTERVAL_MS: i64 = 7 * 24 * 60 * 60 * 1000;
pub const VACUUM_FREE_RATIO: f64 = 0.2;
const LAST_VACUUM_KV: &str = "maintenance.last_vacuum";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BlobRow {
    pub id: String,
    pub size: u64,
    pub mime: Option<String>,
    pub refcount: i64,
    pub created: i64,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MaintenanceReport {
    pub sessions_deleted: usize,
    pub logs_pruned: usize,
    pub blobs_deleted: usize,
    pub vacuumed: bool,
}

impl Store {
    /// Encrypt and store `data`, and make sure its `blob` row exists (refcount 0 until
    /// a row references it; referencing rows bump it in their own transaction).
    pub fn put_blob(&self, data: &[u8], mime: Option<&str>) -> Result<String> {
        let _guard = self.lock_blobs()?;
        let id = self.blob_files().write(data)?;
        let size =
            i64::try_from(data.len()).map_err(|_| StoreError::Invalid("blob too large".into()))?;
        self.write(|tx| {
            tx.execute(
                "INSERT INTO blob (id, size, mime, refcount, created) VALUES (?1, ?2, ?3, 0, ?4)
                 ON CONFLICT (id) DO UPDATE SET created = max(blob.created, excluded.created)",
                params![id, size, mime, now_ms()],
            )?;
            Ok(())
        })?;
        Ok(id)
    }

    fn lock_blobs(&self) -> Result<std::sync::MutexGuard<'_, ()>> {
        self.blob_lock
            .lock()
            .map_err(|_| StoreError::Invalid("blob lock poisoned".into()))
    }

    pub fn read_blob(&self, id: &str) -> Result<Vec<u8>> {
        self.blob_files().read(id)
    }

    pub fn read_blob_range(&self, id: &str, offset: u64, len: u64) -> Result<Vec<u8>> {
        self.blob_files().read_range(id, offset, len)
    }

    pub fn blob_row(&self, id: &str) -> Result<Option<BlobRow>> {
        self.read(|c| {
            Ok(c.query_row(
                "SELECT id, size, mime, refcount, created FROM blob WHERE id = ?1",
                [id],
                |r| {
                    Ok(BlobRow {
                        id: r.get(0)?,
                        size: u64::try_from(r.get::<_, i64>(1)?).unwrap_or(0),
                        mime: r.get(2)?,
                        refcount: r.get(3)?,
                        created: r.get(4)?,
                    })
                },
            )
            .optional()?)
        })
    }

    /// Add an artifact version; content above 256 KiB goes to a blob (04 §4).
    pub fn add_artifact_version(
        &self,
        artifact_id: &str,
        message_id: &str,
        content: &str,
    ) -> Result<ArtifactVersion> {
        if content.len() > INLINE_MAX {
            let blob = self.put_blob(content.as_bytes(), Some("text/plain"))?;
            self.write(|tx| artifacts::add_version(tx, artifact_id, message_id, None, Some(&blob)))
        } else {
            self.write(|tx| {
                artifacts::add_version(tx, artifact_id, message_id, Some(content), None)
            })
        }
    }

    /// Delete blobs with refcount 0 older than the grace period: rows first, then files.
    pub fn gc_blobs(&self, now: i64) -> Result<usize> {
        let _guard = self.lock_blobs()?;
        let ids: Vec<String> = self.write(|tx| {
            let ids: Vec<String> = {
                let mut st =
                    tx.prepare("SELECT id FROM blob WHERE refcount <= 0 AND created < ?1")?;
                st.query_map([now - BLOB_GRACE_MS], |r| r.get(0))?
                    .collect::<rusqlite::Result<_>>()?
            };
            for id in &ids {
                tx.execute("DELETE FROM blob WHERE id = ?1 AND refcount <= 0", [id])?;
            }
            Ok(ids)
        })?;
        for id in &ids {
            self.blob_files().remove(id)?;
        }
        Ok(ids.len())
    }

    /// Admin `retention.maxDays` (D38): delete sessions not updated for `max_days`
    /// that are not running.
    pub fn apply_retention(&self, now: i64, max_days: u32) -> Result<usize> {
        let cutoff = now - i64::from(max_days) * 24 * 60 * 60 * 1000;
        self.write(|tx| {
            Ok(tx.execute(
                "DELETE FROM session WHERE updated < ?1 AND status NOT IN ('running', 'awaiting_ask')
                 AND parent_session_id IS NULL",
                [cutoff],
            )?)
        })
    }

    /// VACUUM if forced, or if a week has passed and free pages exceed 20% (SA-19).
    /// The caller makes sure no turn is running. FTS indexes are rebuilt afterwards.
    pub fn vacuum_if_needed(&self, now: i64, force: bool) -> Result<bool> {
        let conn = self.writer()?;
        if !force {
            let last = kv::get(&conn, LAST_VACUUM_KV)?
                .and_then(|v| v.as_i64())
                .unwrap_or(0);
            let pages: i64 = conn.query_row("PRAGMA page_count", [], |r| r.get(0))?;
            let free: i64 = conn.query_row("PRAGMA freelist_count", [], |r| r.get(0))?;
            let ratio = if pages == 0 {
                0.0
            } else {
                free as f64 / pages as f64
            };
            if now - last < VACUUM_INTERVAL_MS || ratio <= VACUUM_FREE_RATIO {
                return Ok(false);
            }
        }
        conn.execute_batch("VACUUM")?;
        conn.execute_batch("BEGIN IMMEDIATE")?;
        let res = search::rebuild_all(&conn)
            .and_then(|()| kv::set(&conn, LAST_VACUUM_KV, &serde_json::json!(now)));
        match res {
            Ok(()) => conn.execute_batch("COMMIT")?,
            Err(e) => {
                let _ = conn.execute_batch("ROLLBACK");
                return Err(e);
            }
        }
        Ok(true)
    }

    /// The daily job (SA-19): retention, event-log pruning (02 §9), blob GC, VACUUM.
    pub fn run_maintenance(
        &self,
        now: i64,
        retention_days: Option<u32>,
    ) -> Result<MaintenanceReport> {
        let sessions_deleted = match retention_days {
            Some(d) => self.apply_retention(now, d)?,
            None => 0,
        };
        let logs_pruned = self
            .write(|tx| events::prune_idle(tx, now, events::PRUNE_IDLE_MS))?
            .len();
        let blobs_deleted = self.gc_blobs(now)?;
        let vacuumed = self.vacuum_if_needed(now, false)?;
        Ok(MaintenanceReport {
            sessions_deleted,
            logs_pruned,
            blobs_deleted,
            vacuumed,
        })
    }

    /// Settings → Privacy → "Delete all data" (04 §8): closes the store and removes the
    /// DB (+ WAL/SHM and backups), blobs, snapshots, caches, the recovery key and the
    /// keychain entry. Settings, logs and `run/` are left alone.
    pub fn delete_all_data(self, keys: &dyn KeyStore) -> Result<()> {
        let dir = self.dir().to_path_buf();
        let db_name = self
            .db_path()
            .file_name()
            .and_then(|n| n.to_str())
            .map(String::from)
            .ok_or_else(|| StoreError::Invalid("bad db path".into()))?;
        drop(self);
        for entry in std::fs::read_dir(&dir)?.flatten() {
            let name = entry.file_name().to_string_lossy().to_string();
            if name == db_name
                || name.starts_with(&format!("{db_name}-"))
                || name.starts_with(&format!("{db_name}.bak-"))
            {
                remove_path(&entry.path())?;
            }
        }
        for sub in ["blobs", "snapshots", "cache", RECOVERY_FILE] {
            remove_path(&dir.join(sub))?;
        }
        keys.delete(DATA_KEY_ACCOUNT)
    }
}

fn remove_path(p: &Path) -> Result<()> {
    let res = if p.is_dir() {
        std::fs::remove_dir_all(p)
    } else {
        std::fs::remove_file(p)
    };
    match res {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e.into()),
    }
}
