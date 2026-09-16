// SPDX-License-Identifier: AGPL-3.0-only
//! The GUI-coverage ratchet, as a test.
//!
//! Walk the compositor's own schema table, subtract the collection nodes and
//! whatever `ci/gui-coverage-exceptions.txt` still admits to, and assert that
//! every key left over both maps to a widget and lands on a pane. This is the
//! local half of the `gui-coverage` job in `gate.yml`: that job stops the
//! exception list from growing, this test stops a key from quietly skipping
//! the list entirely.

use std::collections::HashSet;

use serde_json::{json, Value};

use abyss::config::schema::{Ty, COLLECTIONS, TABLE};
use eclipse_settings::pane::pane_for;
use eclipse_settings::schema::control_for;

/// The wire shape `config_rpc::ty_json` produces for a `Ty`. Written out once
/// here so the test exercises the exact strings the app will see, rather than
/// the app's own idea of them.
fn ty_json(ty: &Ty) -> (&'static str, Value) {
    match ty {
        Ty::Bool => ("bool", Value::Null),
        Ty::Int { min, max } => ("int", json!({ "min": min, "max": max })),
        Ty::Float { min, max } => ("float", json!({ "min": min, "max": max })),
        Ty::Str => ("string", Value::Null),
        Ty::Enum(values) => ("enum", json!({ "values": values })),
        Ty::Color => ("color", Value::Null),
        Ty::StrList => ("string-list", Value::Null),
    }
}

fn exceptions() -> HashSet<String> {
    let path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../ci/gui-coverage-exceptions.txt"
    );
    let text = std::fs::read_to_string(path).expect("the exception file is part of the repo");
    text.lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .map(str::to_owned)
        .collect()
}

#[test]
fn every_schema_key_has_a_control_and_a_pane() {
    let skip = exceptions();
    let collections: HashSet<&str> = COLLECTIONS.iter().map(|c| c.node).collect();

    for key in TABLE {
        let node = key.path.split('.').next().unwrap_or(key.path);
        if collections.contains(node) || skip.contains(node) || skip.contains(key.path) {
            continue;
        }

        let (ty, constraints) = ty_json(&key.ty);
        assert!(
            control_for(ty, &constraints).is_some(),
            "{} is a {ty} the settings app cannot render",
            key.path
        );
        assert!(pane_for(key.path).is_some(), "{} belongs to no pane", key.path);
    }
}

#[test]
fn the_exception_file_only_names_things_that_exist() {
    let collections: HashSet<&str> = COLLECTIONS.iter().map(|c| c.node).collect();
    for entry in exceptions() {
        let known = collections.contains(entry.as_str())
            || TABLE.iter().any(|k| k.path == entry)
            || TABLE
                .iter()
                .any(|k| k.path.split('.').next() == Some(entry.as_str()));
        assert!(known, "{entry} is excused from a schema it is not in");
    }
}
