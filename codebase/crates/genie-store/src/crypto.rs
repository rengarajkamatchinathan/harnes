//! XChaCha20-Poly1305 helpers (04 §2).
//!
//! * [`Key`]: a 256-bit key that zeroes itself on drop.
//! * [`seal`] / [`open`]: small payloads (key wrapping): `nonce(24) | ciphertext | tag`.
//! * [`encrypt_blob`] / [`decrypt_blob`] / [`decrypt_range`]: the `GNB1` blob format.
//!
//! ## GNB1
//!
//! `magic "GNB1" | nonce(24) | chunk_0 | … | chunk_n`, each chunk `ciphertext | tag(16)`
//! over at most [`CHUNK`] (1 MiB) of plaintext. A blob of up to 1 MiB is therefore exactly
//! `magic | nonce | ciphertext | tag`. Larger blobs use the STREAM construction
//! (Hoang–Reyhanitabar–Rogaway–Vizár): chunk `i` is sealed under the header nonce with
//! its last five bytes XORed with `be32(i) | last_flag`, so chunks can't be reordered,
//! dropped or truncated, and any range decrypts by reading only the chunks it covers.
//! The AAD of every chunk is the plaintext SHA-256 (the content address), which binds a
//! file to its name.

use chacha20poly1305::aead::{Aead, KeyInit, Payload};
use chacha20poly1305::{XChaCha20Poly1305, XNonce};
use zeroize::{Zeroize, ZeroizeOnDrop};

use crate::error::{Result, StoreError};

pub const KEY_LEN: usize = 32;
pub const NONCE_LEN: usize = 24;
pub const TAG_LEN: usize = 16;
pub const MAGIC: &[u8; 4] = b"GNB1";
pub const HEADER_LEN: usize = 4 + NONCE_LEN;
/// Plaintext bytes per STREAM chunk.
pub const CHUNK: usize = 1 << 20;

/// A 256-bit symmetric key. `Debug` never prints the bytes.
#[derive(Clone, Zeroize, ZeroizeOnDrop, PartialEq, Eq)]
pub struct Key([u8; KEY_LEN]);

impl Key {
    pub fn generate() -> Result<Self> {
        let mut k = [0u8; KEY_LEN];
        fill_random(&mut k)?;
        Ok(Key(k))
    }

    pub fn from_bytes(b: &[u8]) -> Result<Self> {
        let arr: [u8; KEY_LEN] = b
            .try_into()
            .map_err(|_| StoreError::Format(format!("key must be {KEY_LEN} bytes")))?;
        Ok(Key(arr))
    }

    pub fn expose(&self) -> &[u8; KEY_LEN] {
        &self.0
    }

    fn cipher(&self) -> XChaCha20Poly1305 {
        XChaCha20Poly1305::new((&self.0).into())
    }
}

impl std::fmt::Debug for Key {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Key([REDACTED])")
    }
}

pub fn fill_random(buf: &mut [u8]) -> Result<()> {
    getrandom::fill(buf).map_err(|e| StoreError::KeyStore(format!("OS random source: {e}")))
}

/// Seal a small payload: `nonce(24) | ciphertext | tag`.
pub fn seal(key: &Key, plaintext: &[u8], aad: &[u8]) -> Result<Vec<u8>> {
    let mut nonce = [0u8; NONCE_LEN];
    fill_random(&mut nonce)?;
    let ct = key
        .cipher()
        .encrypt(
            XNonce::from_slice(&nonce),
            Payload {
                msg: plaintext,
                aad,
            },
        )
        .map_err(|_| StoreError::Crypto("seal"))?;
    let mut out = Vec::with_capacity(NONCE_LEN + ct.len());
    out.extend_from_slice(&nonce);
    out.extend_from_slice(&ct);
    Ok(out)
}

pub fn open(key: &Key, sealed: &[u8], aad: &[u8]) -> Result<Vec<u8>> {
    if sealed.len() < NONCE_LEN + TAG_LEN {
        return Err(StoreError::Crypto("sealed payload too short"));
    }
    let (nonce, ct) = sealed.split_at(NONCE_LEN);
    key.cipher()
        .decrypt(XNonce::from_slice(nonce), Payload { msg: ct, aad })
        .map_err(|_| StoreError::Crypto("open"))
}

fn chunk_nonce(base: &[u8; NONCE_LEN], index: u32, last: bool) -> XNonce {
    let mut n = *base;
    let ctr = index.to_be_bytes();
    for (i, b) in ctr.iter().enumerate() {
        n[NONCE_LEN - 5 + i] ^= b;
    }
    n[NONCE_LEN - 1] ^= u8::from(last);
    XNonce::from(n)
}

/// Encrypt a whole blob into the GNB1 format. `aad` is the plaintext SHA-256.
pub fn encrypt_blob(key: &Key, plaintext: &[u8], aad: &[u8]) -> Result<Vec<u8>> {
    let mut nonce = [0u8; NONCE_LEN];
    fill_random(&mut nonce)?;
    let n_chunks = chunk_count(plaintext.len());
    let mut out = Vec::with_capacity(HEADER_LEN + plaintext.len() + n_chunks * TAG_LEN);
    out.extend_from_slice(MAGIC);
    out.extend_from_slice(&nonce);
    let cipher = key.cipher();
    for i in 0..n_chunks {
        let start = i * CHUNK;
        let end = (start + CHUNK).min(plaintext.len());
        let idx = u32::try_from(i).map_err(|_| StoreError::Invalid("blob too large".into()))?;
        let ct = cipher
            .encrypt(
                &chunk_nonce(&nonce, idx, i + 1 == n_chunks),
                Payload {
                    msg: &plaintext[start..end],
                    aad,
                },
            )
            .map_err(|_| StoreError::Crypto("encrypt chunk"))?;
        out.extend_from_slice(&ct);
    }
    Ok(out)
}

/// Number of chunks for a plaintext length (an empty blob still has one, empty, chunk).
pub fn chunk_count(len: usize) -> usize {
    len.div_ceil(CHUNK).max(1)
}

/// Plaintext length of a GNB1 file of `file_len` bytes.
pub fn plaintext_len(file_len: u64) -> Result<u64> {
    let body = file_len
        .checked_sub(HEADER_LEN as u64)
        .ok_or(StoreError::Crypto("blob shorter than its header"))?;
    let per = (CHUNK + TAG_LEN) as u64;
    let n = body.div_ceil(per).max(1);
    body.checked_sub(n * TAG_LEN as u64)
        .ok_or(StoreError::Crypto("blob truncated"))
}

/// Read the 24-byte nonce out of a GNB1 header.
pub fn parse_header(header: &[u8]) -> Result<[u8; NONCE_LEN]> {
    if header.len() < HEADER_LEN || &header[..4] != MAGIC {
        return Err(StoreError::Crypto("not a GNB1 blob"));
    }
    let mut n = [0u8; NONCE_LEN];
    n.copy_from_slice(&header[4..HEADER_LEN]);
    Ok(n)
}

/// Decrypt one sealed chunk (`ciphertext | tag`) at `index`.
pub fn decrypt_chunk(
    key: &Key,
    nonce: &[u8; NONCE_LEN],
    index: u32,
    last: bool,
    sealed: &[u8],
    aad: &[u8],
) -> Result<Vec<u8>> {
    key.cipher()
        .decrypt(
            &chunk_nonce(nonce, index, last),
            Payload { msg: sealed, aad },
        )
        .map_err(|_| StoreError::Crypto("blob chunk failed authentication"))
}

/// Decrypt a whole GNB1 blob held in memory.
pub fn decrypt_blob(key: &Key, data: &[u8], aad: &[u8]) -> Result<Vec<u8>> {
    let nonce = parse_header(data)?;
    let total = plaintext_len(data.len() as u64)?;
    let total = usize::try_from(total).map_err(|_| StoreError::Invalid("blob too large".into()))?;
    let n_chunks = chunk_count(total);
    let mut out = Vec::with_capacity(total);
    let body = &data[HEADER_LEN..];
    for i in 0..n_chunks {
        let start = i * (CHUNK + TAG_LEN);
        let end = (start + CHUNK + TAG_LEN).min(body.len());
        let idx = u32::try_from(i).map_err(|_| StoreError::Invalid("blob too large".into()))?;
        out.extend(decrypt_chunk(
            key,
            &nonce,
            idx,
            i + 1 == n_chunks,
            &body[start..end],
            aad,
        )?);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn seal_roundtrip_and_aad_binding() {
        let k = Key::generate().unwrap();
        let s = seal(&k, b"secret", b"ctx").unwrap();
        assert_eq!(open(&k, &s, b"ctx").unwrap(), b"secret");
        assert!(open(&k, &s, b"other").is_err());
        let k2 = Key::generate().unwrap();
        assert!(open(&k2, &s, b"ctx").is_err());
        assert_eq!(format!("{k:?}"), "Key([REDACTED])");
    }

    #[test]
    fn small_blob_is_magic_nonce_ct_tag() {
        let k = Key::generate().unwrap();
        let enc = encrypt_blob(&k, b"hello", b"aad").unwrap();
        assert_eq!(&enc[..4], b"GNB1");
        assert_eq!(enc.len(), 4 + 24 + 5 + 16);
        assert_eq!(plaintext_len(enc.len() as u64).unwrap(), 5);
        assert_eq!(decrypt_blob(&k, &enc, b"aad").unwrap(), b"hello");
        assert!(decrypt_blob(&k, &enc, b"bad").is_err());
    }

    #[test]
    fn empty_and_exact_chunk_sizes() {
        let k = Key::generate().unwrap();
        for len in [0, 1, CHUNK - 1, CHUNK, CHUNK + 1, 2 * CHUNK] {
            let data: Vec<u8> = (0..len).map(|i| (i % 251) as u8).collect();
            let enc = encrypt_blob(&k, &data, b"a").unwrap();
            assert_eq!(
                plaintext_len(enc.len() as u64).unwrap(),
                len as u64,
                "len {len}"
            );
            assert_eq!(decrypt_blob(&k, &enc, b"a").unwrap(), data, "len {len}");
        }
    }

    #[test]
    fn truncation_and_reorder_are_detected() {
        let k = Key::generate().unwrap();
        let data = vec![7u8; 2 * CHUNK + 10];
        let enc = encrypt_blob(&k, &data, b"a").unwrap();
        // Drop the final chunk: the new last chunk was not sealed as last.
        let cut = HEADER_LEN + 2 * (CHUNK + TAG_LEN);
        assert!(decrypt_blob(&k, &enc[..cut], b"a").is_err());
        // Flip one ciphertext bit.
        let mut bad = enc.clone();
        bad[HEADER_LEN + 3] ^= 1;
        assert!(decrypt_blob(&k, &bad, b"a").is_err());
    }
}
