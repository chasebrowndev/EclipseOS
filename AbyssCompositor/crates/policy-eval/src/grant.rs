// SPDX-License-Identifier: AGPL-3.0-only
//! S-01 §4 grants: the signed, immutable statement of what a principal may
//! do inside one task.
//!
//! A grant is not a session. It is a fixed set of capabilities bound to a
//! task id, an issuer, and an expiry that cannot outlive the task's deadline.
//! There is no edit operation: changing what a principal may do means issuing
//! a new grant and revoking the old one, which is what makes a grant safe to
//! cache and safe to quote in an audit record.
//!
//! `policyd` mints and signs; `abyss` only ever verifies. The asymmetry is
//! deliberate — [`Grant::verify`] takes a public key and nothing else, so no
//! amount of misuse inside the compositor can forge authority.
//!
//! Wire format is COSE_Sign1 over canonical CBOR, with every parameter the
//! COSE spec leaves open pinned by ADR 0045: EdDSA over Ed25519 (`alg` -8)
//! and nothing else, `kid` derived from the key rather than chosen, an empty
//! unprotected header, an embedded payload, and an empty `external_aad`.

use crate::cbor::{self, enc, MapBuilder, Reader, Writer};
use crate::task::Ulid;
use ed25519_dalek::{Signature, Verifier, VerifyingKey};

/// The one algorithm this verifier accepts (RFC 9053 §2.2: EdDSA).
pub const ALG_EDDSA: i64 = -8;
/// COSE header label for the algorithm (RFC 9052 §3.1).
const LABEL_ALG: i64 = 1;
/// COSE header label for the key id.
const LABEL_KID: i64 = 4;
/// The `Sig_structure` context string for a COSE_Sign1 (RFC 9052 §4.4).
const SIG_CONTEXT: &str = "Signature1";

/// A grant that failed to verify, and why.
///
/// Every variant is a deny. They are kept separate not because the caller
/// chooses differently between them — it never does — but because the audit
/// record says which one, and "expired" and "bad signature" are very
/// different things to find in a log.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VerifyError {
    /// The COSE_Sign1 or the payload is not well-formed canonical CBOR.
    Malformed(cbor::Error),
    /// `alg` was absent or was something other than [`ALG_EDDSA`].
    WrongAlgorithm,
    /// `kid` was absent or names a key other than the one we hold.
    UnknownKey,
    /// The unprotected header carried entries. Anything meaningful there is
    /// unsigned, so a non-empty one is either a mistake or an attack.
    UnprotectedHeaderNotEmpty,
    /// Ed25519 rejected the signature.
    BadSignature,
    /// `expires` has passed. Checked at request time, with no grace (S-01 §4).
    Expired,
    /// `issued` is in the future, beyond the allowed clock skew.
    NotYetValid,
}

impl From<cbor::Error> for VerifyError {
    fn from(e: cbor::Error) -> Self {
        VerifyError::Malformed(e)
    }
}

impl std::fmt::Display for VerifyError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            VerifyError::Malformed(e) => write!(f, "malformed grant: {e}"),
            VerifyError::WrongAlgorithm => f.write_str("grant algorithm is not EdDSA"),
            VerifyError::UnknownKey => f.write_str("grant kid names an unknown key"),
            VerifyError::UnprotectedHeaderNotEmpty => f.write_str("grant carries an unprotected header"),
            VerifyError::BadSignature => f.write_str("grant signature is invalid"),
            VerifyError::Expired => f.write_str("grant has expired"),
            VerifyError::NotYetValid => f.write_str("grant is not yet valid"),
        }
    }
}

impl std::error::Error for VerifyError {}

/// Tolerated clock disagreement between issue and verification.
///
/// Both ends are on the same machine today, so this exists only to absorb a
/// monotonic/wall-clock step, not to accommodate a network. It applies to
/// `issued` only — never to `expires`, which S-01 §4 says has no grace.
pub const CLOCK_SKEW_MS: u64 = 2_000;

/// One capability line: `capability "screen.capture" scope="output:DP-1" quota="…"`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Capability {
    pub name: String,
    /// Zero or more scopes. An empty scope list is *not* a wildcard — it is a
    /// capability with nothing to act on, and the enforcement table treats it
    /// as such.
    pub scopes: Vec<String>,
    pub quota: Option<String>,
}

/// One `rate` line inside `constraints`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Rate {
    pub event: String,
    pub count: u32,
    pub per_ms: u64,
}

/// The `constraints` block (S-01 §4).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Constraints {
    pub rates: Vec<Rate>,
    pub prompt_budget: Option<u32>,
}

/// The grant body — everything the signature covers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Grant {
    pub id: Ulid,
    pub principal: String,
    pub issued_ms: u64,
    /// Must be ≤ the named task's deadline. `policyd` refuses to issue
    /// otherwise; see its task store.
    pub expires_ms: u64,
    pub issuer: String,
    pub task_id: Ulid,
    pub capabilities: Vec<Capability>,
    pub constraints: Constraints,
    /// True when no human is present. Prompt-class capabilities require
    /// trusted UI unless this is set, and an unattended grant may not live
    /// longer than [`UNATTENDED_MAX_MS`] (S-01 §4).
    pub unattended: bool,
}

/// The ceiling on an unattended grant's lifetime: one hour (S-01 §4).
pub const UNATTENDED_MAX_MS: u64 = 3_600_000;

impl Grant {
    /// True if `now_ms` falls inside the validity window. Expiry is exact:
    /// a grant that expired one millisecond ago is not usable.
    pub fn is_valid_at(&self, now_ms: u64) -> Result<(), VerifyError> {
        if now_ms >= self.expires_ms {
            return Err(VerifyError::Expired);
        }
        if self.issued_ms > now_ms.saturating_add(CLOCK_SKEW_MS) {
            return Err(VerifyError::NotYetValid);
        }
        Ok(())
    }

    /// Whether this grant carries `name` with `scope`.
    ///
    /// Allocation-free, because this sits on the policy-check hot path: it
    /// compares borrowed strings and returns.
    pub fn allows(&self, name: &str, scope: &str) -> bool {
        self.capabilities
            .iter()
            .any(|c| c.name == name && c.scopes.iter().any(|s| s == scope))
    }

    /// The signed payload bytes: canonical CBOR per ADR 0044.
    pub fn encode(&self) -> Vec<u8> {
        let mut m = MapBuilder::new();
        let mut caps = Writer::new();
        caps.array(self.capabilities.len());
        for c in &self.capabilities {
            let mut cm = MapBuilder::new();
            cm.insert("name", enc(|w| w.text(&c.name)));
            cm.insert_opt("quota", c.quota.as_ref().map(|q| enc(|w| w.text(q))));
            let mut sc = Writer::new();
            sc.array(c.scopes.len());
            for s in &c.scopes {
                sc.text(s);
            }
            cm.insert("scopes", sc.finish());
            caps.raw(&cm.finish());
        }
        m.insert("capabilities", caps.finish());

        let mut cons = MapBuilder::new();
        cons.insert_opt(
            "prompt_budget",
            self.constraints.prompt_budget.map(|b| enc(|w| w.u64(b as u64))),
        );
        let mut rates = Writer::new();
        rates.array(self.constraints.rates.len());
        for r in &self.constraints.rates {
            let mut rm = MapBuilder::new();
            rm.insert("count", enc(|w| w.u64(r.count as u64)));
            rm.insert("event", enc(|w| w.text(&r.event)));
            rm.insert("per_ms", enc(|w| w.u64(r.per_ms)));
            rates.raw(&rm.finish());
        }
        cons.insert("rates", rates.finish());
        m.insert("constraints", cons.finish());

        m.insert("expires", enc(|w| w.u64(self.expires_ms)));
        m.insert("id", enc(|w| w.bytes(&self.id.0)));
        m.insert("issued", enc(|w| w.u64(self.issued_ms)));
        m.insert("issuer", enc(|w| w.text(&self.issuer)));
        m.insert("principal", enc(|w| w.text(&self.principal)));
        m.insert("task_id", enc(|w| w.bytes(&self.task_id.0)));
        m.insert("unattended", enc(|w| w.bool(self.unattended)));
        m.finish()
    }

    /// Parses a payload. Unknown keys are *refused*, not skipped: a grant is
    /// authority, and a field we do not understand may be the one that
    /// narrows it.
    pub fn decode(bytes: &[u8]) -> cbor::Result<Grant> {
        let mut r = Reader::new(bytes);
        let g = Grant::read(&mut r)?;
        r.finish()?;
        Ok(g)
    }

    fn read(r: &mut Reader<'_>) -> cbor::Result<Grant> {
        let n = r.map_begin()?;
        let mut id = None;
        let mut task_id = None;
        let mut principal = None;
        let mut issuer = None;
        let mut issued = None;
        let mut expires = None;
        let mut unattended = None;
        let mut capabilities = Vec::new();
        let mut constraints = Constraints::default();
        for _ in 0..n {
            match r.key()? {
                "capabilities" => {
                    let k = r.array_len()?;
                    for _ in 0..k {
                        capabilities.push(read_capability(r)?);
                    }
                }
                "constraints" => constraints = read_constraints(r)?,
                "expires" => expires = Some(r.u64()?),
                "id" => id = Some(Ulid(r.byte_array::<16>()?)),
                "issued" => issued = Some(r.u64()?),
                "issuer" => issuer = Some(r.text()?.to_owned()),
                "principal" => principal = Some(r.text()?.to_owned()),
                "task_id" => task_id = Some(Ulid(r.byte_array::<16>()?)),
                "unattended" => unattended = Some(r.bool()?),
                _ => return Err(cbor::Error::Type),
            }
        }
        r.map_end()?;
        Ok(Grant {
            id: id.ok_or(cbor::Error::Type)?,
            principal: principal.ok_or(cbor::Error::Type)?,
            issued_ms: issued.ok_or(cbor::Error::Type)?,
            expires_ms: expires.ok_or(cbor::Error::Type)?,
            issuer: issuer.ok_or(cbor::Error::Type)?,
            task_id: task_id.ok_or(cbor::Error::Type)?,
            capabilities,
            constraints,
            unattended: unattended.ok_or(cbor::Error::Type)?,
        })
    }

    /// Verifies a COSE_Sign1 against `key` and returns the grant inside.
    ///
    /// The order is deliberate and it is the whole security argument: nothing
    /// is parsed *as a grant* until the signature over the exact bytes has
    /// verified, and the validity window is checked only after that. A caller
    /// cannot get a `Grant` value out of this function without all three
    /// having passed.
    pub fn verify(cose: &[u8], key: &VerifyingKey, now_ms: u64) -> Result<Grant, VerifyError> {
        let mut r = Reader::new(cose);
        if r.array_len()? != 4 {
            return Err(VerifyError::Malformed(cbor::Error::Type));
        }
        let protected = r.bytes()?;
        // The unprotected header is signed by nothing, so we require it to
        // be empty rather than deciding which of its entries to trust.
        if r.map_begin()? != 0 {
            return Err(VerifyError::UnprotectedHeaderNotEmpty);
        }
        r.map_end()?;
        let payload = r.bytes()?;
        let sig = r.byte_array::<64>()?;
        r.finish()?;

        let (alg, kid) = read_protected(protected)?;
        if alg != ALG_EDDSA {
            return Err(VerifyError::WrongAlgorithm);
        }
        if kid != key_id(key) {
            return Err(VerifyError::UnknownKey);
        }

        let tbs = sig_structure(protected, payload);
        key.verify(&tbs, &Signature::from_bytes(&sig))
            .map_err(|_| VerifyError::BadSignature)?;

        let grant = Grant::decode(payload)?;
        grant.is_valid_at(now_ms)?;
        Ok(grant)
    }
}

fn read_capability(r: &mut Reader<'_>) -> cbor::Result<Capability> {
    let n = r.map_begin()?;
    let mut name = None;
    let mut quota = None;
    let mut scopes = Vec::new();
    for _ in 0..n {
        match r.key()? {
            "name" => name = Some(r.text()?.to_owned()),
            "quota" => quota = Some(r.text()?.to_owned()),
            "scopes" => {
                let k = r.array_len()?;
                for _ in 0..k {
                    scopes.push(r.text()?.to_owned());
                }
            }
            _ => return Err(cbor::Error::Type),
        }
    }
    r.map_end()?;
    Ok(Capability {
        name: name.ok_or(cbor::Error::Type)?,
        scopes,
        quota,
    })
}

fn read_constraints(r: &mut Reader<'_>) -> cbor::Result<Constraints> {
    let n = r.map_begin()?;
    let mut c = Constraints::default();
    for _ in 0..n {
        match r.key()? {
            "prompt_budget" => {
                c.prompt_budget = Some(u32::try_from(r.u64()?).map_err(|_| cbor::Error::Type)?)
            }
            "rates" => {
                let k = r.array_len()?;
                for _ in 0..k {
                    c.rates.push(read_rate(r)?);
                }
            }
            _ => return Err(cbor::Error::Type),
        }
    }
    r.map_end()?;
    Ok(c)
}

fn read_rate(r: &mut Reader<'_>) -> cbor::Result<Rate> {
    let n = r.map_begin()?;
    let (mut count, mut event, mut per_ms) = (None, None, None);
    for _ in 0..n {
        match r.key()? {
            "count" => count = Some(u32::try_from(r.u64()?).map_err(|_| cbor::Error::Type)?),
            "event" => event = Some(r.text()?.to_owned()),
            "per_ms" => per_ms = Some(r.u64()?),
            _ => return Err(cbor::Error::Type),
        }
    }
    r.map_end()?;
    Ok(Rate {
        count: count.ok_or(cbor::Error::Type)?,
        event: event.ok_or(cbor::Error::Type)?,
        per_ms: per_ms.ok_or(cbor::Error::Type)?,
    })
}

/// The key id: BLAKE3-256 of the raw 32-byte public key (ADR 0045).
///
/// Deriving it rather than letting the issuer pick one means a `kid` cannot
/// be made to name a key it does not belong to, and two deployments that hold
/// the same key agree on its name without coordinating.
pub fn key_id(key: &VerifyingKey) -> [u8; 32] {
    *blake3::hash(key.as_bytes()).as_bytes()
}

/// Builds the protected header bstr for a key. `policyd` signs over this
/// exact byte string; nothing else may produce a different encoding of the
/// same header, which is what ADR 0044 buys us.
pub fn protected_header(key: &VerifyingKey) -> Vec<u8> {
    let mut m = MapBuilder::new();
    m.insert_int(LABEL_ALG, enc(|w| w.i64(ALG_EDDSA)));
    m.insert_int(LABEL_KID, enc(|w| w.bytes(&key_id(key))));
    m.finish()
}

fn read_protected(protected: &[u8]) -> Result<(i64, [u8; 32]), VerifyError> {
    let mut r = Reader::new(protected);
    let n = r.map_begin()?;
    let mut alg = None;
    let mut kid = None;
    for _ in 0..n {
        match r.int_key()? {
            LABEL_ALG => alg = Some(r.i64()?),
            LABEL_KID => kid = Some(r.byte_array::<32>()?),
            // A protected header entry we do not understand is still signed,
            // and may still change the meaning of the signature. Refuse.
            _ => return Err(VerifyError::Malformed(cbor::Error::Type)),
        }
    }
    r.map_end()?;
    r.finish()?;
    Ok((
        alg.ok_or(VerifyError::WrongAlgorithm)?,
        kid.ok_or(VerifyError::UnknownKey)?,
    ))
}

/// The RFC 9052 §4.4 `Sig_structure`, with `external_aad` empty per ADR 0045.
pub fn sig_structure(protected: &[u8], payload: &[u8]) -> Vec<u8> {
    let mut w = Writer::new();
    w.array(4);
    w.text(SIG_CONTEXT);
    w.bytes(protected);
    w.bytes(&[]);
    w.bytes(payload);
    w.finish()
}

/// Assembles a COSE_Sign1 from its parts. Signing itself lives in `policyd`,
/// which holds the key; this is the encoding half, kept here so that the
/// bytes a signer produces and the bytes a verifier expects come from one
/// piece of source.
pub fn cose_sign1(protected: &[u8], payload: &[u8], signature: &[u8; 64]) -> Vec<u8> {
    let mut w = Writer::new();
    w.array(4);
    w.bytes(protected);
    w.raw(&MapBuilder::new().finish());
    w.bytes(payload);
    w.bytes(signature);
    w.finish()
}

#[cfg(test)]
mod tests {
    use super::*;
    use ed25519_dalek::{Signer, SigningKey};

    fn key() -> SigningKey {
        SigningKey::from_bytes(&[7u8; 32])
    }

    fn grant() -> Grant {
        Grant {
            id: Ulid::from_parts(1_000, [1; 10]),
            principal: "agent:claude".into(),
            issued_ms: 1_000,
            expires_ms: 61_000,
            issuer: "policyd".into(),
            task_id: Ulid::from_parts(900, [2; 10]),
            capabilities: vec![Capability {
                name: "screen.capture".into(),
                scopes: vec!["output:DP-1".into()],
                quota: Some("4/min".into()),
            }],
            constraints: Constraints {
                rates: vec![Rate {
                    event: "requests".into(),
                    count: 200,
                    per_ms: 10_000,
                }],
                prompt_budget: Some(3),
            },
            unattended: false,
        }
    }

    fn sign(g: &Grant, sk: &SigningKey) -> Vec<u8> {
        let vk = sk.verifying_key();
        let protected = protected_header(&vk);
        let payload = g.encode();
        let sig = sk.sign(&sig_structure(&protected, &payload));
        cose_sign1(&protected, &payload, &sig.to_bytes())
    }

    #[test]
    fn round_trips_through_cose() {
        let sk = key();
        let cose = sign(&grant(), &sk);
        let out = Grant::verify(&cose, &sk.verifying_key(), 30_000).unwrap();
        assert_eq!(out, grant());
    }

    #[test]
    fn encoding_is_deterministic() {
        assert_eq!(grant().encode(), grant().encode());
    }

    #[test]
    fn a_flipped_payload_byte_fails() {
        let sk = key();
        let mut cose = sign(&grant(), &sk);
        let n = cose.len();
        cose[n - 80] ^= 1;
        assert_eq!(
            Grant::verify(&cose, &sk.verifying_key(), 30_000),
            Err(VerifyError::BadSignature)
        );
    }

    #[test]
    fn another_key_fails_on_kid_not_on_the_signature() {
        let sk = key();
        let other = SigningKey::from_bytes(&[9u8; 32]);
        let cose = sign(&grant(), &sk);
        assert_eq!(
            Grant::verify(&cose, &other.verifying_key(), 30_000),
            Err(VerifyError::UnknownKey)
        );
    }

    #[test]
    fn expiry_has_no_grace() {
        let sk = key();
        let cose = sign(&grant(), &sk);
        assert!(Grant::verify(&cose, &sk.verifying_key(), 60_999).is_ok());
        assert_eq!(
            Grant::verify(&cose, &sk.verifying_key(), 61_000),
            Err(VerifyError::Expired)
        );
    }

    #[test]
    fn a_non_eddsa_alg_is_refused() {
        let sk = key();
        let vk = sk.verifying_key();
        let mut m = MapBuilder::new();
        m.insert_int(LABEL_ALG, enc(|w| w.i64(-7))); // ES256
        m.insert_int(LABEL_KID, enc(|w| w.bytes(&key_id(&vk))));
        let protected = m.finish();
        let payload = grant().encode();
        let sig = sk.sign(&sig_structure(&protected, &payload));
        let cose = cose_sign1(&protected, &payload, &sig.to_bytes());
        assert_eq!(
            Grant::verify(&cose, &vk, 30_000),
            Err(VerifyError::WrongAlgorithm)
        );
    }

    #[test]
    fn an_unprotected_header_entry_is_refused() {
        let sk = key();
        let vk = sk.verifying_key();
        let protected = protected_header(&vk);
        let payload = grant().encode();
        let sig = sk.sign(&sig_structure(&protected, &payload));
        let mut unprot = MapBuilder::new();
        unprot.insert_int(LABEL_KID, enc(|w| w.bytes(&[0u8; 32])));
        let mut w = Writer::new();
        w.array(4);
        w.bytes(&protected);
        w.raw(&unprot.finish());
        w.bytes(&payload);
        w.bytes(&sig.to_bytes());
        assert_eq!(
            Grant::verify(&w.finish(), &vk, 30_000),
            Err(VerifyError::UnprotectedHeaderNotEmpty)
        );
    }

    #[test]
    fn an_unknown_payload_field_is_refused_not_skipped() {
        let mut m = MapBuilder::new();
        m.insert("expires", enc(|w| w.u64(1)));
        m.insert("surprise", enc(|w| w.u64(1)));
        assert!(Grant::decode(&m.finish()).is_err());
    }

    #[test]
    fn a_missing_field_is_refused() {
        let mut m = MapBuilder::new();
        m.insert("id", enc(|w| w.bytes(&[0u8; 16])));
        assert!(Grant::decode(&m.finish()).is_err());
    }

    #[test]
    fn allows_matches_name_and_scope_together() {
        let g = grant();
        assert!(g.allows("screen.capture", "output:DP-1"));
        assert!(!g.allows("screen.capture", "output:DP-2"));
        assert!(!g.allows("input.inject", "output:DP-1"));
    }
}
