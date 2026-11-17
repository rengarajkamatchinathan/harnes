//! Ed25519 signature check for the Genie-hosted catalog feed (03 §8.1 step 3, D40).
//!
//! The signature is detached: base64 of the 64-byte Ed25519 signature over the exact bytes
//! of `catalog.json`. The feed keys are pinned in the binary; a list allows rotation.

use base64::Engine;
use ed25519_dalek::{Signature, Verifier, VerifyingKey};

/// Pinned feed public keys (hex). Placeholder until the release signing key from D40 is
/// generated; tests and mirrors pass their own keys to [`verify_with`].
pub const PINNED_KEYS_HEX: &[&str] = &[];

#[derive(Debug, PartialEq, Eq, thiserror::Error)]
pub enum SignatureError {
    #[error("signature is not valid base64 / 64 bytes")]
    Malformed,
    #[error("no pinned key verifies this catalog")]
    Invalid,
    #[error("no feed keys are configured")]
    NoKeys,
}

pub fn parse_key(hex_or_b64: &str) -> Option<VerifyingKey> {
    let s = hex_or_b64.trim();
    let bytes: Vec<u8> = if s.len() == 64 && s.chars().all(|c| c.is_ascii_hexdigit()) {
        (0..32)
            .map(|i| u8::from_str_radix(&s[2 * i..2 * i + 2], 16))
            .collect::<Result<_, _>>()
            .ok()?
    } else {
        base64::engine::general_purpose::STANDARD.decode(s).ok()?
    };
    VerifyingKey::from_bytes(&bytes.try_into().ok()?).ok()
}

pub fn verify_with(
    keys: &[VerifyingKey],
    body: &[u8],
    sig_b64: &str,
) -> Result<(), SignatureError> {
    if keys.is_empty() {
        return Err(SignatureError::NoKeys);
    }
    let raw = base64::engine::general_purpose::STANDARD
        .decode(sig_b64.trim())
        .map_err(|_| SignatureError::Malformed)?;
    let sig = Signature::from_slice(&raw).map_err(|_| SignatureError::Malformed)?;
    if keys.iter().any(|k| k.verify(body, &sig).is_ok()) {
        Ok(())
    } else {
        Err(SignatureError::Invalid)
    }
}

pub fn pinned_keys() -> Vec<VerifyingKey> {
    PINNED_KEYS_HEX
        .iter()
        .filter_map(|k| parse_key(k))
        .collect()
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use ed25519_dalek::{Signer, SigningKey};

    pub fn key() -> SigningKey {
        SigningKey::from_bytes(&[7u8; 32])
    }
    pub fn sign(body: &[u8]) -> String {
        base64::engine::general_purpose::STANDARD.encode(key().sign(body).to_bytes())
    }

    #[test]
    fn verify_roundtrip_and_tamper() {
        let vk = key().verifying_key();
        let body = br#"{"version":1,"models":[]}"#;
        let sig = sign(body);
        assert_eq!(verify_with(&[vk], body, &sig), Ok(()));
        assert_eq!(
            verify_with(&[vk], br#"{"version":2,"models":[]}"#, &sig),
            Err(SignatureError::Invalid)
        );
        assert_eq!(
            verify_with(&[vk], body, "bm9wZQ=="),
            Err(SignatureError::Malformed)
        );
        assert_eq!(verify_with(&[], body, &sig), Err(SignatureError::NoKeys));
        let hex: String = vk.to_bytes().iter().map(|b| format!("{b:02x}")).collect();
        assert_eq!(parse_key(&hex), Some(vk));
    }
}
