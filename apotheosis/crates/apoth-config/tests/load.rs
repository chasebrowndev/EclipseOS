// SPDX-License-Identifier: AGPL-3.0-only
use apoth_config::edit::Editor;
use apoth_config::fs::MemFs;
use apoth_config::load::{LoadInput, load};
use apoth_config::schema::TABLE;
use apoth_config::types::Value;
use std::path::PathBuf;

fn run(files: &[(&str, &str)]) -> apoth_config::LoadResult {
    let mut fs = MemFs::new();
    for (p, t) in files {
        fs = fs.with(p, t);
    }
    let input = LoadInput {
        user: Some(PathBuf::from("/home/u/apotheosis.kdl")),
        ..Default::default()
    };
    load(&fs, &input, None)
}

fn user(text: &str) -> apoth_config::LoadResult {
    run(&[("/home/u/apotheosis.kdl", text)])
}

#[test]
fn empty_file_is_defaults() {
    let r = user("");
    assert!(r.is_valid(), "{}", r.render());
    assert!(apoth_config::diff::diff_from_defaults(&r.config).is_empty());
}

/// A representative non-default value for a key that has no written default.
fn sample(ty: &apoth_config::types::Ty) -> Vec<Value> {
    use apoth_config::types::Ty;
    match ty {
        Ty::Font => vec![Value::Str("Test Mono".into())],
        Ty::Color => vec![Value::Color([1, 2, 3, 255]), Value::Color([9, 8, 7, 128])],
        Ty::Palette16 => vec![Value::List(
            (0..16)
                .map(|i| format!("#{:02x}{:02x}{:02x}", i, i * 2, i * 3))
                .collect(),
        )],
        Ty::ThemeRef => vec![
            Value::Str("system".into()),
            Value::Str("Gruvbox".into()),
            Value::Str("light=Day dark=Night".into()),
        ],
        Ty::Float { min, max } => vec![Value::Float((min + max) / 2.0)],
        Ty::Enum(set) => set.iter().map(|s| Value::Str((*s).into())).collect(),
        Ty::Path => vec![Value::Str("/tmp/x".into())],
        Ty::Str => vec![Value::Str("abc".into())],
        other => panic!("no sample for null-default type {other:?}"),
    }
}

#[test]
fn every_default_validates_and_round_trips() {
    use apoth_config::types::Dv;
    let mut ed = Editor::parse("").unwrap();
    let mut written = Vec::new();
    let mut null_keys = Vec::new();
    for k in TABLE {
        let v = k.default_value();
        if matches!(v, Value::Null) {
            null_keys.push(k);
            continue;
        }
        ed.set(k.path, &v)
            .unwrap_or_else(|e| panic!("{}: {e}", k.path));
        written.push((k.path, v));
    }
    let text = ed.text();
    let r = user(&text);
    assert!(r.is_valid(), "{}", r.render());
    for (path, v) in written {
        assert_eq!(r.config.get(path), Some(&v), "round trip of {path}");
    }
    // The unset-by-default and computed-default keys round-trip with a sample value of
    // their type (every variant of an enum, both colour forms, both theme forms).
    assert!(!null_keys.is_empty());
    assert!(
        null_keys
            .iter()
            .any(|k| matches!(k.default, Dv::Computed { .. })),
        "computed keys are covered"
    );
    let mut covered = 0;
    for k in null_keys {
        for v in sample(&k.ty) {
            let mut ed = Editor::parse("").unwrap();
            ed.set(k.path, &v)
                .unwrap_or_else(|e| panic!("{}: {e}", k.path));
            let r = user(&ed.text());
            assert!(r.is_valid(), "{}: {}", k.path, r.render());
            assert_eq!(r.config.get(k.path), Some(&v), "round trip of {}", k.path);
            // And the editor reads back what it wrote, then unsets it.
            assert_eq!(ed.get(k.path).unwrap(), Some(v));
            assert!(ed.unset(k.path).unwrap());
            covered += 1;
        }
    }
    assert!(covered >= 20, "{covered}");
}

#[test]
fn errors_carry_file_line_column_and_suggestion() {
    let r = user("font {\n    sizee 12\n}\n");
    let d = r
        .diagnostics
        .iter()
        .find(|d| d.is_error())
        .expect("an error");
    assert_eq!(
        d.file.as_deref(),
        Some(std::path::Path::new("/home/u/apotheosis.kdl"))
    );
    assert_eq!(d.line, 2);
    assert!(d.column >= 1);
    assert!(d.render().contains(":2:"));
}

#[test]
fn all_diagnostics_are_collected() {
    let r = user("bogus-one 1\nbogus-two 2\nfont { sizee 1 }\n");
    assert!(r.error_count() >= 3, "{}", r.render());
}

#[test]
fn bad_scalar_keeps_default() {
    let r = user("font { size \"huge\" }\n");
    assert!(!r.is_valid());
    assert_eq!(
        r.config.get("font.size"),
        apoth_config::Config::defaults().get("font.size")
    );
}

#[test]
fn include_cycle_is_reported() {
    let r = run(&[
        ("/home/u/apotheosis.kdl", "include \"a.kdl\"\n"),
        ("/home/u/a.kdl", "include \"b.kdl\"\n"),
        ("/home/u/b.kdl", "include \"a.kdl\"\n"),
    ]);
    assert!(
        r.diagnostics.iter().any(|d| d.message.contains("cycle")),
        "{}",
        r.render()
    );
}

#[test]
fn include_depth_is_capped_at_eight() {
    let mut files: Vec<(String, String)> = vec![(
        "/home/u/apotheosis.kdl".into(),
        "include \"f1.kdl\"\n".into(),
    )];
    for i in 1..=10 {
        files.push((
            format!("/home/u/f{i}.kdl"),
            format!("include \"f{}.kdl\"\n", i + 1),
        ));
    }
    files.push(("/home/u/f11.kdl".into(), String::new()));
    let refs: Vec<(&str, &str)> = files
        .iter()
        .map(|(a, b)| (a.as_str(), b.as_str()))
        .collect();
    let r = run(&refs);
    assert!(
        r.diagnostics.iter().any(|d| d.message.contains("depth")),
        "{}",
        r.render()
    );
}

#[test]
fn later_layers_win_and_origin_is_recorded() {
    let fs = MemFs::new()
        .with("/etc/apotheosis.kdl", "font { size 10 }\n")
        .with("/home/u/apotheosis.kdl", "font { size 14 }\n");
    let input = LoadInput {
        system: Some(PathBuf::from("/etc/apotheosis.kdl")),
        user: Some(PathBuf::from("/home/u/apotheosis.kdl")),
        ..Default::default()
    };
    let r = load(&fs, &input, None);
    assert!(r.is_valid(), "{}", r.render());
    assert_eq!(
        r.config.get("font.size").and_then(Value::as_float),
        Some(14.0)
    );
    let o = r.config.origin("font.size").unwrap();
    assert_eq!(o.layer, apoth_config::model::Layer::User);
    assert_eq!(o.line, 1);
}

#[test]
fn syntax_error_marks_rejected() {
    let r = user("font {\n");
    assert!(r.rejected);
    assert!(r.error_count() >= 1);
}

#[test]
fn newer_version_is_refused() {
    let r = user("version 999\n");
    assert!(!r.is_valid());
}

#[test]
fn chord_and_argv_validation_matches_apoth_action() {
    use apoth_config::chord::parse_chord_seq;
    use apoth_config::lang::validate_argv;
    for bad in [
        "hyper+a",
        "meta+a",
        "windows+a",
        "ctrl+shift",
        "a>a>a>a>a>a>a>a>a",
    ] {
        assert!(parse_chord_seq(bad).is_err(), "{bad}");
        assert!(apoth_action::key::parse_sequence(bad).is_err(), "{bad}");
    }
    for ok in ["cmd+a", "option+a", "ctrl+shift+t"] {
        assert!(parse_chord_seq(ok).is_ok(), "{ok}");
    }
    let s = |v: &[&str]| v.iter().map(|x| x.to_string()).collect::<Vec<_>>();
    assert!(validate_argv(&s(&["sh", "-c", "echo {selection}x"])).is_err());
    assert!(validate_argv(&s(&["sh", "{selection}"])).is_ok());
    assert!(validate_argv(&s(&["a{{b"])).is_ok());
    assert!(validate_argv(&["x".repeat(20_000)]).is_err());
}

#[test]
fn failed_lower_layer_does_not_discard_valid_higher_layer() {
    let fs = MemFs::new()
        .with("/etc/apotheosis.kdl", "font { size \"x\" }\n")
        .with("/home/u/apotheosis.kdl", "font { size 14 }\n");
    let input = LoadInput {
        system: Some(PathBuf::from("/etc/apotheosis.kdl")),
        user: Some(PathBuf::from("/home/u/apotheosis.kdl")),
        ..Default::default()
    };
    let prev = load(&MemFs::new(), &LoadInput::default(), None).config;
    let r = load(&fs, &input, Some(&prev));
    assert_eq!(
        r.config.get("font.size").and_then(Value::as_float),
        Some(14.0)
    );
}

#[test]
fn bad_profile_value_does_not_clobber_global_key() {
    let prev = user("font { size 10 }\n").config;
    let r = run_prev(
        "font { size 14 }\nprofile \"x\" { font { size 999 } }\n",
        &prev,
    );
    assert_eq!(
        r.config.get("font.size").and_then(Value::as_float),
        Some(14.0)
    );
}

fn run_prev(text: &str, prev: &apoth_config::Config) -> apoth_config::LoadResult {
    let fs = MemFs::new().with("/home/u/apotheosis.kdl", text);
    let input = LoadInput {
        user: Some(PathBuf::from("/home/u/apotheosis.kdl")),
        ..Default::default()
    };
    load(&fs, &input, Some(prev))
}

#[test]
fn include_fanout_is_budgeted_and_dir_globs_are_diagnosed() {
    let line = "include \"b.kdl\"\n";
    let fs = MemFs::new()
        .with("/home/u/apotheosis.kdl", &line.repeat(20))
        .with("/home/u/b.kdl", &line.repeat(20));
    let input = LoadInput {
        user: Some(PathBuf::from("/home/u/apotheosis.kdl")),
        ..Default::default()
    };
    let r = load(&fs, &input, None);
    assert!(r.files.len() <= apoth_config::load::MAX_FILE_LOADS + 64);
    assert!(!r.is_valid());
    let r = user("include \"conf.d/*/x.kdl\"\n");
    assert!(
        r.render().contains("final path component"),
        "{}",
        r.render()
    );
}
