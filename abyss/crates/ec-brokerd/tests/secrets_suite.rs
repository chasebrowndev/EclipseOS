// SPDX-License-Identifier: AGPL-3.0-only
//! The secrets suite: the exit gate of COMP-16 milestone 19 (S-08 §8).
//!
//! * `secret.use` never materialises a value in the sandbox.
//! * Every `field_fill` precondition fails closed, before any value is read.
//! * No secret value, or hash of one, appears in any audit record.
//! * Core-dump and `/proc/<pid>/mem` attempts recover nothing.
//!
//! Plus the unlock handshake, rotation, revocation and TOTP.

use ec_brokerd::audit::MemorySink;
use ec_brokerd::bind::Binding;
use ec_brokerd::broker::*;
use ec_brokerd::gate::Peer;
use ec_brokerd::record::{Kind, Mode, NewSecret};
use ec_brokerd::sealer::{KdfParams, PassphraseSealer, SoftSealer};
use ec_brokerd::wire::{
    FillFields, FillWire, MaterializeFields, MaterializeWire, Request, Response, Secret, SubstituteFields,
    SubstituteWire, TotpFields, TotpWire,
};
use ec_policy_eval::audit::{Emission, Kind as AuditKind};
use ec_policy_eval::cbor::Reader;
use std::fs;
use std::path::{Path, PathBuf};

const API_VALUE: &[u8] = b"sk_live_DISTINCT_PROXY_VALUE_1";
const LOGIN_VALUE: &[u8] = b"hunter2-DISTINCT-FILL-VALUE";
const TOKEN_VALUE: &[u8] = b"crates-io-DISTINCT-TOKEN-VALUE";
const TOTP_SEED: &[u8] = b"12345678901234567890";
const NOW: u64 = 1_800_000_000;

fn tmpdir(tag: &str) -> PathBuf {
    let mut b = [0u8; 8];
    getrandom::fill(&mut b).unwrap();
    let d = std::env::temp_dir().join(format!("ec-brokerd-suite-{tag}-{:x}", u64::from_be_bytes(b)));
    let _ = fs::remove_dir_all(&d);
    d
}

fn bindings(v: &[&str]) -> Vec<Binding> {
    v.iter().map(|s| Binding::parse(s).unwrap()).collect()
}

struct Rig {
    b: Broker,
    sink: MemorySink,
    dir: PathBuf,
}

impl Drop for Rig {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.dir);
    }
}

fn spec(name: &str, kind: Kind, bound: &[&str], modes: &[Mode]) -> NewSecret {
    NewSecret {
        name: name.into(),
        kind,
        bound_to: bindings(bound),
        modes: modes.to_vec(),
        requires_prompt: false,
        rotation_hint_days: None,
    }
}

fn rig() -> Rig {
    let dir = tmpdir("rig");
    let sink = MemorySink::default();
    let mut b = Broker::new(
        dir.clone(),
        Box::new(SoftSealer::new([7; 32], b"policy")),
        Box::new(sink.clone()),
    );
    b.initialize(Peer::Owner, None).unwrap();
    let o = Peer::Owner;
    b.add(
        o,
        &spec(
            "acme-api",
            Kind::Bearer,
            &["host:api.acme-invoices.com"],
            &[Mode::ProxyHeader],
        ),
        API_VALUE,
        NOW,
    )
    .unwrap();
    b.add(
        o,
        &spec(
            "acme-login",
            Kind::Password,
            &["url:https://login.acme.com/*", "app:org.mozilla.firefox"],
            &[Mode::FieldFill],
        ),
        LOGIN_VALUE,
        NOW,
    )
    .unwrap();
    b.add(
        o,
        &spec(
            "builder-token",
            Kind::Env,
            &["host:crates.io"],
            &[Mode::Materialize],
        ),
        TOKEN_VALUE,
        NOW,
    )
    .unwrap();
    b.add(
        o,
        &spec("acme-totp", Kind::Totp, &["host:login.acme.com"], &[]),
        TOTP_SEED,
        NOW,
    )
    .unwrap();
    Rig { b, sink, dir }
}

fn sub(name: &str, host: &str, port: u16) -> SubstituteReq {
    SubstituteReq {
        principal: "agent:a1".into(),
        grant_id: None,
        name: name.into(),
        host: host.into(),
        port,
        url: None,
        holds_secret_use: true,
        expected_rotation: None,
    }
}

fn fill(f: impl FnOnce(&mut FillReq)) -> FillReq {
    let mut r = FillReq {
        principal: "agent:a1".into(),
        grant_id: None,
        name: "acme-login".into(),
        role: NodeRole::Password,
        credential: false,
        app_id: "org.mozilla.firefox".into(),
        url: Some("https://login.acme.com/signin".into()),
        generation: 5,
        expected_generation: 5,
        holds_secret_use: true,
        holds_seat_text: true,
        app_listed: true,
        expected_rotation: None,
    };
    f(&mut r);
    r
}

fn mat(name: &str, expose: bool) -> MaterializeReq {
    MaterializeReq {
        principal: "agent:b1".into(),
        grant_id: None,
        name: name.into(),
        target: "env:CARGO_REGISTRY_TOKEN".into(),
        holds_secret_expose: expose,
        expected_rotation: None,
    }
}

fn totp_req(host: &str) -> TotpReq {
    TotpReq {
        principal: "agent:a1".into(),
        grant_id: None,
        name: "acme-totp".into(),
        host: host.into(),
        port: 443,
        holds_secret_use: true,
    }
}

/// Flips the last byte of every record file: the value blob is last, so a
/// metadata read still works and any attempt to read a value fails.
fn corrupt_values(dir: &Path) {
    for e in fs::read_dir(dir).unwrap() {
        let p = e.unwrap().path();
        if p.extension().is_some_and(|x| x == "sec") {
            let mut b = fs::read(&p).unwrap();
            let n = b.len();
            b[n - 1] ^= 0xff;
            fs::write(&p, b).unwrap();
        }
    }
}

// ---- secret.use never materialises a value in the sandbox ----------------

#[test]
fn only_the_proxy_can_substitute_and_only_the_launcher_can_materialize() {
    let mut r = rig();
    for peer in [Peer::Compositor, Peer::Agentd, Peer::Owner] {
        assert_eq!(
            r.b.substitute(peer, &sub("acme-api", "api.acme-invoices.com", 443))
                .unwrap_err(),
            Status::NoCapability,
            "{peer:?}"
        );
    }
    for peer in [Peer::Proxy, Peer::Compositor, Peer::Owner] {
        assert_eq!(
            r.b.materialize(peer, &mat("builder-token", true)).unwrap_err(),
            Status::NoCapability
        );
    }
    // The sandbox-side peer cannot reach either use-only path.
    assert_eq!(
        r.b.field_fill(Peer::Agentd, &fill(|_| {})).unwrap_err(),
        Status::NoCapability
    );
}

#[test]
fn secret_use_alone_never_materializes() {
    let mut r = rig();
    // Holding `secret.use` (what the proxy asserts) is not `secret.expose`.
    assert_eq!(
        r.b.materialize(Peer::Agentd, &mat("builder-token", false))
            .unwrap_err(),
        Status::NoCapability
    );
    // A proxy_header-only or field_fill-only secret cannot be materialized
    // even with `secret.expose`: the mode is the record's, not the caller's.
    assert_eq!(
        r.b.materialize(Peer::Agentd, &mat("acme-api", true)).unwrap_err(),
        Status::OutOfScope
    );
    assert_eq!(
        r.b.materialize(Peer::Agentd, &mat("acme-login", true))
            .unwrap_err(),
        Status::OutOfScope
    );
    // And a materialize-only secret cannot be substituted or filled.
    assert_eq!(
        r.b.substitute(Peer::Proxy, &sub("builder-token", "crates.io", 443))
            .unwrap_err(),
        Status::OutOfScope
    );
    // With the separate capability it works, and says where it went.
    let v =
        r.b.materialize(Peer::Agentd, &mat("builder-token", true))
            .unwrap();
    assert_eq!(v.value.as_slice(), TOKEN_VALUE);
    let recs = r.sink.records.lock().unwrap();
    let last = recs.last().unwrap();
    assert_eq!(last.kind, AuditKind::Secret);
    assert!(String::from_utf8_lossy(&last.encode()).contains("env:CARGO_REGISTRY_TOKEN"));
}

#[test]
fn substitution_matrix_only_the_bound_destination_gets_the_value() {
    let mut r = rig();
    let ok =
        r.b.substitute(Peer::Proxy, &sub("acme-api", "api.acme-invoices.com", 443))
            .unwrap();
    assert_eq!(ok.value.as_slice(), API_VALUE);
    assert_eq!(ok.rotation_counter, 0);
    for (host, port) in [
        ("evil.com", 443),
        ("api.acme-invoices.com", 8443),
        ("api.acme-invoices.com.evil.com", 443),
        ("xapi.acme-invoices.com", 443),
        ("api.acme-invoices.com.", 443),
        ("10.0.0.1", 443),
        ("", 443),
    ] {
        assert_eq!(
            r.b.substitute(Peer::Proxy, &sub("acme-api", host, port))
                .unwrap_err(),
            Status::OutOfScope,
            "{host}:{port}"
        );
    }
    // A redirect to another host is another connection, asked afresh.
    assert_eq!(
        r.b.substitute(Peer::Proxy, &sub("acme-api", "api.other.com", 443))
            .unwrap_err(),
        Status::OutOfScope
    );
    // No capability asserted: nothing.
    let mut s = sub("acme-api", "api.acme-invoices.com", 443);
    s.holds_secret_use = false;
    assert_eq!(r.b.substitute(Peer::Proxy, &s).unwrap_err(), Status::NoCapability);
    // Unknown name and not-held look identical.
    assert_eq!(
        r.b.substitute(Peer::Proxy, &sub("nope", "api.acme-invoices.com", 443))
            .unwrap_err(),
        Status::NoCapability
    );
}

#[test]
fn list_and_status_responses_never_carry_a_value() {
    let mut r = rig();
    for req in [Request::List, Request::Status] {
        let out = r.b.handle(Peer::Owner, &req.encode(), NOW);
        for v in [API_VALUE, LOGIN_VALUE, TOKEN_VALUE, TOTP_SEED] {
            assert!(!contains(&out, v));
        }
    }
    // The metadata does list, so the test is not vacuous.
    let out = r.b.handle(Peer::Owner, &Request::List.encode(), NOW);
    assert!(contains(&out, b"acme-api"));
}

// ---- field_fill preconditions fail closed before any read ----------------

#[test]
fn every_field_fill_precondition_fails_before_a_value_is_read() {
    let mut r = rig();
    // Any attempt to read a value now fails with Internal. A precondition
    // that is reported with its own status therefore never reached the read.
    corrupt_values(&r.dir);
    let cases: Vec<(&str, FillReq, Status)> = vec![
        (
            "no secret.use",
            fill(|f| f.holds_secret_use = false),
            Status::NoCapability,
        ),
        (
            "no seat.text",
            fill(|f| f.holds_seat_text = false),
            Status::NoCapability,
        ),
        (
            "wrong role",
            fill(|f| f.role = NodeRole::Other),
            Status::InvalidArgument,
        ),
        (
            "textfield without ext.credential",
            fill(|f| {
                f.role = NodeRole::TextField;
                f.credential = false;
            }),
            Status::InvalidArgument,
        ),
        (
            "stale generation",
            fill(|f| f.generation = 6),
            Status::StaleGeneration,
        ),
        (
            "app not on the owner's list",
            fill(|f| f.app_listed = false),
            Status::OutOfScope,
        ),
        (
            "wrong app",
            fill(|f| f.app_id = "org.evil.browser".into()),
            Status::OutOfScope,
        ),
        (
            "wrong url",
            fill(|f| f.url = Some("https://login.evil.com/".into())),
            Status::OutOfScope,
        ),
        (
            "lookalike url",
            fill(|f| f.url = Some("https://login.acme.com@evil.com/".into())),
            Status::OutOfScope,
        ),
        (
            "no url on a web-bound secret",
            fill(|f| f.url = None),
            Status::OutOfScope,
        ),
        (
            "secret not in field_fill mode",
            fill(|f| f.name = "acme-api".into()),
            Status::OutOfScope,
        ),
        (
            "unknown secret",
            fill(|f| f.name = "nope".into()),
            Status::NoCapability,
        ),
        (
            "rotated since",
            fill(|f| f.expected_rotation = Some(9)),
            Status::SecretRotated,
        ),
        (
            "malformed principal",
            fill(|f| f.principal = "root".into()),
            Status::InvalidArgument,
        ),
    ];
    for (what, req, want) in cases {
        assert_eq!(
            r.b.field_fill(Peer::Compositor, &req).unwrap_err(),
            want,
            "{what}"
        );
    }
    // Wrong peer.
    assert_eq!(
        r.b.field_fill(Peer::Proxy, &fill(|_| {})).unwrap_err(),
        Status::NoCapability
    );
    // Control: the fully valid request does reach the read, and fails there.
    assert_eq!(
        r.b.field_fill(Peer::Compositor, &fill(|_| {})).unwrap_err(),
        Status::Internal
    );
    // And nothing was released or recorded as released.
    assert!(!r
        .sink
        .encoded()
        .iter()
        .any(|e| contains(e, b"\x62ok") && contains(e, b"field_fill")));
}

#[test]
fn a_locked_broker_fails_field_fill_before_any_read() {
    let mut r = rig();
    r.b.lock(Peer::Compositor).unwrap();
    assert_eq!(
        r.b.field_fill(Peer::Compositor, &fill(|_| {})).unwrap_err(),
        Status::BrokerLocked
    );
    assert_eq!(
        r.b.substitute(Peer::Proxy, &sub("acme-api", "api.acme-invoices.com", 443))
            .unwrap_err(),
        Status::BrokerLocked
    );
    assert_eq!(
        r.b.materialize(Peer::Agentd, &mat("builder-token", true))
            .unwrap_err(),
        Status::BrokerLocked
    );
    assert_eq!(
        r.b.totp(Peer::Agentd, &totp_req("login.acme.com"), NOW)
            .unwrap_err(),
        Status::BrokerLocked
    );
    assert_eq!(r.b.list(Peer::Owner).unwrap_err(), Status::BrokerLocked);
}

#[test]
fn a_valid_field_fill_releases_the_value_and_audits_only_its_length() {
    let mut r = rig();
    let v = r.b.field_fill(Peer::Compositor, &fill(|_| {})).unwrap();
    assert_eq!(v.value.as_slice(), LOGIN_VALUE);
    // `textfield` with ext.credential is also a valid target.
    let t = fill(|f| {
        f.role = NodeRole::TextField;
        f.credential = true;
    });
    assert!(r.b.field_fill(Peer::Compositor, &t).is_ok());
    let recs = r.sink.records.lock().unwrap();
    let rec = recs
        .iter()
        .rev()
        .find(|e| contains(&e.body, b"field_fill"))
        .unwrap();
    assert!(contains(&rec.body, b"length"));
    // The URL is recorded, the value is not.
    assert!(contains(&rec.body, b"https://login.acme.com/signin"));
    let mut rd = Reader::new(&rec.body);
    let n = rd.map_begin().unwrap();
    for _ in 0..n {
        if rd.key().unwrap() == "length" {
            assert_eq!(rd.u64().unwrap(), LOGIN_VALUE.len() as u64);
        } else {
            rd.skip().unwrap();
        }
    }
}

// ---- audit hygiene --------------------------------------------------------

fn contains(hay: &[u8], needle: &[u8]) -> bool {
    !needle.is_empty() && hay.windows(needle.len()).any(|w| w == needle)
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

/// Every digest an audit reader could be handed to brute-force offline.
fn digests(v: &[u8]) -> Vec<Vec<u8>> {
    use sha2::{Digest, Sha256};
    let b3 = blake3::hash(v);
    let s2 = Sha256::digest(v);
    let s1 = sha1::Sha1::digest(v);
    let mut out = vec![b3.as_bytes().to_vec(), s2.to_vec(), s1.to_vec()];
    for d in out.clone() {
        out.push(hex(&d).into_bytes());
    }
    out.push(hex(v).into_bytes());
    out
}

#[test]
fn no_audit_record_holds_a_value_or_a_hash_of_one() {
    let mut r = rig();
    // Exercise every path, success and denial.
    r.b.substitute(Peer::Proxy, &sub("acme-api", "api.acme-invoices.com", 443))
        .unwrap();
    let _ = r.b.substitute(Peer::Proxy, &sub("acme-api", "evil.com", 443));
    r.b.field_fill(Peer::Compositor, &fill(|_| {})).unwrap();
    let _ = r.b.field_fill(
        Peer::Compositor,
        &fill(|f| f.url = Some("https://evil.com/?pw=hunter2-DISTINCT-FILL-VALUE".into())),
    );
    r.b.materialize(Peer::Agentd, &mat("builder-token", true))
        .unwrap();
    r.b.totp(Peer::Agentd, &totp_req("login.acme.com"), NOW).unwrap();
    r.b.rotate(Peer::Owner, "acme-api", b"sk_live_ROTATED_VALUE_2")
        .unwrap();
    r.b.revoke(Peer::Owner, "builder-token").unwrap();

    let all = r.sink.encoded();
    assert!(all.len() >= 8, "every path emitted: {}", all.len());
    let values: [&[u8]; 6] = [
        API_VALUE,
        LOGIN_VALUE,
        TOKEN_VALUE,
        TOTP_SEED,
        b"sk_live_ROTATED_VALUE_2",
        b"hunter2",
    ];
    for rec in &all {
        let e = Emission::decode(rec).unwrap();
        assert_eq!(e.kind, AuditKind::Secret);
        for v in values {
            assert!(!contains(rec, v), "a value appears in an audit record");
            for d in digests(v) {
                assert!(
                    !contains(rec, &d),
                    "a digest of a value appears in an audit record"
                );
            }
        }
    }
    // The denied fill carried the value in its query string; the audit
    // destination dropped the query.
    assert!(all.iter().any(|e| contains(e, b"out_of_scope")));
}

#[test]
fn each_record_has_exactly_the_spec_fields() {
    let mut r = rig();
    r.b.substitute(Peer::Proxy, &sub("acme-api", "api.acme-invoices.com", 443))
        .unwrap();
    r.b.field_fill(Peer::Compositor, &fill(|_| {})).unwrap();
    let recs = r.sink.records.lock().unwrap();
    for e in recs.iter().skip(4) {
        let mut rd = Reader::new(&e.body);
        let n = rd.map_begin().unwrap();
        let mut keys = vec![];
        for _ in 0..n {
            keys.push(rd.key().unwrap().to_owned());
            rd.skip().unwrap();
        }
        let allowed = [
            "destination",
            "grant_id",
            "length",
            "mode",
            "outcome",
            "principal",
            "rotation_counter",
            "secret_id",
        ];
        assert!(keys.iter().all(|k| allowed.contains(&k.as_str())), "{keys:?}");
        let is_fill = keys.contains(&"length".to_owned());
        assert_eq!(
            is_fill,
            contains(&e.body, b"field_fill"),
            "length only for field_fill"
        );
    }
}

#[test]
fn audit_down_denies_the_use_and_releases_nothing() {
    let mut r = rig();
    *r.sink.down.lock().unwrap() = true;
    assert_eq!(
        r.b.substitute(Peer::Proxy, &sub("acme-api", "api.acme-invoices.com", 443))
            .unwrap_err(),
        Status::Internal
    );
    assert_eq!(
        r.b.field_fill(Peer::Compositor, &fill(|_| {})).unwrap_err(),
        Status::Internal
    );
    assert_eq!(
        r.b.rotate(Peer::Owner, "acme-api", b"x").unwrap_err(),
        Status::Internal
    );
    // A secret whose creation cannot be journalled does not exist.
    let s = spec("new", Kind::Bearer, &["host:a.com"], &[Mode::ProxyHeader]);
    assert_eq!(r.b.add(Peer::Owner, &s, b"v", NOW).unwrap_err(), Status::Internal);
    *r.sink.down.lock().unwrap() = false;
    assert!(!r.b.list(Peer::Owner).unwrap().iter().any(|m| m.name == "new"));
    // The rotation that could not be journalled did not happen.
    assert_eq!(
        r.b.substitute(Peer::Proxy, &sub("acme-api", "api.acme-invoices.com", 443))
            .unwrap()
            .rotation_counter,
        0
    );
}

// ---- rotation, revocation, TOTP -------------------------------------------

#[test]
fn rotation_fails_in_flight_uses_and_revocation_is_immediate() {
    let mut r = rig();
    let mut s = sub("acme-api", "api.acme-invoices.com", 443);
    s.expected_rotation = Some(0);
    assert!(r.b.substitute(Peer::Proxy, &s).is_ok());
    let m =
        r.b.rotate(Peer::Owner, "acme-api", b"sk_live_ROTATED_VALUE_2")
            .unwrap();
    assert_eq!(m.rotation_counter, 1);
    assert_eq!(
        r.b.substitute(Peer::Proxy, &s).unwrap_err(),
        Status::SecretRotated
    );
    s.expected_rotation = Some(1);
    assert_eq!(
        r.b.substitute(Peer::Proxy, &s).unwrap().value.as_slice(),
        b"sk_live_ROTATED_VALUE_2"
    );
    r.b.revoke(Peer::Owner, "acme-api").unwrap();
    assert_eq!(r.b.substitute(Peer::Proxy, &s).unwrap_err(), Status::NoCapability);
    assert!(fs::read_dir(&r.dir).unwrap().all(|e| !e
        .unwrap()
        .file_name()
        .to_string_lossy()
        .contains(&m.id.to_text())));
}

#[test]
fn totp_issues_a_code_not_a_seed_and_is_rate_limited() {
    let mut r = rig();
    let (code, life) = r.b.totp(Peer::Agentd, &totp_req("login.acme.com"), 59).unwrap();
    assert_eq!(code, "287082");
    assert_eq!(life, 1);
    assert_eq!(
        r.b.totp(Peer::Agentd, &totp_req("login.acme.com"), 60)
            .unwrap()
            .0
            .len(),
        6
    );
    assert!(r.b.totp(Peer::Agentd, &totp_req("login.acme.com"), 61).is_ok());
    assert_eq!(
        r.b.totp(Peer::Agentd, &totp_req("login.acme.com"), 62)
            .unwrap_err(),
        Status::RateLimited
    );
    assert!(
        r.b.totp(Peer::Agentd, &totp_req("login.acme.com"), 400).is_ok(),
        "window passed"
    );
    assert_eq!(
        r.b.totp(Peer::Agentd, &totp_req("evil.com"), 900).unwrap_err(),
        Status::OutOfScope
    );
    let mut wrong_kind = totp_req("api.acme-invoices.com");
    wrong_kind.name = "acme-api".into();
    assert_eq!(
        r.b.totp(Peer::Agentd, &wrong_kind, 900).unwrap_err(),
        Status::InvalidArgument
    );
}

// ---- unlock via trusted UI --------------------------------------------------

fn pass_broker(tag: &str) -> (Broker, PathBuf) {
    let dir = tmpdir(tag);
    let sealer = PassphraseSealer {
        params: KdfParams::TEST,
    };
    let mut b = Broker::new(dir.clone(), Box::new(sealer), Box::new(MemorySink::default()));
    assert_eq!(
        b.initialize(Peer::Owner, None).unwrap_err(),
        Status::InvalidArgument,
        "a passphrase is required"
    );
    b.initialize(Peer::Owner, Some(b"correct horse")).unwrap();
    b.add(
        Peer::Owner,
        &spec("t", Kind::Bearer, &["host:a.com"], &[Mode::ProxyHeader]),
        b"v",
        NOW,
    )
    .unwrap();
    b.lock(Peer::Compositor).unwrap();
    (b, dir)
}

fn submit(nonce: u64, pw: &[u8]) -> UnlockAnswer {
    UnlockAnswer::Submit {
        nonce,
        passphrase: Some(Secret::new(pw.to_vec())),
    }
}

#[test]
fn unlock_handshake_is_nonce_bound_single_use_and_expires() {
    let (mut b, dir) = pass_broker("unlock");
    assert!(!b.is_unlocked());
    // Only the compositor (and the owner's TTY) can prompt.
    assert_eq!(
        b.begin_unlock(Peer::Proxy, 100).unwrap_err(),
        Status::NoCapability
    );
    assert_eq!(
        b.begin_unlock(Peer::Agentd, 100).unwrap_err(),
        Status::NoCapability
    );

    let q = b.begin_unlock(Peer::Compositor, 100).unwrap();
    assert_eq!(q.method, UnlockMethod::Passphrase);
    assert_eq!(q.attempts_left, MAX_ATTEMPTS);
    // Wrong passphrase: refused, and the request is consumed.
    assert_eq!(
        b.answer_unlock(Peer::Compositor, submit(q.nonce, b"wrong"), 101)
            .unwrap_err(),
        Status::BadCredential
    );
    assert!(!b.is_unlocked());
    // Replaying the answer finds nothing pending.
    assert_eq!(
        b.answer_unlock(Peer::Compositor, submit(q.nonce, b"correct horse"), 102)
            .unwrap_err(),
        Status::InvalidArgument
    );
    // A wrong nonce is refused and consumes the request too.
    let q = b.begin_unlock(Peer::Compositor, 110).unwrap();
    assert_eq!(q.attempts_left, MAX_ATTEMPTS - 1);
    assert_eq!(
        b.answer_unlock(Peer::Compositor, submit(q.nonce ^ 1, b"correct horse"), 111)
            .unwrap_err(),
        Status::InvalidArgument
    );
    // An expired prompt is refused.
    let q = b.begin_unlock(Peer::Compositor, 200).unwrap();
    assert_eq!(
        b.answer_unlock(
            Peer::Compositor,
            submit(q.nonce, b"correct horse"),
            200 + UNLOCK_TTL_S
        )
        .unwrap_err(),
        Status::InvalidArgument
    );
    // Only the compositor/owner can answer.
    let q = b.begin_unlock(Peer::Compositor, 300).unwrap();
    assert_eq!(
        b.answer_unlock(Peer::Agentd, submit(q.nonce, b"correct horse"), 301)
            .unwrap_err(),
        Status::NoCapability
    );
    // Cancel leaves it locked.
    let q = b.begin_unlock(Peer::Compositor, 310).unwrap();
    assert_eq!(
        b.answer_unlock(Peer::Compositor, UnlockAnswer::Cancel { nonce: q.nonce }, 311)
            .unwrap(),
        UnlockOutcome::Cancelled
    );
    assert!(!b.is_unlocked());
    // The right answer unlocks.
    let q = b.begin_unlock(Peer::Compositor, 400).unwrap();
    assert_eq!(
        b.answer_unlock(Peer::Compositor, submit(q.nonce, b"correct horse"), 401)
            .unwrap(),
        UnlockOutcome::Unlocked
    );
    assert!(b.is_unlocked());
    assert_eq!(b.list(Peer::Compositor).unwrap().len(), 1);
    // A second prompt while unlocked is refused.
    assert_eq!(
        b.begin_unlock(Peer::Compositor, 500).unwrap_err(),
        Status::InvalidArgument
    );
    // Locking again (screen lock, session end) closes it.
    b.lock(Peer::Compositor).unwrap();
    assert_eq!(b.list(Peer::Compositor).unwrap_err(), Status::BrokerLocked);
    let _ = fs::remove_dir_all(dir);
}

#[test]
fn repeated_wrong_passphrases_lock_the_prompt_out() {
    let (mut b, dir) = pass_broker("lockout");
    let mut t = 1000;
    for _ in 0..MAX_ATTEMPTS {
        let q = b.begin_unlock(Peer::Compositor, t).unwrap();
        assert_eq!(
            b.answer_unlock(Peer::Compositor, submit(q.nonce, b"nope"), t + 1)
                .unwrap_err(),
            Status::BadCredential
        );
        t += 2;
    }
    assert_eq!(
        b.begin_unlock(Peer::Compositor, t).unwrap_err(),
        Status::RateLimited
    );
    let q = b.begin_unlock(Peer::Compositor, t + LOCKOUT_S + 10).unwrap();
    assert_eq!(
        b.answer_unlock(
            Peer::Compositor,
            submit(q.nonce, b"correct horse"),
            t + LOCKOUT_S + 11
        )
        .unwrap(),
        UnlockOutcome::Unlocked
    );
    let _ = fs::remove_dir_all(dir);
}

#[test]
fn a_sealer_bound_to_a_policy_does_not_unlock_under_another() {
    let dir = tmpdir("policy");
    let mut b = Broker::new(
        dir.clone(),
        Box::new(SoftSealer::new([1; 32], b"boot-policy-A")),
        Box::new(MemorySink::default()),
    );
    b.initialize(Peer::Owner, None).unwrap();
    b.lock(Peer::Owner).unwrap();
    let mut other = Broker::new(
        dir.clone(),
        Box::new(SoftSealer::new([1; 32], b"boot-policy-B")),
        Box::new(MemorySink::default()),
    );
    let q = other.begin_unlock(Peer::Compositor, 1).unwrap();
    assert_eq!(q.method, UnlockMethod::Confirm);
    assert_eq!(
        other
            .answer_unlock(
                Peer::Compositor,
                UnlockAnswer::Submit {
                    nonce: q.nonce,
                    passphrase: None
                },
                2
            )
            .unwrap_err(),
        Status::BadCredential
    );
    let q = b.begin_unlock(Peer::Compositor, 3).unwrap();
    assert_eq!(
        b.answer_unlock(
            Peer::Compositor,
            UnlockAnswer::Submit {
                nonce: q.nonce,
                passphrase: None
            },
            4
        )
        .unwrap(),
        UnlockOutcome::Unlocked
    );
    let _ = fs::remove_dir_all(dir);
}

// ---- the wire path -----------------------------------------------------------

#[test]
fn the_wire_path_releases_and_denies_like_the_api() {
    let mut r = rig();
    let req = Request::FieldFill(FillWire(FillFields {
        principal: "agent:a1".into(),
        grant: Some([9; 16]),
        name: "acme-login".into(),
        role: "password".into(),
        cred: false,
        app: "org.mozilla.firefox".into(),
        url: Some("https://login.acme.com/signin".into()),
        gen: 5,
        egen: 5,
        holds_use: true,
        holds_seat: true,
        listed: true,
        erot: None,
    }));
    let out = r.b.handle(Peer::Compositor, &req.encode(), NOW);
    match Response::decode(&out).unwrap() {
        Response::Value {
            rotation_counter,
            value,
        } => {
            assert_eq!(rotation_counter, 0);
            assert_eq!(&*value, LOGIN_VALUE);
        }
        other => panic!("{other:?}"),
    }
    // The same request from the wrong peer is a bare status.
    let out = r.b.handle(Peer::Proxy, &req.encode(), NOW);
    assert_eq!(
        Response::decode(&out).unwrap(),
        Response::Err(Status::NoCapability.code())
    );
    assert!(!contains(&out, LOGIN_VALUE));
    // Garbage is invalid_argument.
    let out = r.b.handle(Peer::Compositor, b"\xff\xff", NOW);
    assert_eq!(
        Response::decode(&out).unwrap(),
        Response::Err(Status::InvalidArgument.code())
    );
    // Substitute, materialize and TOTP over the wire.
    let s = Request::Substitute(SubstituteWire(SubstituteFields {
        principal: "agent:a1".into(),
        grant: None,
        name: "acme-api".into(),
        host: "api.acme-invoices.com".into(),
        port: 443,
        url: None,
        holds_use: true,
        erot: None,
    }));
    assert!(matches!(
        Response::decode(&r.b.handle(Peer::Proxy, &s.encode(), NOW)).unwrap(),
        Response::Value { .. }
    ));
    let m = Request::Materialize(MaterializeWire(MaterializeFields {
        principal: "agent:b1".into(),
        grant: None,
        name: "builder-token".into(),
        target: "file:/run/token".into(),
        holds_expose: false,
        erot: None,
    }));
    assert_eq!(
        Response::decode(&r.b.handle(Peer::Agentd, &m.encode(), NOW)).unwrap(),
        Response::Err(Status::NoCapability.code())
    );
    let t = Request::Totp(TotpWire(TotpFields {
        principal: "agent:a1".into(),
        grant: None,
        name: "acme-totp".into(),
        host: "login.acme.com".into(),
        port: 443,
        holds_use: true,
    }));
    match Response::decode(&r.b.handle(Peer::Agentd, &t.encode(), 59)).unwrap() {
        Response::Code { code, life_s } => assert_eq!((code.as_str(), life_s), ("287082", 1)),
        other => panic!("{other:?}"),
    }
}

// ---- memory hygiene: core dumps and /proc/<pid>/mem ---------------------------

struct Probe {
    child: std::process::Child,
    pid: i32,
    cwd: PathBuf,
    line: String,
}

fn probe() -> Probe {
    use std::io::{BufRead, BufReader};
    use std::process::{Command, Stdio};
    let cwd = tmpdir("probe");
    fs::create_dir_all(&cwd).unwrap();
    let mut child = Command::new(env!("CARGO_BIN_EXE_ec-brokerd"))
        .arg("--hygiene-probe")
        .current_dir(&cwd)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    let mut line = String::new();
    BufReader::new(child.stdout.as_mut().unwrap())
        .read_line(&mut line)
        .unwrap();
    assert!(line.starts_with("ready "), "probe said {line:?}");
    let pid = child.id() as i32;
    Probe {
        child,
        pid,
        cwd,
        line,
    }
}

impl Probe {
    fn field(&self, k: &str) -> u64 {
        self.line
            .split_whitespace()
            .find_map(|w| w.strip_prefix(&format!("{k}=")))
            .unwrap()
            .parse()
            .unwrap()
    }

    /// Kills by the pid captured at spawn.
    fn kill(&mut self, sig: i32) {
        // SAFETY: signalling the child this test spawned, by its own pid.
        assert_eq!(unsafe { libc::kill(self.pid, sig) }, 0);
    }
}

impl Drop for Probe {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
        let _ = fs::remove_dir_all(&self.cwd);
    }
}

#[test]
fn the_daemon_hardens_itself_and_locks_its_memory() {
    let p = probe();
    assert_eq!(p.field("dumpable"), 0, "PR_SET_DUMPABLE=0");
    assert_eq!(p.field("core_soft"), 0, "RLIMIT_CORE soft");
    assert_eq!(p.field("core_hard"), 0, "RLIMIT_CORE hard");
    assert!(p.field("vmlck_kib") >= 4, "mlock took effect");
}

#[test]
fn a_crash_leaves_no_core_file() {
    use std::os::unix::process::ExitStatusExt;
    let mut p = probe();
    p.kill(libc::SIGABRT);
    let st = p.child.wait().unwrap();
    assert_eq!(st.signal(), Some(libc::SIGABRT));
    assert!(!st.core_dumped(), "kernel reports no core dump");
    let stray: Vec<_> = fs::read_dir(&p.cwd)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .filter(|n| n.starts_with("core"))
        .collect();
    assert!(stray.is_empty(), "core files: {stray:?}");
}

#[test]
fn proc_pid_mem_is_closed_to_an_unprivileged_same_uid_reader() {
    use std::os::unix::fs::MetadataExt;
    let p = probe();
    // SAFETY: geteuid has no preconditions.
    if unsafe { libc::geteuid() } == 0 {
        // Root (CAP_SYS_PTRACE) reads any process; that is outside the
        // threat model (the sandbox is neither). The dumpable and core
        // assertions above are what this runner can check.
        eprintln!("skipping /proc/<pid>/mem read: running as root");
        return;
    }
    // A non-dumpable process's /proc entries are owned by root. The entries,
    // not the /proc/<pid> directory: current kernels leave the directory
    // with the task's own uid, so `mem` itself is what is checked.
    let mem = format!("/proc/{}/mem", p.pid);
    assert_eq!(fs::metadata(&mem).unwrap().uid(), 0);
    let e = fs::File::open(&mem).unwrap_err();
    assert_eq!(e.kind(), std::io::ErrorKind::PermissionDenied);
}
