//! Content-addressed, encrypted blob files (04 §1, §2, §4).
//!
//! `blobs/<sha256[0..2]>/<sha256>`, each file in the GNB1 format (see [`crate::crypto`]),
//! keyed with the blob key. The address is the plaintext SHA-256, so identical content
//! is stored once. Rows in the `blob` table (size, mime, refcount) are managed by
//! [`crate::Store`]; this module only touches files.

use std::fs::File;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

use crate::crypto::{self, CHUNK, HEADER_LEN, Key, TAG_LEN};
use crate::error::{Result, StoreError};
use crate::keys::write_private;

pub struct BlobFiles {
    root: PathBuf,
    key: Key,
}

/// SHA-256 of `data` as lowercase hex (the blob id).
pub fn sha256_hex(data: &[u8]) -> String {
    hex::encode(Sha256::digest(data))
}

fn check_id(id: &str) -> Result<[u8; 32]> {
    let mut raw = [0u8; 32];
    if id.len() != 64 || hex::decode_to_slice(id, &mut raw).is_err() || id != id.to_lowercase() {
        return Err(StoreError::Invalid(format!("not a blob id: {id:?}")));
    }
    Ok(raw)
}

impl BlobFiles {
    pub fn new(root: PathBuf, key: Key) -> Self {
        BlobFiles { root, key }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn path_for(&self, id: &str) -> Result<PathBuf> {
        check_id(id)?;
        Ok(self.root.join(&id[..2]).join(id))
    }

    /// Encrypt and write `data` unless a file with that address already exists.
    /// Returns the blob id.
    pub fn write(&self, data: &[u8]) -> Result<String> {
        let id = sha256_hex(data);
        let path = self.path_for(&id)?;
        if !path.exists() {
            let aad = check_id(&id)?;
            let enc = crypto::encrypt_blob(&self.key, data, &aad)?;
            if let Some(dir) = path.parent() {
                create_private_dir(dir)?;
            }
            write_private(&path, &enc)?;
        }
        Ok(id)
    }

    /// Read and decrypt a whole blob; checks the plaintext hash.
    pub fn read(&self, id: &str) -> Result<Vec<u8>> {
        let aad = check_id(id)?;
        let enc = std::fs::read(self.path_for(id)?)?;
        let plain = crypto::decrypt_blob(&self.key, &enc, &aad)?;
        if sha256_hex(&plain) != id {
            return Err(StoreError::Crypto(
                "blob content does not match its address",
            ));
        }
        Ok(plain)
    }

    /// Plaintext size of a stored blob, from its file length.
    pub fn len(&self, id: &str) -> Result<u64> {
        let meta = std::fs::metadata(self.path_for(id)?)?;
        crypto::plaintext_len(meta.len())
    }

    /// Read `len` plaintext bytes from `offset`, decrypting only the chunks covered.
    /// The range is clamped to the blob's end.
    pub fn read_range(&self, id: &str, offset: u64, len: u64) -> Result<Vec<u8>> {
        let aad = check_id(id)?;
        let mut f = File::open(self.path_for(id)?)?;
        let file_len = f.metadata()?.len();
        let total = crypto::plaintext_len(file_len)?;
        let end = offset.saturating_add(len).min(total);
        if offset >= end {
            return Ok(vec![]);
        }
        let mut header = [0u8; HEADER_LEN];
        f.read_exact(&mut header)?;
        let nonce = crypto::parse_header(&header)?;
        let chunk = CHUNK as u64;
        let n_chunks = total.div_ceil(chunk).max(1);
        let first = offset / chunk;
        let last = (end - 1) / chunk;
        let mut out = Vec::with_capacity(usize::try_from(end - offset).unwrap_or(0));
        for i in first..=last {
            let pos = HEADER_LEN as u64 + i * (chunk + TAG_LEN as u64);
            let plain_len = (total - i * chunk).min(chunk);
            let mut sealed = vec![0u8; usize::try_from(plain_len).unwrap_or(0) + TAG_LEN];
            f.seek(SeekFrom::Start(pos))?;
            f.read_exact(&mut sealed)?;
            let idx = u32::try_from(i).map_err(|_| StoreError::Invalid("blob too large".into()))?;
            let plain =
                crypto::decrypt_chunk(&self.key, &nonce, idx, i + 1 == n_chunks, &sealed, &aad)?;
            let chunk_start = i * chunk;
            let from = usize::try_from(offset.max(chunk_start) - chunk_start).unwrap_or(0);
            let to = usize::try_from(end.min(chunk_start + plain_len) - chunk_start).unwrap_or(0);
            out.extend_from_slice(&plain[from..to]);
        }
        Ok(out)
    }

    pub fn exists(&self, id: &str) -> bool {
        self.path_for(id).map(|p| p.exists()).unwrap_or(false)
    }

    /// Remove a blob file; missing files are fine.
    pub fn remove(&self, id: &str) -> Result<()> {
        match std::fs::remove_file(self.path_for(id)?) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(e.into()),
        }
    }
}

/// `mkdir -p` with 0700 on Unix.
pub(crate) fn create_private_dir(dir: &Path) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        std::fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(dir)?;
    }
    #[cfg(not(unix))]
    std::fs::create_dir_all(dir)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store() -> (tempfile::TempDir, BlobFiles) {
        let dir = tempfile::tempdir().unwrap();
        let b = BlobFiles::new(dir.path().join("blobs"), Key::generate().unwrap());
        (dir, b)
    }

    #[test]
    fn write_read_dedupe() {
        let (_d, b) = store();
        let id = b.write(b"hello").unwrap();
        assert_eq!(id, sha256_hex(b"hello"));
        let p = b.path_for(&id).unwrap();
        assert!(p.ends_with(format!("{}/{}", &id[..2], id)));
        let before = std::fs::read(&p).unwrap();
        assert_eq!(b.write(b"hello").unwrap(), id);
        assert_eq!(std::fs::read(&p).unwrap(), before, "dedupe keeps the file");
        assert_eq!(b.read(&id).unwrap(), b"hello");
        assert_eq!(b.len(&id).unwrap(), 5);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(&p).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
        // Not plaintext on disk.
        assert!(!before.windows(5).any(|w| w == b"hello"));
    }

    #[test]
    fn range_reads_across_chunks() {
        let (_d, b) = store();
        let data: Vec<u8> = (0..(2 * CHUNK + 1234)).map(|i| (i % 253) as u8).collect();
        let id = b.write(&data).unwrap();
        for (off, len) in [
            (0u64, 10u64),
            (CHUNK as u64 - 5, 10),
            (CHUNK as u64, CHUNK as u64),
            (2 * CHUNK as u64 + 1000, 9999),
            (5, 0),
            (10 * CHUNK as u64, 5),
        ] {
            let got = b.read_range(&id, off, len).unwrap();
            let s = (off as usize).min(data.len());
            let e = ((off + len) as usize).min(data.len());
            assert_eq!(got, &data[s..e], "range {off}+{len}");
        }
        assert_eq!(b.read(&id).unwrap(), data);
    }

    #[test]
    fn swapped_file_is_rejected() {
        let (_d, b) = store();
        let a = b.write(b"aaa").unwrap();
        let c = b.write(b"ccc").unwrap();
        std::fs::copy(b.path_for(&a).unwrap(), b.path_for(&c).unwrap()).unwrap();
        assert!(b.read(&c).is_err(), "AAD binds the file to its address");
        assert!(b.path_for("../etc/passwd").is_err());
        b.remove(&a).unwrap();
        b.remove(&a).unwrap();
        assert!(!b.exists(&a));
    }
}
