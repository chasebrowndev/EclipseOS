// SPDX-License-Identifier: AGPL-3.0-only
//! The audit store's reader (S-04 §4, §5): verify the chain, reconstruct a
//! request's chain by `req_id`, and filter records, each as a JSON
//! projection (S-04 §1).
//!
//! Every answer comes from [`ec_policyd::audit::verify`]: a store that does not
//! verify yields its break, never a partial listing of records that might
//! follow a forged one.

#![forbid(unsafe_code)]

use std::path::{Path, PathBuf};

use ec_policy_eval::audit::Kind;
use ec_policy_eval::cbor::Reader;
use ec_policyd::audit::{verify, Record, StoreError, Verified};
use serde_json::{json, Map, Value};

/// `policyd`'s state directory, as `policyd` itself resolves it.
pub fn store_dir() -> PathBuf {
    if let Some(d) = std::env::var_os("ECLIPSE_STATE_DIR") {
        return PathBuf::from(d).join("policyd");
    }
    let home = std::env::var_os("HOME").map(PathBuf::from).unwrap_or_default();
    home.join(".local/state/eclipse/policyd")
}

/// Every verified record, oldest first.
pub fn records(dir: &Path) -> Result<(Verified, Vec<Record>), StoreError> {
    let mut out = Vec::new();
    let v = verify(dir, |r| out.push(r.clone()))?;
    Ok((v, out))
}

/// `trace --req-id` (S-04 §5): every record of one request, in the order
/// they were written — request, decision, prompt, input, result. A `req_id`
/// is unique per agent, not globally, so `principal` narrows it to one.
pub fn trace(records: &[Record], req_id: u64, principal: Option<&str>) -> Vec<Record> {
    records
        .iter()
        .filter(|r| r.req_id == Some(req_id))
        .filter(|r| principal.is_none_or(|p| r.principal == p))
        .cloned()
        .collect()
}

/// `query` (S-04 §5). Every set field must match.
#[derive(Debug, Default, Clone)]
pub struct Filter {
    pub principal: Option<String>,
    pub kind: Option<Kind>,
    /// A `decision`'s `outcome`: `allow`, `deny`, `prompt`, `defer`.
    pub outcome: Option<String>,
    /// Wall-clock nanoseconds; records older are left out.
    pub since_ns: Option<u64>,
}

pub fn query(records: &[Record], f: &Filter) -> Vec<Record> {
    records
        .iter()
        .filter(|r| f.principal.as_ref().is_none_or(|p| &r.principal == p))
        .filter(|r| f.kind.is_none_or(|k| r.kind == k))
        .filter(|r| f.since_ns.is_none_or(|t| r.ts >= t))
        .filter(|r| {
            f.outcome.as_ref().is_none_or(|o| {
                r.kind == Kind::Decision && body_text(&r.body, "outcome").as_deref() == Some(o.as_str())
            })
        })
        .cloned()
        .collect()
}

/// `--since 90s|15m|1h|7d`, as nanoseconds.
pub fn parse_since(s: &str) -> Option<u64> {
    let (n, unit) = s.split_at(s.find(|c: char| !c.is_ascii_digit())?);
    let n: u64 = n.parse().ok()?;
    let secs = match unit {
        "s" => 1,
        "m" => 60,
        "h" => 3_600,
        "d" => 86_400,
        _ => return None,
    };
    n.checked_mul(secs)?.checked_mul(1_000_000_000)
}

fn body_text(body: &[u8], key: &str) -> Option<String> {
    let mut r = Reader::new(body);
    let n = r.map_begin().ok()?;
    for _ in 0..n {
        if r.key().ok()? == key {
            return r.text().ok().map(str::to_owned);
        }
        r.skip().ok()?;
    }
    None
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

/// The JSON projection of one record (S-04 §1). Absent fields stay absent.
pub fn project(r: &Record) -> Value {
    let mut m = Map::new();
    m.insert("seq".into(), json!(r.seq));
    m.insert("ts".into(), json!(r.ts));
    m.insert("mono".into(), json!(r.mono));
    m.insert("kind".into(), json!(r.kind.as_str()));
    m.insert("principal".into(), json!(r.principal));
    if let Some(g) = r.grant_id {
        m.insert("grant_id".into(), json!(g.to_string()));
    }
    if let Some(t) = r.task_id {
        m.insert("task_id".into(), json!(t.to_string()));
    }
    if let Some(c) = r.chain_id {
        m.insert("chain_id".into(), json!(hex(&c.to_be_bytes())));
    }
    if let Some(q) = r.req_id {
        m.insert("req_id".into(), json!(q));
    }
    if let Some(s) = r.serial {
        m.insert("serial".into(), json!(s));
    }
    m.insert("body".into(), cbor_json(&r.body));
    m.insert("hash".into(), json!(hex(&r.hash)));
    Value::Object(m)
}

/// A body as JSON: maps, arrays, text, integers, booleans and null as
/// themselves, byte strings as hex. A body the profile cannot read is
/// shown as its raw hex rather than dropped.
pub fn cbor_json(body: &[u8]) -> Value {
    let mut r = Reader::new(body);
    match item(body, &mut r) {
        Some(v) if r.finish().is_ok() => v,
        _ => json!({ "undecoded": hex(body) }),
    }
}

fn item(buf: &[u8], r: &mut Reader<'_>) -> Option<Value> {
    let first = *buf.get(r.position())?;
    Some(match first >> 5 {
        0 => json!(r.u64().ok()?),
        1 => json!(r.i64().ok()?),
        2 => json!(hex(r.bytes().ok()?)),
        3 => json!(r.text().ok()?),
        4 => {
            let n = r.array_len().ok()?;
            let mut a = Vec::new();
            for _ in 0..n {
                a.push(item(buf, r)?);
            }
            Value::Array(a)
        }
        5 => {
            let n = r.map_begin().ok()?;
            let mut m = Map::new();
            for _ in 0..n {
                let k = match *buf.get(r.position())? >> 5 {
                    3 => r.key().ok()?.to_owned(),
                    _ => r.int_key().ok()?.to_string(),
                };
                m.insert(k, item(buf, r)?);
            }
            r.map_end().ok()?;
            Value::Object(m)
        }
        7 if first == 0xf6 => {
            r.skip().ok()?;
            Value::Null
        }
        7 => json!(r.bool().ok()?),
        _ => return None,
    })
}
