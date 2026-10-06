// SPDX-License-Identifier: AGPL-3.0-only
//! The S-02 §7 golden decision suite, and the COMP-11 §3 latency budget.
//!
//! Every combination of request facts in [`cases`] is decided against the
//! shipped default policy twice: by the table `policyd` compiles, and by the
//! same table after the trip abyss makes (encode, sign, verify, decode). Both
//! must agree with each other and with the recorded expectation in
//! `tests/golden/default-policy.tsv`. A change to the default policy, the
//! compiler or the evaluator that moves any decision shows up as a diff of
//! that file, to be read and accepted on purpose:
//!
//! `ECLIPSE_GOLDEN_BLESS=1 cargo test -p ec-policyd --test golden`
//!
//! rewrites it.

use ec_policy_eval::check::{check, ProvenanceFacts, RequestCtx, Table, Trust};
use ec_policy_eval::scope::Class;
use ec_policy_eval::table as wire;
use ec_policyd::policy;

const GOLDEN: &str = "tests/golden/default-policy.tsv";

fn default_table() -> Table {
    let c = policy::compile(&[("default".into(), policy::DEFAULT_POLICY.into())]).expect("default policy");
    policy::table(c, 1)
}

/// The table as abyss holds it: through the wire and the signature.
fn as_abyss(t: &Table) -> Table {
    let key = ed25519_dalek::SigningKey::from_bytes(&[11; 32]);
    let bytes = wire::encode(t);
    let sig = wire::sign(&key, &bytes);
    wire::verify(&key.verifying_key(), &bytes, &sig).expect("verifies");
    wire::decode(&bytes).expect("decodes")
}

const CAPS: [&str; 11] = [
    "seat.key",
    "seat.text",
    "seat.pointer",
    "click",
    "seat.action",
    "capture.window",
    "capture.output",
    "workspace.human",
    "seat.focus.human",
    "policy.edit",
    "scene.list",
];
const CLASSES: [Class; 3] = [Class::Public, Class::Private, Class::Secret];
const APPS: [Option<&str>; 3] = [None, Some("foot"), Some("org.mozilla.firefox")];
const IRREVERSIBLE: [Option<&str>; 3] = [None, Some("communication.send"), Some("system.policy")];
/// (empty, untrusted link, channel source)
const PROVENANCE: [(bool, bool, bool); 4] = [
    (false, false, false),
    (true, false, false),
    (false, true, false),
    (false, false, true),
];
const VISION: [bool; 2] = [false, true];

struct Case {
    key: String,
    cap: &'static str,
    class: Class,
    app: Option<&'static str>,
    irreversible: Option<&'static str>,
    prov: (bool, bool, bool),
    vision: bool,
}

fn cases() -> Vec<Case> {
    let mut out = Vec::new();
    for cap in CAPS {
        for class in CLASSES {
            for app in APPS {
                for irreversible in IRREVERSIBLE {
                    for prov in PROVENANCE {
                        for vision in VISION {
                            let key = format!(
                                "{cap}\t{class:?}\t{}\t{}\t{}\t{}",
                                app.unwrap_or("-"),
                                irreversible.unwrap_or("-"),
                                match prov {
                                    (true, _, _) => "absent",
                                    (_, true, _) => "untrusted",
                                    (_, _, true) => "channel",
                                    _ => "plain",
                                },
                                if vision { "vision" } else { "tree" },
                            );
                            out.push(Case {
                                key,
                                cap,
                                class,
                                app,
                                irreversible,
                                prov,
                                vision,
                            });
                        }
                    }
                }
            }
        }
    }
    out
}

fn decide(t: &Table, c: &Case) -> String {
    let sources: &[&str] = if c.prov.2 { &["channel"] } else { &[] };
    let node = ec_policy_eval::check::NodeFacts {
        role: "button",
        name: "Send",
        source: if c.vision { "vision" } else { "tree" },
        confidence: 0.95,
    };
    let ctx = RequestCtx {
        principal: "agent:golden",
        profile: "operator",
        grants: &[],
        capability: c.cap,
        app_id: c.app,
        title: Some("Inbox"),
        class: c.class,
        app_trust: Trust::Standard,
        app_irreversible_capable: c.app == Some("org.mozilla.firefox"),
        node: Some(node),
        url: None,
        irreversible: c.irreversible,
        provenance: ProvenanceFacts {
            empty: c.prov.0,
            trusts: [c.prov.1, false, false, false],
            sources,
        },
    };
    let d = check(t, &ctx);
    format!("{:?}\t{}", d.outcome, d.rule)
}

#[test]
fn the_default_policy_decides_as_recorded_on_both_sides_of_the_wire() {
    let compiled = default_table();
    let decoded = as_abyss(&compiled);
    let mut lines = Vec::new();
    for c in cases() {
        let here = decide(&compiled, &c);
        let there = decide(&decoded, &c);
        assert_eq!(here, there, "policyd and abyss disagree on {}", c.key);
        lines.push(format!("{}\t{here}", c.key));
    }
    let got = lines.join("\n") + "\n";
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(GOLDEN);
    if std::env::var_os("ECLIPSE_GOLDEN_BLESS").is_some() {
        std::fs::write(&path, &got).unwrap();
        return;
    }
    let want = std::fs::read_to_string(&path).expect("golden file; bless it with ECLIPSE_GOLDEN_BLESS=1");
    if want != got {
        let first = want
            .lines()
            .zip(got.lines())
            .find(|(a, b)| a != b)
            .map(|(a, b)| format!("\n  recorded: {a}\n  now:      {b}"))
            .unwrap_or_default();
        panic!("default-policy decisions moved; re-read and bless on purpose{first}");
    }
}

/// COMP-11 §3: ≤ 50 µs p99 per check. Measured over the golden cases
/// against the decoded table; generous on purpose so a slow CI runner in a
/// debug build still holds it, and anything near it is a real regression.
#[test]
fn a_check_takes_under_fifty_microseconds_at_p99() {
    let t = as_abyss(&default_table());
    let cs = cases();
    let mut samples = Vec::with_capacity(cs.len() * 4);
    for _ in 0..4 {
        for c in &cs {
            let sources: &[&str] = if c.prov.2 { &["channel"] } else { &[] };
            let ctx = RequestCtx {
                principal: "agent:golden",
                profile: "operator",
                grants: &[],
                capability: c.cap,
                app_id: c.app,
                title: Some("Inbox"),
                class: c.class,
                app_trust: Trust::Standard,
                app_irreversible_capable: false,
                node: None,
                url: None,
                irreversible: c.irreversible,
                provenance: ProvenanceFacts {
                    empty: c.prov.0,
                    trusts: [c.prov.1, false, false, false],
                    sources,
                },
            };
            let start = std::time::Instant::now();
            std::hint::black_box(check(&t, std::hint::black_box(&ctx)));
            samples.push(start.elapsed());
        }
    }
    samples.sort();
    let p99 = samples[samples.len() * 99 / 100];
    assert!(p99 < std::time::Duration::from_micros(50), "p99 {p99:?}");
}
