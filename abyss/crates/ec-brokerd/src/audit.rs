// SPDX-License-Identifier: AGPL-3.0-only
//! The `secret` audit kind (S-08 §7, S-04 §1.1 as amended by A-05).
//!
//! ```text
//! secret { secret_id, rotation_counter, mode, destination, principal,
//!          grant_id, outcome, length? }
//! ```
//!
//! [`SecretRecord`] is the only way brokerd builds one, and it has no field
//! that can carry a value, a hash of a value, or any bytes derived from one.
//! `length` is present only for `field_fill`, where S-08 §7 wants it for
//! incident triage. That is a structural guarantee, and the secrets suite
//! also scans every emitted byte for the value and its common digests.

use ec_policy_eval::audit::{Emission, Kind};
use ec_policy_eval::cbor::{enc, MapBuilder};
use ec_policy_eval::Ulid;
use std::sync::{Arc, Mutex};

/// Where emissions go. The daemon's sink is policyd's audit stream; a test's
/// is memory. A sink that cannot take a record makes the broker deny the
/// use that caused it: a secret is never released unjournalled (S-04 §4).
pub trait AuditSink {
    fn emit(&mut self, e: Emission) -> Result<(), SinkError>;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SinkError;

/// `outcome` strings. A closed set, so a record cannot carry free text.
pub const OUTCOMES: [&str; 12] = [
    "ok",
    "no_capability",
    "out_of_scope",
    "rate_limited",
    "stale_generation",
    "invalid_argument",
    "broker_locked",
    "secret_rotated",
    "added",
    "rotated",
    "revoked",
    "failed",
];

pub struct SecretRecord<'a> {
    pub secret_id: Ulid,
    pub rotation_counter: u64,
    /// `proxy_header`, `field_fill`, `materialize`, `totp`, or an owner
    /// action: `add`, `rotate`, `revoke`.
    pub mode: &'a str,
    /// A host:port, a URL, an app id or a sandbox target. Never a value.
    pub destination: &'a str,
    pub principal: &'a str,
    pub grant_id: Option<Ulid>,
    pub outcome: &'a str,
    pub length: Option<u64>,
}

impl SecretRecord<'_> {
    pub fn body(&self) -> Vec<u8> {
        debug_assert!(OUTCOMES.contains(&self.outcome), "outcome outside the closed set");
        let mut m = MapBuilder::new();
        m.insert("destination", enc(|w| w.text(self.destination)));
        m.insert_opt("grant_id", self.grant_id.map(|g| enc(|w| w.bytes(&g.0))));
        m.insert_opt("length", self.length.map(|l| enc(|w| w.u64(l))));
        m.insert("mode", enc(|w| w.text(self.mode)));
        m.insert("outcome", enc(|w| w.text(self.outcome)));
        m.insert("principal", enc(|w| w.text(self.principal)));
        m.insert("rotation_counter", enc(|w| w.u64(self.rotation_counter)));
        m.insert("secret_id", enc(|w| w.bytes(&self.secret_id.0)));
        m.finish()
    }

    pub fn emission(&self) -> Emission {
        Emission {
            kind: Kind::Secret,
            principal: self.principal.to_owned(),
            grant_id: self.grant_id,
            task_id: None,
            chain_id: None,
            req_id: None,
            serial: None,
            body: self.body(),
        }
    }
}

/// A sink that keeps what it is given. Clones share the buffer, so a test
/// keeps one handle and gives the broker the other.
#[derive(Clone, Default)]
pub struct MemorySink {
    pub records: Arc<Mutex<Vec<Emission>>>,
    /// When set, every `emit` fails: the audit stream is down.
    pub down: Arc<Mutex<bool>>,
}

impl MemorySink {
    pub fn encoded(&self) -> Vec<Vec<u8>> {
        self.records
            .lock()
            .unwrap()
            .iter()
            .map(Emission::encode)
            .collect()
    }
}

impl AuditSink for MemorySink {
    fn emit(&mut self, e: Emission) -> Result<(), SinkError> {
        if *self.down.lock().unwrap() {
            return Err(SinkError);
        }
        self.records.lock().unwrap().push(e);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ec_policy_eval::cbor::Reader;

    #[test]
    fn a_record_decodes_with_exactly_the_spec_fields() {
        let r = SecretRecord {
            secret_id: Ulid([1; 16]),
            rotation_counter: 3,
            mode: "field_fill",
            destination: "app:org.mozilla.firefox",
            principal: "agent:a1",
            grant_id: Some(Ulid([2; 16])),
            outcome: "ok",
            length: Some(12),
        };
        let e = r.emission();
        assert_eq!(e.kind, Kind::Secret);
        assert_eq!(Emission::decode(&e.encode()), Ok(e.clone()));
        let mut rd = Reader::new(&e.body);
        let n = rd.map_begin().unwrap();
        let keys: Vec<&str> = (0..n)
            .map(|_| {
                let k = rd.key().unwrap();
                rd.skip().unwrap();
                k
            })
            .collect();
        assert_eq!(
            keys,
            [
                "mode",
                "length",
                "outcome",
                "grant_id",
                "principal",
                "secret_id",
                "destination",
                "rotation_counter"
            ]
        );
    }

    #[test]
    fn length_and_grant_are_absent_when_not_given() {
        let r = SecretRecord {
            secret_id: Ulid([1; 16]),
            rotation_counter: 0,
            mode: "proxy_header",
            destination: "api.acme.com:443",
            principal: "agent:a1",
            grant_id: None,
            outcome: "ok",
            length: None,
        };
        let b = r.body();
        assert!(!b.windows(6).any(|w| w == b"length"));
        assert!(!b.windows(8).any(|w| w == b"grant_id"));
    }
}
