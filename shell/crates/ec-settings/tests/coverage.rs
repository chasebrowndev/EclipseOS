// SPDX-License-Identifier: AGPL-3.0-only
//! The GUI-coverage ratchet, as a test.
//!
//! Walk the compositor's own schema table, subtract the collection nodes and
//! whatever `ci/gui-coverage-exceptions.txt` still admits to, and assert that
//! every key left over both maps to a widget and lands on exactly one page
//! (`place_for` is a function, so one key has one place by construction). This is the
//! local half of the `gui-coverage` job in `gate.yml`: that job stops the
//! exception list from growing, this test stops a key from quietly skipping
//! the list entirely.

use std::collections::HashSet;

use serde_json::{json, Value};

use ec_abyss_config::schema::{Ty, COLLECTIONS, TABLE};
use ec_settings::pane::place_for;
use ec_settings::schema::control_for;

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
        // Terminal-only types: abyss's table has none, so nothing reaches here.
        _ => ("string", Value::Null),
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
        assert!(place_for(key.path).is_some(), "{} belongs to no page", key.path);
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

/// The Terminal pages (Apotheosis T-10 §9, M26): every `apoth-config` key
/// either has a control on a Terminal page, or is a `Map` collection that the
/// M27 editors own and `COLLECTION_KEYS` names. A key the table grows with
/// neither fails here.
#[test]
fn every_apotheosis_key_has_a_control_or_is_a_collection() {
    use apoth_config::meta::META;
    use apoth_config::schema::TABLE as APOTH;
    use apoth_config::types::Ty as ATy;
    use ec_settings::terminal::{control_for, page_for, COLLECTION_KEYS};

    assert_eq!(APOTH.len(), META.len(), "TABLE and META are paired");
    for (key, meta) in APOTH.iter().zip(META.iter()) {
        assert_eq!(key.path, meta.path, "META is out of order at {}", key.path);
        let is_map = matches!(key.ty, ATy::Map { .. });
        if is_map {
            assert!(
                COLLECTION_KEYS.contains(&key.path),
                "{} is a collection with no M27 editor listed",
                key.path
            );
            continue;
        }
        assert!(control_for(&key.ty).is_some(), "{} has no control", key.path);
        assert!(
            page_for(meta.page).is_some(),
            "{} is on {:?}, a page the Terminal section does not have",
            key.path,
            meta.page
        );
    }
    for c in COLLECTION_KEYS {
        assert!(
            APOTH
                .iter()
                .any(|k| k.path == *c && matches!(k.ty, ATy::Map { .. })),
            "{c} is listed as a collection but is not a Map key"
        );
    }
}

/// The M27 editors (T-11 §3, §5, §6): every option of a bind, every node of a
/// rule, every part of a profile and every profilable key has a place in a
/// form, and what a form writes loads without a new error and reads back equal.
#[test]
fn every_bind_rule_and_profile_field_is_in_a_form() {
    use apoth_config::schema::TABLE as APOTH;
    use ec_settings::term_edit::{
        apply, gate, list_binds, list_profiles, list_rules, BindForm, MatchForm, Op, ProfileForm, RuleForm,
        Scope, BIND_OPTIONS, MATCH_FIELDS, PROFILE_NODES, RULE_NODES,
    };
    let user = std::env::temp_dir().join("ec-settings-coverage-m27.kdl");

    let bind = BindForm {
        unbind: false,
        chord: "ctrl+a>c".into(),
        actions: vec!["tab.new".into(), "pane.split direction=right".into()],
        repeat: false,
        mode: "alt-screen,!copy-mode".into(),
        release: true,
        global: true,
        performable: true,
    };
    // One option per name the loader accepts on `bind`.
    assert_eq!(
        BIND_OPTIONS,
        ["repeat", "mode", "release", "global", "performable"]
    );
    let text = apply("", &Op::BindAdd(Scope::Global, bind.clone())).unwrap();
    gate(&user, "", &text).unwrap();
    let got = &list_binds(&text, &Scope::Global)[0].form;
    assert_eq!(got.mode, bind.mode);
    assert!(!got.repeat && got.release && got.global && got.performable);
    let unbind = BindForm {
        unbind: true,
        chord: "ctrl+shift+c".into(),
        ..BindForm::default()
    };
    let text = apply(&text, &Op::BindAdd(Scope::Global, unbind)).unwrap();
    gate(&user, "", &text).unwrap();
    assert!(list_binds(&text, &Scope::Global)[1].form.unbind);

    assert_eq!(RULE_NODES, ["on", "where", "do", "allow-input", "allow-spawn"]);
    let rule = RuleForm {
        name: "all".into(),
        event: "output-idle".into(),
        when: "not focused and process:ssh".into(),
        actions: vec!["notify title=idle".into(), "input.send-text text=x".into()],
        allow_input: true,
        allow_spawn: true,
    };
    let text = apply("", &Op::RuleAdd(rule.clone())).unwrap();
    gate(&user, "", &text).unwrap();
    assert_eq!(list_rules(&text)[0].form.when, rule.when);
    assert!(list_rules(&text)[0].form.allow_input && list_rules(&text)[0].form.allow_spawn);
    // Every event the loader knows is selectable.
    for e in apoth_config::RULE_EVENTS.iter().chain(apoth_config::MEDIA_EVENTS) {
        let r = RuleForm {
            event: (*e).to_owned(),
            ..rule.clone()
        };
        assert!(r.problems().get("event").is_none(), "{e}");
    }

    assert_eq!(PROFILE_NODES, ["inherits", "sticky", "match"]);
    assert_eq!(MATCH_FIELDS, ["host", "job", "user", "path"]);
    let base = ProfileForm {
        name: "base".into(),
        ..ProfileForm::default()
    };
    let text = apply("", &Op::ProfileSet(None, base, vec![])).unwrap();
    let full = ProfileForm {
        name: "work".into(),
        inherits: "base".into(),
        sticky: true,
        matches: vec![MatchForm {
            host: "*.corp".into(),
            job: "ssh".into(),
            user: "me".into(),
            path: "~/work".into(),
        }],
        keys: vec![],
    };
    let text = apply(&text, &Op::ProfileSet(None, full, vec![])).unwrap();
    gate(&user, "", &text).unwrap();
    let work = &list_profiles(&text)[1].form;
    assert!(work.sticky && work.inherits == "base" && work.matches[0].path == "~/work");

    // Every profilable key can be typed into a profile, and writes back.
    let mut text = text;
    for k in APOTH.iter().filter(|k| k.profilable) {
        let v = k.default_value();
        let v = if matches!(v, apoth_config::types::Value::Map(_)) {
            apoth_config::types::Value::Map(vec![("A".to_owned(), vec!["1".to_owned()])])
        } else {
            v
        };
        let typed = ec_settings::term_edit::typed_of(k, &v);
        if typed.is_empty() {
            continue;
        }
        let form = ProfileForm {
            keys: vec![(k.path.to_owned(), typed.clone())],
            ..list_profiles(&text)[1].form.clone()
        };
        assert!(
            form.problems(&["base".to_owned()], &Default::default())
                .get("key:0")
                .is_none(),
            "{}: `{typed}` is refused",
            k.path
        );
        text = apply(&text, &Op::ProfileSet(Some(1), form, vec![])).unwrap();
        let back = list_profiles(&text);
        assert!(
            back[1].form.keys.iter().any(|(p, _)| p == k.path),
            "{} did not land",
            k.path
        );
        text = apply(
            &text,
            &Op::ProfileSet(
                Some(1),
                ProfileForm {
                    keys: vec![],
                    ..back[1].form.clone()
                },
                vec![k.path.to_owned()],
            ),
        )
        .unwrap();
    }
}
