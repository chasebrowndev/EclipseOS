// SPDX-License-Identifier: AGPL-3.0-only
//! The master-key sealer (S-08 §2, §9.1).
//!
//! The store's keys are wrapped by one master key. Where that key rests is
//! the sealer's business, behind [`Sealer`]:
//!
//! * [`SoftSealer`] is the software/mock backend. It wraps the master under a
//!   device key held in memory and binds a *policy* string as associated
//!   data, which stands in for "sealed to a policy signed by a local key"
//!   (S-08 §9.1) so a test can show that a changed policy refuses to unseal.
//!   It is **not** hardware bound. It exists for tests and for a first
//!   bring-up on a machine with no TPM and no wish to type a passphrase.
//! * [`PassphraseSealer`] is the spec's fallback: an Argon2id-derived key.
//! * A TPM backend (`tss-esapi`, `TPM2_Create` under `PolicyAuthorize`) is a
//!   third implementation of the same trait. It is not here: no TSS
//!   dependency until the hardware path is wanted (TCB-HOOK in the report).

use crate::aead::{self, CryptoError};
use crate::hygiene::LockedBuf;
use argon2::{Algorithm, Argon2, Params, Version};
use zeroize::Zeroizing;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SealError {
    /// Wrong passphrase, wrong policy, tampered blob.
    Auth,
    Format,
    /// The backend needs a passphrase and was given none.
    NeedsPassphrase,
    /// No TPM, no entropy, no lockable memory.
    Unavailable,
}

impl std::fmt::Display for SealError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            SealError::Auth => "could not unseal the master key",
            SealError::Format => "sealed master key is malformed",
            SealError::NeedsPassphrase => "a passphrase is required",
            SealError::Unavailable => "sealing backend unavailable",
        })
    }
}

impl std::error::Error for SealError {}

impl From<CryptoError> for SealError {
    fn from(e: CryptoError) -> Self {
        match e {
            CryptoError::Auth => SealError::Auth,
            CryptoError::Format => SealError::Format,
            CryptoError::Resource => SealError::Unavailable,
        }
    }
}

/// Wraps and unwraps the 32-byte master key.
pub trait Sealer {
    fn name(&self) -> &'static str;
    /// Whether unlock must collect a passphrase from the human. The trusted
    /// UI prompt differs: a passphrase field, or only a presence confirm.
    fn needs_passphrase(&self) -> bool;
    fn seal(&self, master: &[u8], pass: Option<&[u8]>) -> Result<Vec<u8>, SealError>;
    fn unseal(&self, blob: &[u8], pass: Option<&[u8]>) -> Result<LockedBuf, SealError>;
}

const SOFT_MAGIC: &[u8; 5] = b"SOFT1";
const PASS_MAGIC: &[u8; 5] = b"PASS1";

/// The software/mock sealer. See the module comment.
pub struct SoftSealer {
    device_key: Zeroizing<[u8; 32]>,
    policy: Vec<u8>,
}

impl SoftSealer {
    pub fn new(device_key: [u8; 32], policy: &[u8]) -> Self {
        SoftSealer {
            device_key: Zeroizing::new(device_key),
            policy: policy.to_vec(),
        }
    }

    fn aad(&self) -> Vec<u8> {
        let mut a = b"eclipse/brokerd/seal/soft/v1".to_vec();
        a.extend_from_slice(&self.policy);
        a
    }
}

impl Sealer for SoftSealer {
    fn name(&self) -> &'static str {
        "soft"
    }

    fn needs_passphrase(&self) -> bool {
        false
    }

    fn seal(&self, master: &[u8], _pass: Option<&[u8]>) -> Result<Vec<u8>, SealError> {
        let mut out = SOFT_MAGIC.to_vec();
        out.extend(aead::seal(&*self.device_key, &self.aad(), master)?);
        Ok(out)
    }

    fn unseal(&self, blob: &[u8], _pass: Option<&[u8]>) -> Result<LockedBuf, SealError> {
        let body = blob.strip_prefix(SOFT_MAGIC).ok_or(SealError::Format)?;
        Ok(aead::open(&*self.device_key, &self.aad(), body)?)
    }
}

/// Argon2id cost parameters, stored in the sealed header so they can be
/// raised without orphaning an existing store.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct KdfParams {
    pub m_kib: u32,
    pub t: u32,
    pub p: u32,
}

impl KdfParams {
    /// OWASP's second recommendation: 64 MiB, 3 passes, one lane.
    pub const PRODUCTION: KdfParams = KdfParams {
        m_kib: 65536,
        t: 3,
        p: 1,
    };
    /// For tests only; the minimum Argon2 accepts.
    pub const TEST: KdfParams = KdfParams { m_kib: 8, t: 1, p: 1 };

    /// A tampered header must not be able to ask for a gigabyte of memory.
    fn sane(self) -> bool {
        self.m_kib >= 8 && self.m_kib <= 1 << 20 && (1..=16).contains(&self.t) && (1..=16).contains(&self.p)
    }
}

/// The passphrase fallback (S-08 §2).
pub struct PassphraseSealer {
    pub params: KdfParams,
}

impl PassphraseSealer {
    fn derive(params: KdfParams, pass: &[u8], salt: &[u8]) -> Result<LockedBuf, SealError> {
        if !params.sane() {
            return Err(SealError::Format);
        }
        let p = Params::new(params.m_kib, params.t, params.p, Some(32)).map_err(|_| SealError::Format)?;
        let mut key = LockedBuf::new(32).map_err(|_| SealError::Unavailable)?;
        Argon2::new(Algorithm::Argon2id, Version::V0x13, p)
            .hash_password_into(pass, salt, key.as_mut_slice())
            .map_err(|_| SealError::Format)?;
        Ok(key)
    }

    fn aad(header: &[u8]) -> Vec<u8> {
        let mut a = b"eclipse/brokerd/seal/pass/v1".to_vec();
        a.extend_from_slice(header);
        a
    }
}

impl Sealer for PassphraseSealer {
    fn name(&self) -> &'static str {
        "passphrase"
    }

    fn needs_passphrase(&self) -> bool {
        true
    }

    fn seal(&self, master: &[u8], pass: Option<&[u8]>) -> Result<Vec<u8>, SealError> {
        let pass = pass.ok_or(SealError::NeedsPassphrase)?;
        let salt: [u8; 16] = aead::random()?;
        // header = m(4) t(4) p(4) salt(16), all big endian.
        let mut header = Vec::with_capacity(28);
        header.extend_from_slice(&self.params.m_kib.to_be_bytes());
        header.extend_from_slice(&self.params.t.to_be_bytes());
        header.extend_from_slice(&self.params.p.to_be_bytes());
        header.extend_from_slice(&salt);
        let key = Self::derive(self.params, pass, &salt)?;
        let mut out = PASS_MAGIC.to_vec();
        out.extend_from_slice(&header);
        out.extend(aead::seal(key.as_slice(), &Self::aad(&header), master)?);
        Ok(out)
    }

    fn unseal(&self, blob: &[u8], pass: Option<&[u8]>) -> Result<LockedBuf, SealError> {
        let pass = pass.ok_or(SealError::NeedsPassphrase)?;
        let rest = blob.strip_prefix(PASS_MAGIC).ok_or(SealError::Format)?;
        if rest.len() < 28 {
            return Err(SealError::Format);
        }
        let (header, sealed) = rest.split_at(28);
        let word = |i: usize| u32::from_be_bytes([header[i], header[i + 1], header[i + 2], header[i + 3]]);
        let params = KdfParams {
            m_kib: word(0),
            t: word(4),
            p: word(8),
        };
        let key = Self::derive(params, pass, &header[12..28])?;
        Ok(aead::open(key.as_slice(), &Self::aad(header), sealed)?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn soft_round_trip_and_policy_binding() {
        let master = [9u8; 32];
        let s = SoftSealer::new([1; 32], b"policy-A");
        let blob = s.seal(&master, None).unwrap();
        assert_eq!(s.unseal(&blob, None).unwrap().as_slice(), &master);
        // A different policy (a re-signed boot chain it was not sealed to)
        // refuses, which is how a "PCR changed" TPM would behave.
        let other = SoftSealer::new([1; 32], b"policy-B");
        assert_eq!(other.unseal(&blob, None).unwrap_err(), SealError::Auth);
        let wrong_dev = SoftSealer::new([2; 32], b"policy-A");
        assert_eq!(wrong_dev.unseal(&blob, None).unwrap_err(), SealError::Auth);
    }

    #[test]
    fn passphrase_round_trip() {
        let s = PassphraseSealer {
            params: KdfParams::TEST,
        };
        let blob = s.seal(&[5u8; 32], Some(b"correct horse")).unwrap();
        assert_eq!(
            s.unseal(&blob, Some(b"correct horse")).unwrap().as_slice(),
            &[5u8; 32]
        );
        assert_eq!(s.unseal(&blob, Some(b"wrong")).unwrap_err(), SealError::Auth);
        assert_eq!(s.unseal(&blob, None).unwrap_err(), SealError::NeedsPassphrase);
    }

    #[test]
    fn a_tampered_kdf_header_is_refused() {
        let s = PassphraseSealer {
            params: KdfParams::TEST,
        };
        let mut blob = s.seal(&[5u8; 32], Some(b"pw")).unwrap();
        // Ask for 4 GiB of memory.
        blob[5..9].copy_from_slice(&u32::MAX.to_be_bytes());
        assert_eq!(s.unseal(&blob, Some(b"pw")).unwrap_err(), SealError::Format);
    }

    #[test]
    fn the_blob_does_not_contain_the_master() {
        let s = SoftSealer::new([1; 32], b"p");
        let blob = s.seal(&[0x5a; 32], None).unwrap();
        assert!(!blob.windows(8).any(|w| w == [0x5a; 8]));
    }
}
