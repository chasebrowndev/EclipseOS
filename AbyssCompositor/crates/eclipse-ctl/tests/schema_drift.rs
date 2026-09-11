// SPDX-License-Identifier: AGPL-3.0-only
//! `migrate.rs` carries its own copy of the policy-owned key list so the CLI
//! does not link the compositor. That copy is only safe if it cannot drift, so
//! this test compares it against the schema it mirrors. `abyss` is a
//! dev-dependency: it is here and nowhere else.

use abyss::config::schema::{Owner, RULE_ACTIONS, TABLE};

/// Keep in sync with `migrate::POLICY_KEYS`.
const POLICY_KEYS: &[&str] = &[
    "misc.scripted-input",
    "clipboard.data-control-allow",
    "capture.allow",
    "capture.redact-app-id",
];

/// Keep in sync with `migrate::POLICY_RULE_ACTIONS`.
const POLICY_RULE_ACTIONS: &[&str] = &["sensitivity", "app-trust", "seat-compat", "no-agent"];

#[test]
fn migrate_knows_every_policy_owned_key() {
    let mut from_schema: Vec<&str> = TABLE
        .iter()
        .filter(|k| matches!(k.owner, Owner::Policy))
        .map(|k| k.path)
        .collect();
    from_schema.sort_unstable();
    let mut mirrored = POLICY_KEYS.to_vec();
    mirrored.sort_unstable();
    assert_eq!(
        from_schema, mirrored,
        "migrate::POLICY_KEYS has drifted from the schema; update it and this test"
    );
}

#[test]
fn migrate_knows_every_policy_owned_rule_action() {
    let mut from_schema: Vec<&str> = RULE_ACTIONS
        .iter()
        .filter(|(_, o)| matches!(o, Owner::Policy))
        .map(|(a, _)| *a)
        .collect();
    from_schema.sort_unstable();
    let mut mirrored = POLICY_RULE_ACTIONS.to_vec();
    mirrored.sort_unstable();
    assert_eq!(
        from_schema, mirrored,
        "migrate::POLICY_RULE_ACTIONS has drifted from the schema; update it and this test"
    );
}
