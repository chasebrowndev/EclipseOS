// SPDX-License-Identifier: AGPL-3.0-only
//! COMP-16 M12 exit and COMP-15 audit completeness, over synthetic records:
//! `trace --req-id` reconstructs a request's chain, every acting request
//! has its request, decision and result, a tampered store answers nothing,
//! and no record carries human keystroke content.

use std::fs;
use std::path::PathBuf;

use ec_audit::{project, query, records, trace, Filter};
use ec_policy_eval::audit::{Emission, Kind};
use ec_policy_eval::cbor::{enc, MapBuilder};
use ec_policyd::audit::{Record, Store};

fn tmp(name: &str) -> PathBuf {
    let p = std::env::temp_dir().join(format!("ec-audit-{name}-{}", std::process::id()));
    let _ = fs::remove_dir_all(&p);
    p
}

fn body(pairs: &[(&str, &str)]) -> Vec<u8> {
    let mut m = MapBuilder::new();
    for (k, v) in pairs {
        m.insert(k, enc(|w| w.text(v)));
    }
    m.finish()
}

fn emit(s: &mut Store, kind: Kind, principal: &str, req_id: Option<u64>, b: Vec<u8>) {
    let e = Emission {
        kind,
        principal: principal.into(),
        grant_id: None,
        task_id: None,
        chain_id: None,
        req_id,
        serial: None,
        body: b,
    };
    s.append(Record::from_emission(e)).unwrap();
}

/// Two agents reuse req_id 1, interleaved with each other and a human.
fn world(name: &str) -> PathBuf {
    let dir = tmp(name);
    let mut s = Store::open(&dir).unwrap();
    for (who, outcome) in [("agent:a", "allow"), ("agent:b", "deny")] {
        emit(
            &mut s,
            Kind::Request,
            who,
            Some(1),
            body(&[("request", "get_toplevel")]),
        );
        emit(&mut s, Kind::Focus, "human", None, body(&[("cause", "human")]));
        emit(
            &mut s,
            Kind::Decision,
            who,
            Some(1),
            body(&[("outcome", outcome)]),
        );
        emit(&mut s, Kind::Result, who, Some(1), body(&[("detail", "")]));
    }
    s.rotate().unwrap();
    emit(
        &mut s,
        Kind::Request,
        "agent:a",
        Some(2),
        body(&[("request", "hit_test")]),
    );
    emit(
        &mut s,
        Kind::Decision,
        "agent:a",
        Some(2),
        body(&[("outcome", "allow")]),
    );
    emit(&mut s, Kind::Result, "agent:a", Some(2), body(&[("detail", "")]));
    s.sync().unwrap();
    dir
}

#[test]
fn trace_reconstructs_one_agents_chain_across_a_rotation() {
    let dir = world("trace");
    let (_, all) = records(&dir).unwrap();
    for (req, who) in [(1, "agent:a"), (1, "agent:b"), (2, "agent:a")] {
        let t = trace(&all, req, Some(who));
        let kinds: Vec<_> = t.iter().map(|r| r.kind).collect();
        assert_eq!(
            kinds,
            [Kind::Request, Kind::Decision, Kind::Result],
            "{who} {req}"
        );
        assert!(t.iter().all(|r| r.principal == who));
        assert!(t.windows(2).all(|w| w[0].seq < w[1].seq));
    }
    // Without a principal, a req_id is every agent's that used it.
    assert_eq!(trace(&all, 1, None).len(), 6);
    fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn every_acting_request_has_a_decision_and_a_result() {
    let dir = world("complete");
    let (_, all) = records(&dir).unwrap();
    for req in all.iter().filter(|r| r.kind == Kind::Request) {
        let id = req.req_id.expect("a request carries its req_id");
        let t = trace(&all, id, Some(&req.principal));
        for k in [Kind::Decision, Kind::Result] {
            assert!(t.iter().any(|r| r.kind == k && r.seq > req.seq), "{k:?} for {id}");
        }
    }
    fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn query_filters_on_outcome_and_kind() {
    let dir = world("query");
    let (_, all) = records(&dir).unwrap();
    let denied = query(
        &all,
        &Filter {
            outcome: Some("deny".into()),
            ..Default::default()
        },
    );
    assert_eq!(denied.len(), 1);
    assert_eq!(denied[0].principal, "agent:b");
    let focus = query(
        &all,
        &Filter {
            kind: Some(Kind::Focus),
            ..Default::default()
        },
    );
    assert_eq!(focus.len(), 2);
    fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn a_tampered_store_answers_nothing() {
    let dir = world("tamper");
    let p = dir.join("current.open");
    let mut bytes = fs::read(&p).unwrap();
    let n = bytes.len();
    bytes[n - 40] ^= 0xff;
    fs::write(&p, &bytes).unwrap();
    assert!(records(&dir).is_err());
    fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn the_projection_is_json_with_absent_fields_absent() {
    let dir = world("project");
    let (_, all) = records(&dir).unwrap();
    let j = project(&all[0]);
    assert_eq!(j["kind"], "request");
    assert_eq!(j["req_id"], 1);
    assert_eq!(j["body"]["request"], "get_toplevel");
    assert!(j.get("grant_id").is_none() && j.get("chain_id").is_none());
    fs::remove_dir_all(&dir).unwrap();
}

/// S-04 §2: human input is never recorded by content. No human `input`
/// record exists, and no human record's body carries a key or text field.
#[test]
fn no_human_record_carries_keystroke_content() {
    let dir = world("keys");
    let (_, all) = records(&dir).unwrap();
    for r in all.iter().filter(|r| r.principal == "human") {
        assert_ne!(r.kind, Kind::Input);
        let b = project(r)["body"].clone();
        for k in ["keycode", "keysym", "text", "utf8"] {
            assert!(b.get(k).is_none(), "{k} in {b}");
        }
    }
    fs::remove_dir_all(&dir).unwrap();
}
