// SPDX-License-Identifier: AGPL-3.0-only
//! XChaCha20-Poly1305 (S-08 §2) over locked buffers.
//!
//! Sealed form: `nonce(24) || ciphertext || tag(16)`. Plaintext is staged in
//! and returned in a [`LockedBuf`], never a `Vec`, so a decrypted secret is
//! not on the ordinary heap at any point.

use crate::hygiene::LockedBuf;
use chacha20poly1305::aead::{AeadInPlace, KeyInit};
use chacha20poly1305::XChaCha20Poly1305;

pub const KEY: usize = 32;
pub const NONCE: usize = 24;
pub const TAG: usize = 16;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CryptoError {
    /// Wrong key, tampered bytes, or wrong associated data. Deliberately one
    /// case: telling them apart is an oracle.
    Auth,
    /// Shorter than a nonce and a tag.
    Format,
    /// No entropy, or no lockable memory.
    Resource,
}

impl std::fmt::Display for CryptoError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            CryptoError::Auth => "authentication failed",
            CryptoError::Format => "sealed blob is malformed",
            CryptoError::Resource => "entropy or locked memory unavailable",
        })
    }
}

impl std::error::Error for CryptoError {}

/// `N` random bytes from the kernel.
pub fn random<const N: usize>() -> Result<[u8; N], CryptoError> {
    let mut b = [0u8; N];
    getrandom::fill(&mut b).map_err(|_| CryptoError::Resource)?;
    Ok(b)
}

/// Encrypts `pt` under `key`, binding `aad`.
pub fn seal(key: &[u8], aad: &[u8], pt: &[u8]) -> Result<Vec<u8>, CryptoError> {
    let key: [u8; KEY] = key.try_into().map_err(|_| CryptoError::Format)?;
    let nonce: [u8; NONCE] = random()?;
    let mut stage = LockedBuf::from_slice(pt).map_err(|_| CryptoError::Resource)?;
    let cipher = XChaCha20Poly1305::new(&key.into());
    let tag = cipher
        .encrypt_in_place_detached(&nonce.into(), aad, stage.as_mut_slice())
        .map_err(|_| CryptoError::Auth)?;
    let mut out = Vec::with_capacity(NONCE + pt.len() + TAG);
    out.extend_from_slice(&nonce);
    out.extend_from_slice(stage.as_slice());
    out.extend_from_slice(tag.as_slice());
    Ok(out)
}

/// Decrypts a sealed blob into locked memory.
pub fn open(key: &[u8], aad: &[u8], blob: &[u8]) -> Result<LockedBuf, CryptoError> {
    let key: [u8; KEY] = key.try_into().map_err(|_| CryptoError::Format)?;
    if blob.len() < NONCE + TAG {
        return Err(CryptoError::Format);
    }
    let (nonce, rest) = blob.split_at(NONCE);
    let (body, tag) = rest.split_at(rest.len() - TAG);
    let nonce: [u8; NONCE] = nonce.try_into().map_err(|_| CryptoError::Format)?;
    let tag: [u8; TAG] = tag.try_into().map_err(|_| CryptoError::Format)?;
    let mut buf = LockedBuf::from_slice(body).map_err(|_| CryptoError::Resource)?;
    let cipher = XChaCha20Poly1305::new(&key.into());
    cipher
        .decrypt_in_place_detached(&nonce.into(), aad, buf.as_mut_slice(), &tag.into())
        .map_err(|_| CryptoError::Auth)?;
    Ok(buf)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip_and_aad_binding() {
        let key = [7u8; 32];
        let blob = seal(&key, b"a", b"value").unwrap();
        assert_eq!(open(&key, b"a", &blob).unwrap().as_slice(), b"value");
        assert_eq!(open(&key, b"b", &blob).unwrap_err(), CryptoError::Auth);
        assert_eq!(open(&[8u8; 32], b"a", &blob).unwrap_err(), CryptoError::Auth);
    }

    #[test]
    fn tampering_and_truncation_fail() {
        let key = [7u8; 32];
        let mut blob = seal(&key, b"", b"value").unwrap();
        assert!(!blob.windows(5).any(|w| w == b"value"));
        let n = blob.len();
        blob[n - 1] ^= 1;
        assert_eq!(open(&key, b"", &blob).unwrap_err(), CryptoError::Auth);
        assert_eq!(open(&key, b"", &blob[..10]).unwrap_err(), CryptoError::Format);
    }

    #[test]
    fn nonces_are_fresh() {
        let key = [1u8; 32];
        let a = seal(&key, b"", b"x").unwrap();
        let b = seal(&key, b"", b"x").unwrap();
        assert_ne!(a[..NONCE], b[..NONCE]);
    }

    #[test]
    fn empty_plaintext_is_allowed() {
        let key = [1u8; 32];
        let blob = seal(&key, b"", b"").unwrap();
        assert!(open(&key, b"", &blob).unwrap().is_empty());
    }
}
