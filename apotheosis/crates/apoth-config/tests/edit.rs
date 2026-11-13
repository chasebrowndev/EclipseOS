// SPDX-License-Identifier: AGPL-3.0-only
use apoth_config::diff::{Collection, diff};
use apoth_config::edit::{Editor, NodeSel, apply_write};
use apoth_config::fs::{Fs, MemFs, SelfWrites};
use apoth_config::load::{LoadInput, load};
use apoth_config::overrides::{RuntimeOverrides, ScopeKey, check_persist, check_runtime_override};
use apoth_config::types::{OverrideClass, Reload, Scope, Value};
use std::path::{Path, PathBuf};

const SRC: &str =
    "// header\nversion 1\n\n// my font\nfont {\n    size 12 // keep\n}\nunknown-future-node 1 2\n";

#[test]
fn edit_preserves_comments_order_and_unknown_nodes() {
    let mut ed = Editor::parse(SRC).unwrap();
    ed.set("font.size", &Value::Float(14.0)).unwrap();
    let t = ed.text();
    assert!(
        t.contains("// header")
            && t.contains("// my font")
            && t.contains("unknown-future-node 1 2")
    );
    assert!(t.find("font").unwrap() < t.find("unknown-future-node").unwrap());
    assert_eq!(ed.get("font.size").unwrap(), Some(Value::Float(14.0)));
}

#[test]
fn untouched_edit_is_byte_identical() {
    let ed = Editor::parse(SRC).unwrap();
    assert_eq!(ed.text(), SRC);
}

#[test]
fn set_then_unset_removes_node_and_empty_parent() {
    let mut ed = Editor::parse("version 1\n").unwrap();
    ed.set("font.size", &Value::Float(13.0)).unwrap();
    assert!(ed.text().contains("font"));
    assert!(ed.unset("font.size").unwrap());
    assert!(!ed.text().contains("font"));
    assert!(!ed.unset("font.size").unwrap());
}

#[test]
fn unknown_key_and_null_are_refused() {
    let mut ed = Editor::parse("").unwrap();
    assert!(ed.set("nope.nope", &Value::Bool(true)).is_err());
    assert!(ed.set("font.size", &Value::Null).is_err());
    assert!(ed.set("font.size", &Value::Bool(true)).is_err());
}

#[test]
fn profile_scope_is_created() {
    let mut ed = Editor::parse("").unwrap();
    ed.set_in(
        &[NodeSel::profile("work")],
        "font.size",
        &Value::Float(11.0),
    )
    .unwrap();
    let t = ed.text();
    assert!(t.contains("profile work"), "{t}");
    let fs = MemFs::new().with("/u.kdl", &t);
    let r = load(
        &fs,
        &LoadInput {
            user: Some(PathBuf::from("/u.kdl")),
            ..Default::default()
        },
        None,
    );
    assert!(r.is_valid(), "{}", r.render());
    assert_eq!(r.config.profiles.len(), 1);
}

#[test]
fn apply_write_is_atomic_records_self_write_and_loads_back() {
    let fs = MemFs::new().with("/u.kdl", SRC);
    let input = LoadInput {
        user: Some(PathBuf::from("/u.kdl")),
        ..Default::default()
    };
    let cfg = load(&fs, &input, None).config;
    let mut sw = SelfWrites::new();
    let out = apply_write(
        &fs,
        &cfg,
        Path::new("/u.kdl"),
        &[],
        "font.size",
        Some(&Value::Float(15.0)),
        Some(&mut sw),
    )
    .unwrap();
    assert_eq!(out.as_deref(), Some(Path::new("/u.kdl")));
    let text = fs.read_to_string(Path::new("/u.kdl")).unwrap();
    assert!(sw.is_self_write(Path::new("/u.kdl"), &text));
    let again = apply_write(
        &fs,
        &cfg,
        Path::new("/u.kdl"),
        &[],
        "font.size",
        Some(&Value::Float(15.0)),
        None,
    )
    .unwrap();
    assert!(again.is_none());
    // SRC deliberately carries one unknown node (closed node set => one error); nothing else.
    let r = load(&fs, &input, None);
    assert_eq!(r.error_count(), 1, "{}", r.render());
    assert!(r.render().contains("unknown-future-node"), "{}", r.render());
}

#[test]
fn list_add_remove() {
    let mut ed = Editor::parse("").unwrap();
    ed.add("socket.interpreters", "python3").unwrap();
    ed.add("socket.interpreters", "python3").unwrap();
    assert_eq!(
        ed.get("socket.interpreters").unwrap(),
        Some(Value::List(vec!["python3".into()]))
    );
    assert!(ed.remove("socket.interpreters", "python3").unwrap());
    assert_eq!(ed.get("socket.interpreters").unwrap(), None);
}

#[test]
fn override_classes_gate_runtime_and_persist() {
    let r = check_runtime_override("socket.interpreters", Scope::Global).unwrap_err();
    assert_eq!(r.class, Some(OverrideClass::Grant));
    assert!(check_runtime_override("no.such", Scope::Global).is_err());
    assert!(check_persist("socket.interpreters").is_err());
    let mut ov = RuntimeOverrides::new();
    let at = ScopeKey {
        scope: Scope::Global,
        id: 0,
    };
    assert_eq!(ov.set_text(at, "font.size", "16"), Ok(true));
    assert_eq!(ov.set_text(at, "font.size", "16"), Ok(false));
    let eff = ov.effective(&apoth_config::Config::defaults(), &[at]);
    assert_eq!(eff.get("font.size"), Some(&Value::Float(16.0)));
}

#[test]
fn diff_classifies_reload() {
    let a = apoth_config::Config::defaults();
    let mut b = a.clone();
    assert!(diff(&a, &b).is_empty());
    let live = apoth_config::TABLE
        .iter()
        .find(|k| {
            k.reload == Reload::Live
                && !matches!(k.default_value(), Value::Null)
                && matches!(k.default_value(), Value::Bool(_))
        })
        .unwrap();
    let Value::Bool(cur) = live.default_value() else {
        unreachable!()
    };
    b.set_at(
        live.path,
        Value::Bool(!cur),
        apoth_config::model::Origin::builtin(),
    );
    let d = diff(&a, &b);
    assert_eq!(d.keys.len(), 1);
    assert_eq!(d.reload_class(), Reload::Live);
    assert!(d.collections.is_empty());
    let _ = Collection::Binds;
}

#[test]
fn editor_addresses_the_last_duplicate_and_unset_clears_all() {
    let mut ed = Editor::parse("font {\n    size 10\n}\nfont {\n    size 12\n}\n").unwrap();
    assert_eq!(ed.get("font.size").unwrap(), Some(Value::Float(12.0)));
    ed.set("font.size", &Value::Float(14.0)).unwrap();
    let t = ed.text();
    assert!(t.contains("size 10") && t.contains("size 14") && !t.contains("size 12"));
    assert!(ed.unset("font.size").unwrap());
    assert!(!ed.text().contains("size"));
}

#[test]
fn runtime_values_are_content_validated() {
    use apoth_config::types::Scope;
    let at = ScopeKey {
        scope: Scope::Global,
        id: 0,
    };
    let mut ov = RuntimeOverrides::new();
    let tmpl = apoth_config::TABLE.iter().find(|k| {
        matches!(k.ty, apoth_config::types::Ty::Template) && k.class.runtime_override_allowed()
    });
    if let Some(k) = tmpl {
        assert!(ov.set(at, k.path, Value::Str("{if".into())).is_err());
    }
    let mut ed = Editor::parse("").unwrap();
    assert!(
        ed.map_set("shell.env", "A", &["B\nC".to_string(), "x".to_string()])
            .is_err()
    );
    assert!(ed.map_set("shell.env", "A=B", &["v".to_string()]).is_err());
}

fn input(system: Option<&str>, user: &str) -> LoadInput {
    LoadInput {
        system: system.map(PathBuf::from),
        user: Some(PathBuf::from(user)),
        ..Default::default()
    }
}

#[test]
fn write_target_edits_an_included_user_file_in_place() {
    let fs = MemFs::new()
        .with("/h/apotheosis.kdl", "include \"inc.kdl\"\n")
        .with("/h/inc.kdl", "font {\n    size 10\n}\n");
    let cfg = load(&fs, &input(None, "/h/apotheosis.kdl"), None).config;
    let main = fs.read_to_string(Path::new("/h/apotheosis.kdl")).unwrap();
    let out = apply_write(
        &fs,
        &cfg,
        Path::new("/h/apotheosis.kdl"),
        &[],
        "font.size",
        Some(&Value::Float(15.0)),
        None,
    )
    .unwrap();
    assert_eq!(out.as_deref(), Some(Path::new("/h/inc.kdl")));
    assert!(
        fs.read_to_string(Path::new("/h/inc.kdl"))
            .unwrap()
            .contains("size 15")
    );
    assert_eq!(
        fs.read_to_string(Path::new("/h/apotheosis.kdl")).unwrap(),
        main
    );
}

#[test]
fn write_target_overrides_system_values_with_a_new_user_value() {
    let fs = MemFs::new()
        .with("/etc/a.kdl", "font {\n    size 10\n}\n")
        .with("/h/apotheosis.kdl", "version 1\n");
    let cfg = load(&fs, &input(Some("/etc/a.kdl"), "/h/apotheosis.kdl"), None).config;
    let out = apply_write(
        &fs,
        &cfg,
        Path::new("/h/apotheosis.kdl"),
        &[],
        "font.size",
        Some(&Value::Float(15.0)),
        None,
    )
    .unwrap();
    assert_eq!(out.as_deref(), Some(Path::new("/h/apotheosis.kdl")));
    assert_eq!(
        fs.read_to_string(Path::new("/etc/a.kdl")).unwrap(),
        "font {\n    size 10\n}\n",
        "/etc is never edited"
    );
    let r = load(&fs, &input(Some("/etc/a.kdl"), "/h/apotheosis.kdl"), None);
    assert_eq!(r.config.get("font.size"), Some(&Value::Float(15.0)));
}

#[test]
fn write_target_for_fragment_values_lands_in_the_main_user_file() {
    let fs = MemFs::new()
        .with("/usr/frag.kdl", "profile \"p\" {\n}\n")
        .with("/h/apotheosis.kdl", "version 1\n");
    let mut inp = input(None, "/h/apotheosis.kdl");
    inp.fragments = vec![PathBuf::from("/usr/frag.kdl")];
    let cfg = load(&fs, &inp, None).config;
    let t = apoth_config::edit::write_target(&cfg, Path::new("/h/apotheosis.kdl"), "font.size");
    assert_eq!(t, PathBuf::from("/h/apotheosis.kdl"));
}

#[test]
fn write_target_scoped_when_write_stays_inside_the_when_block() {
    let fs = MemFs::new().with(
        "/h/apotheosis.kdl",
        "when scheme=\"dark\" {\n    font {\n        size 10\n    }\n}\n",
    );
    let cfg = load(&fs, &input(None, "/h/apotheosis.kdl"), None).config;
    // The winning value is under `when`: an unscoped write goes to the main file, as a new
    // top-level node, never rewriting the conditional block.
    let before = fs.read_to_string(Path::new("/h/apotheosis.kdl")).unwrap();
    apply_write(
        &fs,
        &cfg,
        Path::new("/h/apotheosis.kdl"),
        &[],
        "font.size",
        Some(&Value::Float(15.0)),
        None,
    )
    .unwrap();
    let after = fs.read_to_string(Path::new("/h/apotheosis.kdl")).unwrap();
    assert!(after.starts_with(&before), "{after}");
    // A scoped write edits inside the block.
    apply_write(
        &fs,
        &cfg,
        Path::new("/h/apotheosis.kdl"),
        &[NodeSel::when(&[("scheme", "dark")])],
        "font.size",
        Some(&Value::Float(20.0)),
        None,
    )
    .unwrap();
    let scoped = fs.read_to_string(Path::new("/h/apotheosis.kdl")).unwrap();
    assert!(
        scoped.contains("size 20") && scoped.contains("size 15"),
        "{scoped}"
    );
}

/// Tiny deterministic generator so the property test needs no dependency.
struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
    fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }
}

#[test]
fn random_edits_never_touch_bytes_outside_the_edited_nodes() {
    // Never-edited regions: header, an oddly spaced block with comments, an unknown node.
    const HEADER: &str = "// HEADER  comment\n/* block */\nversion 1\n";
    const MIDDLE: &str = "cursor   {  // c1\n    style   \"bar\"   // c2\n}\n\n";
    const TAIL: &str = "// tail\nunknown-future-node   1   2 // keep\n";
    let base = format!("{HEADER}\nfont {{\n    size 12 // fs\n}}\n{MIDDLE}{TAIL}");
    type Gen = fn(&mut Rng) -> Value;
    let keys: [(&str, Gen); 3] = [
        ("font.size", |r| Value::Float(4.0 + r.below(80) as f64)),
        ("font.family", |r| {
            Value::Str(format!("Face {}", r.below(50)))
        }),
        ("scrollback.lines", |r| Value::Int(r.below(10_000) as i64)),
    ];
    let mut rng = Rng(0x9E37_79B9_7F4A_7C15);
    for _ in 0..300 {
        let mut ed = Editor::parse(&base).unwrap();
        let mut model: std::collections::HashMap<&str, Option<Value>> = Default::default();
        model.insert("font.size", Some(Value::Float(12.0)));
        for _ in 0..(1 + rng.below(25)) {
            let (path, gen_v) = keys[rng.below(keys.len())];
            if rng.below(3) == 0 {
                let had = model.get(path).is_some_and(Option::is_some);
                assert_eq!(ed.unset(path).unwrap(), had, "unset {path}");
                model.insert(path, None);
            } else {
                let v = gen_v(&mut rng);
                ed.set(path, &v).unwrap();
                model.insert(path, Some(v));
            }
            let t = ed.text();
            assert!(t.starts_with(HEADER), "header changed:\n{t}");
            assert!(t.contains(MIDDLE), "untouched block changed:\n{t}");
            assert!(t.contains(TAIL), "unknown node or tail changed:\n{t}");
            let reparsed = Editor::parse(&t).unwrap();
            for (p, want) in &model {
                assert_eq!(&reparsed.get(p).unwrap(), want, "{p} in\n{t}");
            }
        }
    }
}
