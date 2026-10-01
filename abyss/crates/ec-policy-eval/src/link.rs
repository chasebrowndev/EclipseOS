// SPDX-License-Identifier: AGPL-3.0-only
//! The first message on `policyd.sock`: `policyd` offering abyss its grant
//! verifying key (F-05, COMP-01 §6).
//!
//! The offer carries no authority of its own. abyss trusts it only because
//! of who sent it (the peer checks in `abyss::policy::link`), and only the
//! first time: a different key on a later connection is refused.
//!
//! `{"key": bstr .size 32, "v": 1}`, canonical CBOR (ADR 0044).

use crate::cbor::{self, enc, MapBuilder, Reader};
use ed25519_dalek::VerifyingKey;

/// The only offer version this build speaks.
pub const VERSION: u64 = 1;

/// Why bytes from `policyd.sock` are not a key offer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OfferError {
    Cbor(cbor::Error),
    /// A version, key or field this build does not know.
    Shape,
    /// 32 bytes that are not an Ed25519 point.
    BadKey,
}

impl From<cbor::Error> for OfferError {
    fn from(e: cbor::Error) -> Self {
        OfferError::Cbor(e)
    }
}

pub fn encode_key_offer(key: &VerifyingKey) -> Vec<u8> {
    let mut m = MapBuilder::new();
    m.insert("key", enc(|w| w.bytes(key.as_bytes())));
    m.insert("v", enc(|w| w.u64(VERSION)));
    m.finish()
}

pub fn decode_key_offer(buf: &[u8]) -> Result<VerifyingKey, OfferError> {
    let mut r = Reader::new(buf);
    if r.map_begin()? != 2 {
        return Err(OfferError::Shape);
    }
    // Canonical order is by encoded key, so the shorter "v" comes first.
    if r.key()? != "v" || r.u64()? != VERSION {
        return Err(OfferError::Shape);
    }
    if r.key()? != "key" {
        return Err(OfferError::Shape);
    }
    let key: [u8; 32] = r.byte_array()?;
    r.map_end()?;
    r.finish()?;
    VerifyingKey::from_bytes(&key).map_err(|_| OfferError::BadKey)
}

#[cfg(test)]
mod tests {
    use super::*;
    use ed25519_dalek::SigningKey;

    #[test]
    fn round_trips() {
        let key = SigningKey::from_bytes(&[7; 32]).verifying_key();
        assert_eq!(decode_key_offer(&encode_key_offer(&key)), Ok(key));
    }

    #[test]
    fn refuses_other_versions_extra_fields_and_trailing_bytes() {
        let key = SigningKey::from_bytes(&[7; 32]).verifying_key();
        let mut m = MapBuilder::new();
        m.insert("key", enc(|w| w.bytes(key.as_bytes())));
        m.insert("v", enc(|w| w.u64(2)));
        assert_eq!(decode_key_offer(&m.finish()), Err(OfferError::Shape));

        let mut m = MapBuilder::new();
        m.insert("key", enc(|w| w.bytes(key.as_bytes())));
        m.insert("v", enc(|w| w.u64(1)));
        m.insert("x", enc(|w| w.u64(0)));
        assert_eq!(decode_key_offer(&m.finish()), Err(OfferError::Shape));

        let mut b = encode_key_offer(&key);
        b.push(0);
        assert_eq!(decode_key_offer(&b), Err(OfferError::Cbor(cbor::Error::Trailing)));

        let mut m = MapBuilder::new();
        m.insert("key", enc(|w| w.bytes(&[0; 31])));
        m.insert("v", enc(|w| w.u64(1)));
        assert!(decode_key_offer(&m.finish()).is_err());
    }
}
