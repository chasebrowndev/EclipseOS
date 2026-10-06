// SPDX-License-Identifier: AGPL-3.0-only
//! The broker: lock state, unlock handshake and the three release paths.
//! Owner review: this is the enforcement path for S-08.
//!
//! # Order of checks
//!
//! Every release path (`substitute`, `field_fill`, `materialize`, `totp`) runs
//! the same sequence, and **no step reads a value from the store until the
//! last one has passed**:
//!
//! 1. the peer is allowed this operation ([`gate`]);
//! 2. the request's own facts are well formed and, for `field_fill`, every
//!    compositor-side precondition holds (role, generation, capability,
//!    owner's app list). These need no store, so they run first and a locked
//!    broker cannot be used to probe which of them failed;
//! 3. the broker is unlocked (`broker_locked`);
//! 4. the name exists (an unknown name is `no_capability`, the same answer
//!    as a name the principal does not hold, so names are not enumerable);
//! 5. the secret's record permits this mode, and `bound_to` matches the
//!    destination (`out_of_scope`);
//! 6. the caller's expected `rotation_counter` is current (`secret_rotated`).
//!
//! Only then is the value decrypted, and the audit record is handed to the
//! sink **before** the value leaves this module: if the audit stream is down
//! the use is denied.
//!
//! Denials after step 4 are audited too (the secret id is known by then);
//! earlier ones are not, because there is no secret to name.

use crate::audit::{AuditSink, SecretRecord};
use crate::gate::{self, Op, Peer};
use crate::hygiene::LockedBuf;
use crate::record::{Meta, Mode, NewSecret};
use crate::sealer::Sealer;
use crate::store::{Store, StoreError};
use crate::totp::{self, IssueLog};
use crate::wire::Secret;
use ec_policy_eval::audit::principal_ok;
use ec_policy_eval::Ulid;
use std::collections::HashMap;
use std::path::PathBuf;

/// How long an unlock prompt stays answerable.
pub const UNLOCK_TTL_S: u64 = 60;
/// Wrong answers before the broker refuses to prompt for a while.
pub const MAX_ATTEMPTS: u32 = 5;
pub const LOCKOUT_S: u64 = 60;

/// Failure statuses, numbered as in `result.status` (COMP-08 §2.1, A-06.1) so
/// the compositor can pass them through. 64 and up are brokerd's own and are
/// mapped before they reach an agent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Status {
    NoCapability = 1,
    OutOfScope = 2,
    RateLimited = 3,
    StaleGeneration = 4,
    InvalidArgument = 15,
    BrokerLocked = 20,
    SecretRotated = 21,
    /// Audit stream down, store unreadable, no lockable memory.
    Internal = 64,
    /// Unlock answer did not open the store.
    BadCredential = 65,
}

impl Status {
    pub fn code(self) -> u64 {
        self as u64
    }

    pub fn from_code(c: u64) -> Option<Status> {
        use Status::*;
        [
            NoCapability,
            OutOfScope,
            RateLimited,
            StaleGeneration,
            InvalidArgument,
            BrokerLocked,
            SecretRotated,
            Internal,
            BadCredential,
        ]
        .into_iter()
        .find(|s| s.code() == c)
    }

    /// The audit `outcome` string (a member of [`crate::audit::OUTCOMES`]).
    pub fn outcome(self) -> &'static str {
        match self {
            Status::NoCapability => "no_capability",
            Status::OutOfScope => "out_of_scope",
            Status::RateLimited => "rate_limited",
            Status::StaleGeneration => "stale_generation",
            Status::InvalidArgument => "invalid_argument",
            Status::BrokerLocked => "broker_locked",
            Status::SecretRotated => "secret_rotated",
            Status::Internal | Status::BadCredential => "failed",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NodeRole {
    Password,
    TextField,
    Other,
}

/// `proxy_header` substitution (S-08 §3.1), asked by the egress proxy for one
/// connection it is about to carry the secret over.
pub struct SubstituteReq {
    pub principal: String,
    pub grant_id: Option<Ulid>,
    pub name: String,
    /// The host the connection is going to, and its port.
    pub host: String,
    pub port: u16,
    /// The request URL, when the proxy has terminated TLS and has one.
    pub url: Option<String>,
    pub holds_secret_use: bool,
    /// The rotation counter the proxy's rule was built against.
    pub expected_rotation: Option<u64>,
}

/// `field_fill` (S-08 §3.2, COMP-08 §4.3). The compositor asserts what it
/// validated; brokerd re-checks everything it can and refuses on any miss.
/// This is defence in depth, not a substitute for the compositor's check.
pub struct FillReq {
    pub principal: String,
    pub grant_id: Option<Ulid>,
    pub name: String,
    pub role: NodeRole,
    /// `ext.credential` on the node.
    pub credential: bool,
    pub app_id: String,
    pub url: Option<String>,
    pub generation: u64,
    pub expected_generation: u64,
    pub holds_secret_use: bool,
    pub holds_seat_text: bool,
    /// The target app is on the owner's `field_fill` list.
    pub app_listed: bool,
    pub expected_rotation: Option<u64>,
}

/// `materialize` (S-08 §3.3): needs `secret.expose`, never `secret.use`.
pub struct MaterializeReq {
    pub principal: String,
    pub grant_id: Option<Ulid>,
    pub name: String,
    /// `env:NAME` or `file:/path`, recorded as the destination.
    pub target: String,
    pub holds_secret_expose: bool,
    pub expected_rotation: Option<u64>,
}

/// A TOTP issuance (S-08 §4).
pub struct TotpReq {
    pub principal: String,
    pub grant_id: Option<Ulid>,
    pub name: String,
    pub host: String,
    pub port: u16,
    pub holds_secret_use: bool,
}

/// A value on its way to its one consumer.
#[derive(Debug)]
pub struct Released {
    pub value: LockedBuf,
    pub rotation_counter: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnlockMethod {
    /// Collect a passphrase in the trusted UI.
    Passphrase,
    /// Presence only: the master key is sealed to the machine.
    Confirm,
}

/// What brokerd asks the compositor to put on the trusted unlock prompt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UnlockRequest {
    pub nonce: u64,
    pub method: UnlockMethod,
    pub attempts_left: u32,
    pub expires_unix: u64,
}

/// The human's answer, relayed by the compositor.
pub enum UnlockAnswer {
    Submit { nonce: u64, passphrase: Option<Secret> },
    Cancel { nonce: u64 },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnlockOutcome {
    Unlocked,
    Cancelled,
}

enum State {
    Locked,
    Unlocked(Store),
}

struct Pending {
    nonce: u64,
    expires: u64,
}

pub struct Broker {
    dir: PathBuf,
    sealer: Box<dyn Sealer>,
    audit: Box<dyn AuditSink>,
    state: State,
    pending: Option<Pending>,
    failures: u32,
    lockout_until: u64,
    totp: HashMap<Ulid, IssueLog>,
}

/// A destination as it may appear in an audit record: no query, no fragment,
/// no userinfo, bounded.
fn audit_dest(s: &str) -> String {
    if s.contains('@') {
        return "<redacted>".into();
    }
    let s = s.split(['?', '#']).next().unwrap_or("");
    s.chars().take(256).collect()
}

fn emit(sink: &mut dyn AuditSink, r: &SecretRecord<'_>) -> Result<(), Status> {
    sink.emit(r.emission()).map_err(|_| Status::Internal)
}

impl Broker {
    /// A locked broker over `dir`. Nothing is read until unlock.
    pub fn new(dir: PathBuf, sealer: Box<dyn Sealer>, audit: Box<dyn AuditSink>) -> Broker {
        Broker {
            dir,
            sealer,
            audit,
            state: State::Locked,
            pending: None,
            failures: 0,
            lockout_until: 0,
            totp: HashMap::new(),
        }
    }

    pub fn is_unlocked(&self) -> bool {
        matches!(self.state, State::Unlocked(_))
    }

    pub fn is_initialised(&self) -> bool {
        Store::exists(&self.dir)
    }

    pub fn sealer_needs_passphrase(&self) -> bool {
        self.sealer.needs_passphrase()
    }

    pub fn sealer_name(&self) -> &'static str {
        self.sealer.name()
    }

    fn authorize(peer: Peer, op: Op) -> Result<(), Status> {
        if gate::allowed(peer, op) {
            Ok(())
        } else {
            Err(Status::NoCapability)
        }
    }

    // ---- lifecycle -------------------------------------------------------

    /// First-run: creates the sealed master key and leaves the store
    /// unlocked (the human who set it up is present).
    pub fn initialize(&mut self, peer: Peer, pass: Option<&[u8]>) -> Result<(), Status> {
        Self::authorize(peer, Op::Init)?;
        if self.is_unlocked() {
            return Err(Status::InvalidArgument);
        }
        if self.sealer.needs_passphrase() && pass.is_none_or(|p| p.is_empty()) {
            return Err(Status::InvalidArgument);
        }
        Store::init(&self.dir, &*self.sealer, pass).map_err(|e| match e {
            StoreError::Exists => Status::InvalidArgument,
            _ => Status::Internal,
        })?;
        let store = Store::open(&self.dir, &*self.sealer, pass).map_err(|_| Status::Internal)?;
        self.state = State::Unlocked(store);
        Ok(())
    }

    /// Step one of unlock: the compositor asks for a prompt to show. The
    /// answer is bound to the returned nonce, is single use, and expires.
    pub fn begin_unlock(&mut self, peer: Peer, now: u64) -> Result<UnlockRequest, Status> {
        Self::authorize(peer, Op::BeginUnlock)?;
        if self.is_unlocked() || !self.is_initialised() {
            return Err(Status::InvalidArgument);
        }
        if now < self.lockout_until {
            return Err(Status::RateLimited);
        }
        let nonce = u64::from_be_bytes(crate::aead::random().map_err(|_| Status::Internal)?);
        let expires = now + UNLOCK_TTL_S;
        // A new request supersedes an unanswered one.
        self.pending = Some(Pending { nonce, expires });
        Ok(UnlockRequest {
            nonce,
            method: if self.sealer.needs_passphrase() {
                UnlockMethod::Passphrase
            } else {
                UnlockMethod::Confirm
            },
            attempts_left: MAX_ATTEMPTS - self.failures,
            expires_unix: expires,
        })
    }

    /// Step two: the human's answer. The pending request is consumed whatever
    /// the outcome, so a wrong passphrase needs a fresh prompt and a fresh
    /// nonce, and a replayed answer finds nothing to answer.
    pub fn answer_unlock(
        &mut self,
        peer: Peer,
        ans: UnlockAnswer,
        now: u64,
    ) -> Result<UnlockOutcome, Status> {
        Self::authorize(peer, Op::AnswerUnlock)?;
        let pending = self.pending.take().ok_or(Status::InvalidArgument)?;
        let (nonce, pass) = match ans {
            UnlockAnswer::Cancel { nonce } => {
                return if nonce == pending.nonce {
                    Ok(UnlockOutcome::Cancelled)
                } else {
                    Err(Status::InvalidArgument)
                };
            }
            UnlockAnswer::Submit { nonce, passphrase } => (nonce, passphrase),
        };
        if nonce != pending.nonce || now >= pending.expires {
            return Err(Status::InvalidArgument);
        }
        if now < self.lockout_until {
            return Err(Status::RateLimited);
        }
        match Store::open(&self.dir, &*self.sealer, pass.as_deref()) {
            Ok(store) => {
                self.failures = 0;
                self.state = State::Unlocked(store);
                Ok(UnlockOutcome::Unlocked)
            }
            Err(StoreError::Seal(_)) => {
                self.failures += 1;
                if self.failures >= MAX_ATTEMPTS {
                    self.failures = 0;
                    self.lockout_until = now + LOCKOUT_S;
                }
                Err(Status::BadCredential)
            }
            Err(_) => Err(Status::Internal),
        }
    }

    /// Screen lock, session end, or the owner asking. Zeroes the master key
    /// and every cached name; in-flight uses see `broker_locked`.
    pub fn lock(&mut self, peer: Peer) -> Result<(), Status> {
        Self::authorize(peer, Op::Lock)?;
        self.state = State::Locked;
        self.pending = None;
        self.totp.clear();
        Ok(())
    }

    // ---- owner actions ---------------------------------------------------

    pub fn list(&self, peer: Peer) -> Result<Vec<Meta>, Status> {
        Self::authorize(peer, Op::List)?;
        match &self.state {
            State::Locked => Err(Status::BrokerLocked),
            State::Unlocked(s) => Ok(s.list().cloned().collect()),
        }
    }

    /// Adds a secret. The audit record follows the write because the id is
    /// minted by the store; if the record cannot be emitted the write is
    /// undone, so no secret exists that the audit log does not know of.
    pub fn add(&mut self, peer: Peer, spec: &NewSecret, value: &[u8], now: u64) -> Result<Meta, Status> {
        Self::authorize(peer, Op::Add)?;
        let State::Unlocked(store) = &mut self.state else {
            return Err(Status::BrokerLocked);
        };
        let meta = store.add(spec, value, now).map_err(|e| match e {
            StoreError::Exists | StoreError::Record(_) | StoreError::TooLarge => Status::InvalidArgument,
            _ => Status::Internal,
        })?;
        let rec = SecretRecord {
            secret_id: meta.id,
            rotation_counter: 0,
            mode: "add",
            destination: "-",
            principal: "human",
            grant_id: None,
            outcome: "added",
            length: None,
        };
        if let Err(s) = emit(&mut *self.audit, &rec) {
            let _ = store.revoke(&spec.name);
            return Err(s);
        }
        Ok(meta)
    }

    /// Rotation (S-08 §6). The record is emitted first: a rotation that
    /// cannot be journalled does not happen.
    pub fn rotate(&mut self, peer: Peer, name: &str, value: &[u8]) -> Result<Meta, Status> {
        Self::authorize(peer, Op::Rotate)?;
        let State::Unlocked(store) = &mut self.state else {
            return Err(Status::BrokerLocked);
        };
        let m = store.meta(name).ok_or(Status::NoCapability)?;
        let rec = SecretRecord {
            secret_id: m.id,
            rotation_counter: m.rotation_counter + 1,
            mode: "rotate",
            destination: "-",
            principal: "human",
            grant_id: None,
            outcome: "rotated",
            length: None,
        };
        emit(&mut *self.audit, &rec)?;
        store.rotate(name, value).map_err(|e| match e {
            StoreError::TooLarge => Status::InvalidArgument,
            _ => Status::Internal,
        })
    }

    /// Revocation (S-08 §6): immediate. Brokerd holds no state across
    /// requests, so an in-flight fill that has not yet read the value finds
    /// the name gone and gets `no_capability`.
    pub fn revoke(&mut self, peer: Peer, name: &str) -> Result<(), Status> {
        Self::authorize(peer, Op::Revoke)?;
        let State::Unlocked(store) = &mut self.state else {
            return Err(Status::BrokerLocked);
        };
        let m = store.meta(name).ok_or(Status::NoCapability)?;
        let id = m.id;
        let rec = SecretRecord {
            secret_id: id,
            rotation_counter: m.rotation_counter,
            mode: "revoke",
            destination: "-",
            principal: "human",
            grant_id: None,
            outcome: "revoked",
            length: None,
        };
        emit(&mut *self.audit, &rec)?;
        store.revoke(name).map_err(|_| Status::Internal)?;
        self.totp.remove(&id);
        Ok(())
    }

    // ---- release paths ---------------------------------------------------

    /// Steps 3 to 6, then the read, then the audit record. `scope` is the
    /// path's own `bound_to` rule and runs with the record's metadata only.
    #[allow(clippy::too_many_arguments)]
    fn release(
        &mut self,
        name: &str,
        need: Option<Mode>,
        mode_label: &str,
        dest: &str,
        principal: &str,
        grant: Option<Ulid>,
        expected_rotation: Option<u64>,
        report_len: bool,
        scope: impl FnOnce(&Meta, &mut HashMap<Ulid, IssueLog>) -> Result<(), Status>,
    ) -> Result<Released, Status> {
        let State::Unlocked(store) = &self.state else {
            return Err(Status::BrokerLocked);
        };
        let Some(meta) = store.meta(name) else {
            return Err(Status::NoCapability);
        };
        let (id, rc) = (meta.id, meta.rotation_counter);
        let dest = audit_dest(dest);
        let mut rec = SecretRecord {
            secret_id: id,
            rotation_counter: rc,
            mode: mode_label,
            destination: &dest,
            principal,
            grant_id: grant,
            outcome: "ok",
            length: None,
        };
        let pre = (|| {
            if need.is_some_and(|m| !meta.has_mode(m)) {
                return Err(Status::OutOfScope);
            }
            scope(meta, &mut self.totp)?;
            if expected_rotation.is_some_and(|e| e != rc) {
                return Err(Status::SecretRotated);
            }
            Ok(())
        })();
        if let Err(s) = pre {
            rec.outcome = s.outcome();
            // A denial that cannot be journalled is still a denial.
            let _ = emit(&mut *self.audit, &rec);
            return Err(s);
        }
        // Every precondition has passed. First read of the value.
        let value = store.read_value(name).map_err(|_| Status::Internal)?;
        if report_len {
            rec.length = Some(value.len() as u64);
        }
        // Journal before release: on failure `value` drops, and is zeroed.
        emit(&mut *self.audit, &rec)?;
        Ok(Released {
            value,
            rotation_counter: rc,
        })
    }

    /// `proxy_header` (S-08 §3.1).
    pub fn substitute(&mut self, peer: Peer, req: &SubstituteReq) -> Result<Released, Status> {
        Self::authorize(peer, Op::Substitute)?;
        if !principal_ok(&req.principal) {
            return Err(Status::InvalidArgument);
        }
        if !req.holds_secret_use {
            return Err(Status::NoCapability);
        }
        let dest = match &req.url {
            Some(u) => u.clone(),
            None => format!("{}:{}", req.host, req.port),
        };
        self.release(
            &req.name,
            Some(Mode::ProxyHeader),
            Mode::ProxyHeader.as_str(),
            &dest,
            &req.principal,
            req.grant_id,
            req.expected_rotation,
            false,
            |meta, _| {
                let ok = meta.bound_to.iter().any(|b| match b {
                    crate::bind::Binding::Host { .. } => b.matches_endpoint(&req.host, req.port),
                    crate::bind::Binding::Url { .. } => req.url.as_deref().is_some_and(|u| b.matches_url(u)),
                    crate::bind::Binding::App(_) => false,
                });
                if ok {
                    Ok(())
                } else {
                    Err(Status::OutOfScope)
                }
            },
        )
    }

    /// `field_fill` (S-08 §3.2, COMP-08 §4.3).
    pub fn field_fill(&mut self, peer: Peer, req: &FillReq) -> Result<Released, Status> {
        Self::authorize(peer, Op::FieldFill)?;
        if !principal_ok(&req.principal) {
            return Err(Status::InvalidArgument);
        }
        // Capability first: a principal without `secret.use` or `seat.text`
        // learns nothing else about why.
        if !req.holds_secret_use || !req.holds_seat_text {
            return Err(Status::NoCapability);
        }
        // Role: `password`, or `textfield` that takes a credential.
        let role_ok = match req.role {
            NodeRole::Password => true,
            NodeRole::TextField => req.credential,
            NodeRole::Other => false,
        };
        if !role_ok {
            return Err(Status::InvalidArgument);
        }
        if req.generation != req.expected_generation {
            return Err(Status::StaleGeneration);
        }
        if !req.app_listed {
            return Err(Status::OutOfScope);
        }
        let dest = match &req.url {
            Some(u) => u.clone(),
            None => format!("app:{}", req.app_id),
        };
        self.release(
            &req.name,
            Some(Mode::FieldFill),
            Mode::FieldFill.as_str(),
            &dest,
            &req.principal,
            req.grant_id,
            req.expected_rotation,
            true,
            |meta, _| {
                // A web binding needs a matching URL; an app binding needs a
                // matching app id; a record with both needs both. A host-only
                // binding with no URL on the target matches nothing.
                let has_web = meta.bound_to.iter().any(|b| b.is_web());
                let has_app = meta.bound_to.iter().any(|b| !b.is_web());
                let web_ok = !has_web
                    || req
                        .url
                        .as_deref()
                        .is_some_and(|u| meta.bound_to.iter().any(|b| b.matches_url(u)));
                let app_ok = !has_app || meta.bound_to.iter().any(|b| b.matches_app(&req.app_id));
                if web_ok && app_ok {
                    Ok(())
                } else {
                    Err(Status::OutOfScope)
                }
            },
        )
    }

    /// `materialize` (S-08 §3.3). The value is released into a sandbox the
    /// agent can read, which is why it needs the separate `secret.expose`.
    /// `bound_to` names network destinations and a sandbox env var has none,
    /// so it is not consulted here; the compiler warning of S-08 §5 and the
    /// egress policy are what bound where the value can go.
    pub fn materialize(&mut self, peer: Peer, req: &MaterializeReq) -> Result<Released, Status> {
        Self::authorize(peer, Op::Materialize)?;
        if !principal_ok(&req.principal) {
            return Err(Status::InvalidArgument);
        }
        if !req.holds_secret_expose {
            return Err(Status::NoCapability);
        }
        if !(req.target.starts_with("env:") || req.target.starts_with("file:")) {
            return Err(Status::InvalidArgument);
        }
        self.release(
            &req.name,
            Some(Mode::Materialize),
            Mode::Materialize.as_str(),
            &req.target,
            &req.principal,
            req.grant_id,
            req.expected_rotation,
            false,
            |_, _| Ok(()),
        )
    }

    /// A TOTP code (S-08 §4): six digits, valid for the rest of the current
    /// 30 s step. The seed is read, used and zeroed here; it is never
    /// released. Whether the issuance should prompt (`identity.credential`)
    /// is policy's call, made before this is asked.
    pub fn totp(&mut self, peer: Peer, req: &TotpReq, now: u64) -> Result<(String, u64), Status> {
        Self::authorize(peer, Op::Totp)?;
        if !principal_ok(&req.principal) {
            return Err(Status::InvalidArgument);
        }
        if !req.holds_secret_use {
            return Err(Status::NoCapability);
        }
        let dest = format!("{}:{}", req.host, req.port);
        let released = self.release(
            &req.name,
            None,
            "totp",
            &dest,
            &req.principal,
            req.grant_id,
            None,
            false,
            |meta, log| {
                if meta.kind != crate::record::Kind::Totp {
                    return Err(Status::InvalidArgument);
                }
                if !meta
                    .bound_to
                    .iter()
                    .any(|b| b.matches_endpoint(&req.host, req.port))
                {
                    return Err(Status::OutOfScope);
                }
                if !log.entry(meta.id).or_default().try_issue(now) {
                    return Err(Status::RateLimited);
                }
                Ok(())
            },
        )?;
        let c = totp::code(released.value.as_slice(), now, totp::DIGITS);
        Ok((c, totp::life_left(now)))
    }
}

#[cfg(test)]
mod tests {
    // Behaviour is covered end to end in tests/secrets_suite.rs, which drives
    // this module through its public API the way the daemon does.
    use super::*;

    #[test]
    fn status_codes_match_the_shared_table() {
        assert_eq!(Status::BrokerLocked.code(), 20);
        assert_eq!(Status::SecretRotated.code(), 21);
        assert_eq!(Status::StaleGeneration.code(), 4);
        for s in [
            Status::NoCapability,
            Status::OutOfScope,
            Status::BrokerLocked,
            Status::Internal,
        ] {
            assert_eq!(Status::from_code(s.code()), Some(s));
            assert!(crate::audit::OUTCOMES.contains(&s.outcome()));
        }
    }

    #[test]
    fn audit_destinations_carry_no_query_or_userinfo() {
        assert_eq!(
            audit_dest("https://a.com/login?token=abc#x"),
            "https://a.com/login"
        );
        assert_eq!(audit_dest("https://user:pw@a.com/"), "<redacted>");
        assert_eq!(audit_dest(&"a".repeat(1000)).len(), 256);
    }
}
