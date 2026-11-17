//! Blob transfer methods (02 §6.9): chunked upload (`blob.put.begin/chunk/end`, 1 MiB
//! chunks), `blob.put.path` (the daemon reads a local file of the same user) and ranged
//! `blob.get`. Blobs land in the encrypted, content-addressed store (04 §4); the id is
//! the plaintext SHA-256.
//!
//! Uploads are buffered in memory until `end` (the store's address is the hash of the
//! whole plaintext, 04 gaps). Chunks must arrive in order; re-sending the last chunk
//! (same offset and bytes) after a reconnect is accepted. Uploads idle for more than an
//! hour are dropped.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use base64::Engine as _;
use genie_protocol::api::project::{
    BLOB_CHUNK_BYTES, BlobGetParams, BlobGetResult, BlobPutBeginParams, BlobPutChunkParams,
    BlobPutPathParams, BlobPutResult, UploadIdResult,
};
use genie_protocol::{ErrorKind, RpcError};

/// Largest blob accepted through the protocol.
pub const MAX_BLOB: u64 = 512 * 1024 * 1024;
/// Largest range one `blob.get` returns.
pub const MAX_GET: u64 = 4 * 1024 * 1024;
/// Concurrent uploads per daemon.
pub const MAX_UPLOADS: usize = 32;
const STALE: Duration = Duration::from_secs(60 * 60);

struct Upload {
    size: u64,
    mime: Option<String>,
    data: Vec<u8>,
    touched: Instant,
}

/// In-flight chunked uploads.
#[derive(Default)]
pub struct Uploads {
    m: Mutex<HashMap<String, Upload>>,
}

fn b64() -> base64::engine::GeneralPurpose {
    base64::engine::general_purpose::STANDARD
}

fn store_err(e: genie_store::StoreError) -> RpcError {
    match e {
        genie_store::StoreError::NotFound(what) => {
            RpcError::new(ErrorKind::NotFound, format!("{what} not found"))
        }
        genie_store::StoreError::Invalid(m) => RpcError::invalid_params(m),
        other => RpcError::new(ErrorKind::InternalError, other.to_string()),
    }
}

impl Uploads {
    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<String, Upload>> {
        self.m.lock().unwrap_or_else(|e| e.into_inner())
    }

    pub fn begin(&self, p: &BlobPutBeginParams) -> Result<UploadIdResult, RpcError> {
        if p.size > MAX_BLOB {
            return Err(RpcError::invalid_params(format!(
                "blobs are limited to {MAX_BLOB} bytes"
            )));
        }
        let mut g = self.lock();
        g.retain(|_, u| u.touched.elapsed() < STALE);
        if g.len() >= MAX_UPLOADS {
            return Err(RpcError::new(
                ErrorKind::Busy,
                "too many uploads in progress",
            ));
        }
        let id = genie_core::ids::new_id("upl");
        g.insert(
            id.clone(),
            Upload {
                size: p.size,
                mime: p.mime.clone().filter(|m| !m.is_empty()),
                data: Vec::with_capacity(usize::try_from(p.size.min(64 << 20)).unwrap_or(0)),
                touched: Instant::now(),
            },
        );
        Ok(UploadIdResult { upload_id: id })
    }

    pub fn chunk(&self, p: &BlobPutChunkParams) -> Result<(), RpcError> {
        let bytes = b64()
            .decode(p.data_b64.as_bytes())
            .map_err(|e| RpcError::invalid_params(format!("data_b64: {e}")))?;
        if bytes.len() > BLOB_CHUNK_BYTES {
            return Err(RpcError::invalid_params(format!(
                "chunks are at most {BLOB_CHUNK_BYTES} bytes"
            )));
        }
        let mut g = self.lock();
        let u = g
            .get_mut(&p.upload_id)
            .ok_or_else(|| RpcError::new(ErrorKind::NotFound, "upload not found"))?;
        u.touched = Instant::now();
        let have = u.data.len() as u64;
        let end = p.offset.saturating_add(bytes.len() as u64);
        if end > u.size {
            return Err(RpcError::invalid_params(
                "chunk goes past the declared size",
            ));
        }
        if p.offset == have {
            u.data.extend_from_slice(&bytes);
            return Ok(());
        }
        // A retried chunk that is already there.
        if end <= have {
            let s = usize::try_from(p.offset).unwrap_or(usize::MAX);
            let e = usize::try_from(end).unwrap_or(usize::MAX);
            if u.data.get(s..e) == Some(bytes.as_slice()) {
                return Ok(());
            }
        }
        Err(RpcError::new(
            ErrorKind::Conflict,
            format!("expected offset {have}, got {}", p.offset),
        )
        .with_data(serde_json::json!({ "expected_offset": have })))
    }

    pub fn end(
        &self,
        store: &genie_store::Store,
        upload_id: &str,
    ) -> Result<BlobPutResult, RpcError> {
        let u = {
            let mut g = self.lock();
            let Some(u) = g.get(upload_id) else {
                return Err(RpcError::new(ErrorKind::NotFound, "upload not found"));
            };
            if u.data.len() as u64 != u.size {
                return Err(RpcError::invalid_params(format!(
                    "received {} of {} bytes",
                    u.data.len(),
                    u.size
                )));
            }
            g.remove(upload_id)
        };
        let Some(u) = u else {
            return Err(RpcError::new(ErrorKind::NotFound, "upload not found"));
        };
        let id = store
            .put_blob(&u.data, u.mime.as_deref())
            .map_err(store_err)?;
        Ok(BlobPutResult {
            sha256: id.clone(),
            blob_id: id,
        })
    }
}

/// `blob.put.path`: read a local file (the client runs as the same user, 01 §1.2).
pub fn put_path(
    store: &genie_store::Store,
    p: &BlobPutPathParams,
) -> Result<BlobPutResult, RpcError> {
    let path = std::path::Path::new(&p.path);
    let meta = std::fs::metadata(path)
        .map_err(|e| RpcError::new(ErrorKind::NotFound, format!("{}: {e}", p.path)))?;
    if !meta.is_file() {
        return Err(RpcError::invalid_params(format!(
            "{} is not a file",
            p.path
        )));
    }
    if meta.len() > MAX_BLOB {
        return Err(RpcError::invalid_params(format!(
            "blobs are limited to {MAX_BLOB} bytes"
        )));
    }
    let data = std::fs::read(path)
        .map_err(|e| RpcError::new(ErrorKind::PermissionDenied, format!("{}: {e}", p.path)))?;
    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let mime = genie_core::runtime::extract::guess_mime(&name);
    let id = store.put_blob(&data, Some(mime)).map_err(store_err)?;
    Ok(BlobPutResult {
        sha256: id.clone(),
        blob_id: id,
    })
}

/// `blob.get`: a decrypted range (≤ 4 MiB) and the total size.
pub fn get(store: &genie_store::Store, p: &BlobGetParams) -> Result<BlobGetResult, RpcError> {
    if p.len > MAX_GET {
        return Err(RpcError::invalid_params(format!(
            "read at most {MAX_GET} bytes per call"
        )));
    }
    let size = store
        .blob_files()
        .len(&p.blob_id)
        .map_err(|_| RpcError::new(ErrorKind::NotFound, "blob not found"))?;
    let data = store
        .read_blob_range(&p.blob_id, p.offset, p.len)
        .map_err(store_err)?;
    Ok(BlobGetResult {
        data_b64: b64().encode(data),
        size,
    })
}
