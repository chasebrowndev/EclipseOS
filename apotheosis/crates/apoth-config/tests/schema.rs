// SPDX-License-Identifier: AGPL-3.0-only
use apoth_config::meta::META;
use apoth_config::schema::{self, TABLE};
use apoth_config::types::{OverrideClass, Reload, TOP_LEVEL_NODES, Ty};
use std::collections::HashSet;

#[test]
fn paths_are_unique_and_meta_is_parallel() {
    let mut seen = HashSet::new();
    for k in TABLE {
        assert!(seen.insert(k.path), "duplicate key {}", k.path);
    }
    assert_eq!(TABLE.len(), META.len());
    for (k, m) in TABLE.iter().zip(META.iter()) {
        assert_eq!(k.path, m.path, "META out of step with TABLE");
        assert!(!m.label.is_empty(), "{} has no label", k.path);
        assert!(!k.doc.is_empty(), "{} has no doc", k.path);
        assert!(
            !m.page.is_empty() && !m.group.is_empty(),
            "{} has no page",
            k.path
        );
    }
}

#[test]
fn every_key_has_an_override_class_and_the_forced_ones_hold() {
    for k in TABLE {
        if k.path.starts_with("socket.") || k.path == "socket" {
            assert_eq!(k.class, OverrideClass::Grant, "{}", k.path);
        }
        if matches!(k.ty, Ty::Argv | Ty::Action | Ty::Match | Ty::Map { .. }) {
            assert!(
                matches!(k.class, OverrideClass::Security | OverrideClass::Grant),
                "{} must be security or stricter",
                k.path
            );
        }
        assert_eq!(schema::class_of(k.path), k.class);
    }
    assert_eq!(schema::class_of("no.such.key"), OverrideClass::Grant);
}

#[test]
fn live_reload_dominates() {
    let live = TABLE.iter().filter(|k| k.reload == Reload::Live).count();
    assert!(
        live * 100 >= TABLE.len() * 95,
        "{live}/{} live",
        TABLE.len()
    );
}

#[test]
fn top_level_nodes_cover_every_key_root() {
    for k in TABLE {
        let root = k.path.split('.').next().unwrap();
        assert!(
            TOP_LEVEL_NODES.contains(&root),
            "{} root `{root}` not a top-level node",
            k.path
        );
    }
}

#[test]
fn lookup_helpers_agree() {
    for (i, k) in TABLE.iter().enumerate() {
        assert_eq!(schema::index_of(k.path), Some(i));
    }
    assert!(schema::is_container("font"));
    assert!(schema::children_of("").contains(&"font"));
}

#[test]
fn security_and_grant_keys_gate_runtime_and_persist() {
    use apoth_config::overrides::{check_persist, check_runtime_override};
    use apoth_config::types::Scope;
    for k in TABLE {
        match k.class {
            OverrideClass::Security => {
                assert!(
                    check_runtime_override(k.path, Scope::Global).is_err(),
                    "{} runtime",
                    k.path
                );
                assert_eq!(check_persist(k.path), Ok("config-write"), "{}", k.path);
            }
            OverrideClass::Grant => {
                assert!(check_runtime_override(k.path, Scope::Global).is_err());
                assert!(check_persist(k.path).is_err(), "{} persist", k.path);
            }
            _ => {}
        }
    }
}

#[test]
fn named_roots_are_security_and_socket_is_grant() {
    let roots = [
        "security",
        "clipboard",
        "paste",
        "links",
        "shell",
        "include",
    ];
    for k in TABLE {
        let root = k.path.split('.').next().unwrap();
        if roots.contains(&root)
            || k.path == "graphics.file-transmission"
            || k.path == "media.allow-paths"
        {
            assert_eq!(k.class, OverrideClass::Security, "{}", k.path);
        }
        if root == "socket" {
            assert_eq!(k.class, OverrideClass::Grant, "{}", k.path);
        }
    }
}
