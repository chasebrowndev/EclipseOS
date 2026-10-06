// SPDX-License-Identifier: AGPL-3.0-only
//! The enforcement table's wire form and signature (COMP-11 §2, S-02 §4).
//! TCB.
//!
//! `policyd` compiles policy into a [`Table`], encodes it here, signs the
//! bytes and pushes them; abyss verifies the signature against the key it
//! pinned, refuses a version that does not move forward, then decodes. Both
//! ends use this one codec, so the table abyss evaluates is the table
//! `policyd` compiled (ADR 0008).
//!
//! - Canonical CBOR (ADR 0044). The decoder refuses unknown predicates,
//!   unknown fields and trailing bytes: a table this build cannot fully read
//!   is rejected whole, and the previous one stays live.
//! - Regexes and globs travel as source text and are rebuilt on decode, the
//!   regexes under a size limit, so a table cannot buy unbounded memory.
//! - The signature is over a domain-separated message
//!   ([`SIGNING_CONTEXT`] then the table bytes), so a table signature can
//!   never be replayed as any other object the key signs.

use ed25519_dalek::{Signature, Signer, SigningKey, Verifier, VerifyingKey};
use regex::{Regex, RegexBuilder};

use crate::cbor::{self, enc, MapBuilder, Reader, Writer};
use crate::check::{CompiledRule, Pred, Table, Trust, Unless};
use crate::scope::{Class, Glob};

pub const VERSION: u64 = 1;

/// Prefixed to the table bytes before signing.
pub const SIGNING_CONTEXT: &[u8] = b"eclipse-enforcement-table-v1\0";

/// A compiled regex may use at most this much memory.
pub const REGEX_LIMIT: usize = 1 << 20;

/// Rules per phase, predicates per rule, and values per predicate. Far past
/// any human-written policy; a table beyond them is refused.
const MAX_RULES: u64 = 4096;
const MAX_ITEMS: u64 = 256;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TableError {
    Cbor(cbor::Error),
    Shape,
    /// A regex that does not compile within [`REGEX_LIMIT`].
    Regex,
    BadSignature,
}

impl From<cbor::Error> for TableError {
    fn from(e: cbor::Error) -> Self {
        TableError::Cbor(e)
    }
}

pub fn regex(src: &str) -> Result<Regex, TableError> {
    RegexBuilder::new(src)
        .size_limit(REGEX_LIMIT)
        .build()
        .map_err(|_| TableError::Regex)
}

fn class_str(c: Class) -> &'static str {
    match c {
        Class::Public => "public",
        Class::Private => "private",
        Class::Secret => "secret",
    }
}

pub fn class_of(s: &str) -> Option<Class> {
    Some(match s {
        "public" => Class::Public,
        "private" => Class::Private,
        "secret" => Class::Secret,
        _ => return None,
    })
}

fn trust_str(t: Trust) -> &'static str {
    match t {
        Trust::Untrusted => "untrusted",
        Trust::Standard => "standard",
        Trust::Trusted => "trusted",
        Trust::Human => "human",
    }
}

pub fn trust_of(s: &str) -> Option<Trust> {
    Some(match s {
        "untrusted" => Trust::Untrusted,
        "standard" => Trust::Standard,
        "trusted" => Trust::Trusted,
        "human" => Trust::Human,
        _ => return None,
    })
}

fn texts<'a>(w: &mut Writer, items: impl ExactSizeIterator<Item = &'a str>) {
    w.array(items.len());
    for s in items {
        w.text(s);
    }
}

fn encode_pred(p: &Pred) -> Vec<u8> {
    let mut m = MapBuilder::new();
    let (name, value): (&str, Option<(&str, Vec<u8>)>) = match p {
        Pred::Capability(g) => (
            "capability",
            Some(("a", enc(|w| texts(w, g.iter().map(Glob::as_str))))),
        ),
        Pred::TargetClass(c) => (
            "target_class",
            Some(("a", enc(|w| texts(w, c.iter().map(|c| class_str(*c)))))),
        ),
        Pred::TargetAppId(g) => (
            "target_app_id",
            Some(("a", enc(|w| texts(w, g.iter().map(Glob::as_str))))),
        ),
        Pred::Title(re) => ("title", Some(("re", enc(|w| w.text(re.as_str()))))),
        Pred::NodeRole(r) => (
            "node_role",
            Some(("a", enc(|w| texts(w, r.iter().map(String::as_str))))),
        ),
        Pred::NodeName(re) => ("node_name", Some(("re", enc(|w| w.text(re.as_str()))))),
        Pred::NodeSource(s) => (
            "node_source",
            Some(("a", enc(|w| texts(w, s.iter().map(String::as_str))))),
        ),
        // Thousandths: the table carries no floats, so it has one encoding.
        Pred::NodeConfidenceLt(v) => (
            "node_confidence_lt",
            Some((
                "milli",
                enc(|w| w.u64((v.clamp(0.0, 1.0) * 1000.0).round() as u64)),
            )),
        ),
        Pred::Irreversible(g) => (
            "irreversible",
            Some(("a", enc(|w| texts(w, g.iter().map(Glob::as_str))))),
        ),
        Pred::Url(g) => ("url", Some(("a", enc(|w| texts(w, g.iter().map(Glob::as_str)))))),
        Pred::Principal(g) => (
            "principal",
            Some(("a", enc(|w| texts(w, g.iter().map(Glob::as_str))))),
        ),
        Pred::PrincipalProfile(p) => (
            "principal_profile",
            Some(("a", enc(|w| texts(w, p.iter().map(String::as_str))))),
        ),
        Pred::AppTrust(t) => (
            "app_trust",
            Some(("a", enc(|w| texts(w, t.iter().map(|t| trust_str(*t)))))),
        ),
        Pred::AppIrreversibleCapable(b) => ("app_irreversible_capable", Some(("b", enc(|w| w.bool(*b))))),
        Pred::ProvenanceContainsTrust(t) => (
            "provenance_contains_trust",
            Some(("a", enc(|w| texts(w, [trust_str(*t)].into_iter())))),
        ),
        Pred::ProvenanceContainsSource(s) => (
            "provenance_contains_source",
            Some(("a", enc(|w| texts(w, [s.as_str()].into_iter())))),
        ),
        Pred::ProvenanceAbsent => ("provenance_absent", None),
    };
    m.insert("p", enc(|w| w.text(name)));
    if let Some((k, v)) = value {
        m.insert(k, v);
    }
    m.finish()
}

fn encode_rule(r: &CompiledRule) -> Vec<u8> {
    let mut m = MapBuilder::new();
    m.insert("id", enc(|w| w.text(&r.id)));
    m.insert(
        "preds",
        enc(|w| {
            w.array(r.preds.len());
            for p in &r.preds {
                w.raw(&encode_pred(p));
            }
        }),
    );
    m.insert(
        "unless",
        enc(|w| {
            w.array(r.unless.len());
            for u in &r.unless {
                let mut e = MapBuilder::new();
                e.insert("capability", enc(|w| w.text(&u.capability)));
                e.insert("unattended", enc(|w| w.bool(u.unattended)));
                w.raw(&e.finish());
            }
        }),
    );
    m.finish()
}

fn encode_phase(rules: &[CompiledRule]) -> Vec<u8> {
    enc(|w| {
        w.array(rules.len());
        for r in rules {
            w.raw(&encode_rule(r));
        }
    })
}

/// The table's canonical bytes: what is signed and what is pushed.
pub fn encode(t: &Table) -> Vec<u8> {
    let mut m = MapBuilder::new();
    m.insert("v", enc(|w| w.u64(VERSION)));
    m.insert("deny", encode_phase(&t.rules.deny));
    m.insert("allow", encode_phase(&t.rules.allow));
    m.insert("defer", encode_phase(&t.rules.defer));
    m.insert("prompt", encode_phase(&t.rules.prompt));
    m.insert("version", enc(|w| w.u64(t.version)));
    m.finish()
}

fn message(bytes: &[u8]) -> Vec<u8> {
    let mut m = Vec::with_capacity(SIGNING_CONTEXT.len() + bytes.len());
    m.extend_from_slice(SIGNING_CONTEXT);
    m.extend_from_slice(bytes);
    m
}

pub fn sign(key: &SigningKey, bytes: &[u8]) -> [u8; 64] {
    key.sign(&message(bytes)).to_bytes()
}

pub fn verify(key: &VerifyingKey, bytes: &[u8], sig: &[u8; 64]) -> Result<(), TableError> {
    key.verify(&message(bytes), &Signature::from_bytes(sig))
        .map_err(|_| TableError::BadSignature)
}

fn read_texts<'a>(r: &mut Reader<'a>) -> Result<Vec<&'a str>, TableError> {
    let n = r.array_len()?;
    if n == 0 || n > MAX_ITEMS {
        return Err(TableError::Shape);
    }
    (0..n).map(|_| r.text().map_err(TableError::from)).collect()
}

fn one<T>(mut v: Vec<T>) -> Result<T, TableError> {
    if v.len() == 1 {
        Ok(v.remove(0))
    } else {
        Err(TableError::Shape)
    }
}

fn decode_pred(r: &mut Reader<'_>) -> Result<Pred, TableError> {
    let len = r.map_begin()?;
    let (mut name, mut a, mut re, mut b, mut milli) = (None, None, None, None, None);
    for _ in 0..len {
        match r.key()? {
            "a" => a = Some(read_texts(r)?),
            "b" => b = Some(r.bool()?),
            "p" => name = Some(r.text()?),
            "re" => re = Some(r.text()?),
            "milli" => milli = Some(r.u64()?),
            _ => return Err(TableError::Shape),
        }
    }
    r.map_end()?;
    let fields = [a.is_some(), b.is_some(), re.is_some(), milli.is_some()]
        .iter()
        .filter(|x| **x)
        .count();
    let globs = |v: Vec<&str>| v.into_iter().map(Glob::new).collect::<Vec<_>>();
    let strings = |v: Vec<&str>| v.into_iter().map(str::to_owned).collect::<Vec<_>>();
    let need_a = || a.clone().ok_or(TableError::Shape);
    let pred = match name.ok_or(TableError::Shape)? {
        "capability" => Pred::Capability(globs(need_a()?)),
        "target_class" => Pred::TargetClass(
            need_a()?
                .into_iter()
                .map(|c| class_of(c).ok_or(TableError::Shape))
                .collect::<Result<_, _>>()?,
        ),
        "target_app_id" => Pred::TargetAppId(globs(need_a()?)),
        "title" => Pred::Title(regex(re.ok_or(TableError::Shape)?)?),
        "node_role" => Pred::NodeRole(strings(need_a()?)),
        "node_name" => Pred::NodeName(regex(re.ok_or(TableError::Shape)?)?),
        "node_source" => Pred::NodeSource(strings(need_a()?)),
        "node_confidence_lt" => {
            let m = milli.ok_or(TableError::Shape)?;
            if m > 1000 {
                return Err(TableError::Shape);
            }
            Pred::NodeConfidenceLt(m as f32 / 1000.0)
        }
        "irreversible" => Pred::Irreversible(globs(need_a()?)),
        "url" => Pred::Url(globs(need_a()?)),
        "principal" => Pred::Principal(globs(need_a()?)),
        "principal_profile" => Pred::PrincipalProfile(strings(need_a()?)),
        "app_trust" => Pred::AppTrust(
            need_a()?
                .into_iter()
                .map(|t| trust_of(t).ok_or(TableError::Shape))
                .collect::<Result<_, _>>()?,
        ),
        "app_irreversible_capable" => Pred::AppIrreversibleCapable(b.ok_or(TableError::Shape)?),
        "provenance_contains_trust" => {
            Pred::ProvenanceContainsTrust(trust_of(one(need_a()?)?).ok_or(TableError::Shape)?)
        }
        "provenance_contains_source" => Pred::ProvenanceContainsSource(one(need_a()?)?.to_owned()),
        "provenance_absent" => {
            if fields != 0 {
                return Err(TableError::Shape);
            }
            Pred::ProvenanceAbsent
        }
        _ => return Err(TableError::Shape),
    };
    // Exactly one value field for every predicate but `provenance_absent`.
    if !matches!(pred, Pred::ProvenanceAbsent) && fields != 1 {
        return Err(TableError::Shape);
    }
    Ok(pred)
}

fn decode_rule(r: &mut Reader<'_>) -> Result<CompiledRule, TableError> {
    let len = r.map_begin()?;
    let (mut id, mut preds, mut unless) = (None, None, None);
    for _ in 0..len {
        match r.key()? {
            "id" => id = Some(r.text()?.to_owned()),
            "preds" => {
                let n = r.array_len()?;
                if n > MAX_ITEMS {
                    return Err(TableError::Shape);
                }
                preds = Some((0..n).map(|_| decode_pred(r)).collect::<Result<Vec<_>, _>>()?);
            }
            "unless" => {
                let n = r.array_len()?;
                if n > MAX_ITEMS {
                    return Err(TableError::Shape);
                }
                let mut v = Vec::with_capacity(n as usize);
                for _ in 0..n {
                    let len = r.map_begin()?;
                    let (mut cap, mut un) = (None, None);
                    for _ in 0..len {
                        match r.key()? {
                            "capability" => cap = Some(r.text()?.to_owned()),
                            "unattended" => un = Some(r.bool()?),
                            _ => return Err(TableError::Shape),
                        }
                    }
                    r.map_end()?;
                    v.push(Unless {
                        capability: cap.ok_or(TableError::Shape)?,
                        unattended: un.ok_or(TableError::Shape)?,
                    });
                }
                unless = Some(v);
            }
            _ => return Err(TableError::Shape),
        }
    }
    r.map_end()?;
    let id = id.ok_or(TableError::Shape)?;
    if id.is_empty() {
        return Err(TableError::Shape);
    }
    Ok(CompiledRule {
        id,
        preds: preds.ok_or(TableError::Shape)?,
        unless: unless.ok_or(TableError::Shape)?,
    })
}

fn decode_phase(r: &mut Reader<'_>) -> Result<Vec<CompiledRule>, TableError> {
    let n = r.array_len()?;
    if n > MAX_RULES {
        return Err(TableError::Shape);
    }
    (0..n).map(|_| decode_rule(r)).collect()
}

/// The table's hash, for its `policy` audit record (COMP-11 §2 step 4).
pub fn hash(bytes: &[u8]) -> [u8; 32] {
    *blake3::hash(bytes).as_bytes()
}

/// Decode table bytes. Call only after [`verify`] has passed.
pub fn decode(buf: &[u8]) -> Result<Table, TableError> {
    let mut r = Reader::new(buf);
    let len = r.map_begin()?;
    let mut t = Table::default();
    let (mut v, mut version) = (None, None);
    let mut seen = [false; 4];
    for _ in 0..len {
        match r.key()? {
            "v" => v = Some(r.u64()?),
            "deny" => (t.rules.deny, seen[0]) = (decode_phase(&mut r)?, true),
            "allow" => (t.rules.allow, seen[3]) = (decode_phase(&mut r)?, true),
            "defer" => (t.rules.defer, seen[2]) = (decode_phase(&mut r)?, true),
            "prompt" => (t.rules.prompt, seen[1]) = (decode_phase(&mut r)?, true),
            "version" => version = Some(r.u64()?),
            _ => return Err(TableError::Shape),
        }
    }
    r.map_end()?;
    r.finish()?;
    if v != Some(VERSION) || !seen.iter().all(|s| *s) {
        return Err(TableError::Shape);
    }
    t.version = version.ok_or(TableError::Shape)?;
    Ok(t)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::check::{check, Outcome, Phases};

    pub(crate) fn sample() -> Table {
        let rule = |id: &str, preds| CompiledRule {
            id: id.into(),
            preds,
            unless: Vec::new(),
        };
        let mut t = Table {
            version: 7,
            rules: Phases::default(),
        };
        let mut no_secret = rule(
            "no-secret-capture",
            vec![
                Pred::Capability(vec![Glob::new("capture.*")]),
                Pred::TargetClass(vec![Class::Secret]),
            ],
        );
        no_secret.unless.push(Unless {
            capability: "capture.secret".into(),
            unattended: true,
        });
        t.rules.deny.push(no_secret);
        t.rules.prompt.push(rule(
            "irreversible-prompts",
            vec![
                Pred::Capability(vec![Glob::new("seat.action"), Glob::new("click")]),
                Pred::Irreversible(vec![Glob::new("*")]),
                Pred::PrincipalProfile(vec!["operator".into()]),
            ],
        ));
        t.rules.prompt.push(rule(
            "vision",
            vec![
                Pred::NodeSource(vec!["vision".into()]),
                Pred::NodeConfidenceLt(0.9),
                Pred::NodeName(regex("(?i)^(send|pay)$").unwrap()),
                Pred::Title(regex("Inbox").unwrap()),
            ],
        ));
        t.rules.defer.push(rule(
            "untrusted-origin-into-shell",
            vec![
                Pred::TargetAppId(vec![Glob::new("foot")]),
                Pred::ProvenanceContainsTrust(Trust::Untrusted),
                Pred::ProvenanceContainsSource("channel".into()),
                Pred::AppTrust(vec![Trust::Untrusted, Trust::Standard]),
            ],
        ));
        t.rules.prompt.push(rule("blind", vec![Pred::ProvenanceAbsent]));
        t.rules.allow.push(rule(
            "default-allow",
            vec![
                Pred::Capability(vec![Glob::new("*")]),
                Pred::Principal(vec![Glob::new("agent:*")]),
                Pred::Url(vec![Glob::new("https://*")]),
                Pred::AppIrreversibleCapable(false),
            ],
        ));
        t
    }

    #[test]
    fn a_table_round_trips_to_the_same_bytes() {
        let t = sample();
        let b = encode(&t);
        let back = decode(&b).unwrap();
        assert_eq!(
            encode(&back),
            b,
            "re-encoding the decoded table is byte-identical"
        );
        assert_eq!(back.version, 7);
        assert_eq!(back.rules.prompt.len(), 3);
    }

    #[test]
    fn the_decoded_table_decides_like_the_original() {
        let t = sample();
        let back = decode(&encode(&t)).unwrap();
        let mut ctx = crate::check::RequestCtx {
            principal: "agent:a",
            profile: "operator",
            grants: &[],
            capability: "click",
            app_id: None,
            title: None,
            class: Class::Private,
            app_trust: Trust::Standard,
            app_irreversible_capable: false,
            node: None,
            url: None,
            irreversible: Some("communication.send"),
            provenance: crate::check::ProvenanceFacts {
                empty: false,
                trusts: [false; 4],
                sources: &[],
            },
        };
        assert_eq!(check(&t, &ctx), check(&back, &ctx));
        assert_eq!(check(&back, &ctx).outcome, Outcome::Prompt);
        ctx.irreversible = None;
        ctx.url = Some("https://x");
        assert_eq!(check(&t, &ctx), check(&back, &ctx));
    }

    #[test]
    fn a_signature_verifies_only_for_its_key_and_bytes() {
        let key = SigningKey::from_bytes(&[3; 32]);
        let b = encode(&sample());
        let sig = sign(&key, &b);
        assert!(verify(&key.verifying_key(), &b, &sig).is_ok());
        let other = SigningKey::from_bytes(&[4; 32]).verifying_key();
        assert_eq!(verify(&other, &b, &sig), Err(TableError::BadSignature));
        let mut tampered = b.clone();
        let last = tampered.len() - 1;
        tampered[last] ^= 1;
        assert_eq!(
            verify(&key.verifying_key(), &tampered, &sig),
            Err(TableError::BadSignature)
        );
        // A bare Ed25519 signature over the bytes, without the context, is not
        // a table signature.
        let bare = key.sign(&b).to_bytes();
        assert_eq!(
            verify(&key.verifying_key(), &b, &bare),
            Err(TableError::BadSignature)
        );
    }

    #[test]
    fn unknown_predicates_fields_and_missing_phases_are_refused() {
        let mut m = MapBuilder::new();
        m.insert("p", enc(|w| w.text("time")));
        m.insert("a", enc(|w| texts(w, ["9-17"].into_iter())));
        let bytes = m.finish();
        let mut r = Reader::new(&bytes);
        assert!(decode_pred(&mut r).is_err());
        // A table missing its allow phase.
        let mut m = MapBuilder::new();
        m.insert("v", enc(|w| w.u64(VERSION)));
        m.insert("deny", encode_phase(&[]));
        m.insert("defer", encode_phase(&[]));
        m.insert("prompt", encode_phase(&[]));
        m.insert("version", enc(|w| w.u64(1)));
        assert_eq!(decode(&m.finish()).err(), Some(TableError::Shape));
        // A regex past the size limit.
        assert_eq!(regex("a{1000}{1000}").err(), Some(TableError::Regex));
    }
}
