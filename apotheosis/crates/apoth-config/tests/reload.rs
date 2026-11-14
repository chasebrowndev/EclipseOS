// SPDX-License-Identifier: AGPL-3.0-only
use apoth_config::fs::{Fs, MemFs, SelfWrites};
use apoth_config::load::LoadInput;
use apoth_config::reload::{Outcome, Reloader, Subsystem, subsystem_of, watch_dirs};
use apoth_config::types::{Reload, Value};
use std::path::{Path, PathBuf};

const P: &str = "/home/u/apotheosis.kdl";

fn input() -> LoadInput {
    LoadInput {
        user: Some(PathBuf::from(P)),
        ..Default::default()
    }
}

fn applied(o: Outcome) -> apoth_config::Applied {
    match o {
        Outcome::Applied(a) => *a,
        other => panic!("expected Applied, got {other:?}"),
    }
}

#[test]
fn every_key_has_a_deliberate_subsystem() {
    for k in apoth_config::TABLE {
        let top = k.path.split('.').next().unwrap();
        assert!(
            apoth_config::types::TOP_LEVEL_NODES.contains(&top),
            "{}",
            k.path
        );
        // Unknown nodes fall to Term; make sure only listed Term nodes do.
        if subsystem_of(k.path) == Subsystem::Term {
            assert!(
                [
                    "scrollback",
                    "blocks",
                    "bell",
                    "notifications",
                    "progress",
                    "shell-integration",
                    "hints",
                    "links"
                ]
                .contains(&top),
                "{} fell through to Term",
                k.path
            );
        }
    }
}

#[test]
fn diff_names_changed_keys_and_subsystems() {
    let fs = MemFs::new().with(P, "version 1\nfont { size 12 }\n");
    let mut r = Reloader::start(&fs, &input());
    fs.set(
        P,
        "version 1\nfont { size 14 }\nrender { max-fps 60 }\nterminal { term \"xterm-256color\" }\n",
    );
    let a = applied(r.reload(&fs, &input()));
    assert!(a.touches(Subsystem::Font));
    assert!(a.touches(Subsystem::Render));
    assert!(a.touches(Subsystem::Pty));
    assert!(!a.touches(Subsystem::Keys));
    let sizes: Vec<_> = a.keys_in(Subsystem::Font).collect();
    assert_eq!(sizes.len(), 1);
    assert_eq!(sizes[0].path, "font.size");
    assert_eq!(sizes[0].new, Value::Float(14.0));
    // terminal.term is NewPane-class: deferred, no restart.
    assert!(
        a.deferred
            .iter()
            .any(|(p, c)| *p == "terminal.term" && *c == Reload::NewPane)
    );
    assert!(!a.restart_required);
    assert_eq!(r.config().float("font.size"), 14.0);
}

#[test]
fn collection_changes_name_their_subsystem() {
    let fs = MemFs::new().with(P, "version 1\n");
    let mut r = Reloader::start(&fs, &input());
    fs.set(
        P,
        "version 1\nbinds {\n  bind \"ctrl+shift+t\" \"tab.new\"\n}\n",
    );
    let o = r.reload(&fs, &input());
    // Whatever the bind grammar accepts, an applied diff must flag Keys; a rejected bind
    // must be reported with a position instead.
    match o {
        Outcome::Applied(a) => assert!(a.touches(Subsystem::Keys)),
        _ => assert!(r.has_errors()),
    }
}

#[test]
fn unchanged_reload_is_unchanged() {
    let fs = MemFs::new().with(P, "version 1\nfont { size 12 }\n");
    let mut r = Reloader::start(&fs, &input());
    assert_eq!(r.reload(&fs, &input()), Outcome::Unchanged);
    // A comment-only edit changes no value.
    fs.set(P, "// hi\nversion 1\nfont { size 12 }\n");
    assert_eq!(r.reload(&fs, &input()), Outcome::Unchanged);
}

#[test]
fn unparsable_file_keeps_last_good_and_reports_position() {
    let fs = MemFs::new().with(P, "version 1\nfont { size 14 }\n");
    let mut r = Reloader::start(&fs, &input());
    assert_eq!(r.config().float("font.size"), 14.0);
    fs.set(P, "version 1\nfont { size 20 \n");
    assert_eq!(r.reload(&fs, &input()), Outcome::Rejected);
    assert_eq!(r.config().float("font.size"), 14.0);
    assert!(r.has_errors());
    let e = r.errors().next().unwrap();
    assert!(e.line >= 1 && e.column >= 1, "{e:?}");
    assert_eq!(e.file.as_deref(), Some(Path::new(P)));
    assert!(r.error_summary().unwrap().contains(P));
    // Fixing the file clears the bar and applies.
    fs.set(P, "version 1\nfont { size 16 }\n");
    let a = applied(r.reload(&fs, &input()));
    assert!(a.touches(Subsystem::Font));
    assert!(!r.has_errors());
    assert_eq!(r.config().float("font.size"), 16.0);
}

#[test]
fn bad_scalar_keeps_previous_value_with_line_and_key() {
    let fs = MemFs::new().with(P, "version 1\nfont { size 14 }\n");
    let mut r = Reloader::start(&fs, &input());
    fs.set(P, "version 1\nfont {\n    size \"huge\"\n}\n");
    let _ = r.reload(&fs, &input());
    assert_eq!(r.config().float("font.size"), 14.0);
    let e = r.errors().next().expect("an error");
    assert_eq!(e.line, 3, "{e:?}");
    assert_eq!(e.key.as_deref(), Some("font.size"));
}

#[test]
fn first_start_with_broken_file_runs_on_defaults_with_errors() {
    let fs = MemFs::new().with(P, "font {{{");
    let r = Reloader::start(&fs, &input());
    assert!(r.has_errors());
    assert_eq!(r.config().float("font.size"), 12.0);
}

#[test]
fn unknown_key_suggests_near_miss() {
    let fs = MemFs::new().with(P, "version 1\nfont { sise 14 }\n");
    let r = Reloader::start(&fs, &input());
    let e = r.errors().next().expect("an error");
    assert_eq!(e.line, 2);
    assert_eq!(e.suggestion.as_deref(), Some("size"), "{e:?}");
}

#[test]
fn own_writes_are_suppressed_by_content_hash() {
    let fs = MemFs::new().with(P, "version 1\n");
    let mut own = SelfWrites::new();
    own.record(Path::new(P), "version 1\n");
    assert!(!Reloader::is_foreign_change(&fs, Path::new(P), &own));
    fs.set(P, "version 1\nfont { size 13 }\n");
    assert!(Reloader::is_foreign_change(&fs, Path::new(P), &own));
    // A missing file is a change the reload must see.
    assert!(Reloader::is_foreign_change(
        &fs,
        Path::new("/nope.kdl"),
        &own
    ));
}

#[test]
fn watch_dirs_are_parents_of_main_file_and_includes() {
    let fs = MemFs::new()
        .with(P, "version 1\ninclude \"extra/a.kdl\"\n")
        .with("/home/u/extra/a.kdl", "font { size 13 }\n");
    let r = Reloader::start(&fs, &input());
    let dirs = r.watch_dirs(&fs);
    assert!(dirs.contains(&PathBuf::from("/home/u")), "{dirs:?}");
    assert!(dirs.contains(&PathBuf::from("/home/u/extra")), "{dirs:?}");
    // Optional include that does not exist yet still gets its directory watched.
    let d = watch_dirs(&fs, &[PathBuf::from("/home/u/later/x.kdl")]);
    assert_eq!(d, vec![PathBuf::from("/home/u/later")]);
    assert!(fs.exists(Path::new(P)));
}

#[test]
fn runtime_override_survives_a_file_reload_and_reset_restores_the_file() {
    use apoth_config::model::Layer;
    let fs = MemFs::new().with(P, "version 1\nfont { size 12 }\n");
    let mut r = Reloader::start(&fs, &input());
    let c = r.set_runtime("font.size", "20").unwrap();
    assert_eq!(c.previous, None);
    let a = c.applied.expect("font.size changed");
    assert!(a.diff.keys.iter().any(|k| k.path == "font.size"));
    assert_eq!(r.config().float("font.size"), 20.0);
    assert_eq!(
        r.config().origin("font.size").unwrap().layer,
        Layer::Runtime
    );

    // The file changes under the override: the base follows, the override stays on top.
    fs.set(P, "version 1\nfont { size 13 }\ncursor { style \"bar\" }\n");
    let a = applied(r.reload(&fs, &input()));
    assert!(a.diff.keys.iter().any(|k| k.path == "cursor.style"));
    assert!(!a.diff.keys.iter().any(|k| k.path == "font.size"));
    assert_eq!(r.config().float("font.size"), 20.0);
    assert_eq!(r.base().float("font.size"), 13.0);
    assert_eq!(r.config().str("cursor.style"), Some("bar"));

    // Setting again reports the previous override; the same value changes nothing.
    let c = r.set_runtime("font.size", "21").unwrap();
    assert_eq!(c.previous, Some(Value::Float(20.0)));
    assert!(r.set_runtime("font.size", "21").unwrap().applied.is_none());

    let c = r.reset_runtime("font.size").unwrap();
    assert_eq!(c.previous, Some(Value::Float(21.0)));
    assert!(c.applied.is_some());
    assert_eq!(r.config().float("font.size"), 13.0);
    assert_eq!(r.config().origin("font.size").unwrap().layer, Layer::User);
    // Resetting a key with no override is not an error and changes nothing.
    let again = r.reset_runtime("font.size").unwrap();
    assert_eq!((again.previous, again.applied.is_none()), (None, true));
}

#[test]
fn runtime_override_refuses_security_grant_unknown_and_bad_values() {
    let fs = MemFs::new().with(P, "version 1\n");
    let mut r = Reloader::start(&fs, &input());
    for (k, v) in [
        ("bell.command", "true"),
        ("socket.key-stream", "true"),
        ("font.sise", "12"),
        ("font.size", "9999"),
        ("font.size", "huge"),
    ] {
        assert!(r.set_runtime(k, v).is_err(), "{k}={v}");
    }
    assert!(r.reset_runtime("bell.command").is_err());
    assert!(
        r.runtime()
            .entries_at(apoth_config::overrides::ScopeKey {
                scope: apoth_config::types::Scope::Global,
                id: 0
            })
            .is_empty()
    );
}

#[test]
fn a_reload_on_a_copy_adopts_the_live_overrides() {
    let fs = MemFs::new().with(P, "version 1\n");
    let mut live = Reloader::start(&fs, &input());
    let mut copy = live.clone();
    live.set_runtime("font.size", "18").unwrap();
    fs.set(P, "version 1\ncursor { style \"bar\" }\n");
    copy.reload(&fs, &input());
    copy.adopt_runtime(live.runtime());
    assert_eq!(copy.config().float("font.size"), 18.0);
    assert_eq!(copy.config().str("cursor.style"), Some("bar"));
}

#[test]
fn value_arg_text_round_trips_through_the_parser() {
    use apoth_config::overrides::{parse_override_value, value_arg_text};
    for (k, text) in [
        ("font.size", "14.5"),
        ("cursor.style", "bar"),
        ("font.family", "JetBrains Mono"),
        ("window.padding-x", "8"),
        ("window.padding-balance", "true"),
    ] {
        let key = apoth_config::schema::key(k).unwrap();
        let v = parse_override_value(key, text).unwrap();
        let back = value_arg_text(key, &v).unwrap();
        assert_eq!(parse_override_value(key, &back).unwrap(), v, "{k}: {back}");
    }
}
