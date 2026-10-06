// SPDX-License-Identifier: AGPL-3.0-only
//! The first message on `policyd.sock`: `policyd` offering abyss its grant
//! verifying key (F-05, COMP-01 §6).
//!
//! The offer carries no authority of its own. abyss trusts it only because
//! of who sent it (the peer checks in `ec_abyss::policy::link`), and only the
//! first time: a different key on a later connection is refused.
//!
//! `{"key": bstr .size 32, "v": 1}`, canonical CBOR (ADR 0044).
//!
//! After the offer, both directions carry **link messages** (v2): canonical
//! CBOR maps whose first key is `"m"`, the message tag. An audit
//! [`Emission`](crate::audit::Emission) never has an `"m"` key, so the two
//! cannot be confused on the abyss→policyd direction. Every decoder is
//! strict: an unknown tag, an unknown or missing field, or trailing bytes is
//! an error, and the receiver drops the link rather than guess.
//!
//! Neither direction can carry authority by shape alone. A [`FromPolicyd::Table`]
//! is believed only once its signature verifies against the pinned key; a
//! [`FromPolicyd::DeferAnswer`] has no variant that grants (COMP-11 §5); a
//! [`FromPolicyd::Minted`] grant is verified like any other.

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

/// Link-message version this build speaks.
pub const MESSAGE_VERSION: u64 = 2;

/// The largest link message either side accepts. A table bigger than this is
/// refused by the compiler, not truncated by the socket.
pub const MAX_MESSAGE: usize = 256 * 1024;

/// One audit record as the emergency panel shows it (COMP-10 §3.3): what
/// kind, which request, and when. Never a body: the panel names actions, it
/// does not replay them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TailRecord {
    pub kind: String,
    pub req_id: Option<u64>,
    pub at_ns: u64,
}

/// policyd → abyss.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FromPolicyd {
    /// A compiled enforcement table (COMP-11 §2) and policyd's signature
    /// over exactly those bytes.
    Table {
        table: Vec<u8>,
        sig: [u8; 64],
    },
    /// The answer to [`ToPolicyd::Defer`] `req`.
    DeferAnswer {
        req: u64,
        answer: crate::check::DeferAnswer,
    },
    /// A grant minted for [`ToPolicyd::Mint`] `req`: COSE_Sign1 bytes.
    Minted {
        req: u64,
        grant: Vec<u8>,
    },
    MintRefused {
        req: u64,
    },
    /// Every grant `principal` holds is revoked.
    Revoked {
        principal: String,
    },
    /// The answer to [`ToPolicyd::AuditTail`] `req`, newest first.
    AuditRecords {
        req: u64,
        records: Vec<TailRecord>,
    },
    /// [`ToPolicyd::OpenTask`] `req` opened a task; `grant` is its first
    /// grant, COSE_Sign1, naming the new task.
    TaskOpened {
        req: u64,
        grant: Vec<u8>,
    },
    /// [`ToPolicyd::OpenTask`] `req` was refused, and the A-04 or S-01 §4
    /// rule that said no.
    TaskRefused {
        req: u64,
        reason: String,
    },
}

/// abyss → policyd, besides audit emissions.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ToPolicyd {
    /// A `defer` outcome (COMP-11 §5): the request's facts, by name only.
    Defer {
        req: u64,
        principal: String,
        capability: String,
        rule: String,
    },
    /// The human answered "for this task" or "unattended": mint exactly
    /// `scope` (COMP-10 §3.2, the scope that was on screen).
    Mint {
        req: u64,
        principal: String,
        scope: String,
        unattended: bool,
    },
    /// The human revoked every grant `principal` holds (COMP-10 §3.3).
    Revoke { principal: String },
    /// The human terminated `principal`, or used the terminate chord.
    Terminate { principal: String },
    /// The last `n` audit records naming `principal`.
    AuditTail { req: u64, principal: String, n: u64 },
    /// The human opened a task for `principal` (A-04 §3, origin `human`):
    /// `statement` until `deadline_ms`, with a first grant of `scope`, one
    /// capability per line, each `<capability> <scope> <scope>...`.
    OpenTask {
        req: u64,
        principal: String,
        statement: String,
        deadline_ms: u64,
        scope: String,
    },
    /// The human closed `principal`'s live task (A-04 §4): its grants are
    /// revoked and every peer is told with [`FromPolicyd::Revoked`].
    CloseTask { principal: String },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MessageError {
    Cbor(cbor::Error),
    /// Not a link message, or a tag, field or version this build does not know.
    Shape,
}

impl From<cbor::Error> for MessageError {
    fn from(e: cbor::Error) -> Self {
        MessageError::Cbor(e)
    }
}

/// Fields any link message may carry. Each tag allows a fixed subset.
#[derive(Default)]
struct Fields<'a> {
    m: Option<&'a str>,
    v: Option<u64>,
    n: Option<u64>,
    req: Option<u64>,
    sig: Option<&'a [u8]>,
    rule: Option<&'a str>,
    grant: Option<&'a [u8]>,
    scope: Option<&'a str>,
    table: Option<&'a [u8]>,
    answer: Option<u64>,
    records: Option<Vec<TailRecord>>,
    principal: Option<&'a str>,
    capability: Option<&'a str>,
    unattended: Option<bool>,
    reason: Option<&'a str>,
    deadline: Option<u64>,
    statement: Option<&'a str>,
}

impl<'a> Fields<'a> {
    fn names(&self) -> u32 {
        // One bit per field present, so each tag can say exactly which it
        // takes and anything else is refused.
        let mut b = 0;
        for (i, present) in [
            self.n.is_some(),
            self.req.is_some(),
            self.sig.is_some(),
            self.rule.is_some(),
            self.grant.is_some(),
            self.scope.is_some(),
            self.table.is_some(),
            self.answer.is_some(),
            self.records.is_some(),
            self.principal.is_some(),
            self.capability.is_some(),
            self.unattended.is_some(),
            self.reason.is_some(),
            self.deadline.is_some(),
            self.statement.is_some(),
        ]
        .into_iter()
        .enumerate()
        {
            if present {
                b |= 1 << i;
            }
        }
        b
    }
}

const F_N: u32 = 1;
const F_REQ: u32 = 1 << 1;
const F_SIG: u32 = 1 << 2;
const F_RULE: u32 = 1 << 3;
const F_GRANT: u32 = 1 << 4;
const F_SCOPE: u32 = 1 << 5;
const F_TABLE: u32 = 1 << 6;
const F_ANSWER: u32 = 1 << 7;
const F_RECORDS: u32 = 1 << 8;
const F_PRINCIPAL: u32 = 1 << 9;
const F_CAPABILITY: u32 = 1 << 10;
const F_UNATTENDED: u32 = 1 << 11;
const F_REASON: u32 = 1 << 12;
const F_DEADLINE: u32 = 1 << 13;
const F_STATEMENT: u32 = 1 << 14;

fn read_records(r: &mut Reader<'_>) -> Result<Vec<TailRecord>, MessageError> {
    let n = r.array_len()?;
    // 20 is what the panel asks for; anything far past it is not an answer.
    if n > 256 {
        return Err(MessageError::Shape);
    }
    let mut out = Vec::with_capacity(n as usize);
    for _ in 0..n {
        let len = r.map_begin()?;
        let (mut kind, mut req_id, mut at_ns) = (None, None, None);
        for _ in 0..len {
            match r.key()? {
                "kind" => kind = Some(r.text()?.to_owned()),
                "at_ns" => at_ns = Some(r.u64()?),
                "req_id" => req_id = Some(r.u64()?),
                _ => return Err(MessageError::Shape),
            }
        }
        r.map_end()?;
        out.push(TailRecord {
            kind: kind.ok_or(MessageError::Shape)?,
            req_id,
            at_ns: at_ns.ok_or(MessageError::Shape)?,
        });
    }
    Ok(out)
}

fn read_fields(buf: &[u8]) -> Result<Fields<'_>, MessageError> {
    let mut r = Reader::new(buf);
    let len = r.map_begin()?;
    let mut f = Fields::default();
    for i in 0..len {
        let key = r.key()?;
        // The tag sorts first in canonical order; a map that does not open
        // with it is not a link message (it may be an audit emission).
        if i == 0 && key != "m" {
            return Err(MessageError::Shape);
        }
        match key {
            "m" => f.m = Some(r.text()?),
            "n" => f.n = Some(r.u64()?),
            "v" => f.v = Some(r.u64()?),
            "req" => f.req = Some(r.u64()?),
            "sig" => f.sig = Some(r.bytes()?),
            "rule" => f.rule = Some(r.text()?),
            "grant" => f.grant = Some(r.bytes()?),
            "scope" => f.scope = Some(r.text()?),
            "table" => f.table = Some(r.bytes()?),
            "answer" => f.answer = Some(r.u64()?),
            "records" => f.records = Some(read_records(&mut r)?),
            "principal" => f.principal = Some(r.text()?),
            "capability" => f.capability = Some(r.text()?),
            "unattended" => f.unattended = Some(r.bool()?),
            "reason" => f.reason = Some(r.text()?),
            "deadline" => f.deadline = Some(r.u64()?),
            "statement" => f.statement = Some(r.text()?),
            _ => return Err(MessageError::Shape),
        }
    }
    r.map_end()?;
    r.finish()?;
    if f.v != Some(MESSAGE_VERSION) {
        return Err(MessageError::Shape);
    }
    Ok(f)
}

/// The tag, and exactly these fields, or refused.
fn shape(f: &Fields<'_>, want: u32) -> Result<(), MessageError> {
    if f.names() == want {
        Ok(())
    } else {
        Err(MessageError::Shape)
    }
}

fn principal(p: Option<&str>) -> Result<String, MessageError> {
    let p = p.ok_or(MessageError::Shape)?;
    if crate::audit::principal_ok(p) {
        Ok(p.to_owned())
    } else {
        Err(MessageError::Shape)
    }
}

fn message(tag: &str, f: impl FnOnce(&mut MapBuilder)) -> Vec<u8> {
    let mut m = MapBuilder::new();
    m.insert("m", enc(|w| w.text(tag)));
    m.insert("v", enc(|w| w.u64(MESSAGE_VERSION)));
    f(&mut m);
    m.finish()
}

fn text(s: &str) -> Vec<u8> {
    enc(|w| w.text(s))
}

fn uint(n: u64) -> Vec<u8> {
    enc(|w| w.u64(n))
}

impl FromPolicyd {
    pub fn encode(&self) -> Vec<u8> {
        use crate::check::DeferAnswer as A;
        match self {
            FromPolicyd::Table { table, sig } => message("table", |m| {
                m.insert("sig", enc(|w| w.bytes(sig)));
                m.insert("table", enc(|w| w.bytes(table)));
            }),
            FromPolicyd::DeferAnswer { req, answer } => message("defer_answer", |m| {
                m.insert("req", uint(*req));
                let a = match answer {
                    A::Deny => 0,
                    A::Prompt => 1,
                    A::Fallthrough => 2,
                };
                m.insert("answer", uint(a));
            }),
            FromPolicyd::Minted { req, grant } => message("minted", |m| {
                m.insert("req", uint(*req));
                m.insert("grant", enc(|w| w.bytes(grant)));
            }),
            FromPolicyd::MintRefused { req } => message("mint_refused", |m| m.insert("req", uint(*req))),
            FromPolicyd::Revoked { principal } => {
                message("revoked", |m| m.insert("principal", text(principal)))
            }
            FromPolicyd::AuditRecords { req, records } => message("audit_records", |m| {
                m.insert("req", uint(*req));
                m.insert(
                    "records",
                    enc(|w| {
                        w.array(records.len());
                        for r in records {
                            let mut e = MapBuilder::new();
                            e.insert("kind", text(&r.kind));
                            e.insert("at_ns", uint(r.at_ns));
                            e.insert_opt("req_id", r.req_id.map(uint));
                            w.raw(&e.finish());
                        }
                    }),
                );
            }),
            FromPolicyd::TaskOpened { req, grant } => message("task_opened", |m| {
                m.insert("req", uint(*req));
                m.insert("grant", enc(|w| w.bytes(grant)));
            }),
            FromPolicyd::TaskRefused { req, reason } => message("task_refused", |m| {
                m.insert("req", uint(*req));
                m.insert("reason", text(reason));
            }),
        }
    }

    pub fn decode(buf: &[u8]) -> Result<FromPolicyd, MessageError> {
        use crate::check::DeferAnswer as A;
        let mut f = read_fields(buf)?;
        let req = f.req;
        Ok(match f.m.ok_or(MessageError::Shape)? {
            "table" => {
                shape(&f, F_SIG | F_TABLE)?;
                let sig: [u8; 64] = f.sig.and_then(|s| s.try_into().ok()).ok_or(MessageError::Shape)?;
                FromPolicyd::Table {
                    table: f.table.ok_or(MessageError::Shape)?.to_vec(),
                    sig,
                }
            }
            "defer_answer" => {
                shape(&f, F_REQ | F_ANSWER)?;
                let answer = match f.answer {
                    Some(0) => A::Deny,
                    Some(1) => A::Prompt,
                    Some(2) => A::Fallthrough,
                    _ => return Err(MessageError::Shape),
                };
                FromPolicyd::DeferAnswer {
                    req: req.ok_or(MessageError::Shape)?,
                    answer,
                }
            }
            "minted" => {
                shape(&f, F_REQ | F_GRANT)?;
                FromPolicyd::Minted {
                    req: req.ok_or(MessageError::Shape)?,
                    grant: f.grant.ok_or(MessageError::Shape)?.to_vec(),
                }
            }
            "mint_refused" => {
                shape(&f, F_REQ)?;
                FromPolicyd::MintRefused {
                    req: req.ok_or(MessageError::Shape)?,
                }
            }
            "revoked" => {
                shape(&f, F_PRINCIPAL)?;
                FromPolicyd::Revoked {
                    principal: principal(f.principal)?,
                }
            }
            "audit_records" => {
                shape(&f, F_REQ | F_RECORDS)?;
                FromPolicyd::AuditRecords {
                    req: req.ok_or(MessageError::Shape)?,
                    records: f.records.take().ok_or(MessageError::Shape)?,
                }
            }
            "task_opened" => {
                shape(&f, F_REQ | F_GRANT)?;
                FromPolicyd::TaskOpened {
                    req: req.ok_or(MessageError::Shape)?,
                    grant: f.grant.ok_or(MessageError::Shape)?.to_vec(),
                }
            }
            "task_refused" => {
                shape(&f, F_REQ | F_REASON)?;
                FromPolicyd::TaskRefused {
                    req: req.ok_or(MessageError::Shape)?,
                    reason: f.reason.ok_or(MessageError::Shape)?.to_owned(),
                }
            }
            _ => return Err(MessageError::Shape),
        })
    }
}

impl ToPolicyd {
    pub fn encode(&self) -> Vec<u8> {
        match self {
            ToPolicyd::Defer {
                req,
                principal,
                capability,
                rule,
            } => message("defer", |m| {
                m.insert("req", uint(*req));
                m.insert("rule", text(rule));
                m.insert("principal", text(principal));
                m.insert("capability", text(capability));
            }),
            ToPolicyd::Mint {
                req,
                principal,
                scope,
                unattended,
            } => message("mint", |m| {
                m.insert("req", uint(*req));
                m.insert("scope", text(scope));
                m.insert("principal", text(principal));
                m.insert("unattended", enc(|w| w.bool(*unattended)));
            }),
            ToPolicyd::Revoke { principal } => message("revoke", |m| m.insert("principal", text(principal))),
            ToPolicyd::Terminate { principal } => {
                message("terminate", |m| m.insert("principal", text(principal)))
            }
            ToPolicyd::AuditTail { req, principal, n } => message("audit_tail", |m| {
                m.insert("n", uint(*n));
                m.insert("req", uint(*req));
                m.insert("principal", text(principal));
            }),
            ToPolicyd::OpenTask {
                req,
                principal,
                statement,
                deadline_ms,
                scope,
            } => message("open_task", |m| {
                m.insert("req", uint(*req));
                m.insert("scope", text(scope));
                m.insert("deadline", uint(*deadline_ms));
                m.insert("principal", text(principal));
                m.insert("statement", text(statement));
            }),
            ToPolicyd::CloseTask { principal } => {
                message("close_task", |m| m.insert("principal", text(principal)))
            }
        }
    }

    pub fn decode(buf: &[u8]) -> Result<ToPolicyd, MessageError> {
        let f = read_fields(buf)?;
        let req = || f.req.ok_or(MessageError::Shape);
        let txt = |t: Option<&str>| t.map(str::to_owned).ok_or(MessageError::Shape);
        Ok(match f.m.ok_or(MessageError::Shape)? {
            "defer" => {
                shape(&f, F_REQ | F_RULE | F_PRINCIPAL | F_CAPABILITY)?;
                ToPolicyd::Defer {
                    req: req()?,
                    principal: principal(f.principal)?,
                    capability: txt(f.capability)?,
                    rule: txt(f.rule)?,
                }
            }
            "mint" => {
                shape(&f, F_REQ | F_SCOPE | F_PRINCIPAL | F_UNATTENDED)?;
                ToPolicyd::Mint {
                    req: req()?,
                    principal: principal(f.principal)?,
                    scope: txt(f.scope)?,
                    unattended: f.unattended.ok_or(MessageError::Shape)?,
                }
            }
            "revoke" => {
                shape(&f, F_PRINCIPAL)?;
                ToPolicyd::Revoke {
                    principal: principal(f.principal)?,
                }
            }
            "terminate" => {
                shape(&f, F_PRINCIPAL)?;
                ToPolicyd::Terminate {
                    principal: principal(f.principal)?,
                }
            }
            "audit_tail" => {
                shape(&f, F_N | F_REQ | F_PRINCIPAL)?;
                ToPolicyd::AuditTail {
                    req: req()?,
                    principal: principal(f.principal)?,
                    n: f.n.ok_or(MessageError::Shape)?.min(256),
                }
            }
            "open_task" => {
                shape(&f, F_REQ | F_SCOPE | F_DEADLINE | F_PRINCIPAL | F_STATEMENT)?;
                ToPolicyd::OpenTask {
                    req: req()?,
                    principal: principal(f.principal)?,
                    statement: txt(f.statement)?,
                    deadline_ms: f.deadline.ok_or(MessageError::Shape)?,
                    scope: txt(f.scope)?,
                }
            }
            "close_task" => {
                shape(&f, F_PRINCIPAL)?;
                ToPolicyd::CloseTask {
                    principal: principal(f.principal)?,
                }
            }
            _ => return Err(MessageError::Shape),
        })
    }
}

/// Whether `buf` is a link message rather than an audit emission: a map
/// whose first key is `"m"`. A cheap peek; the full decode still decides.
pub fn is_message(buf: &[u8]) -> bool {
    let mut r = Reader::new(buf);
    r.map_begin().is_ok() && r.key().is_ok_and(|k| k == "m")
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

    #[test]
    fn every_message_round_trips() {
        use crate::check::DeferAnswer;
        let from = [
            FromPolicyd::Table {
                table: vec![1, 2, 3],
                sig: [9; 64],
            },
            FromPolicyd::DeferAnswer {
                req: 4,
                answer: DeferAnswer::Fallthrough,
            },
            FromPolicyd::Minted {
                req: 5,
                grant: vec![0xd2],
            },
            FromPolicyd::MintRefused { req: 6 },
            FromPolicyd::Revoked {
                principal: "agent:a".into(),
            },
            FromPolicyd::AuditRecords {
                req: 7,
                records: vec![
                    TailRecord {
                        kind: "request".into(),
                        req_id: Some(3),
                        at_ns: 10,
                    },
                    TailRecord {
                        kind: "focus".into(),
                        req_id: None,
                        at_ns: 11,
                    },
                ],
            },
            FromPolicyd::TaskOpened {
                req: 8,
                grant: vec![0xd2],
            },
            FromPolicyd::TaskRefused {
                req: 9,
                reason: "already_live".into(),
            },
        ];
        for m in from {
            let b = m.encode();
            assert!(is_message(&b));
            assert_eq!(FromPolicyd::decode(&b), Ok(m));
        }
        let to = [
            ToPolicyd::Defer {
                req: 1,
                principal: "agent:a".into(),
                capability: "seat.text".into(),
                rule: "untrusted-origin-into-shell".into(),
            },
            ToPolicyd::Mint {
                req: 2,
                principal: "agent:a".into(),
                scope: "seat.action app=foot".into(),
                unattended: true,
            },
            ToPolicyd::Revoke {
                principal: "agent:a".into(),
            },
            ToPolicyd::Terminate {
                principal: "agent:a".into(),
            },
            ToPolicyd::AuditTail {
                req: 3,
                principal: "agent:a".into(),
                n: 20,
            },
            ToPolicyd::OpenTask {
                req: 4,
                principal: "agent:a".into(),
                statement: "tidy notes".into(),
                deadline_ms: 7_200_000,
                scope: "scene.list workspace:human\nseat.pointer app_id:foot".into(),
            },
            ToPolicyd::CloseTask {
                principal: "agent:a".into(),
            },
        ];
        for m in to {
            let b = m.encode();
            assert!(is_message(&b));
            assert_eq!(ToPolicyd::decode(&b), Ok(m));
        }
    }

    #[test]
    fn a_message_with_a_foreign_field_tag_or_version_is_refused() {
        // A field another tag owns.
        let b = message("revoke", |m| {
            m.insert("principal", text("agent:a"));
            m.insert("req", uint(1));
        });
        assert_eq!(ToPolicyd::decode(&b), Err(MessageError::Shape));
        // An unknown tag.
        let b = message("grant_everything", |m| m.insert("principal", text("agent:a")));
        assert_eq!(FromPolicyd::decode(&b), Err(MessageError::Shape));
        // A defer answer that is not one of the three: there is no allow.
        let b = message("defer_answer", |m| {
            m.insert("req", uint(1));
            m.insert("answer", uint(3));
        });
        assert_eq!(FromPolicyd::decode(&b), Err(MessageError::Shape));
        // Wrong version.
        let mut m = MapBuilder::new();
        m.insert("m", text("revoke"));
        m.insert("v", uint(1));
        m.insert("principal", text("agent:a"));
        assert_eq!(ToPolicyd::decode(&m.finish()), Err(MessageError::Shape));
        // A bad principal.
        let b = message("terminate", |m| m.insert("principal", text("root")));
        assert_eq!(ToPolicyd::decode(&b), Err(MessageError::Shape));
        // Trailing bytes.
        let mut b = ToPolicyd::Terminate {
            principal: "agent:a".into(),
        }
        .encode();
        b.push(0);
        assert!(ToPolicyd::decode(&b).is_err());
    }

    #[test]
    fn an_audit_emission_is_not_a_link_message() {
        let e = crate::audit::Emission {
            kind: crate::audit::Kind::Request,
            principal: "agent:a".into(),
            grant_id: None,
            task_id: None,
            chain_id: None,
            req_id: Some(1),
            serial: None,
            body: enc(|w| w.null()),
        };
        assert!(!is_message(&e.encode()));
        assert!(ToPolicyd::decode(&e.encode()).is_err());
        assert!(!is_message(&encode_key_offer(
            &ed25519_dalek::SigningKey::from_bytes(&[7; 32]).verifying_key()
        )));
    }
}
