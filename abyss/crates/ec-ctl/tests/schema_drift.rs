// SPDX-License-Identifier: AGPL-3.0-only
//! `migrate.rs` carries its own copy of the policy-owned key list so the CLI
//! does not link the compositor. That copy is only safe if it cannot drift, so
//! this test compares it against the schema it mirrors. `abyss` is a
//! dev-dependency: it is here and nowhere else.

use ec_abyss_config::schema::{self, Owner, RULE_ACTIONS, TABLE};

/// Keep in sync with `migrate::POLICY_KEYS`.
const POLICY_KEYS: &[&str] = &[
    "misc.scripted-input",
    "clipboard.data-control-allow",
    "capture.allow",
    "capture.redact-app-id",
    "capture.hide-layer",
];

/// Keep in sync with `migrate::POLICY_RULE_ACTIONS`.
const POLICY_RULE_ACTIONS: &[&str] = &[
    "sensitivity",
    "app-trust",
    "seat-compat",
    "no-agent",
    "irreversible-capable",
];

/// Keep in sync with `migrate::LEGACY_TRAY_BUILTINS`.
const LEGACY_TRAY_BUILTINS: &[&str] = &["network", "bluetooth", "battery", "volume"];

/// Keep in sync with `migrate::BAR_WIDGET_DEFAULT_ORDER`.
const BAR_WIDGET_DEFAULT_ORDER: &[&str] = &[
    "now-playing",
    "volume",
    "network",
    "bluetooth",
    "battery",
    "tray",
    "clock",
];

#[test]
fn migrate_knows_the_widget_ids() {
    assert_eq!(
        LEGACY_TRAY_BUILTINS,
        schema::LEGACY_TRAY_BUILTINS,
        "migrate::LEGACY_TRAY_BUILTINS has drifted from the schema; update it and this test"
    );
    assert_eq!(
        BAR_WIDGET_DEFAULT_ORDER,
        schema::BAR_WIDGET_DEFAULT_ORDER,
        "migrate::BAR_WIDGET_DEFAULT_ORDER has drifted from the schema; update it and this test"
    );
}

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

/// Keep in sync with `migrate::LEGACY_ANIMATION_MAP` and the constants next
/// to it.
#[test]
fn migrate_knows_the_animation_forms() {
    let map: &[(&str, &str, &str)] = &[
        ("windows", "window-move", "glide"),
        ("workspaces", "workspace-switch", "slide"),
        ("fade", "window-open", "fade"),
        ("border", "focus", "crossfade"),
    ];
    assert_eq!(
        map,
        schema::LEGACY_ANIMATION_MAP,
        "migrate::LEGACY_ANIMATION_MAP has drifted"
    );
    assert_eq!(
        &["linear", "ease-in", "ease-out", "ease-in-out"][..],
        schema::EASING_CURVES,
        "migrate::EASING_CURVES has drifted"
    );
    assert_eq!(
        &["linear", "ease-in", "ease-out", "ease-in-out", "spring", "bounce"][..],
        schema::ANIMATION_CURVES,
        "migrate::ANIMATION_CURVES has drifted"
    );
    assert_eq!(schema::ANIMATION_DEFAULT_MS, 150);
    assert_eq!(schema::ANIMATION_DEFAULT_CURVE, "ease-out");
    assert_eq!(schema::ANIMATION_MAX_MS, 10_000);
    let bar = TABLE
        .iter()
        .find(|k| k.path == "bar.motion.duration-ms")
        .expect("bar.motion.duration-ms");
    assert!(
        matches!(bar.ty, schema::Ty::Int { max: 2000, .. }),
        "migrate::BAR_MOTION_MAX_MS has drifted"
    );
}
